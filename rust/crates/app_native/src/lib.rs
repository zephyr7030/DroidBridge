#![deny(unsafe_op_in_unsafe_fn)]

mod android;
mod app_guard_recovery;
mod app_host;
mod automation_wake;
mod command;
mod guard;
mod mcp_listener;
mod network;
mod tunnel;
mod visual;

use app_host::{AppHostControl, recover_dead_magisk_host, start_host};

#[cfg(target_os = "android")]
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use chrono::{SecondsFormat, Utc};
pub(crate) use command::ApkCommandProcessPort;
use contract::{
    Availability, CapabilityState, ContextCall, ErrorCode, PublicError, PublicPayload,
    PublicResponse, RuntimeHost, ScalarValue, UuidV4,
};
use domain::{AdmissionFence, DomainError};
#[cfg(target_os = "android")]
use jni::{
    Env, JValue, JavaVM, jni_sig, jni_str,
    objects::{Global, JList},
};
use jni::{
    EnvUnowned, Outcome,
    objects::{JByteArray, JClass, JIntArray, JObjectArray, JString},
    sys::{JNI_FALSE, JNI_TRUE, jboolean, jbyteArray, jint, jlong, jstring},
};
use network::{ApkNetworkPort, GetifaddrsInterfaces};
#[cfg(test)]
use persistence::GuardProofDirectory;
#[cfg(test)]
use persistence::await_guard_recovery_plan;
use persistence::{
    CanonicalState, FIXTURE_2_MIB, FIXTURE_8_MIB, FaultFileStore, FaultRecord, FaultRole,
    JsonPersistencePort, LifetimeLease, ProcessFacts, RuntimeArtifactPort, RuntimeLive,
    RuntimeOwner, RuntimeTransitionIntent, StateStore, TransitionRecovery, decode_canonical_state,
    read_json, realistic_store_fixture,
};
#[cfg(unix)]
use persistence::{GuardIdentity, GuardRecovery, classify_guard_proof, encode_guard_frame};
use runtime::{
    AdmittedExecution, AndroidExecutionDispatch, AndroidFrameworkFilesystemPort,
    AndroidPrimitiveResult, ApkCapabilityPort, ApkRuntimeVertical, CompositeExecutionSurface,
    FilesystemPrimitiveDirectoryPage, FilesystemPrimitiveMetadata, FilesystemPrimitivePort,
    NativeCommandExecutionSurface, NativeFilesystemExecutionSurface, NativeNetworkExecutionSurface,
    NativeVisualExecutionSurface, RuntimeCore,
};
use serde::Serialize;
use std::{
    fs,
    path::{Path, PathBuf},
    ptr,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};
pub(crate) use visual::ApkVisualPort;

#[cfg(unix)]
use std::{
    collections::HashMap,
    os::{
        fd::{AsRawFd, FromRawFd, RawFd},
        unix::process::ExitStatusExt,
    },
    process::{Child, Command, Stdio},
};

const SHIZUKU_CANCEL_SIGNAL: i32 = 15;
const SHIZUKU_TIMEOUT_SIGNAL: i32 = 14;
#[cfg(any(unix, test))]
const MAX_SHIZUKU_GUARD_HANDLES: usize = 64;
#[cfg(unix)]
const SHIZUKU_CLIENT_CLEANUP_WAIT_MS: u64 = 6_000;

#[cfg(unix)]
pub(crate) fn block_guard_termination_signals() -> std::io::Result<()> {
    let mut signals = unsafe { std::mem::zeroed::<libc::sigset_t>() };
    if unsafe { libc::sigemptyset(&mut signals) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGALRM] {
        if unsafe { libc::sigaddset(&mut signals, signal) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    if unsafe { libc::sigprocmask(libc::SIG_BLOCK, &signals, ptr::null_mut()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(any(unix, test))]
fn validate_shizuku_launch(
    effective_uid: u32,
    native_library_directory: &Path,
    guard_path: &Path,
    active_handles: usize,
) -> bool {
    effective_uid == 2_000
        && guard_path == native_library_directory.join("libdroidbridge_exec_guard.so")
        && active_handles < MAX_SHIZUKU_GUARD_HANDLES
}

#[cfg(any(unix, test))]
fn validate_shizuku_arguments(
    client_id: &str,
    execution_id: &str,
    program: &str,
    arguments: &[String],
    cwd: &str,
) -> bool {
    let canonical_uuid =
        |value: &str| UuidV4::parse(value.to_owned()).is_ok_and(|parsed| parsed.as_str() == value);
    if !canonical_uuid(client_id)
        || !canonical_uuid(execution_id)
        || !program.starts_with('/')
        || program.is_empty()
        || program.len() > 4_096
        || program.contains('\0')
        || !cwd.starts_with('/')
        || cwd.is_empty()
        || cwd.len() > 4_096
        || cwd.contains('\0')
        || arguments.len() > 256
    {
        return false;
    }
    arguments
        .iter()
        .try_fold(0_usize, |total, argument| {
            if argument.len() > 16_384 || argument.contains('\0') {
                None
            } else {
                total.checked_add(argument.len())
            }
        })
        .is_some_and(|total| total <= 65_536)
}

#[cfg(unix)]
struct ShizukuGuardEntry {
    client_id: String,
    execution_id: String,
    pid: i32,
    child: Mutex<Option<Child>>,
    lifetime_writer: Mutex<Option<fs::File>>,
}

#[cfg(unix)]
struct ShizukuGuardRegistry {
    next_handle: u64,
    entries: HashMap<u64, Arc<ShizukuGuardEntry>>,
    owners: HashMap<(String, String), u64>,
}

#[cfg(unix)]
impl Default for ShizukuGuardRegistry {
    fn default() -> Self {
        Self {
            next_handle: 1,
            entries: HashMap::new(),
            owners: HashMap::new(),
        }
    }
}

#[cfg(unix)]
static SHIZUKU_GUARDS: OnceLock<Mutex<ShizukuGuardRegistry>> = OnceLock::new();

#[cfg(unix)]
fn shizuku_guard_registry() -> &'static Mutex<ShizukuGuardRegistry> {
    SHIZUKU_GUARDS.get_or_init(|| Mutex::new(ShizukuGuardRegistry::default()))
}

struct NativeHost {
    base: PathBuf,
    store: Arc<StateStore>,
    _lease: Arc<LifetimeLease>,
    runtime: ApkRuntimeVertical,
    core: ApkCore,
    /// The ArtifactStore this instance owns, which answers S-MCP-006 internal artifact queries.
    artifacts: RuntimeArtifactPort,
    async_runtime: tokio::runtime::Runtime,
    boot_id: UuidV4,
    runtime_instance_id: UuidV4,
    product_version: String,
    admission_open: AtomicBool,
    automation_wake: Arc<ApkAutomationWake>,
}

type ApkAutomationWake =
    automation_wake::ApkAlarmWake<AndroidFrameworkFilesystemDispatcher, ApkCapabilityPort>;

type ApkCore = RuntimeCore<
    JsonPersistencePort,
    RuntimeArtifactPort,
    AndroidExecutionSurface,
    ApkCapabilityPort,
    AppHostControl,
>;

type AndroidFilesystemSurface = NativeFilesystemExecutionSurface<
    RuntimeArtifactPort,
    ApkCapabilityPort,
    AndroidFrameworkFilesystemPort<AndroidFrameworkFilesystemDispatcher>,
    AndroidShizukuFilesystemPort,
>;
type AndroidCommandSurface =
    NativeCommandExecutionSurface<RuntimeArtifactPort, ApkCapabilityPort, ApkCommandProcessPort>;
/// The APK host's S-NET-001 network surface. Its App/framework facts travel the one
/// in-process `android.framework` bridge, its read-only supplement travels the Shizuku
/// filesystem primitive, and its capture bytes travel the filesystem surface it shares
/// with this host; the artifact store owns the default-network event record.
type AndroidNetworkSurface = NativeNetworkExecutionSurface<
    RuntimeArtifactPort,
    ApkCapabilityPort,
    ApkNetworkPort<
        AndroidFrameworkFilesystemDispatcher,
        GetifaddrsInterfaces,
        AndroidShizukuFilesystemPort,
    >,
>;
type AndroidExecutionSurface = CompositeExecutionSurface<
    AndroidFilesystemSurface,
    AndroidCommandSurface,
    AndroidNetworkSurface,
    AndroidVisualSurface,
    AndroidMotherToolSurface,
>;
type AndroidMotherToolSurface =
    runtime::NativeAndroidExecutionSurface<ApkCapabilityPort, android::ApkAndroidPort>;
type AndroidVisualSurface = NativeVisualExecutionSurface<
    RuntimeArtifactPort,
    ApkCapabilityPort,
    ApkVisualPort<ApkCapabilityPort>,
>;

/// The APK-hosted Android execution bridge: one typed primitive call into the
/// in-process Kotlin adapter registry of this same `:runtime` process.
#[derive(Clone, Copy)]
struct AndroidFrameworkFilesystemDispatcher;

#[derive(Clone, Copy)]
struct AndroidShizukuFilesystemPort;

#[cfg(target_os = "android")]
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ShizukuFilesystemMetadata {
    device: u64,
    inode: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    size: u64,
    modified_at_epoch_seconds: i64,
    #[serde(default)]
    selinux_context_base64: Option<String>,
}

#[cfg(target_os = "android")]
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ShizukuReadlinkResult {
    target: String,
}

#[cfg(target_os = "android")]
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ShizukuDirectoryResult {
    names_base64: Vec<String>,
    #[serde(default)]
    next_cookie: Option<u64>,
}

#[cfg(target_os = "android")]
impl AndroidExecutionDispatch for AndroidFrameworkFilesystemDispatcher {
    fn dispatch(
        &self,
        primitive: &str,
        payload: &[u8],
        execution: &AdmittedExecution,
    ) -> Result<AndroidPrimitiveResult, DomainError> {
        dispatch_android_execution(primitive, payload, execution)
    }
}

#[cfg(not(target_os = "android"))]
impl AndroidExecutionDispatch for AndroidFrameworkFilesystemDispatcher {
    fn dispatch(
        &self,
        _primitive: &str,
        _payload: &[u8],
        _execution: &AdmittedExecution,
    ) -> Result<AndroidPrimitiveResult, DomainError> {
        Err(DomainError::new(
            ErrorCode::CapabilityUnavailable,
            "Android framework is unavailable on this platform",
        ))
    }
}

impl FilesystemPrimitivePort for AndroidShizukuFilesystemPort {
    fn lstat(
        &self,
        execution: &AdmittedExecution,
        path: &Path,
    ) -> Result<FilesystemPrimitiveMetadata, DomainError> {
        #[cfg(target_os = "android")]
        {
            let path = path.to_str().ok_or_else(|| {
                DomainError::new(ErrorCode::InvalidArgument, "filesystem path is not UTF-8")
            })?;
            let payload = serde_json::to_vec(&serde_json::json!({
                "operation": "lstat",
                "path": path,
            }))
            .map_err(|_| {
                DomainError::new(
                    ErrorCode::InternalError,
                    "filesystem primitive encoding failed",
                )
            })?;
            let result = dispatch_android_execution_for(
                "shizuku.shell",
                "ShizukuFsPrimitive",
                &payload,
                execution,
            )?;
            if !result.descriptors.is_empty() {
                return Err(DomainError::new(
                    ErrorCode::IoError,
                    "filesystem lstat returned unexpected descriptors",
                ));
            }
            let metadata: ShizukuFilesystemMetadata = serde_json::from_slice(&result.payload)
                .map_err(|_| {
                    DomainError::new(ErrorCode::IoError, "filesystem lstat result is invalid")
                })?;
            let selinux_context = metadata
                .selinux_context_base64
                .map(|value| {
                    BASE64.decode(value).map_err(|_| {
                        DomainError::new(
                            ErrorCode::IoError,
                            "filesystem SELinux metadata is invalid",
                        )
                    })
                })
                .transpose()?;
            if selinux_context.as_ref().is_some_and(|value| {
                value.is_empty() || value.len() > 4_096 || value.last() != Some(&0)
            }) {
                return Err(DomainError::new(
                    ErrorCode::IoError,
                    "filesystem SELinux metadata is invalid",
                ));
            }
            Ok(FilesystemPrimitiveMetadata {
                device: metadata.device,
                inode: metadata.inode,
                mode: metadata.mode,
                uid: metadata.uid,
                gid: metadata.gid,
                size: metadata.size,
                modified_at_epoch_seconds: metadata.modified_at_epoch_seconds,
                selinux_context,
            })
        }
        #[cfg(not(target_os = "android"))]
        {
            let _ = (execution, path);
            Err(DomainError::new(
                ErrorCode::CapabilityUnavailable,
                "Shizuku filesystem is unavailable on this platform",
            ))
        }
    }

    fn open_read(
        &self,
        execution: &AdmittedExecution,
        path: &Path,
    ) -> Result<fs::File, DomainError> {
        #[cfg(target_os = "android")]
        {
            let path = path.to_str().ok_or_else(|| {
                DomainError::new(ErrorCode::InvalidArgument, "filesystem path is not UTF-8")
            })?;
            let payload = serde_json::to_vec(&serde_json::json!({
                "operation": "open_read",
                "path": path,
            }))
            .map_err(|_| {
                DomainError::new(
                    ErrorCode::InternalError,
                    "filesystem primitive encoding failed",
                )
            })?;
            let mut result = dispatch_android_execution_for(
                "shizuku.shell",
                "ShizukuFsPrimitive",
                &payload,
                execution,
            )?;
            if result.payload != br#"{"completed":true}"#
                || result.descriptors.len() != 1
                || result.descriptors[0].0 != "shizuku_path"
            {
                return Err(DomainError::new(
                    ErrorCode::IoError,
                    "filesystem open returned an invalid descriptor set",
                ));
            }
            let file = result.descriptors.remove(0).1;
            let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
            if flags < 0 || flags & libc::O_ACCMODE != libc::O_RDONLY {
                return Err(DomainError::new(
                    ErrorCode::PermissionDenied,
                    "filesystem descriptor is not read-only",
                ));
            }
            Ok(file)
        }
        #[cfg(not(target_os = "android"))]
        {
            let _ = (execution, path);
            Err(DomainError::new(
                ErrorCode::CapabilityUnavailable,
                "Shizuku filesystem is unavailable on this platform",
            ))
        }
    }

    fn read_directory(
        &self,
        execution: &AdmittedExecution,
        path: &Path,
        cookie: u64,
        limit: usize,
    ) -> Result<FilesystemPrimitiveDirectoryPage, DomainError> {
        #[cfg(target_os = "android")]
        {
            use std::os::unix::ffi::OsStringExt;

            if limit == 0 || limit > 5_001 {
                return Err(DomainError::new(
                    ErrorCode::InvalidArgument,
                    "filesystem directory page limit is invalid",
                ));
            }
            let result = dispatch_shizuku_filesystem(
                execution,
                serde_json::json!({
                    "operation": "read_directory",
                    "path": shizuku_path(path)?,
                    "cookie": cookie,
                    "limit": limit,
                }),
                None,
            )?;
            if !result.descriptors.is_empty() {
                return Err(DomainError::new(
                    ErrorCode::IoError,
                    "filesystem directory read returned unexpected descriptors",
                ));
            }
            let result: ShizukuDirectoryResult =
                serde_json::from_slice(&result.payload).map_err(|_| {
                    DomainError::new(ErrorCode::IoError, "filesystem directory result is invalid")
                })?;
            if result.names_base64.len() > limit
                || result.next_cookie.is_some_and(|next| next <= cookie)
            {
                return Err(DomainError::new(
                    ErrorCode::IoError,
                    "filesystem directory result is invalid",
                ));
            }
            let names = result
                .names_base64
                .into_iter()
                .map(|encoded| {
                    let bytes = BASE64.decode(encoded).map_err(|_| {
                        DomainError::new(ErrorCode::IoError, "filesystem directory name is invalid")
                    })?;
                    if bytes.is_empty()
                        || bytes == b"."
                        || bytes == b".."
                        || bytes.contains(&b'/')
                        || bytes.contains(&0)
                    {
                        return Err(DomainError::new(
                            ErrorCode::IoError,
                            "filesystem directory name is invalid",
                        ));
                    }
                    Ok(std::ffi::OsString::from_vec(bytes))
                })
                .collect::<Result<Vec<_>, DomainError>>()?;
            Ok(FilesystemPrimitiveDirectoryPage {
                names,
                next_cookie: result.next_cookie,
            })
        }
        #[cfg(not(target_os = "android"))]
        {
            let _ = (execution, path, cookie, limit);
            Err(shizuku_unavailable())
        }
    }

    fn access_write_search(
        &self,
        execution: &AdmittedExecution,
        path: &Path,
    ) -> Result<(), DomainError> {
        #[cfg(target_os = "android")]
        {
            shizuku_completed(
                dispatch_shizuku_filesystem(
                    execution,
                    serde_json::json!({
                        "operation": "access_write_search",
                        "path": shizuku_path(path)?,
                    }),
                    None,
                )?,
                "filesystem access check returned an invalid result",
            )
        }
        #[cfg(not(target_os = "android"))]
        {
            let _ = (execution, path);
            Err(shizuku_unavailable())
        }
    }

    fn create_exclusive(
        &self,
        execution: &AdmittedExecution,
        path: &Path,
        mode: u32,
    ) -> Result<fs::File, DomainError> {
        #[cfg(target_os = "android")]
        {
            let result = dispatch_shizuku_filesystem(
                execution,
                serde_json::json!({
                    "operation": "create_exclusive",
                    "path": shizuku_path(path)?,
                    "mode": mode,
                }),
                None,
            )?;
            shizuku_descriptor(result, libc::O_WRONLY)
        }
        #[cfg(not(target_os = "android"))]
        {
            let _ = (execution, path, mode);
            Err(shizuku_unavailable())
        }
    }

    fn apply_metadata(
        &self,
        execution: &AdmittedExecution,
        file: &fs::File,
        metadata: &FilesystemPrimitiveMetadata,
    ) -> Result<(), DomainError> {
        #[cfg(target_os = "android")]
        {
            shizuku_completed(
                dispatch_shizuku_filesystem(
                    execution,
                    serde_json::json!({
                        "operation": "apply_metadata",
                        "uid": metadata.uid,
                        "gid": metadata.gid,
                        "mode": metadata.mode & 0o7777,
                        "selinux_context_base64": metadata
                            .selinux_context
                            .as_deref()
                            .map(|value| BASE64.encode(value))
                            .unwrap_or_default(),
                    }),
                    Some(file),
                )?,
                "filesystem metadata update returned an invalid result",
            )
        }
        #[cfg(not(target_os = "android"))]
        {
            let _ = (execution, file, metadata);
            Err(shizuku_unavailable())
        }
    }

    fn rename_atomic(
        &self,
        execution: &AdmittedExecution,
        source: &Path,
        destination: &Path,
        exchange: bool,
    ) -> Result<(), DomainError> {
        #[cfg(target_os = "android")]
        {
            shizuku_completed(
                dispatch_shizuku_filesystem(
                    execution,
                    serde_json::json!({
                        "operation": "rename_atomic",
                        "source": shizuku_path(source)?,
                        "destination": shizuku_path(destination)?,
                        "exchange": exchange,
                    }),
                    None,
                )?,
                "filesystem rename returned an invalid result",
            )
        }
        #[cfg(not(target_os = "android"))]
        {
            let _ = (execution, source, destination, exchange);
            Err(shizuku_unavailable())
        }
    }

    fn fsync_directory(
        &self,
        execution: &AdmittedExecution,
        path: &Path,
    ) -> Result<(), DomainError> {
        #[cfg(target_os = "android")]
        {
            shizuku_completed(
                dispatch_shizuku_filesystem(
                    execution,
                    serde_json::json!({
                        "operation": "fsync_directory",
                        "path": shizuku_path(path)?,
                    }),
                    None,
                )?,
                "filesystem directory sync returned an invalid result",
            )
        }
        #[cfg(not(target_os = "android"))]
        {
            let _ = (execution, path);
            Err(shizuku_unavailable())
        }
    }

    fn mkdir(
        &self,
        execution: &AdmittedExecution,
        path: &Path,
        mode: u32,
    ) -> Result<(), DomainError> {
        #[cfg(target_os = "android")]
        {
            shizuku_completed(
                dispatch_shizuku_filesystem(
                    execution,
                    serde_json::json!({
                        "operation": "mkdir",
                        "path": shizuku_path(path)?,
                        "mode": mode,
                    }),
                    None,
                )?,
                "filesystem mkdir returned an invalid result",
            )
        }
        #[cfg(not(target_os = "android"))]
        {
            let _ = (execution, path, mode);
            Err(shizuku_unavailable())
        }
    }

    fn unlink(&self, execution: &AdmittedExecution, path: &Path) -> Result<(), DomainError> {
        #[cfg(target_os = "android")]
        {
            shizuku_completed(
                dispatch_shizuku_filesystem(
                    execution,
                    serde_json::json!({
                        "operation": "unlink",
                        "path": shizuku_path(path)?,
                    }),
                    None,
                )?,
                "filesystem unlink returned an invalid result",
            )
        }
        #[cfg(not(target_os = "android"))]
        {
            let _ = (execution, path);
            Err(shizuku_unavailable())
        }
    }

    fn readlink(&self, execution: &AdmittedExecution, path: &Path) -> Result<PathBuf, DomainError> {
        #[cfg(target_os = "android")]
        {
            let result = dispatch_shizuku_filesystem(
                execution,
                serde_json::json!({
                    "operation": "readlink",
                    "path": shizuku_path(path)?,
                }),
                None,
            )?;
            if !result.descriptors.is_empty() {
                return Err(DomainError::new(
                    ErrorCode::IoError,
                    "filesystem readlink returned unexpected descriptors",
                ));
            }
            let result: ShizukuReadlinkResult =
                serde_json::from_slice(&result.payload).map_err(|_| {
                    DomainError::new(ErrorCode::IoError, "filesystem readlink result is invalid")
                })?;
            Ok(PathBuf::from(result.target))
        }
        #[cfg(not(target_os = "android"))]
        {
            let _ = (execution, path);
            Err(shizuku_unavailable())
        }
    }

    fn symlink(
        &self,
        execution: &AdmittedExecution,
        target: &Path,
        destination: &Path,
    ) -> Result<(), DomainError> {
        #[cfg(target_os = "android")]
        {
            shizuku_completed(
                dispatch_shizuku_filesystem(
                    execution,
                    serde_json::json!({
                        "operation": "symlink",
                        "target": shizuku_path_value(target)?,
                        "destination": shizuku_path(destination)?,
                    }),
                    None,
                )?,
                "filesystem symlink returned an invalid result",
            )
        }
        #[cfg(not(target_os = "android"))]
        {
            let _ = (execution, target, destination);
            Err(shizuku_unavailable())
        }
    }
}

impl network::SupplementReader for AndroidShizukuFilesystemPort {
    fn read_supplement(
        &self,
        execution: &AdmittedExecution,
        path: &str,
        limit: usize,
    ) -> Result<(Vec<u8>, bool), DomainError> {
        #[cfg(target_os = "android")]
        {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct BoundedRead {
                content_base64: String,
                truncated: bool,
            }

            let invalid = || DomainError::new(ErrorCode::IoError, "bounded read reply is invalid");
            let payload = serde_json::to_vec(&serde_json::json!({
                "operation": "read_bounded",
                "path": path,
                "limit": limit,
            }))
            .map_err(|_| {
                DomainError::new(
                    ErrorCode::InternalError,
                    "filesystem primitive encoding failed",
                )
            })?;
            let result = dispatch_android_execution_for(
                "shizuku.shell",
                "ShizukuFsPrimitive",
                &payload,
                execution,
            )?;
            if !result.descriptors.is_empty() {
                return Err(invalid());
            }
            let read: BoundedRead =
                serde_json::from_slice(&result.payload).map_err(|_| invalid())?;
            let bytes = BASE64.decode(read.content_base64).map_err(|_| invalid())?;
            if bytes.len() > limit {
                return Err(invalid());
            }
            Ok((bytes, read.truncated))
        }
        #[cfg(not(target_os = "android"))]
        {
            let _ = (execution, path, limit);
            Err(DomainError::new(
                ErrorCode::CapabilityUnavailable,
                "Shizuku filesystem is unavailable on this platform",
            ))
        }
    }
}

#[cfg(target_os = "android")]
fn shizuku_path(path: &Path) -> Result<&str, DomainError> {
    let value = shizuku_path_value(path)?;
    if !path.is_absolute() {
        return Err(DomainError::new(
            ErrorCode::InvalidArgument,
            "filesystem primitive path is invalid",
        ));
    }
    Ok(value)
}

#[cfg(target_os = "android")]
fn shizuku_path_value(path: &Path) -> Result<&str, DomainError> {
    path.to_str()
        .ok_or_else(|| DomainError::new(ErrorCode::InvalidArgument, "filesystem path is not UTF-8"))
}

#[cfg(target_os = "android")]
fn dispatch_shizuku_filesystem(
    execution: &AdmittedExecution,
    payload: serde_json::Value,
    descriptor: Option<&fs::File>,
) -> Result<AndroidPrimitiveResult, DomainError> {
    let payload = serde_json::to_vec(&payload).map_err(|_| {
        DomainError::new(
            ErrorCode::InternalError,
            "filesystem primitive encoding failed",
        )
    })?;
    match descriptor {
        Some(descriptor) => dispatch_android_execution_for_with_descriptor(
            "shizuku.shell",
            "ShizukuFsPrimitive",
            &payload,
            execution,
            Some(("shizuku_path", descriptor.as_raw_fd())),
        ),
        None => dispatch_android_execution_for(
            "shizuku.shell",
            "ShizukuFsPrimitive",
            &payload,
            execution,
        ),
    }
}

#[cfg(target_os = "android")]
fn shizuku_completed(
    result: AndroidPrimitiveResult,
    invalid_message: &'static str,
) -> Result<(), DomainError> {
    if result.payload == br#"{"completed":true}"# && result.descriptors.is_empty() {
        Ok(())
    } else {
        Err(DomainError::new(ErrorCode::IoError, invalid_message))
    }
}

#[cfg(target_os = "android")]
fn shizuku_descriptor(
    mut result: AndroidPrimitiveResult,
    access: libc::c_int,
) -> Result<fs::File, DomainError> {
    if result.payload != br#"{"completed":true}"#
        || result.descriptors.len() != 1
        || result.descriptors[0].0 != "shizuku_path"
    {
        return Err(DomainError::new(
            ErrorCode::IoError,
            "filesystem open returned an invalid descriptor set",
        ));
    }
    let file = result.descriptors.remove(0).1;
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 || flags & libc::O_ACCMODE != access {
        return Err(DomainError::new(
            ErrorCode::PermissionDenied,
            "filesystem descriptor access mode is invalid",
        ));
    }
    Ok(file)
}

#[cfg(not(target_os = "android"))]
fn shizuku_unavailable() -> DomainError {
    DomainError::new(
        ErrorCode::CapabilityUnavailable,
        "Shizuku filesystem is unavailable on this platform",
    )
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct StartResult {
    ready: bool,
    runtime_epoch: UuidV4,
    host_generation: u64,
    runtime_instance_id: UuidV4,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct TransitionPrepareResult {
    prepared: bool,
    store_revision: u64,
    intent: RuntimeTransitionIntent,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct RemoteTransitionPrepareResult {
    prepared: bool,
    recovery: bool,
    intent: RuntimeTransitionIntent,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct DeviceBenchmarkResult {
    two_mib_p95_ms: u64,
    two_mib_p99_ms: u64,
    eight_mib_p95_ms: u64,
    eight_mib_p99_ms: u64,
    steady_commits: u64,
    steady_elapsed_ms: u64,
    steady_max_lock_wait_ms: u64,
}

static HOST: OnceLock<Mutex<Option<Arc<NativeHost>>>> = OnceLock::new();

#[cfg(target_os = "android")]
static ANDROID_EXECUTION_DISPATCHER: OnceLock<Global<JClass<'static>>> = OnceLock::new();

#[cfg(target_os = "android")]
fn initialize_android_execution_dispatcher(env: &mut Env<'_>) -> jni::errors::Result<()> {
    if ANDROID_EXECUTION_DISPATCHER.get().is_some() {
        return Ok(());
    }
    let class = env.find_class(jni_str!(
        "com/droidbridge/android/execution/android/NativeAndroidExecutionDispatcher"
    ))?;
    let global = env.new_global_ref(class)?;
    let _ = ANDROID_EXECUTION_DISPATCHER.set(global);
    Ok(())
}

#[cfg(target_os = "android")]
pub(crate) fn publish_task_activity(
    active_tasks: usize,
    canonical_revision: u64,
    runtime_epoch: &UuidV4,
) -> Result<(), DomainError> {
    let dispatcher = ANDROID_EXECUTION_DISPATCHER.get().ok_or_else(|| {
        DomainError::new(
            ErrorCode::CapabilityUnavailable,
            "Android task activity dispatcher is unavailable",
        )
    })?;
    let active_tasks = i64::try_from(active_tasks)
        .map_err(|_| DomainError::new(ErrorCode::ResourceLimit, "active Task count overflow"))?;
    let canonical_revision = i64::try_from(canonical_revision)
        .map_err(|_| DomainError::new(ErrorCode::ResourceLimit, "canonical revision overflow"))?;
    let vm = JavaVM::singleton()
        .map_err(|_| DomainError::new(ErrorCode::InternalError, "Java VM is unavailable"))?;
    vm.attach_current_thread(|env| -> jni::errors::Result<()> {
        let runtime_epoch = env.new_string(runtime_epoch.as_str())?;
        env.call_static_method(
            &**dispatcher,
            jni_str!("taskActivityChanged"),
            jni_sig!("(Ljava/lang/String;JJ)V"),
            &[
                JValue::Object(runtime_epoch.as_ref()),
                JValue::Long(active_tasks),
                JValue::Long(canonical_revision),
            ],
        )?;
        Ok(())
    })
    .map_err(|_| DomainError::new(ErrorCode::IoError, "Android task activity update failed"))
}

#[cfg(not(target_os = "android"))]
pub(crate) fn publish_task_activity(
    _active_tasks: usize,
    _canonical_revision: u64,
    _runtime_epoch: &UuidV4,
) -> Result<(), DomainError> {
    Ok(())
}

#[cfg(target_os = "android")]
fn dispatch_android_execution(
    primitive: &str,
    payload: &[u8],
    execution: &AdmittedExecution,
) -> Result<AndroidPrimitiveResult, DomainError> {
    dispatch_android_execution_for("android.framework", primitive, payload, execution)
}

#[cfg(target_os = "android")]
fn dispatch_android_execution_for(
    key: &str,
    primitive: &str,
    payload: &[u8],
    execution: &AdmittedExecution,
) -> Result<AndroidPrimitiveResult, DomainError> {
    dispatch_android_execution_for_with_descriptor(key, primitive, payload, execution, None)
}

#[cfg(not(target_os = "android"))]
fn dispatch_android_execution_for(
    _key: &str,
    _primitive: &str,
    _payload: &[u8],
    _execution: &AdmittedExecution,
) -> Result<AndroidPrimitiveResult, DomainError> {
    Err(DomainError::new(
        ErrorCode::CapabilityUnavailable,
        "Android execution primitives require Android",
    ))
}

#[cfg(target_os = "android")]
fn dispatch_android_execution_for_with_descriptor(
    key: &str,
    primitive: &str,
    payload: &[u8],
    execution: &AdmittedExecution,
    descriptor: Option<(&str, i32)>,
) -> Result<AndroidPrimitiveResult, DomainError> {
    let dispatcher = ANDROID_EXECUTION_DISPATCHER.get().ok_or_else(|| {
        DomainError::new(
            ErrorCode::CapabilityUnavailable,
            "Android execution dispatcher is unavailable",
        )
    })?;
    let vm = JavaVM::singleton()
        .map_err(|_| DomainError::new(ErrorCode::InternalError, "Java VM is unavailable"))?;
    let (error_code, result) = vm
        .attach_current_thread(|env| {
            dispatch_android_execution_jni(
                env, dispatcher, key, primitive, payload, execution, descriptor,
            )
        })
        .map_err(|_| DomainError::new(ErrorCode::IoError, "Android execution bridge failed"))?;
    if let Some(error_code) = error_code {
        return Err(DomainError::new(
            android_execution_error_code(&error_code),
            "Android execution adapter rejected the request",
        ));
    }
    result.ok_or_else(|| {
        DomainError::new(
            ErrorCode::CapabilityUnavailable,
            "Android execution adapter generation is unavailable",
        )
    })
}

#[cfg(not(target_os = "android"))]
fn dispatch_android_execution_for_with_descriptor(
    _key: &str,
    _primitive: &str,
    _payload: &[u8],
    _execution: &AdmittedExecution,
    _descriptor: Option<(&str, i32)>,
) -> Result<AndroidPrimitiveResult, DomainError> {
    Err(DomainError::new(
        ErrorCode::CapabilityUnavailable,
        "Android execution primitives require Android",
    ))
}

#[cfg(target_os = "android")]
fn dispatch_android_execution_jni(
    env: &mut Env<'_>,
    dispatcher: &Global<JClass<'static>>,
    key: &str,
    primitive: &str,
    payload: &[u8],
    execution: &AdmittedExecution,
    descriptor: Option<(&str, i32)>,
) -> jni::errors::Result<(Option<String>, Option<AndroidPrimitiveResult>)> {
    let key = env.new_string(key)?;
    let primitive = env.new_string(primitive)?;
    let payload = env.byte_array_from_slice(payload)?;
    let execution_id = env.new_string(execution.execution_id.as_str())?;
    let runtime_epoch = env.new_string(execution.executor.fence.runtime_epoch.as_str())?;
    let runtime_instance_id =
        env.new_string(execution.executor.fence.runtime_instance_id.as_str())?;
    let generation = i64::try_from(execution.executor.capability_generation)
        .map_err(|_| jni::errors::Error::JniCall(jni::errors::JniError::InvalidArguments))?;
    let host_generation = i64::try_from(execution.executor.fence.host_generation)
        .map_err(|_| jni::errors::Error::JniCall(jni::errors::JniError::InvalidArguments))?;
    let result = if let Some((role, descriptor_fd)) = descriptor {
        let role = env.new_string(role)?;
        env.call_static_method(
            &**dispatcher,
            jni_str!("executeWithDescriptor"),
            jni_sig!("(Ljava/lang/String;JLjava/lang/String;[BLjava/lang/String;Ljava/lang/String;JLjava/lang/String;Ljava/lang/String;I)Lcom/droidbridge/android/execution/android/AndroidExecutionResult;"),
            &[
                JValue::Object(key.as_ref()),
                JValue::Long(generation),
                JValue::Object(primitive.as_ref()),
                JValue::Object(payload.as_ref()),
                JValue::Object(execution_id.as_ref()),
                JValue::Object(runtime_epoch.as_ref()),
                JValue::Long(host_generation),
                JValue::Object(runtime_instance_id.as_ref()),
                JValue::Object(role.as_ref()),
                JValue::Int(descriptor_fd),
            ],
        )?
    } else {
        env.call_static_method(
            &**dispatcher,
            jni_str!("execute"),
            jni_sig!("(Ljava/lang/String;JLjava/lang/String;[BLjava/lang/String;Ljava/lang/String;JLjava/lang/String;)Lcom/droidbridge/android/execution/android/AndroidExecutionResult;"),
            &[
                JValue::Object(key.as_ref()),
                JValue::Long(generation),
                JValue::Object(primitive.as_ref()),
                JValue::Object(payload.as_ref()),
                JValue::Object(execution_id.as_ref()),
                JValue::Object(runtime_epoch.as_ref()),
                JValue::Long(host_generation),
                JValue::Object(runtime_instance_id.as_ref()),
            ],
        )?
    }
    .into_object()?;
    if result.is_null() {
        return Ok((None, None));
    }
    let error = env
        .call_method(
            &result,
            jni_str!("getErrorCode"),
            jni_sig!("()Ljava/lang/String;"),
            &[],
        )?
        .into_object()?;
    let error_code = if error.is_null() {
        None
    } else {
        let error = env.cast_local::<JString>(error)?;
        Some(error.mutf8_chars(env)?.to_str().into_owned())
    };
    if error_code.is_some() {
        return Ok((error_code, None));
    }
    let payload = env
        .call_method(&result, jni_str!("getPayload"), jni_sig!("()[B"), &[])?
        .into_object()?;
    let payload = env.cast_local::<JByteArray>(payload)?;
    let payload = env.convert_byte_array(&payload)?;
    let descriptors = env
        .call_method(
            &result,
            jni_str!("getDescriptors"),
            jni_sig!("()Ljava/util/List;"),
            &[],
        )?
        .into_object()?;
    let descriptors = env.cast_local::<JList>(descriptors)?;
    let descriptor_count = usize::try_from(descriptors.size(env)?)
        .map_err(|_| jni::errors::Error::JniCall(jni::errors::JniError::InvalidArguments))?;
    if descriptor_count > 4 {
        return Err(jni::errors::Error::JniCall(
            jni::errors::JniError::InvalidArguments,
        ));
    }
    let mut decoded = Vec::with_capacity(descriptor_count);
    for index in 0..descriptor_count {
        let descriptor = descriptors.get(env, index as i32)?;
        let role = env
            .call_method(
                &descriptor,
                jni_str!("getRole"),
                jni_sig!("()Ljava/lang/String;"),
                &[],
            )?
            .into_object()?;
        let role = env.cast_local::<JString>(role)?;
        let role = role.mutf8_chars(env)?.to_str().into_owned();
        let parcel = env
            .call_method(
                &descriptor,
                jni_str!("getDescriptor"),
                jni_sig!("()Landroid/os/ParcelFileDescriptor;"),
                &[],
            )?
            .into_object()?;
        let fd = env
            .call_method(&parcel, jni_str!("detachFd"), jni_sig!("()I"), &[])?
            .into_int()?;
        if fd < 0 {
            return Err(jni::errors::Error::JniCall(
                jni::errors::JniError::InvalidArguments,
            ));
        }
        let file = unsafe { fs::File::from_raw_fd(fd) };
        decoded.push((role, file));
    }
    Ok((
        None,
        Some(AndroidPrimitiveResult {
            payload,
            descriptors: decoded,
        }),
    ))
}

#[cfg(any(target_os = "android", test))]
fn android_execution_error_code(value: &str) -> ErrorCode {
    serde_json::from_value(serde_json::Value::String(value.to_owned()))
        .unwrap_or(ErrorCode::InternalError)
}

fn host_slot() -> &'static Mutex<Option<Arc<NativeHost>>> {
    HOST.get_or_init(|| Mutex::new(None))
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeStart(
    mut env: EnvUnowned,
    _class: JClass,
    canonical_base: JString,
    environment_json: JString,
) -> jstring {
    match env
        .with_env(|owned| -> jni::errors::Result<jstring> {
            #[cfg(target_os = "android")]
            initialize_android_execution_dispatcher(owned)?;
            let base = canonical_base.mutf8_chars(owned)?.to_str().into_owned();
            let environment = environment_json.mutf8_chars(owned)?.to_str().into_owned();
            let encoded = match start_host(PathBuf::from(base), &environment) {
                Ok(value) => serde_json::to_string(&value),
                Err(error) => serde_json::to_string(&serde_json::json!({
                    "ready": false,
                    "code": error_code_token(error.code),
                })),
            }
            .unwrap_or_else(|_| "{\"ready\":false,\"code\":\"INTERNAL_ERROR\"}".to_owned());
            Ok(owned.new_string(encoded)?.into_raw())
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeRecoverDeadMagiskHost(
    mut env: EnvUnowned,
    _class: JClass,
    canonical_base: JString,
    environment_json: JString,
) -> jstring {
    match env
        .with_env(|owned| -> jni::errors::Result<jstring> {
            let base = canonical_base.mutf8_chars(owned)?.to_str().into_owned();
            let environment = environment_json.mutf8_chars(owned)?.to_str().into_owned();
            let encoded = match recover_dead_magisk_host(PathBuf::from(base), &environment) {
                Ok(value) => serde_json::to_string(&value),
                Err(error) => serde_json::to_string(&serde_json::json!({
                    "ready": false,
                    "code": error_code_token(error.code),
                })),
            }
            .unwrap_or_else(|_| "{\"ready\":false,\"code\":\"INTERNAL_ERROR\"}".to_owned());
            Ok(owned.new_string(encoded)?.into_raw())
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeSubmit(
    mut env: EnvUnowned,
    _class: JClass,
    envelope: JByteArray,
) -> jbyteArray {
    match env
        .with_env(|owned| -> jni::errors::Result<jbyteArray> {
            let bytes = owned.convert_byte_array(&envelope)?;
            let response = with_host(|host| submit_apk_public(host, &bytes))
                .unwrap_or_else(|error| native_error_envelope(error.code, &bytes));
            Ok(owned.byte_array_from_slice(&response)?.into_raw())
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => ptr::null_mut(),
    }
}

fn submit_apk_public(host: &NativeHost, encoded: &[u8]) -> Result<Vec<u8>, DomainError> {
    host.store.validate_lease(&host._lease)?;
    let now = Utc::now();
    let now_ms = u64::try_from(now.timestamp_millis())
        .map_err(|_| DomainError::new(ErrorCode::InternalError, "system time is invalid"))?;
    let admission_open = host.admission_open.load(Ordering::SeqCst);
    Ok(host.async_runtime.block_on(runtime::submit_public(
        &host.core,
        encoded,
        now.to_rfc3339_opts(SecondsFormat::Millis, true),
        now_ms,
        admission_open,
        |request| {
            std::future::ready(
                if admission_open
                    || matches!(
                        &request.payload,
                        PublicPayload::Context {
                            call: ContextCall::Status(_)
                        }
                    )
                {
                    host.runtime.dispatch_installed(request)
                } else {
                    Err(DomainError::new(
                        ErrorCode::HostTransitionPending,
                        "Runtime host transition is pending",
                    ))
                },
            )
        },
    )))
}

/// Answers one S-MCP-006 internal artifact query from this APK Runtime's ArtifactStore. The reply
/// is the host payload; a `read` also stores its one read-only descriptor in `descriptor[0]`, whose
/// ownership passes to the caller. A host failure is the typed `{error:{code}}` reply.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeQueryArtifacts(
    mut env: EnvUnowned,
    _class: JClass,
    query: JByteArray,
    descriptor: JIntArray,
) -> jbyteArray {
    match env
        .with_env(|owned| -> jni::errors::Result<jbyteArray> {
            let bytes = owned.convert_byte_array(&query)?;
            let (payload, file) = match with_host(|host| query_apk_artifacts(host, &bytes)) {
                Ok(reply) => (reply.payload, reply.descriptor),
                Err(error) => (
                    serde_json::json!({
                        "error": {"code": error_code_token(error.code), "retryable": false},
                    }),
                    None,
                ),
            };
            let encoded = serde_json::to_vec(&payload).map_err(|_| {
                jni::errors::Error::JniCall(jni::errors::JniError::InvalidArguments)
            })?;
            let array = owned.byte_array_from_slice(&encoded)?;
            if let Some(file) = file {
                hand_out_descriptor(owned, &descriptor, file)?;
            }
            Ok(array.into_raw())
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => ptr::null_mut(),
    }
}

/// S-UI-017 `getMaintenanceState` facts, read from the canonical files and guard proofs alone.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeMaintenanceState(
    mut env: EnvUnowned,
    _class: JClass,
    canonical_base: JString,
) -> jstring {
    match env
        .with_env(|owned| -> jni::errors::Result<jstring> {
            let base = canonical_base.mutf8_chars(owned)?.to_str().into_owned();
            let encoded = maintenance_state(Path::new(&base))
                .unwrap_or_else(|error| maintenance_failure(error.code, false));
            Ok(owned.new_string(encoded)?.into_raw())
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => ptr::null_mut(),
    }
}

/// The S-AUTH-001 malformed-owner reset behind S-UI-017 `resetRuntimeHostToApk`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeResetRuntimeHostToApk(
    mut env: EnvUnowned,
    _class: JClass,
    canonical_base: JString,
) -> jstring {
    match env
        .with_env(|owned| -> jni::errors::Result<jstring> {
            let base = canonical_base.mutf8_chars(owned)?.to_str().into_owned();
            let base = Path::new(&base);
            let encoded = read_boot_id()
                .and_then(|boot_id| persistence::guard_cleanup_verified(base, &boot_id, &ProcFacts))
                .and_then(|cleanup| {
                    StateStore::new(base.to_path_buf()).reset_malformed_owner(new_uuid()?, cleanup)
                })
                .map(|_| serde_json::json!({"reset": true}).to_string())
                .unwrap_or_else(|error| maintenance_failure(error.code, false));
            Ok(owned.new_string(encoded)?.into_raw())
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => ptr::null_mut(),
    }
}

/// The S-UPD-006 Runtime-data reset behind S-UI-017 `resetRuntimeData`. A failure reports whether
/// How many executions a lost Runtime instance left running (zero while a live Runtime owns the
/// store); -1 when the store cannot be read.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeStrandedExecutions(
    mut env: EnvUnowned,
    _class: JClass,
    canonical_base: JString,
) -> jint {
    match env
        .with_env(|owned| -> jni::errors::Result<jint> {
            let base = canonical_base.mutf8_chars(owned)?.to_str().into_owned();
            Ok(persistence::stranded_execution_count(Path::new(&base))
                .ok()
                .and_then(|count| jint::try_from(count).ok())
                .unwrap_or(-1))
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => -1,
    }
}

/// Settles the stranded executions as interrupted: `{"cleared":n}` or `{"code":...}`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeClearStrandedExecutions(
    mut env: EnvUnowned,
    _class: JClass,
    canonical_base: JString,
) -> jstring {
    match env
        .with_env(|owned| -> jni::errors::Result<jstring> {
            let base = canonical_base.mutf8_chars(owned)?.to_str().into_owned();
            let outcome = runtime::AutomationClock::wall(&runtime::BoottimeClock).and_then(
                |(ended_at, now_ms)| {
                    persistence::clear_stranded_executions(
                        Path::new(&base),
                        &ended_at,
                        now_ms,
                        std::time::Duration::from_secs(20),
                    )
                },
            );
            let reply = match outcome {
                Ok(cleared) => serde_json::json!({"cleared": cleared}),
                Err(error) => serde_json::json!({"code": error_code_token(error.code)}),
            };
            Ok(owned.new_string(reply.to_string())?.into_raw())
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => ptr::null_mut(),
    }
}

/// the live APK instance was already released, so HostController knows whether it may restore it.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeResetRuntimeData(
    mut env: EnvUnowned,
    _class: JClass,
    canonical_base: JString,
) -> jstring {
    match env
        .with_env(|owned| -> jni::errors::Result<jstring> {
            let base = canonical_base.mutf8_chars(owned)?.to_str().into_owned();
            Ok(owned
                .new_string(reset_runtime_data(Path::new(&base)))?
                .into_raw())
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => ptr::null_mut(),
    }
}

fn maintenance_state(base: &Path) -> Result<String, DomainError> {
    let blocker = StateStore::new(base.to_path_buf()).maintenance_blocker()?;
    let cleanup = persistence::guard_cleanup_verified(base, &read_boot_id()?, &ProcFacts)?;
    Ok(serde_json::json!({
        "schema_version": 1,
        "blocker": blocker.token(),
        "cleanup": if cleanup { "verified" } else { "unverified" },
    })
    .to_string())
}

fn maintenance_failure(code: ErrorCode, released: bool) -> String {
    serde_json::json!({"code": error_code_token(code), "released": released}).to_string()
}

fn reset_runtime_data(base: &Path) -> String {
    let mut released = false;
    let outcome = (|| -> Result<(), DomainError> {
        if base.join(UPDATE_MAINTENANCE_RECORD).exists() {
            return Err(DomainError::new(
                ErrorCode::HostTransitionPending,
                "update maintenance is recorded",
            ));
        }
        let store = StateStore::new(base.to_path_buf());
        if !base.join("runtime-reset-intent.json").exists() {
            let mut slot = host_slot().lock().map_err(|_| {
                DomainError::new(ErrorCode::InternalError, "native host lock failed")
            })?;
            match slot.clone() {
                Some(host) => {
                    record_live_reset_intent(&host)?;
                    // The released instance's lease closes with its last reference.
                    *slot = None;
                    drop(host);
                    released = true;
                }
                None => {
                    drop(slot);
                    if store.maintenance_blocker()? != persistence::MaintenanceBlocker::StoreCorrupt
                    {
                        return Err(DomainError::new(
                            ErrorCode::HostTransitionPending,
                            "Runtime reset needs the active APK Runtime or a corrupt store",
                        ));
                    }
                    let owner = store.read_owner()?;
                    let cleanup =
                        persistence::guard_cleanup_verified(base, &read_boot_id()?, &ProcFacts)?;
                    store.record_corrupt_store_reset_intent(&reset_intent_for(&owner)?, cleanup)?;
                }
            }
        }
        await_live_lock_release(base)?;
        let cleanup = persistence::guard_cleanup_verified(base, &read_boot_id()?, &ProcFacts)?;
        store.recover_confirmed_reset(cleanup).map(|_| ())
    })();
    match outcome {
        Ok(()) => serde_json::json!({"reset": true}).to_string(),
        Err(error) => maintenance_failure(error.code, released),
    }
}

/// Closes admission on the live APK instance, proves zero work and clean guards, and records the
/// reset intent under its lease; any refusal reopens the unchanged instance.
fn record_live_reset_intent(host: &NativeHost) -> Result<(), DomainError> {
    host.store.validate_lease(&host._lease)?;
    host.admission_open.store(false, Ordering::SeqCst);
    let recorded = (|| {
        require_idle_apk_runtime(host)?;
        let owner = host.store.read_owner()?;
        host.store
            .record_reset_intent(&host._lease, &reset_intent_for(&owner)?, true, true)
    })();
    if recorded.is_err() {
        host.admission_open.store(true, Ordering::SeqCst);
    }
    recorded
}

fn reset_intent_for(owner: &RuntimeOwner) -> Result<persistence::RuntimeResetIntent, DomainError> {
    Ok(persistence::RuntimeResetIntent {
        schema_version: 1,
        reset_id: new_uuid()?,
        runtime_epoch: owner.runtime_epoch.clone(),
        source_host_generation: owner.host_generation,
        target_host: RuntimeHost::ApkRuntime,
        target_host_generation: owner.host_generation.checked_add(1).ok_or_else(|| {
            DomainError::new(ErrorCode::ResourceLimit, "host generation exhausted")
        })?,
    })
}

/// Waits a bounded time for the released instance's live lock, so a lingering reference fails the
/// reset explicitly (leaving its intent for forward recovery) instead of blocking the caller.
fn await_live_lock_release(base: &Path) -> Result<(), DomainError> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if persistence::FileLock::try_acquire(&base.join("runtime-live.lock"))?.is_some() {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(DomainError::new(
                ErrorCode::IoError,
                "the released Runtime instance still holds the live lock",
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[cfg(unix)]
fn hand_out_descriptor(
    env: &mut jni::Env<'_>,
    slot: &JIntArray,
    file: fs::File,
) -> jni::errors::Result<()> {
    use std::os::fd::IntoRawFd;
    slot.set_region(env, 0, &[file.as_raw_fd()])?;
    // Ownership moves to the caller only once the slot holds the descriptor.
    let _ = file.into_raw_fd();
    Ok(())
}

#[cfg(not(unix))]
fn hand_out_descriptor(
    _env: &mut jni::Env<'_>,
    _slot: &JIntArray,
    _file: fs::File,
) -> jni::errors::Result<()> {
    Err(jni::errors::Error::JniCall(
        jni::errors::JniError::InvalidArguments,
    ))
}

fn query_apk_artifacts(
    host: &NativeHost,
    encoded: &[u8],
) -> Result<runtime::McpArtifactReply, DomainError> {
    host.store.validate_lease(&host._lease)?;
    let query = serde_json::from_slice(encoded)
        .map_err(|_| DomainError::invalid("artifact query is not JSON"))?;
    let now_ms = u64::try_from(Utc::now().timestamp_millis())
        .map_err(|_| DomainError::new(ErrorCode::InternalError, "system time is invalid"))?;
    host.artifacts.answer_mcp_query(&query, now_ms)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeObserveOwner(
    mut env: EnvUnowned,
    _class: JClass,
    canonical_base: JString,
) -> jstring {
    match env
        .with_env(|owned| -> jni::errors::Result<jstring> {
            let base = canonical_base.mutf8_chars(owned)?.to_str().into_owned();
            let encoded = StateStore::new(PathBuf::from(base))
                .read_owner()
                .and_then(|owner| {
                    serde_json::to_string(&owner).map_err(|_| {
                        DomainError::new(ErrorCode::InternalError, "owner encoding failed")
                    })
                })
                .unwrap_or_else(|error| transition_error(error.code));
            Ok(owned.new_string(encoded)?.into_raw())
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeObserveHostTransition(
    mut env: EnvUnowned,
    _class: JClass,
    canonical_base: JString,
) -> jstring {
    match env
        .with_env(|owned| -> jni::errors::Result<jstring> {
            let base = canonical_base.mutf8_chars(owned)?.to_str().into_owned();
            let encoded = StateStore::new(PathBuf::from(base))
                .observe_transition()
                .and_then(|observation| {
                    serde_json::to_string(&match observation {
                        None => serde_json::json!({"state":"none"}),
                        Some((TransitionRecovery::RemoveUncommittedIntent, intent)) => {
                            serde_json::json!({"state":"source_pending","intent":intent})
                        }
                        Some((TransitionRecovery::ActivateCommittedTarget, intent)) => {
                            serde_json::json!({"state":"target_committed","intent":intent})
                        }
                    })
                    .map_err(|_| {
                        DomainError::new(ErrorCode::InternalError, "transition encoding failed")
                    })
                })
                .unwrap_or_else(|error| transition_error(error.code));
            Ok(owned.new_string(encoded)?.into_raw())
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativePrepareHostTransition(
    mut env: EnvUnowned,
    _class: JClass,
    target_host: JString,
) -> jstring {
    match env
        .with_env(|owned| -> jni::errors::Result<jstring> {
            let target = target_host.mutf8_chars(owned)?.to_str().into_owned();
            let encoded = prepare_host_transition(&target)
                .and_then(|value| {
                    serde_json::to_string(&value).map_err(|_| {
                        DomainError::new(ErrorCode::InternalError, "transition encoding failed")
                    })
                })
                .unwrap_or_else(|error| transition_error(error.code));
            Ok(owned.new_string(encoded)?.into_raw())
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativePrepareRemoteHostTransition(
    mut env: EnvUnowned,
    _class: JClass,
    canonical_base: JString,
    target_host: JString,
) -> jstring {
    match env
        .with_env(|owned| -> jni::errors::Result<jstring> {
            let base = canonical_base.mutf8_chars(owned)?.to_str().into_owned();
            let target = target_host.mutf8_chars(owned)?.to_str().into_owned();
            let encoded = prepare_remote_host_transition(Path::new(&base), &target)
                .and_then(|value| {
                    serde_json::to_string(&value).map_err(|_| {
                        DomainError::new(ErrorCode::InternalError, "transition encoding failed")
                    })
                })
                .unwrap_or_else(|error| transition_error(error.code));
            Ok(owned.new_string(encoded)?.into_raw())
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeAbortRemoteHostTransition(
    mut env: EnvUnowned,
    _class: JClass,
    canonical_base: JString,
    intent_json: JString,
) -> jboolean {
    match env
        .with_env(|owned| -> jni::errors::Result<jboolean> {
            let base = canonical_base.mutf8_chars(owned)?.to_str().into_owned();
            let encoded = intent_json.mutf8_chars(owned)?.to_str().into_owned();
            let result = serde_json::from_str::<RuntimeTransitionIntent>(&encoded)
                .map_err(|_| DomainError::invalid("invalid transition intent"))
                .and_then(|intent| {
                    StateStore::new(PathBuf::from(base)).abort_remote_transition_intent(&intent)
                });
            Ok(if result.is_ok() { JNI_TRUE } else { JNI_FALSE })
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => JNI_FALSE,
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeAbortHostTransition(
    mut env: EnvUnowned,
    _class: JClass,
    intent_json: JString,
) -> jboolean {
    match env
        .with_env(|owned| -> jni::errors::Result<jboolean> {
            let encoded = intent_json.mutf8_chars(owned)?.to_str().into_owned();
            let intent = serde_json::from_str(&encoded).ok();
            let result = intent.is_some_and(|intent| abort_host_transition(&intent).is_ok());
            Ok(if result { JNI_TRUE } else { JNI_FALSE })
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => JNI_FALSE,
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeReleaseHostTransition(
    mut env: EnvUnowned,
    _class: JClass,
    intent_json: JString,
) -> jboolean {
    match env
        .with_env(|owned| -> jni::errors::Result<jboolean> {
            let encoded = intent_json.mutf8_chars(owned)?.to_str().into_owned();
            let intent = serde_json::from_str(&encoded).ok();
            let result = intent.is_some_and(|intent| release_host_transition(&intent).is_ok());
            Ok(if result { JNI_TRUE } else { JNI_FALSE })
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => JNI_FALSE,
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeCommitHostTransition(
    mut env: EnvUnowned,
    _class: JClass,
    canonical_base: JString,
    intent_json: JString,
) -> jstring {
    match env
        .with_env(|owned| -> jni::errors::Result<jstring> {
            let base = canonical_base.mutf8_chars(owned)?.to_str().into_owned();
            let encoded = intent_json.mutf8_chars(owned)?.to_str().into_owned();
            let result = serde_json::from_str::<RuntimeTransitionIntent>(&encoded)
                .map_err(|_| DomainError::invalid("invalid transition intent"))
                .and_then(|intent| {
                    StateStore::new(PathBuf::from(base)).commit_owner_transition(
                        &intent,
                        &read_boot_id()?,
                        &ProcFacts,
                    )
                })
                .and_then(|owner| {
                    serde_json::to_string(&owner).map_err(|_| {
                        DomainError::new(ErrorCode::InternalError, "owner encoding failed")
                    })
                })
                .unwrap_or_else(|error| transition_error(error.code));
            Ok(owned.new_string(result)?.into_raw())
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeFinishHostTransition(
    mut env: EnvUnowned,
    _class: JClass,
    canonical_base: JString,
    intent_json: JString,
    target_instance_id: JString,
) -> jboolean {
    match env
        .with_env(|owned| -> jni::errors::Result<jboolean> {
            let base = canonical_base.mutf8_chars(owned)?.to_str().into_owned();
            let encoded = intent_json.mutf8_chars(owned)?.to_str().into_owned();
            let instance = target_instance_id.mutf8_chars(owned)?.to_str().into_owned();
            let result = serde_json::from_str::<RuntimeTransitionIntent>(&encoded)
                .map_err(|_| DomainError::invalid("invalid transition intent"))
                .and_then(|intent| {
                    let instance = UuidV4::parse(instance)
                        .map_err(|_| DomainError::invalid("invalid target instance id"))?;
                    StateStore::new(PathBuf::from(base))
                        .finish_remote_owner_transition(&intent, &instance)
                });
            Ok(if result.is_ok() { JNI_TRUE } else { JNI_FALSE })
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => JNI_FALSE,
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeProbeAppGuard(
    mut env: EnvUnowned,
    _class: JClass,
    guard_path: JString,
) -> jboolean {
    match env
        .with_env(|owned| -> jni::errors::Result<jboolean> {
            let path = guard_path.mutf8_chars(owned)?.to_str().into_owned();
            let result = with_host(|host| {
                guard::scope()?.bind_guard_path(PathBuf::from(&path));
                match probe_app_guard(host, Path::new(&path)) {
                    Ok(true) => Ok(true),
                    Ok(false) => {
                        quarantine_app_guard(host)?;
                        Ok(false)
                    }
                    Err(_) => {
                        quarantine_app_guard(host)?;
                        Ok(false)
                    }
                }
            });
            Ok(if result.unwrap_or(false) {
                JNI_TRUE
            } else {
                JNI_FALSE
            })
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => JNI_FALSE,
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativePrepareShizukuGuardProof(
    mut env: EnvUnowned,
    _class: JClass,
    execution_id: JString,
) -> jint {
    match env
        .with_env(|owned| -> jni::errors::Result<jint> {
            let execution_id = execution_id.mutf8_chars(owned)?.to_str().into_owned();
            Ok(prepare_shizuku_guard_proof(&execution_id).unwrap_or(-1))
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => -1,
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeSettleShizukuGuardProof(
    mut env: EnvUnowned,
    _class: JClass,
    execution_id: JString,
) -> jstring {
    match env
        .with_env(|owned| -> jni::errors::Result<jstring> {
            let execution_id = execution_id.mutf8_chars(owned)?.to_str().into_owned();
            let (result, unclean) = match settle_shizuku_guard_proof(&execution_id) {
                Ok(settlement) => {
                    let unclean = !settlement.cleanup_verified;
                    (settlement, unclean)
                }
                Err(_) => (
                    guard::GuardSettlement {
                        cleanup_verified: false,
                        shell_exit_code: None,
                        cause: None,
                    },
                    true,
                ),
            };
            if unclean {
                let _ = with_host(quarantine_app_guard);
            }
            let encoded = serde_json::to_string(&result)
                .unwrap_or_else(|_| "{\"cleanup_verified\":false}".to_owned());
            Ok(owned.new_string(encoded)?.into_raw())
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeAbortShizukuGuardProof(
    mut env: EnvUnowned,
    _class: JClass,
    execution_id: JString,
) -> jboolean {
    match env
        .with_env(|owned| -> jni::errors::Result<jboolean> {
            let execution_id = execution_id.mutf8_chars(owned)?.to_str().into_owned();
            let aborted = abort_shizuku_guard_proof(&execution_id).unwrap_or(false);
            Ok(if aborted { JNI_TRUE } else { JNI_FALSE })
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => JNI_FALSE,
    }
}

/// Proves the App guard for the Magisk host this APK serves as a companion. The adopted
/// companion scope owns the probe proofs; failure or uncertainty quarantines that scope.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeProbeCompanionAppGuard(
    mut env: EnvUnowned,
    _class: JClass,
    guard_path: JString,
) -> jboolean {
    match env
        .with_env(|owned| -> jni::errors::Result<jboolean> {
            let path = guard_path.mutf8_chars(owned)?.to_str().into_owned();
            let clean = guard::scope().is_ok_and(|scope| {
                scope.bind_guard_path(PathBuf::from(&path));
                let clean = matches!(verify_app_guard(Path::new(&path)), Ok(true));
                if !clean {
                    scope.quarantine();
                }
                clean
            });
            Ok(if clean { JNI_TRUE } else { JNI_FALSE })
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => JNI_FALSE,
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeAdoptCompanionGuardScope(
    mut env: EnvUnowned,
    _class: JClass,
    canonical_base: JString,
    runtime_epoch: JString,
    runtime_instance_id: JString,
    guard_path: JString,
) -> jboolean {
    match env
        .with_env(|owned| -> jni::errors::Result<jboolean> {
            let base = canonical_base.mutf8_chars(owned)?.to_str().into_owned();
            let epoch = runtime_epoch.mutf8_chars(owned)?.to_str().into_owned();
            let instance = runtime_instance_id
                .mutf8_chars(owned)?
                .to_str()
                .into_owned();
            let path = guard_path.mutf8_chars(owned)?.to_str().into_owned();
            let adopted = adopt_companion_guard_scope(&base, &epoch, &instance, &path).is_ok();
            Ok(if adopted { JNI_TRUE } else { JNI_FALSE })
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => JNI_FALSE,
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeRunAppCommand(
    mut env: EnvUnowned,
    _class: JClass,
    execution_id: JString,
    request_json: JString,
) -> jstring {
    match env
        .with_env(|owned| -> jni::errors::Result<jstring> {
            let execution_id = execution_id.mutf8_chars(owned)?.to_str().into_owned();
            let request_json = request_json.mutf8_chars(owned)?.to_str().into_owned();
            let encoded = command::run_app_command(&execution_id, &request_json);
            Ok(owned.new_string(encoded)?.into_raw())
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeCancelAppCommand(
    mut env: EnvUnowned,
    _class: JClass,
    execution_id: JString,
) -> jboolean {
    match env
        .with_env(|owned| -> jni::errors::Result<jboolean> {
            let execution_id = execution_id.mutf8_chars(owned)?.to_str().into_owned();
            let cancelled = command::cancel_app_command(&execution_id);
            Ok(if cancelled { JNI_TRUE } else { JNI_FALSE })
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => JNI_FALSE,
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeFinishCommittedHostTransition(
    mut env: EnvUnowned,
    _class: JClass,
    canonical_base: JString,
    target_instance_id: JString,
) -> jboolean {
    match env
        .with_env(|owned| -> jni::errors::Result<jboolean> {
            let base = canonical_base.mutf8_chars(owned)?.to_str().into_owned();
            let encoded_instance = target_instance_id.mutf8_chars(owned)?.to_str().into_owned();
            let result = UuidV4::parse(encoded_instance)
                .map_err(|_| DomainError::invalid("invalid target Runtime instance"))
                .and_then(|instance| {
                    StateStore::new(PathBuf::from(base))
                        .finish_committed_transition(&instance)
                        .map(|_| ())
                });
            Ok(if result.is_ok() { JNI_TRUE } else { JNI_FALSE })
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => JNI_FALSE,
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeValidateLiveMagiskHost(
    mut env: EnvUnowned,
    _class: JClass,
    canonical_base: JString,
    target_instance_id: JString,
) -> jboolean {
    match env
        .with_env(|owned| -> jni::errors::Result<jboolean> {
            let base = canonical_base.mutf8_chars(owned)?.to_str().into_owned();
            let encoded_instance = target_instance_id.mutf8_chars(owned)?.to_str().into_owned();
            let result = UuidV4::parse(encoded_instance)
                .map_err(|_| DomainError::invalid("invalid target Runtime instance"))
                .and_then(|instance| {
                    StateStore::new(PathBuf::from(base))
                        .validate_live_instance(RuntimeHost::MagiskBackend, &instance)
                        .map(|_| ())
                });
            Ok(if result.is_ok() { JNI_TRUE } else { JNI_FALSE })
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => JNI_FALSE,
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeRegisterCapability(
    mut env: EnvUnowned,
    _class: JClass,
    key: JString,
    state: JString,
    reason: JString,
    source_generation: i64,
    has_executor: jboolean,
) -> jboolean {
    match env
        .with_env(|owned| -> jni::errors::Result<jboolean> {
            let key = key.mutf8_chars(owned)?.to_str().into_owned();
            let state = state.mutf8_chars(owned)?.to_str().into_owned();
            let reason = reason.mutf8_chars(owned)?.to_str().into_owned();
            let result = u64::try_from(source_generation)
                .map_err(|_| DomainError::invalid("negative capability generation"))
                .and_then(|source_generation| {
                    Ok((
                        source_generation,
                        Availability {
                            state: parse_capability_state(&state)?,
                            reason: (!reason.is_empty()).then_some(reason),
                        },
                    ))
                })
                .and_then(|(source_generation, availability)| {
                    with_host(|host| {
                        let accepted = host.runtime.register_capability(
                            &key,
                            availability,
                            source_generation,
                            has_executor == JNI_TRUE,
                        )?;
                        if accepted {
                            // A returned grant or App surface re-applies the unchanged due.
                            host.core.canonical_changes().notify_one();
                        }
                        Ok(accepted)
                    })
                })
                .unwrap_or(false);
            Ok(if result { JNI_TRUE } else { JNI_FALSE })
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => JNI_FALSE,
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeAutomationWake(
    _env: EnvUnowned,
    _class: JClass,
) -> jboolean {
    match with_host(|host| {
        host.automation_wake.fire();
        Ok(())
    }) {
        Ok(()) => JNI_TRUE,
        Err(_) => JNI_FALSE,
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeNetworkDefaultChanged(
    mut env: EnvUnowned,
    _class: JClass,
    runtime_epoch: JString,
    host_generation: i64,
    runtime_instance_id: JString,
    subscription_generation: i64,
    source_generation: i64,
    network_id: JString,
    transport: JString,
) -> jboolean {
    match env
        .with_env(|owned| -> jni::errors::Result<jboolean> {
            let runtime_epoch = runtime_epoch.mutf8_chars(owned)?.to_str().into_owned();
            let runtime_instance_id = runtime_instance_id
                .mutf8_chars(owned)?
                .to_str()
                .into_owned();
            let network_id = network_id.mutf8_chars(owned)?.to_str().into_owned();
            let transport = transport.mutf8_chars(owned)?.to_str().into_owned();
            let result = (|| -> Result<(), DomainError> {
                let registration = runtime::NetworkDefaultSourceRegistration {
                    fence: AdmissionFence {
                        runtime_epoch: UuidV4::parse(runtime_epoch)
                            .map_err(DomainError::invalid)?,
                        host_generation: u64::try_from(host_generation).map_err(|_| {
                            DomainError::invalid("negative network event host generation")
                        })?,
                        runtime_instance_id: UuidV4::parse(runtime_instance_id)
                            .map_err(DomainError::invalid)?,
                    },
                    subscription_generation: u64::try_from(subscription_generation).map_err(
                        |_| DomainError::invalid("negative network subscription generation"),
                    )?,
                    source_generation: u64::try_from(source_generation)
                        .map_err(|_| DomainError::invalid("negative network source generation"))?,
                };
                with_host(|host| {
                    host.core
                        .observe_network_default_event(
                            registration,
                            runtime::NetworkDefaultChangedEvent::new(
                                (!network_id.is_empty()).then_some(network_id),
                                (!transport.is_empty()).then_some(transport),
                            ),
                        )
                        .map(|_| ())
                })
            })();
            Ok(if result.is_ok() { JNI_TRUE } else { JNI_FALSE })
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => JNI_FALSE,
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeRecordHostFault(
    mut env: EnvUnowned,
    _class: JClass,
    code: JString,
    phase: JString,
) -> jboolean {
    match env
        .with_env(|owned| -> jni::errors::Result<jboolean> {
            let code = code.mutf8_chars(owned)?.to_str().into_owned();
            let phase = phase.mutf8_chars(owned)?.to_str().into_owned();
            let result = with_host(|host| {
                let now = Utc::now();
                let now_ms = u64::try_from(now.timestamp_millis()).map_err(|_| {
                    DomainError::new(ErrorCode::InternalError, "clock is before epoch")
                })?;
                FaultFileStore::new(&host.base, FaultRole::Host).append(
                    FaultRecord {
                        record_id: new_uuid()?,
                        at: now.to_rfc3339_opts(SecondsFormat::Millis, true),
                        component: "apk_host_controller".to_owned(),
                        code,
                        phase,
                        product_version: host.product_version.clone(),
                        boot_id: host.boot_id.clone(),
                        runtime_instance_id: Some(host.runtime_instance_id.clone()),
                        execution_id: None,
                        exit_code: None,
                        signal: None,
                        repeat_count: 1,
                    },
                    now_ms,
                )
            });
            Ok(if result.is_ok() { JNI_TRUE } else { JNI_FALSE })
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => JNI_FALSE,
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeStop(
    _env: EnvUnowned,
    _class: JClass,
) {
    if let Ok(mut host) = host_slot().lock() {
        *host = None;
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_execution_shizuku_ShizukuNativeLauncher_nativeStart(
    mut env: EnvUnowned,
    _class: JClass,
    client_id: JString,
    execution_id: JString,
    native_library_directory: JString,
    guard_path: JString,
    program: JString,
    arguments: JObjectArray<JString>,
    cwd: JString,
    proof_fd: jint,
    stdin_fd: jint,
    stdout_fd: jint,
    stderr_fd: jint,
) -> jlong {
    match env
        .with_env(|owned| -> jni::errors::Result<jlong> {
            let client_id = client_id.mutf8_chars(owned)?.to_str().into_owned();
            let execution_id = execution_id.mutf8_chars(owned)?.to_str().into_owned();
            let native_library_directory = native_library_directory
                .mutf8_chars(owned)?
                .to_str()
                .into_owned();
            let guard_path = guard_path.mutf8_chars(owned)?.to_str().into_owned();
            let program = program.mutf8_chars(owned)?.to_str().into_owned();
            let cwd = cwd.mutf8_chars(owned)?.to_str().into_owned();
            let mut argv = Vec::with_capacity(arguments.len(owned)?);
            for index in 0..arguments.len(owned)? {
                let value = arguments.get_element(owned, index)?;
                argv.push(value.mutf8_chars(owned)?.to_str().into_owned());
            }
            Ok(start_shizuku_guard(
                client_id,
                execution_id,
                PathBuf::from(native_library_directory),
                PathBuf::from(guard_path),
                program,
                argv,
                Some(cwd),
                proof_fd,
                stdin_fd,
                stdout_fd,
                stderr_fd,
            )
            .and_then(|handle| i64::try_from(handle).map_err(|_| invalid_native_handle()))
            .unwrap_or(0))
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => 0,
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_execution_shizuku_ShizukuNativeLauncher_nativeCloseLifetime(
    mut env: EnvUnowned,
    _class: JClass,
    client_id: JString,
    execution_id: JString,
    handle: jlong,
) -> jboolean {
    match env
        .with_env(|owned| -> jni::errors::Result<jboolean> {
            let client_id = client_id.mutf8_chars(owned)?.to_str().into_owned();
            let execution_id = execution_id.mutf8_chars(owned)?.to_str().into_owned();
            let closed = u64::try_from(handle)
                .ok()
                .is_some_and(|handle| close_shizuku_lifetime(&client_id, &execution_id, handle));
            Ok(if closed { JNI_TRUE } else { JNI_FALSE })
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => JNI_FALSE,
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_execution_shizuku_ShizukuNativeLauncher_nativeCancel(
    mut env: EnvUnowned,
    _class: JClass,
    client_id: JString,
    execution_id: JString,
    handle: jlong,
) -> jboolean {
    signal_shizuku_guard_from_jni(
        &mut env,
        client_id,
        execution_id,
        handle,
        SHIZUKU_CANCEL_SIGNAL,
    )
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_execution_shizuku_ShizukuNativeLauncher_nativeTimeout(
    mut env: EnvUnowned,
    _class: JClass,
    client_id: JString,
    execution_id: JString,
    handle: jlong,
) -> jboolean {
    signal_shizuku_guard_from_jni(
        &mut env,
        client_id,
        execution_id,
        handle,
        SHIZUKU_TIMEOUT_SIGNAL,
    )
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_execution_shizuku_ShizukuNativeLauncher_nativeWait(
    mut env: EnvUnowned,
    _class: JClass,
    client_id: JString,
    execution_id: JString,
    handle: jlong,
) -> jint {
    match env
        .with_env(|owned| -> jni::errors::Result<jint> {
            let client_id = client_id.mutf8_chars(owned)?.to_str().into_owned();
            let execution_id = execution_id.mutf8_chars(owned)?.to_str().into_owned();
            let status = u64::try_from(handle)
                .ok()
                .and_then(|handle| wait_shizuku_guard(&client_id, &execution_id, handle).ok())
                .unwrap_or(-1);
            Ok(status)
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => -1,
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_execution_shizuku_ShizukuNativeLauncher_nativeCloseClient(
    mut env: EnvUnowned,
    _class: JClass,
    client_id: JString,
) -> jint {
    match env
        .with_env(|owned| -> jni::errors::Result<jint> {
            let client_id = client_id.mutf8_chars(owned)?.to_str().into_owned();
            Ok(i32::try_from(close_shizuku_client(&client_id)).unwrap_or(i32::MAX))
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => -1,
    }
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_execution_shizuku_ShizukuNativeLauncher_nativeRename(
    mut env: EnvUnowned,
    _class: JClass,
    source: JString,
    destination: JString,
    exchange: jboolean,
) -> jint {
    match env
        .with_env(|owned| -> jni::errors::Result<jint> {
            let source = source.mutf8_chars(owned)?.to_str().into_owned();
            let destination = destination.mutf8_chars(owned)?.to_str().into_owned();
            let flags = if exchange == JNI_TRUE {
                rustix::fs::RenameFlags::EXCHANGE
            } else {
                rustix::fs::RenameFlags::NOREPLACE
            };
            Ok(rustix::fs::renameat_with(
                rustix::fs::CWD,
                Path::new(&source),
                rustix::fs::CWD,
                Path::new(&destination),
                flags,
            )
            .map_or_else(rustix::io::Errno::raw_os_error, |()| 0))
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => libc::EINVAL,
    }
}

#[cfg(target_os = "android")]
fn read_shizuku_directory(
    path: &Path,
    cookie: u64,
    limit: usize,
) -> Result<(Vec<String>, Option<u64>), rustix::io::Errno> {
    let directory = rustix::fs::openat(
        rustix::fs::CWD,
        path,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )?;
    if cookie != 0 {
        rustix::fs::seek(&directory, rustix::fs::SeekFrom::Start(cookie))?;
    }
    let mut buffer = [std::mem::MaybeUninit::<u8>::uninit(); 8_192];
    let mut entries = rustix::fs::RawDir::new(&directory, &mut buffer);
    let mut names = Vec::with_capacity(limit);
    let mut last_cookie = None;
    let mut has_more = false;
    while let Some(entry) = entries.next() {
        let entry = entry?;
        let name = entry.file_name().to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        if names.len() == limit {
            has_more = true;
            break;
        }
        names.push(BASE64.encode(name));
        last_cookie = Some(entry.next_entry_cookie());
    }
    Ok((names, has_more.then_some(last_cookie).flatten()))
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_execution_shizuku_ShizukuNativeLauncher_nativeReadDirectory(
    mut env: EnvUnowned,
    _class: JClass,
    path: JString,
    cookie: jlong,
    limit: jint,
) -> jbyteArray {
    match env
        .with_env(|owned| -> jni::errors::Result<jbyteArray> {
            let path = path.mutf8_chars(owned)?.to_str().into_owned();
            let request = u64::try_from(cookie)
                .ok()
                .zip(usize::try_from(limit).ok())
                .filter(|(_, limit)| (1..=5_001).contains(limit));
            let payload = match request {
                Some((cookie, limit)) => {
                    match read_shizuku_directory(Path::new(&path), cookie, limit) {
                        Ok((names_base64, next_cookie)) => serde_json::to_vec(&serde_json::json!({
                            "names_base64": names_base64,
                            "next_cookie": next_cookie,
                        })),
                        Err(error) => serde_json::to_vec(&serde_json::json!({
                            "errno": error.raw_os_error(),
                        })),
                    }
                }
                None => serde_json::to_vec(&serde_json::json!({
                    "errno": libc::EINVAL,
                })),
            }
            .unwrap_or_else(|_| br#"{"errno":22}"#.to_vec());
            Ok(owned.byte_array_from_slice(&payload)?.into_raw())
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeRunI5DeviceBenchmark(
    mut env: EnvUnowned,
    _class: JClass,
    benchmark_base: JString,
) -> jstring {
    match env
        .with_env(|owned| -> jni::errors::Result<jstring> {
            let base = benchmark_base.mutf8_chars(owned)?.to_str().into_owned();
            let encoded = match run_i5_device_benchmark(Path::new(&base)) {
                Ok(result) => serde_json::to_string(&result),
                Err(error) => serde_json::to_string(&serde_json::json!({
                    "error": error_code_token(error.code),
                })),
            }
            .unwrap_or_else(|_| "{\"error\":\"INTERNAL_ERROR\"}".to_owned());
            Ok(owned.new_string(encoded)?.into_raw())
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => ptr::null_mut(),
    }
}

/// The HostController-owned S-UPD-002 record; while it exists APK business admission stays closed.
pub(crate) const UPDATE_MAINTENANCE_RECORD: &str = "update-maintenance.json";

/// Loads the canonical state of an APK instance whose admission is already closed and refuses
/// quarantined cleanup, non-terminal Tasks/AutomationExecutions, reservations and live guards.
fn require_idle_apk_runtime(host: &NativeHost) -> Result<CanonicalState, DomainError> {
    if guard::is_quarantined() {
        return Err(DomainError::new(
            ErrorCode::IoError,
            "APK Runtime cleanup is unverified",
        ));
    }
    let state = host.store.load(&host._lease)?;
    let task_work = state.tasks.iter().any(|task| {
        matches!(
            task.state,
            contract::TaskState::Created
                | contract::TaskState::Queued
                | contract::TaskState::Running
        )
    });
    let automation_work = state.automation_executions.iter().any(|execution| {
        matches!(
            execution.summary.state,
            contract::AutomationExecutionState::Queued
                | contract::AutomationExecutionState::Running
        )
    });
    if task_work
        || automation_work
        || !state.reservations.is_empty()
        || !local_execution_guards_idle()
    {
        return Err(DomainError::new(
            ErrorCode::HostTransitionPending,
            "APK Runtime has active work",
        ));
    }
    Ok(state)
}

/// S-UPD-002 barrier: closes new business admission on the active APK instance and proves zero
/// work. A refusal reopens the unchanged instance; success leaves admission closed for the record.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeCloseAdmissionForMaintenance(
    mut env: EnvUnowned,
    _class: JClass,
) -> jstring {
    match env
        .with_env(|owned| -> jni::errors::Result<jstring> {
            let encoded = close_admission_for_maintenance()
                .map(|()| serde_json::json!({"closed": true}).to_string())
                .unwrap_or_else(|error| maintenance_failure(error.code, false));
            Ok(owned.new_string(encoded)?.into_raw())
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => ptr::null_mut(),
    }
}

/// Reopens APK business admission only once no maintenance record remains.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeReopenAdmission(
    mut env: EnvUnowned,
    _class: JClass,
    canonical_base: JString,
) -> jboolean {
    match env
        .with_env(|owned| -> jni::errors::Result<jboolean> {
            let base = canonical_base.mutf8_chars(owned)?.to_str().into_owned();
            let reopened = reopen_admission(Path::new(&base)).is_ok();
            Ok(if reopened { JNI_TRUE } else { JNI_FALSE })
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => JNI_FALSE,
    }
}

fn close_admission_for_maintenance() -> Result<(), DomainError> {
    refuse_recorded_maintenance()?;
    with_host(|host| {
        host.store.validate_lease(&host._lease)?;
        host.admission_open.store(false, Ordering::SeqCst);
        let idle = require_idle_apk_runtime(host).map(|_| ());
        if idle.is_err() {
            host.admission_open.store(true, Ordering::SeqCst);
        }
        idle
    })
}

fn reopen_admission(base: &Path) -> Result<(), DomainError> {
    if base.join(UPDATE_MAINTENANCE_RECORD).exists() {
        return Err(DomainError::new(
            ErrorCode::HostTransitionPending,
            "update maintenance is still recorded",
        ));
    }
    with_host(|host| {
        host.store.validate_lease(&host._lease)?;
        if base.join("runtime-transition.json").exists()
            || base.join("runtime-reset-intent.json").exists()
        {
            return Err(DomainError::new(
                ErrorCode::HostTransitionPending,
                "a Runtime intent is pending",
            ));
        }
        host.admission_open.store(true, Ordering::SeqCst);
        Ok(())
    })
}

/// Maintenance keeps admission closed; a refused transition or reset must not reopen it.
fn refuse_recorded_maintenance() -> Result<(), DomainError> {
    let recorded = with_host(|host| Ok(host.base.join(UPDATE_MAINTENANCE_RECORD).exists()))?;
    if recorded {
        return Err(DomainError::new(
            ErrorCode::HostTransitionPending,
            "update maintenance is recorded",
        ));
    }
    Ok(())
}

fn prepare_host_transition(target: &str) -> Result<TransitionPrepareResult, DomainError> {
    if target != "magisk_backend" {
        return Err(DomainError::invalid(
            "unsupported Runtime transition target",
        ));
    }
    refuse_recorded_maintenance()?;
    let slot = host_slot()
        .lock()
        .map_err(|_| DomainError::new(ErrorCode::InternalError, "native host lock failed"))?;
    let host = slot.as_ref().ok_or_else(|| {
        DomainError::new(
            ErrorCode::CapabilityUnavailable,
            "APK Runtime is not active",
        )
    })?;
    host.store.validate_lease(&host._lease)?;
    host.admission_open.store(false, Ordering::SeqCst);
    let result = (|| {
        let state = require_idle_apk_runtime(host)?;
        let owner = host.store.read_owner()?;
        if owner.host != RuntimeHost::ApkRuntime
            || owner.runtime_epoch != host.runtime_instance_fence().runtime_epoch
            || owner.host_generation != host.runtime_instance_fence().host_generation
        {
            return Err(DomainError::new(
                ErrorCode::StaleAuthority,
                "APK Runtime owner fence is stale",
            ));
        }
        let intent = RuntimeTransitionIntent {
            schema_version: 1,
            transition_id: new_uuid()?,
            runtime_epoch: owner.runtime_epoch,
            from_host: RuntimeHost::ApkRuntime,
            from_generation: owner.host_generation,
            from_instance_id: host.runtime_instance_id.clone(),
            target_host: RuntimeHost::MagiskBackend,
            target_generation: owner.host_generation.checked_add(1).ok_or_else(|| {
                DomainError::new(ErrorCode::ResourceLimit, "host generation exhausted")
            })?,
        };
        host.store.record_transition_intent(&host._lease, &intent)?;
        Ok(TransitionPrepareResult {
            prepared: true,
            store_revision: state.store_revision,
            intent,
        })
    })();
    if result.is_err() {
        host.admission_open.store(true, Ordering::SeqCst);
    }
    result
}

fn prepare_remote_host_transition(
    base: &Path,
    target: &str,
) -> Result<RemoteTransitionPrepareResult, DomainError> {
    if target != "apk_runtime" {
        return Err(DomainError::invalid(
            "unsupported remote Runtime transition target",
        ));
    }
    let store = StateStore::new(base.to_path_buf());
    let owner = store.read_owner()?;
    let live: RuntimeLive = read_json(&base.join("runtime-live.json"))?;
    if owner.host != RuntimeHost::MagiskBackend
        || live.runtime_epoch != owner.runtime_epoch
        || live.host != owner.host
        || live.host_generation != owner.host_generation
    {
        return Err(DomainError::new(
            ErrorCode::StaleAuthority,
            "remote Runtime source identity is stale",
        ));
    }
    let transition_path = base.join("runtime-transition.json");
    if transition_path.exists() {
        let intent: RuntimeTransitionIntent = read_json(&transition_path)?;
        if intent.schema_version != 1
            || intent.runtime_epoch != owner.runtime_epoch
            || intent.from_host != owner.host
            || intent.from_generation != owner.host_generation
            || intent.from_instance_id != live.runtime_instance_id
            || intent.target_host != RuntimeHost::ApkRuntime
            || intent.target_generation
                != owner.host_generation.checked_add(1).ok_or_else(|| {
                    DomainError::new(ErrorCode::ResourceLimit, "host generation exhausted")
                })?
        {
            return Err(DomainError::new(
                ErrorCode::StaleAuthority,
                "pending remote Runtime transition is stale",
            ));
        }
        return Ok(RemoteTransitionPrepareResult {
            prepared: true,
            recovery: true,
            intent,
        });
    }
    let intent = RuntimeTransitionIntent {
        schema_version: 1,
        transition_id: new_uuid()?,
        runtime_epoch: owner.runtime_epoch,
        from_host: owner.host,
        from_generation: owner.host_generation,
        from_instance_id: live.runtime_instance_id,
        target_host: RuntimeHost::ApkRuntime,
        target_generation: owner.host_generation.checked_add(1).ok_or_else(|| {
            DomainError::new(ErrorCode::ResourceLimit, "host generation exhausted")
        })?,
    };
    store.record_remote_transition_intent(&intent)?;
    Ok(RemoteTransitionPrepareResult {
        prepared: true,
        recovery: false,
        intent,
    })
}

fn abort_host_transition(intent: &RuntimeTransitionIntent) -> Result<(), DomainError> {
    let slot = host_slot()
        .lock()
        .map_err(|_| DomainError::new(ErrorCode::InternalError, "native host lock failed"))?;
    let host = slot
        .as_ref()
        .ok_or_else(|| DomainError::new(ErrorCode::StaleAuthority, "APK Runtime is not active"))?;
    host.store.abort_owner_transition(&host._lease, intent)?;
    host.admission_open.store(true, Ordering::SeqCst);
    Ok(())
}

fn release_host_transition(intent: &RuntimeTransitionIntent) -> Result<(), DomainError> {
    let mut slot = host_slot()
        .lock()
        .map_err(|_| DomainError::new(ErrorCode::InternalError, "native host lock failed"))?;
    let host = slot
        .as_ref()
        .ok_or_else(|| DomainError::new(ErrorCode::StaleAuthority, "APK Runtime is not active"))?;
    host.store.validate_lease(&host._lease)?;
    if host.admission_open.load(Ordering::SeqCst)
        || intent.runtime_epoch != host._lease.live().runtime_epoch
        || intent.from_host != RuntimeHost::ApkRuntime
        || intent.from_generation != host._lease.live().host_generation
        || intent.from_instance_id != host.runtime_instance_id
        || intent.target_host != RuntimeHost::MagiskBackend
    {
        return Err(DomainError::new(
            ErrorCode::StaleAuthority,
            "APK Runtime release fence is stale",
        ));
    }
    *slot = None;
    Ok(())
}

impl NativeHost {
    fn runtime_instance_fence(&self) -> AdmissionFence {
        AdmissionFence {
            runtime_epoch: self._lease.live().runtime_epoch.clone(),
            host_generation: self._lease.live().host_generation,
            runtime_instance_id: self.runtime_instance_id.clone(),
        }
    }
}

#[cfg(unix)]
fn local_execution_guards_idle() -> bool {
    shizuku_guard_registry()
        .lock()
        .is_ok_and(|registry| registry.entries.is_empty())
}

#[cfg(not(unix))]
fn local_execution_guards_idle() -> bool {
    true
}

fn transition_error(code: ErrorCode) -> String {
    serde_json::to_string(&serde_json::json!({
        "prepared": false,
        "code": error_code_token(code),
    }))
    .unwrap_or_else(|_| "{\"prepared\":false,\"code\":\"INTERNAL_ERROR\"}".to_owned())
}

/// Runs `operation` against the published APK Runtime host. The slot lock only publishes
/// the host: it is released before the operation runs, because an operation can re-enter
/// this module with the host demanded again (a guard proof callback, or a capability
/// registration raised from inside a running execution), and a non-reentrant lock held
/// across an execution would deadlock that re-entry.
fn with_host<T>(
    operation: impl FnOnce(&NativeHost) -> Result<T, DomainError>,
) -> Result<T, DomainError> {
    let host = host_slot()
        .lock()
        .map_err(|_| DomainError::new(ErrorCode::InternalError, "native host lock failed"))?
        .clone()
        .ok_or_else(|| {
            DomainError::new(
                ErrorCode::CapabilityUnavailable,
                "APK Runtime is not started",
            )
        })?;
    operation(&host)
}

fn invalid_native_handle() -> DomainError {
    DomainError::new(
        ErrorCode::ResourceLimit,
        "native guard handle space exhausted",
    )
}

#[cfg(unix)]
fn prepare_shizuku_guard_proof(execution_id: &str) -> Result<i32, DomainError> {
    use std::os::fd::IntoRawFd;

    let execution_id = UuidV4::parse(execution_id.to_owned())
        .map_err(|_| DomainError::invalid("invalid execution ID"))?;
    Ok(guard::prepare_proof(&execution_id)?.into_raw_fd())
}

#[cfg(not(unix))]
fn prepare_shizuku_guard_proof(_execution_id: &str) -> Result<i32, DomainError> {
    Err(DomainError::new(
        ErrorCode::CapabilityUnavailable,
        "execution guard requires Unix",
    ))
}

fn settle_shizuku_guard_proof(execution_id: &str) -> Result<guard::GuardSettlement, DomainError> {
    let execution_id = UuidV4::parse(execution_id.to_owned())
        .map_err(|_| DomainError::invalid("invalid execution ID"))?;
    guard::settle_proof(&execution_id)
}

fn abort_shizuku_guard_proof(execution_id: &str) -> Result<bool, DomainError> {
    let execution_id = UuidV4::parse(execution_id.to_owned())
        .map_err(|_| DomainError::invalid("invalid execution ID"))?;
    guard::abort_proof(&execution_id)
}

/// The guard scope of the Magisk host this APK serves as an authenticated companion
/// (S-AUTH-CMD-001, S-EXEC-001). A companion that was never the Runtime host still runs
/// the App/Shizuku commands the Magisk host forwards to it, so it runs them under the
/// guard identity of the Runtime instance that admitted them.
fn adopt_companion_guard_scope(
    canonical_base: &str,
    runtime_epoch: &str,
    runtime_instance_id: &str,
    guard_path: &str,
) -> Result<(), DomainError> {
    let scope = guard::GuardScope::new(
        PathBuf::from(canonical_base),
        UuidV4::parse(runtime_epoch.to_owned())
            .map_err(|_| DomainError::invalid("invalid Runtime epoch"))?,
        UuidV4::parse(runtime_instance_id.to_owned())
            .map_err(|_| DomainError::invalid("invalid Runtime instance"))?,
    )?;
    scope.bind_guard_path(PathBuf::from(guard_path));
    guard::publish_scope(scope)
}

#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn start_shizuku_guard(
    client_id: String,
    execution_id: String,
    native_library_directory: PathBuf,
    guard_path: PathBuf,
    program: String,
    arguments: Vec<String>,
    cwd: Option<String>,
    proof_fd: i32,
    stdin_fd: i32,
    stdout_fd: i32,
    stderr_fd: i32,
) -> Result<u64, DomainError> {
    if !validate_shizuku_arguments(
        &client_id,
        &execution_id,
        &program,
        &arguments,
        cwd.as_deref().unwrap_or("/"),
    ) || [proof_fd, stdin_fd, stdout_fd, stderr_fd]
        .iter()
        .any(|fd| *fd < 0)
    {
        return Err(DomainError::invalid("invalid Shizuku guard launch"));
    }
    let mut registry = shizuku_guard_registry()
        .lock()
        .map_err(|_| DomainError::new(ErrorCode::InternalError, "Shizuku guard lock failed"))?;
    let effective_uid = unsafe { libc::geteuid() };
    if !validate_shizuku_launch(
        effective_uid,
        &native_library_directory,
        &guard_path,
        registry.entries.len(),
    ) || registry
        .owners
        .contains_key(&(client_id.clone(), execution_id.clone()))
    {
        return Err(DomainError::new(
            ErrorCode::CapabilityUnavailable,
            "Shizuku guard launch rejected",
        ));
    }
    let handle = registry.next_handle;
    let next_handle = registry
        .next_handle
        .checked_add(1)
        .filter(|next| *next <= i64::MAX as u64)
        .ok_or_else(invalid_native_handle)?;

    let proof = duplicate_fd(proof_fd)?;
    let stdin = duplicate_fd(stdin_fd)?;
    let stdout = duplicate_fd(stdout_fd)?;
    let stderr = duplicate_fd(stderr_fd)?;
    let mut lifetime_pipe = [0_i32; 2];
    if unsafe { libc::pipe2(lifetime_pipe.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io_error(std::io::Error::last_os_error()));
    }
    let lifetime_reader = unsafe { fs::File::from_raw_fd(lifetime_pipe[0]) };
    let lifetime_writer = unsafe { fs::File::from_raw_fd(lifetime_pipe[1]) };
    let proof_raw = proof.as_raw_fd();
    let lifetime_raw = lifetime_reader.as_raw_fd();

    let mut command = Command::new(&guard_path);
    command
        .arg("--proof-fd")
        .arg(proof_raw.to_string())
        .arg("--lifetime-fd")
        .arg(lifetime_raw.to_string())
        .arg("--")
        .arg(&program)
        .args(&arguments)
        .stdin(Stdio::from(stdin))
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .current_dir(cwd.as_deref().unwrap_or("/"));
    unsafe {
        use std::os::unix::process::CommandExt;
        command.pre_exec(move || {
            block_guard_termination_signals()?;
            clear_cloexec(proof_raw)?;
            clear_cloexec(lifetime_raw)?;
            Ok(())
        });
    }
    let child = command.spawn().map_err(io_error)?;
    let pid = i32::try_from(child.id())
        .map_err(|_| DomainError::new(ErrorCode::InternalError, "Shizuku guard PID is invalid"))?;
    drop(proof);
    drop(lifetime_reader);

    registry.next_handle = next_handle;
    let entry = Arc::new(ShizukuGuardEntry {
        client_id: client_id.clone(),
        execution_id: execution_id.clone(),
        pid,
        child: Mutex::new(Some(child)),
        lifetime_writer: Mutex::new(Some(lifetime_writer)),
    });
    registry.owners.insert((client_id, execution_id), handle);
    registry.entries.insert(handle, entry);
    Ok(handle)
}

#[cfg(not(unix))]
#[allow(clippy::too_many_arguments)]
fn start_shizuku_guard(
    _client_id: String,
    _execution_id: String,
    _native_library_directory: PathBuf,
    _guard_path: PathBuf,
    _program: String,
    _arguments: Vec<String>,
    _cwd: Option<String>,
    _proof_fd: i32,
    _stdin_fd: i32,
    _stdout_fd: i32,
    _stderr_fd: i32,
) -> Result<u64, DomainError> {
    Err(DomainError::new(
        ErrorCode::CapabilityUnavailable,
        "Shizuku guard requires Unix",
    ))
}

#[cfg(unix)]
fn duplicate_fd(fd: RawFd) -> Result<fs::File, DomainError> {
    let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 10) };
    if duplicate < 0 {
        Err(io_error(std::io::Error::last_os_error()))
    } else {
        Ok(unsafe { fs::File::from_raw_fd(duplicate) })
    }
}

#[cfg(unix)]
fn clear_cloexec(fd: RawFd) -> std::io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn shizuku_entry(
    client_id: &str,
    execution_id: &str,
    handle: u64,
) -> Option<Arc<ShizukuGuardEntry>> {
    let registry = shizuku_guard_registry().lock().ok()?;
    let entry = registry.entries.get(&handle)?;
    (entry.client_id == client_id && entry.execution_id == execution_id).then(|| Arc::clone(entry))
}

#[cfg(unix)]
fn close_shizuku_lifetime(client_id: &str, execution_id: &str, handle: u64) -> bool {
    let Some(entry) = shizuku_entry(client_id, execution_id, handle) else {
        return false;
    };
    entry
        .lifetime_writer
        .lock()
        .is_ok_and(|mut writer| writer.take().is_some())
}

fn signal_shizuku_guard_from_jni(
    env: &mut EnvUnowned,
    client_id: JString,
    execution_id: JString,
    handle: jlong,
    signal: i32,
) -> jboolean {
    match env
        .with_env(|owned| -> jni::errors::Result<jboolean> {
            let client_id = client_id.mutf8_chars(owned)?.to_str().into_owned();
            let execution_id = execution_id.mutf8_chars(owned)?.to_str().into_owned();
            let signalled = u64::try_from(handle).ok().is_some_and(|handle| {
                signal_shizuku_guard(&client_id, &execution_id, handle, signal)
            });
            Ok(if signalled { JNI_TRUE } else { JNI_FALSE })
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => JNI_FALSE,
    }
}

#[cfg(unix)]
fn signal_shizuku_guard(client_id: &str, execution_id: &str, handle: u64, signal: i32) -> bool {
    let Some(entry) = shizuku_entry(client_id, execution_id, handle) else {
        return false;
    };
    let signalled = unsafe { libc::kill(entry.pid, signal) } == 0;
    if let Ok(mut writer) = entry.lifetime_writer.lock() {
        writer.take();
    }
    signalled
}

#[cfg(not(unix))]
fn signal_shizuku_guard(_client_id: &str, _execution_id: &str, _handle: u64, _signal: i32) -> bool {
    false
}

#[cfg(not(unix))]
fn close_shizuku_lifetime(_client_id: &str, _execution_id: &str, _handle: u64) -> bool {
    false
}

#[cfg(unix)]
fn close_shizuku_client(client_id: &str) -> usize {
    use std::{
        thread,
        time::{Duration, Instant},
    };

    let entries = shizuku_guard_registry()
        .lock()
        .map(|registry| {
            registry
                .entries
                .values()
                .filter(|entry| entry.client_id == client_id)
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for entry in entries {
        if let Ok(mut writer) = entry.lifetime_writer.lock() {
            writer.take();
        }
    }

    let deadline = Instant::now() + Duration::from_millis(SHIZUKU_CLIENT_CLEANUP_WAIT_MS);
    loop {
        let remaining = shizuku_guard_registry()
            .lock()
            .map(|registry| {
                registry
                    .entries
                    .values()
                    .filter(|entry| entry.client_id == client_id)
                    .count()
            })
            .unwrap_or(1);
        if remaining == 0 || Instant::now() >= deadline {
            return remaining;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(not(unix))]
fn close_shizuku_client(_client_id: &str) -> usize {
    0
}

#[cfg(unix)]
fn wait_shizuku_guard(
    client_id: &str,
    execution_id: &str,
    handle: u64,
) -> Result<i32, DomainError> {
    let entry = shizuku_entry(client_id, execution_id, handle).ok_or_else(|| {
        DomainError::new(ErrorCode::StaleAuthority, "Shizuku guard handle is stale")
    })?;
    let mut child = entry
        .child
        .lock()
        .map_err(|_| DomainError::new(ErrorCode::InternalError, "Shizuku child lock failed"))?
        .take()
        .ok_or_else(|| {
            DomainError::new(ErrorCode::AlreadyExists, "Shizuku guard is already awaited")
        })?;
    let status = child.wait().map_err(io_error)?;
    if let Ok(mut writer) = entry.lifetime_writer.lock() {
        writer.take();
    }
    if let Ok(mut registry) = shizuku_guard_registry().lock()
        && registry
            .entries
            .get(&handle)
            .is_some_and(|current| Arc::ptr_eq(current, &entry))
    {
        registry.entries.remove(&handle);
        registry
            .owners
            .remove(&(client_id.to_owned(), execution_id.to_owned()));
    }
    Ok(status
        .code()
        .or_else(|| status.signal().map(|signal| 128 + signal))
        .unwrap_or(255))
}

#[cfg(not(unix))]
fn wait_shizuku_guard(
    _client_id: &str,
    _execution_id: &str,
    _handle: u64,
) -> Result<i32, DomainError> {
    Err(DomainError::new(
        ErrorCode::CapabilityUnavailable,
        "Shizuku guard requires Unix",
    ))
}

fn run_i5_device_benchmark(base: &Path) -> Result<DeviceBenchmarkResult, DomainError> {
    let two_mib = benchmark_latency(&base.join("two-mib"), FIXTURE_2_MIB, 100)?;
    let eight_mib = benchmark_latency(&base.join("eight-mib"), FIXTURE_8_MIB, 100)?;
    let (steady_commits, steady_elapsed_ms, steady_max_lock_wait_ms) =
        benchmark_steady(&base.join("steady-two-mib"), FIXTURE_2_MIB)?;
    fs::remove_dir_all(base).map_err(io_error)?;
    if let Some(parent) = base.parent() {
        sync_directory(parent)?;
    }
    Ok(DeviceBenchmarkResult {
        two_mib_p95_ms: percentile_ms(&two_mib, 95),
        two_mib_p99_ms: percentile_ms(&two_mib, 99),
        eight_mib_p95_ms: percentile_ms(&eight_mib, 95),
        eight_mib_p99_ms: percentile_ms(&eight_mib, 99),
        steady_commits,
        steady_elapsed_ms,
        steady_max_lock_wait_ms,
    })
}

fn benchmark_latency(
    directory: &Path,
    target_bytes: usize,
    samples: usize,
) -> Result<Vec<u128>, DomainError> {
    let (store, lease, mut revision) = initialized_benchmark_store(directory, target_bytes)?;
    let mut timings = Vec::with_capacity(samples);
    for _ in 0..samples {
        let timing = store.compare_and_commit(&lease, revision, |_| Ok(()))?;
        revision += 1;
        timings.push(timing.total_ns);
    }
    drop(lease);
    fs::remove_dir_all(directory).map_err(io_error)?;
    Ok(timings)
}

fn benchmark_steady(directory: &Path, target_bytes: usize) -> Result<(u64, u64, u64), DomainError> {
    use std::time::{Duration, Instant};

    let (store, lease, mut revision) = initialized_benchmark_store(directory, target_bytes)?;
    let started = Instant::now();
    let duration = Duration::from_secs(60);
    let mut commits = 0_u64;
    let mut max_lock_wait_ns = 0_u128;
    while started.elapsed() < duration {
        let timing = store.compare_and_commit(&lease, revision, |_| Ok(()))?;
        revision += 1;
        commits += 1;
        max_lock_wait_ns = max_lock_wait_ns.max(timing.lock_wait_ns);
    }
    let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    drop(lease);
    fs::remove_dir_all(directory).map_err(io_error)?;
    Ok((commits, elapsed_ms, nanos_to_millis(max_lock_wait_ns)))
}

fn initialized_benchmark_store(
    directory: &Path,
    target_bytes: usize,
) -> Result<(StateStore, LifetimeLease, u64), DomainError> {
    if directory.exists() {
        fs::remove_dir_all(directory).map_err(io_error)?;
    }
    let mut state = decode_canonical_state(&realistic_store_fixture(target_bytes))?;
    state.store_revision = 1_000_000_000_000_000_000;
    trim_fixture_headroom(&mut state, 64)?;
    let runtime_epoch = new_uuid()?;
    let owner = RuntimeOwner {
        schema_version: 1,
        runtime_epoch: runtime_epoch.clone(),
        host: RuntimeHost::ApkRuntime,
        host_generation: 1,
    };
    let store = StateStore::new(directory.to_path_buf());
    store.initialize(&owner, &state)?;
    let boot_id = read_boot_id()?;
    let live = RuntimeLive {
        runtime_epoch,
        host: RuntimeHost::ApkRuntime,
        host_generation: 1,
        runtime_instance_id: new_uuid()?,
        boot_id,
        pid: std::process::id(),
        start_ticks: read_start_ticks(Path::new("/proc/self/stat"))?,
    };
    let lease = store.acquire_lifetime(live)?;
    let revision = state.store_revision;
    Ok((store, lease, revision))
}

fn trim_fixture_headroom(state: &mut CanonicalState, headroom: usize) -> Result<(), DomainError> {
    let current = serde_json::to_vec(state)
        .map_err(|_| DomainError::new(ErrorCode::InternalError, "fixture serialization failed"))?
        .len();
    let target = current.saturating_sub(headroom);
    let needed = current - target;
    for automation in &mut state.automations {
        for value in automation.automation.state.values_mut() {
            if let ScalarValue::String(text) = value
                && text.len() >= needed
            {
                text.truncate(text.len() - needed);
                return Ok(());
            }
        }
    }
    Err(DomainError::new(
        ErrorCode::InternalError,
        "fixture has no headroom field",
    ))
}

fn percentile_ms(samples: &[u128], percentile: usize) -> u64 {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let index = (sorted.len() * percentile).div_ceil(100).saturating_sub(1);
    nanos_to_millis(sorted[index])
}

fn nanos_to_millis(value: u128) -> u64 {
    u64::try_from(value.div_ceil(1_000_000)).unwrap_or(u64::MAX)
}

fn read_boot_id() -> Result<UuidV4, DomainError> {
    let value = fs::read_to_string("/proc/sys/kernel/random/boot_id").map_err(io_error)?;
    UuidV4::parse(value.trim().to_owned())
        .map_err(|_| DomainError::new(ErrorCode::IoError, "kernel boot ID is invalid"))
}

fn read_start_ticks(path: &Path) -> Result<u64, DomainError> {
    let stat = fs::read_to_string(path).map_err(io_error)?;
    let close = stat
        .rfind(')')
        .ok_or_else(|| DomainError::new(ErrorCode::IoError, "process stat is invalid"))?;
    stat.get(close + 2..)
        .and_then(|tail| tail.split_whitespace().nth(19))
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| DomainError::new(ErrorCode::IoError, "process start time is invalid"))
}

fn new_uuid() -> Result<UuidV4, DomainError> {
    UuidV4::parse(uuid::Uuid::new_v4().hyphenated().to_string())
        .map_err(|_| DomainError::new(ErrorCode::InternalError, "UUID generation failed"))
}

fn parse_capability_state(value: &str) -> Result<CapabilityState, DomainError> {
    match value {
        "available" => Ok(CapabilityState::Available),
        "unavailable" => Ok(CapabilityState::Unavailable),
        "unknown" => Ok(CapabilityState::Unknown),
        _ => Err(DomainError::invalid("invalid capability state")),
    }
}

fn io_error(_: std::io::Error) -> DomainError {
    DomainError::new(ErrorCode::IoError, "native filesystem operation failed")
}

fn error_code_token(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::InvalidArgument => "INVALID_ARGUMENT",
        ErrorCode::NotFound => "NOT_FOUND",
        ErrorCode::AlreadyExists => "ALREADY_EXISTS",
        ErrorCode::PermissionDenied => "PERMISSION_DENIED",
        ErrorCode::CapabilityUnavailable => "CAPABILITY_UNAVAILABLE",
        ErrorCode::Unsupported => "UNSUPPORTED",
        ErrorCode::StaleAuthority => "STALE_AUTHORITY",
        ErrorCode::StaleReference => "STALE_REFERENCE",
        ErrorCode::RevisionConflict => "REVISION_CONFLICT",
        ErrorCode::Timeout => "TIMEOUT",
        ErrorCode::Cancelled => "CANCELLED",
        ErrorCode::IoError => "IO_ERROR",
        ErrorCode::ProtocolIncompatible => "PROTOCOL_INCOMPATIBLE",
        ErrorCode::ResourceLimit => "RESOURCE_LIMIT",
        ErrorCode::InternalError => "INTERNAL_ERROR",
        ErrorCode::NotEmpty => "NOT_EMPTY",
        ErrorCode::ArchiveCorrupt => "ARCHIVE_CORRUPT",
        ErrorCode::ArchiveEncrypted => "ARCHIVE_ENCRYPTED",
        ErrorCode::RunAsUnavailable => "RUN_AS_UNAVAILABLE",
        ErrorCode::ExecutionFailed => "EXECUTION_FAILED",
        ErrorCode::CancelFailed => "CANCEL_FAILED",
        ErrorCode::CaptureFailed => "CAPTURE_FAILED",
        ErrorCode::HostTransitionPending => "HOST_TRANSITION_PENDING",
    }
}

fn native_error_envelope(code: ErrorCode, encoded: &[u8]) -> Vec<u8> {
    let request_id = serde_json::from_slice::<serde_json::Value>(encoded)
        .ok()
        .and_then(|value| value.get("request_id")?.as_str().map(str::to_owned))
        .and_then(|value| UuidV4::parse(value).ok());
    serde_json::to_vec(&PublicResponse::<serde_json::Value>::error(
        request_id,
        PublicError {
            code,
            operation: "runtime.submit".to_owned(),
            retryable: false,
            message: None,
            capability: None,
            details: None,
        },
    ))
    .unwrap_or_default()
}

/// Runs the S-EXEC-001 App-identity guard probe under the published guard scope. It needs
/// no Runtime host, so the Magisk-host companion proves the same App guard.
#[cfg(unix)]
fn verify_app_guard(guard_path: &Path) -> Result<bool, DomainError> {
    if guard::is_quarantined() {
        return Ok(false);
    }
    if !guard_path.is_absolute() || !guard_path.is_file() {
        return Err(DomainError::new(
            ErrorCode::NotFound,
            "execution guard is not installed",
        ));
    }
    let exited = run_guard_probe(guard_path, GuardProbe::ChildExit)?;
    let owner_lost = run_guard_probe(guard_path, GuardProbe::OwnerLost)?;
    Ok(exited && owner_lost)
}

#[cfg(unix)]
fn probe_app_guard(host: &NativeHost, guard_path: &Path) -> Result<bool, DomainError> {
    let clean = verify_app_guard(guard_path)?;
    if clean {
        host.runtime.register_capability(
            "execution.app_guard",
            Availability {
                state: CapabilityState::Available,
                reason: None,
            },
            host._lease.live().host_generation,
            true,
        )?;
    } else {
        quarantine_app_guard(host)?;
    }
    Ok(clean)
}

fn quarantine_app_guard(host: &NativeHost) -> Result<(), DomainError> {
    // The scope gate and the Runtime readiness withdrawal are independent, so an absent
    // scope still withdraws this host's execution surfaces.
    if let Ok(scope) = guard::scope() {
        scope.quarantine();
    }
    quarantine_runtime(&host.runtime)
}

fn quarantine_runtime(runtime: &ApkRuntimeVertical) -> Result<(), DomainError> {
    runtime.set_unavailable("CLEANUP_UNVERIFIED")?;
    runtime.withdraw_capabilities(&["execution.app_guard"], "CLEANUP_UNVERIFIED")
}

#[cfg(unix)]
#[derive(Clone, Copy)]
enum GuardProbe {
    ChildExit,
    OwnerLost,
}

/// S-EXEC-002: startup probe evidence stays outside the recovery-read `execution-guards/`.
#[cfg(unix)]
const GUARD_PROBE_DIRECTORY: &str = "execution-guard-probes";

#[cfg(unix)]
static GUARD_PROBE_LOCK: Mutex<()> = Mutex::new(());

#[cfg(unix)]
fn run_guard_probe(guard_path: &Path, mode: GuardProbe) -> Result<bool, DomainError> {
    use persistence::GuardCleanCause;
    use std::{
        fs::OpenOptions,
        io::Write,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::fs::OpenOptionsExt,
        },
        process::Command,
        thread,
        time::{Duration, Instant},
    };

    let scope = guard::scope()?;
    if !guard_proof_capacity_available(scope.base()) {
        return Ok(false);
    }
    let _probe = GUARD_PROBE_LOCK
        .lock()
        .map_err(|_| DomainError::new(ErrorCode::InternalError, "guard probe lock failed"))?;
    // Only this lease-holding App process probes, so every earlier entry belongs to a finished or
    // killed probe; none of it is execution evidence (S-EXEC-002).
    let probe_root = scope.base().join(GUARD_PROBE_DIRECTORY);
    match fs::remove_dir_all(&probe_root) {
        Ok(()) => sync_directory(scope.base())?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_error(error)),
    }
    let execution_id = new_uuid()?;
    let directory = probe_root.join(scope.boot_id().as_str());
    fs::create_dir_all(&directory).map_err(io_error)?;
    sync_directory(&directory)?;
    let proof_path = directory.join(format!("{}.proof", execution_id.as_str()));
    let marker_path = directory.join(format!("{}.ready", execution_id.as_str()));
    let mut proof = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&proof_path)
        .map_err(io_error)?;
    let identity = GuardIdentity {
        runtime_epoch: scope.runtime_epoch().clone(),
        runtime_instance_id: scope.runtime_instance_id().clone(),
        execution_id: execution_id.clone(),
        boot_id: scope.boot_id().clone(),
    };
    proof
        .write_all(&encode_guard_frame(&identity)?)
        .map_err(io_error)?;
    proof.sync_all().map_err(io_error)?;

    let mut pipe = [0_i32; 2];
    if unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io_error(std::io::Error::last_os_error()));
    }
    let lifetime_read = unsafe { fs::File::from_raw_fd(pipe[0]) };
    let lifetime_write = unsafe { fs::File::from_raw_fd(pipe[1]) };
    make_inheritable(proof.as_raw_fd())?;
    make_inheritable(lifetime_read.as_raw_fd())?;
    let mut command = Command::new(guard_path);
    command
        .arg("--proof-fd")
        .arg(proof.as_raw_fd().to_string())
        .arg("--lifetime-fd")
        .arg(lifetime_read.as_raw_fd().to_string())
        .arg("--")
        .arg(guard_path);
    match mode {
        GuardProbe::ChildExit => {
            command.arg("--probe-child");
        }
        GuardProbe::OwnerLost => {
            command.arg("--probe-owner-death-child").arg(&marker_path);
        }
    }
    let mut child = command.spawn().map_err(io_error)?;
    drop(proof);
    drop(lifetime_read);
    let mut lifetime_write = Some(lifetime_write);
    if matches!(mode, GuardProbe::OwnerLost) {
        let marker_deadline = Instant::now() + Duration::from_millis(2_000);
        while !marker_path.exists() && Instant::now() < marker_deadline {
            thread::sleep(Duration::from_millis(10));
        }
        if !marker_path.exists() {
            drop(lifetime_write.take());
            return Ok(false);
        }
        drop(lifetime_write.take());
    }
    let deadline = Instant::now() + Duration::from_millis(8_000);
    let exited = loop {
        if let Some(status) = child.try_wait().map_err(io_error)? {
            break status.success();
        }
        if Instant::now() >= deadline {
            drop(lifetime_write.take());
            return Ok(false);
        }
        thread::sleep(Duration::from_millis(20));
    };
    drop(lifetime_write.take());
    let bytes = match fs::read(&proof_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(io_error(error)),
    };
    let recovery = classify_guard_proof(
        scope.boot_id().as_str(),
        scope.boot_id(),
        &identity,
        &bytes,
        &ProcFacts,
    )?;
    let expected = match mode {
        GuardProbe::ChildExit => GuardCleanCause::Exited,
        GuardProbe::OwnerLost => GuardCleanCause::OwnerLost,
    };
    let clean = matches!(
        recovery,
        GuardRecovery::Clean { clean: Some(ref value) } if value.cause == expected
    ) && exited;
    if clean {
        fs::remove_file(proof_path).map_err(io_error)?;
        if marker_path.exists() {
            fs::remove_file(marker_path).map_err(io_error)?;
        }
        sync_directory(&directory)?;
    }
    Ok(clean)
}

#[cfg(unix)]
fn guard_proof_capacity_available(base: &Path) -> bool {
    let root = base.join("execution-guards");
    let boot_entries = match fs::read_dir(root) {
        Ok(entries) => match entries.collect::<Result<Vec<_>, _>>() {
            Ok(entries) => entries,
            Err(_) => return false,
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return true,
        Err(_) => return false,
    };
    let mut count = 0_usize;
    let mut bytes = 0_u64;
    for boot_entry in boot_entries {
        let metadata = match fs::symlink_metadata(boot_entry.path()) {
            Ok(metadata) if metadata.is_dir() => metadata,
            _ => return false,
        };
        let _ = metadata;
        let entries = match fs::read_dir(boot_entry.path())
            .and_then(|entries| entries.collect::<Result<Vec<_>, _>>())
        {
            Ok(entries) => entries,
            Err(_) => return false,
        };
        for entry in entries {
            let name = match entry.file_name().into_string() {
                Ok(value) => value,
                Err(_) => return false,
            };
            if !name.ends_with(".proof") {
                continue;
            }
            let metadata = match fs::symlink_metadata(entry.path()) {
                Ok(metadata) if metadata.is_file() => metadata,
                _ => return false,
            };
            count = match count.checked_add(1) {
                Some(value) => value,
                None => return false,
            };
            bytes = match bytes.checked_add(metadata.len()) {
                Some(value) => value,
                None => return false,
            };
        }
    }
    count < persistence::GUARD_PROOF_COUNT_LIMIT
        && bytes
            .checked_add(persistence::GUARD_PROOF_LIMIT_BYTES as u64)
            .is_some_and(|value| value <= persistence::GUARD_PROOF_TOTAL_LIMIT_BYTES)
}

#[cfg(not(unix))]
fn probe_app_guard(_host: &NativeHost, _guard_path: &Path) -> Result<bool, DomainError> {
    Ok(false)
}

#[cfg(not(unix))]
fn verify_app_guard(_guard_path: &Path) -> Result<bool, DomainError> {
    Ok(false)
}

#[cfg(unix)]
fn make_inheritable(fd: std::os::fd::RawFd) -> Result<(), DomainError> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0 {
        Err(io_error(std::io::Error::last_os_error()))
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), DomainError> {
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(io_error)
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), DomainError> {
    Ok(())
}

struct ProcFacts;

impl ProcessFacts for ProcFacts {
    fn is_same_process(&self, pid: u32, start_ticks: u64) -> Result<bool, DomainError> {
        let path = PathBuf::from(format!("/proc/{pid}/stat"));
        match read_start_ticks(&path) {
            Ok(actual) => Ok(actual == start_ticks),
            Err(_) if !path.exists() => Ok(false),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn i8_cmd_android_execution_error_mapping_preserves_the_closed_contract_codes() {
        for (token, expected) in [
            ("RUN_AS_UNAVAILABLE", ErrorCode::RunAsUnavailable),
            ("EXECUTION_FAILED", ErrorCode::ExecutionFailed),
            ("TIMEOUT", ErrorCode::Timeout),
            ("CANCELLED", ErrorCode::Cancelled),
            ("CANCEL_FAILED", ErrorCode::CancelFailed),
            ("PROTOCOL_INCOMPATIBLE", ErrorCode::ProtocolIncompatible),
        ] {
            assert_eq!(android_execution_error_code(token), expected);
        }
        assert_eq!(
            android_execution_error_code("CLEANUP_UNVERIFIED"),
            ErrorCode::InternalError
        );
        assert_eq!(
            android_execution_error_code("NOT_A_CONTRACT_CODE"),
            ErrorCode::InternalError
        );
    }

    fn id(index: u64) -> UuidV4 {
        UuidV4::parse(format!("00000000-0000-4000-8000-{index:012x}")).unwrap()
    }

    fn recovery_task(task_id: u64, execution_id: u64, instance_id: u64) -> persistence::StoredTask {
        use contract::{ExecutionClass, MotherTool, RunAs, TaskState};
        use persistence::StoredRoute;
        use runtime::{ExecutionPayload, ExecutorRecord, ProviderToken};

        persistence::StoredTask {
            request_id: Some(id(task_id + 100)),
            task_id: id(task_id),
            execution_id: id(execution_id),
            state: TaskState::Created,
            cancel_requested: false,
            tool: MotherTool::Command,
            action: "run".to_owned(),
            created_at: "2026-09-09T00:00:00.000Z".to_owned(),
            started_at: None,
            ended_at: None,
            waiting_reason: None,
            executor: Some(ExecutorRecord {
                host: RuntimeHost::ApkRuntime,
                provider: ProviderToken::AppNative,
                execution_class: ExecutionClass::App,
                capability_generation: 1,
                fence: contract::Fence {
                    runtime_epoch: id(1),
                    host_generation: 1,
                    runtime_instance_id: id(instance_id),
                },
            }),
            route: Some(StoredRoute::Command { run_as: RunAs::App }),
            payload: Some(ExecutionPayload::OpaqueOperation("command.run".to_owned())),
            result: None,
            error: None,
            reserved_bytes: runtime::RESERVE_FLOOR_BYTES,
            automation_owner: None,
        }
    }

    fn recovery_synchronous(
        request_id: u64,
        execution_id: u64,
        instance_id: u64,
    ) -> persistence::RequestRecord {
        use contract::{ExecutionClass, RunAs};
        use persistence::{StoredRoute, StoredSynchronousExecution};
        use runtime::{ExecutionPayload, ExecutorRecord, ProviderToken, SynchronousExecutionState};

        persistence::RequestRecord {
            request_id: id(request_id),
            payload_sha256: "aa".repeat(32),
            expires_at_ms: None,
            task_id: None,
            synchronous_execution: Some(StoredSynchronousExecution {
                execution_id: id(execution_id),
                operation: "command.run".to_owned(),
                state: SynchronousExecutionState::Running,
                ended_at: None,
                executor: ExecutorRecord {
                    host: RuntimeHost::ApkRuntime,
                    provider: ProviderToken::AppNative,
                    execution_class: ExecutionClass::App,
                    capability_generation: 1,
                    fence: contract::Fence {
                        runtime_epoch: id(1),
                        host_generation: 1,
                        runtime_instance_id: id(instance_id),
                    },
                },
                route: StoredRoute::Command { run_as: RunAs::App },
                payload: ExecutionPayload::OpaqueOperation("command.run".to_owned()),
                result: None,
                error: None,
                reserved_bytes: runtime::RESERVE_FLOOR_BYTES,
                terminal_bytes: 0,
            }),
            mutation_result: None,
        }
    }

    #[test]
    fn process_stat_parser_uses_field_twenty_two() {
        let path = std::env::temp_dir().join(format!("droidbridge-stat-{}", uuid::Uuid::new_v4()));
        fs::write(
            &path,
            "42 (name with spaces) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 12345 22",
        )
        .unwrap();
        assert_eq!(read_start_ticks(&path).unwrap(), 12345);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn i4_g04_i5_g10_shared_guard_recovery_vectors_drive_app_plan() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../fixtures/guard-recovery-vectors.json"
        ))
        .unwrap();
        assert_eq!(fixture["schema_version"], 1);
        assert!(fixture["vectors"].as_array().unwrap().iter().any(|vector| {
            vector["name"] == "mixed_records"
                && vector["expected"]["prior_instances"] == serde_json::json!([7003, 7004])
        }));

        let base =
            std::env::temp_dir().join(format!("droidbridge-i5-recovery-{}", uuid::Uuid::new_v4()));
        let old_boot_id = id(90);
        let current_boot_id = id(91);
        let current_instance_id = id(900);
        let first_execution_id = id(920);
        let second_execution_id = id(921);
        let synchronous_execution_id = id(922);
        let proof_directory = base.join("execution-guards").join(old_boot_id.as_str());
        fs::create_dir_all(&proof_directory).unwrap();
        fs::write(
            proof_directory.join(format!("{}.proof", first_execution_id.as_str())),
            [],
        )
        .unwrap();
        fs::write(
            proof_directory.join(format!("{}.proof", second_execution_id.as_str())),
            [],
        )
        .unwrap();
        fs::write(
            proof_directory.join(format!("{}.proof", synchronous_execution_id.as_str())),
            [],
        )
        .unwrap();
        let mut state = CanonicalState::default();
        state.tasks.push(recovery_task(820, 920, 720));
        state.tasks.push(recovery_task(821, 921, 721));
        state
            .request_records
            .push(recovery_synchronous(822, 922, 722));

        let first = await_guard_recovery_plan(
            &state,
            &current_instance_id,
            &current_boot_id,
            &GuardProofDirectory::new(&base),
            &ProcFacts,
        )
        .unwrap();
        assert!(first.guards_are_clean());
        assert_eq!(first.prior_instances(), &[id(720), id(721), id(722)]);
        assert_eq!(first.records().len(), 3);

        state.tasks[0].state = contract::TaskState::Interrupted;
        let resumed = await_guard_recovery_plan(
            &state,
            &current_instance_id,
            &current_boot_id,
            &GuardProofDirectory::new(&base),
            &ProcFacts,
        )
        .unwrap();
        assert!(resumed.guards_are_clean());
        assert_eq!(resumed.prior_instances(), &[id(721), id(722)]);
        assert_eq!(resumed.records().len(), 3);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn i6_native_launcher_accepts_only_uid2000_installed_guard_and_free_slot() {
        let directory = Path::new("/data/app/example/lib/arm64");
        let guard = directory.join("libdroidbridge_exec_guard.so");
        assert!(validate_shizuku_launch(2_000, directory, &guard, 63));
        assert!(!validate_shizuku_launch(0, directory, &guard, 0));
        assert!(!validate_shizuku_launch(
            2_000,
            directory,
            Path::new("/data/local/tmp/libdroidbridge_exec_guard.so"),
            0,
        ));
        assert!(!validate_shizuku_launch(2_000, directory, &guard, 64));
    }

    #[test]
    fn i6_native_launcher_rejects_invalid_identity_and_unbounded_argv() {
        let client_id = "11111111-1111-4111-8111-111111111111";
        let execution_id = "22222222-2222-4222-8222-222222222222";
        assert!(validate_shizuku_arguments(
            client_id,
            execution_id,
            "/system/bin/id",
            &["-u".to_owned()],
            "/",
        ));
        assert!(!validate_shizuku_arguments(
            "client",
            execution_id,
            "/system/bin/id",
            &[],
            "/",
        ));
        assert!(!validate_shizuku_arguments(
            client_id,
            execution_id,
            "relative",
            &[],
            "/",
        ));
        assert!(!validate_shizuku_arguments(
            client_id,
            execution_id,
            "/system/bin/id",
            &["x".repeat(16_385)],
            "/",
        ));
        assert!(!validate_shizuku_arguments(
            client_id,
            execution_id,
            "/system/bin/id",
            &vec!["x".to_owned(); 257],
            "/",
        ));
        assert!(!validate_shizuku_arguments(
            client_id,
            execution_id,
            "/system/bin/id",
            &[],
            "relative",
        ));
        assert!(!validate_shizuku_arguments(
            client_id,
            execution_id,
            "/system/bin/id",
            &[],
            "",
        ));
        assert!(!validate_shizuku_arguments(
            client_id,
            execution_id,
            "/system/bin/id",
            &[],
            "/\0",
        ));
    }
}
