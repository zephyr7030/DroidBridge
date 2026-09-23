use crate::{
    AdmittedExecution, ArtifactPort, CapabilityPort, ExecutionCancelOutcome, ExecutionCompletion,
    ExecutionFailure, ExecutionOutcome, ExecutionPayload, ExecutionPort, HostControlPort,
    LocalExecutionClaims, PersistencePort, PortFuture, ProviderToken, RESERVE_FLOOR_BYTES,
    RuntimeCore, SynchronousAdmission, TaskAdmission, TaskAdmissionResult, UI_ENVELOPE_LIMIT_BYTES,
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use contract::{
    ArchiveEntry, ArchiveEntryType, ArchiveListOperation, DataEncoding, ErrorCode, FileEntry,
    FileTarget, FileTargetType, FileType, FilesystemArchiveInput, FilesystemArchiveListResult,
    FilesystemArchiveTaskResult, FilesystemDownloadInput, FilesystemDownloadResult,
    FilesystemInspectInput, FilesystemInspectResult, FilesystemManageInput, FilesystemManageResult,
    FilesystemReadInput, FilesystemReadResult, FilesystemWriteInput, FilesystemWriteResult,
    ManageOperation, MotherTool, ReadSource, Replacement, RequestId, TaskAccepted, True, UuidV4,
};
use domain::DomainError;
use domain::{AdmittedExecutor, ExecutorRequest, FilesystemRoute, Preflight, resolve_executor};
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::{
    cmp::min,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

const MAX_INLINE_ENVELOPE_OVERHEAD: usize = 1_024;
const PRIMITIVE_DIRECTORY_PAGE_ENTRIES: usize = 512;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, PartialEq)]
pub enum FilesystemExecutionResult {
    Synchronous(serde_json::Value),
    Task(contract::TaskTerminalResult),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilesystemCandidate {
    App,
    Shizuku,
}

pub trait FilesystemPreflightPort {
    fn preflight(
        &self,
        candidate: FilesystemCandidate,
        call: &contract::FilesystemCall,
    ) -> Result<Preflight, DomainError>;
}

pub async fn handle_filesystem_public<P, A, E, C, H>(
    core: &RuntimeCore<P, A, E, C, H>,
    request_id: RequestId,
    payload_sha256: String,
    call: contract::FilesystemCall,
    timestamp: String,
    now_ms: u64,
) -> Result<serde_json::Value, DomainError>
where
    P: PersistencePort + 'static,
    A: ArtifactPort + Clone + 'static,
    E: ExecutionPort + FilesystemPreflightPort + 'static,
    C: CapabilityPort + 'static,
    H: HostControlPort + 'static,
{
    let capability = core.capability_snapshot()?;
    let Some(route) = filesystem_executor_request(&capability, core.execution_port(), &call)?
    else {
        let result = FilesystemKernel::new(core.artifact_port().clone())
            .execute(call, None)
            .await?;
        return match result {
            FilesystemExecutionResult::Synchronous(value) => Ok(value),
            FilesystemExecutionResult::Task(_) => Err(DomainError::new(
                ErrorCode::InternalError,
                "executor-free filesystem request produced a Task",
            )),
        };
    };
    let action = filesystem_action(&call);
    let execution_id = new_uuid()?;
    let payload = ExecutionPayload::FilesystemCall(call.clone());
    if is_filesystem_task(&call) {
        let task_id = new_uuid()?;
        let admission = core
            .admit_task(TaskAdmission {
                request_id,
                payload_sha256,
                task_id: task_id.clone(),
                execution_id,
                tool: MotherTool::Filesystem,
                action: action.to_owned(),
                route,
                payload,
                created_at: timestamp.clone(),
                settlement_bound_bytes: RESERVE_FLOOR_BYTES,
                now_ms,
            })
            .await?;
        let admitted_task_id = match admission {
            TaskAdmissionResult::Admitted(snapshot) => {
                let core = core.clone();
                let running_id = snapshot.task_id.clone();
                let started_at = timestamp.clone();
                tokio::spawn(async move {
                    let _ = core.run_task(&running_id, started_at, now_ms).await;
                });
                snapshot.task_id
            }
            TaskAdmissionResult::Replay(snapshot) => snapshot.task_id,
        };
        serde_json::to_value(TaskAccepted {
            task_id: admitted_task_id,
        })
        .map_err(|_| DomainError::new(ErrorCode::InternalError, "Task result encoding failed"))
    } else {
        core.run_synchronous(
            SynchronousAdmission {
                request_id,
                payload_sha256,
                execution_id,
                operation: format!("filesystem.{action}"),
                route,
                payload,
                settlement_bound_bytes: RESERVE_FLOOR_BYTES,
                now_ms,
            },
            timestamp,
            now_ms,
        )
        .await
        .map_err(|error| DomainError::new(error.code, "filesystem execution failed"))
    }
}

fn filesystem_action(call: &contract::FilesystemCall) -> &'static str {
    match call {
        contract::FilesystemCall::Inspect(_) => "inspect",
        contract::FilesystemCall::Read(_) => "read",
        contract::FilesystemCall::Write(_) => "write",
        contract::FilesystemCall::Manage(_) => "manage",
        contract::FilesystemCall::Download(_) => "download",
        contract::FilesystemCall::Archive(_) => "archive",
    }
}

fn is_filesystem_task(call: &contract::FilesystemCall) -> bool {
    matches!(
        call,
        contract::FilesystemCall::Download(_)
            | contract::FilesystemCall::Archive(
                FilesystemArchiveInput::Extract { .. } | FilesystemArchiveInput::Create { .. }
            )
    )
}

fn new_uuid() -> Result<UuidV4, DomainError> {
    UuidV4::parse(uuid::Uuid::new_v4().to_string())
        .map_err(|_| DomainError::new(ErrorCode::InternalError, "UUID generation failed"))
}

#[derive(Debug)]
pub struct FilesystemFrameworkSource {
    pub file: File,
    pub total_size: Option<u64>,
}

pub trait FilesystemFrameworkPort: Send + Sync {
    fn inspect(
        &self,
        execution: &AdmittedExecution,
        input: FilesystemInspectInput,
    ) -> Result<FilesystemInspectResult, DomainError>;

    fn open_read(
        &self,
        execution: &AdmittedExecution,
        target: &FileTarget,
    ) -> Result<FilesystemFrameworkSource, DomainError>;
}

/// The read-only descriptor role the authenticated Android framework surface returns
/// for one content stream. The role is the canonical wire label declared by the daemon
/// descriptor vocabulary, so one companion result carries it end to end.
pub const CONTENT_READ_ROLE: &str = "content";

/// One typed result the Android execution bridge returned for an already-admitted
/// primitive, with the descriptors whose ownership transferred with it.
#[derive(Debug)]
pub struct AndroidPrimitiveResult {
    pub payload: Vec<u8>,
    pub descriptors: Vec<(String, File)>,
}

/// The transport that carries one already-admitted S-ANDROID-001 typed primitive
/// request to whichever Android surface owns it, per S-HANDOFF-014. The APK-hosted
/// surface dispatches in process; the Magisk host dispatches over the authenticated
/// companion channel. Both reach the same port implementation below.
pub trait AndroidExecutionDispatch: Send + Sync {
    fn dispatch(
        &self,
        primitive: &str,
        payload: &[u8],
        execution: &AdmittedExecution,
    ) -> Result<AndroidPrimitiveResult, DomainError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilesystemPrimitiveMetadata {
    pub device: u64,
    pub inode: u64,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub size: u64,
    pub modified_at_epoch_seconds: i64,
    pub selinux_context: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilesystemPrimitiveDirectoryPage {
    pub names: Vec<std::ffi::OsString>,
    pub next_cookie: Option<u64>,
}

pub trait FilesystemPrimitivePort: Send + Sync {
    fn lstat(
        &self,
        execution: &AdmittedExecution,
        path: &Path,
    ) -> Result<FilesystemPrimitiveMetadata, DomainError>;

    fn open_read(&self, execution: &AdmittedExecution, path: &Path) -> Result<File, DomainError>;

    fn read_directory(
        &self,
        _execution: &AdmittedExecution,
        _path: &Path,
        _cookie: u64,
        _limit: usize,
    ) -> Result<FilesystemPrimitiveDirectoryPage, DomainError> {
        Err(unavailable_primitive())
    }

    fn access_write_search(
        &self,
        _execution: &AdmittedExecution,
        _path: &Path,
    ) -> Result<(), DomainError> {
        Err(unavailable_primitive())
    }

    fn create_exclusive(
        &self,
        _execution: &AdmittedExecution,
        _path: &Path,
        _mode: u32,
    ) -> Result<File, DomainError> {
        Err(unavailable_primitive())
    }

    fn apply_metadata(
        &self,
        _execution: &AdmittedExecution,
        _file: &File,
        _metadata: &FilesystemPrimitiveMetadata,
    ) -> Result<(), DomainError> {
        Err(unavailable_primitive())
    }

    fn rename_atomic(
        &self,
        _execution: &AdmittedExecution,
        _source: &Path,
        _destination: &Path,
        _exchange: bool,
    ) -> Result<(), DomainError> {
        Err(unavailable_primitive())
    }

    fn fsync_directory(
        &self,
        _execution: &AdmittedExecution,
        _path: &Path,
    ) -> Result<(), DomainError> {
        Err(unavailable_primitive())
    }

    fn mkdir(
        &self,
        _execution: &AdmittedExecution,
        _path: &Path,
        _mode: u32,
    ) -> Result<(), DomainError> {
        Err(unavailable_primitive())
    }

    fn unlink(&self, _execution: &AdmittedExecution, _path: &Path) -> Result<(), DomainError> {
        Err(unavailable_primitive())
    }

    fn readlink(
        &self,
        _execution: &AdmittedExecution,
        _path: &Path,
    ) -> Result<PathBuf, DomainError> {
        Err(unavailable_primitive())
    }

    fn symlink(
        &self,
        _execution: &AdmittedExecution,
        _target: &Path,
        _destination: &Path,
    ) -> Result<(), DomainError> {
        Err(unavailable_primitive())
    }
}

fn unavailable_primitive() -> DomainError {
    DomainError::new(
        ErrorCode::CapabilityUnavailable,
        "privileged filesystem primitive adapter is unavailable",
    )
}

#[derive(Clone, Copy, Default)]
pub struct UnavailableFilesystemPrimitivePort;

impl FilesystemPrimitivePort for UnavailableFilesystemPrimitivePort {
    fn lstat(
        &self,
        _execution: &AdmittedExecution,
        _path: &Path,
    ) -> Result<FilesystemPrimitiveMetadata, DomainError> {
        Err(DomainError::new(
            ErrorCode::CapabilityUnavailable,
            "privileged filesystem primitive adapter is unavailable",
        ))
    }

    fn open_read(&self, _execution: &AdmittedExecution, _path: &Path) -> Result<File, DomainError> {
        Err(DomainError::new(
            ErrorCode::CapabilityUnavailable,
            "privileged filesystem primitive adapter is unavailable",
        ))
    }
}

#[derive(Clone, Copy, Default)]
pub struct UnavailableFilesystemFrameworkPort;

impl FilesystemFrameworkPort for UnavailableFilesystemFrameworkPort {
    fn inspect(
        &self,
        _execution: &AdmittedExecution,
        _input: FilesystemInspectInput,
    ) -> Result<FilesystemInspectResult, DomainError> {
        Err(DomainError::new(
            ErrorCode::CapabilityUnavailable,
            "Android framework filesystem adapter is unavailable",
        ))
    }

    fn open_read(
        &self,
        _execution: &AdmittedExecution,
        _target: &FileTarget,
    ) -> Result<FilesystemFrameworkSource, DomainError> {
        Err(DomainError::new(
            ErrorCode::CapabilityUnavailable,
            "Android framework filesystem adapter is unavailable",
        ))
    }
}

/// The one S-AUTH-FS-001 Android framework filesystem port over an
/// `AndroidExecutionDispatch`. The admitted request, its encoding and the verification
/// of what came back are identical on both hosts, so only the transport below differs.
#[derive(Clone)]
pub struct AndroidFrameworkFilesystemPort<D> {
    dispatch: D,
}

impl<D> AndroidFrameworkFilesystemPort<D> {
    pub const fn new(dispatch: D) -> Self {
        Self { dispatch }
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ContentReadMetadata {
    #[serde(default)]
    total_size: Option<u64>,
}

impl<D: AndroidExecutionDispatch> FilesystemFrameworkPort for AndroidFrameworkFilesystemPort<D> {
    fn inspect(
        &self,
        execution: &AdmittedExecution,
        input: FilesystemInspectInput,
    ) -> Result<FilesystemInspectResult, DomainError> {
        let payload = serde_json::to_vec(&input).map_err(|_| {
            DomainError::new(ErrorCode::InternalError, "content inspect encoding failed")
        })?;
        let result = self
            .dispatch
            .dispatch("ContentInspect", &payload, execution)?;
        if !result.descriptors.is_empty() {
            return Err(DomainError::new(
                ErrorCode::IoError,
                "content inspect returned unexpected descriptors",
            ));
        }
        let inspected: FilesystemInspectResult =
            serde_json::from_slice(&result.payload).map_err(|_| {
                DomainError::new(ErrorCode::IoError, "content inspect result is invalid")
            })?;
        if inspected.target != input.target {
            return Err(DomainError::new(
                ErrorCode::IoError,
                "content inspect returned a different target",
            ));
        }
        Ok(inspected)
    }

    fn open_read(
        &self,
        execution: &AdmittedExecution,
        target: &FileTarget,
    ) -> Result<FilesystemFrameworkSource, DomainError> {
        let payload = serde_json::to_vec(target).map_err(|_| {
            DomainError::new(ErrorCode::InternalError, "content target encoding failed")
        })?;
        let mut result = self
            .dispatch
            .dispatch("ContentOpenRead", &payload, execution)?;
        if result.descriptors.len() != 1 || result.descriptors[0].0 != CONTENT_READ_ROLE {
            return Err(DomainError::new(
                ErrorCode::IoError,
                "content read returned an invalid descriptor set",
            ));
        }
        let metadata: ContentReadMetadata =
            serde_json::from_slice(&result.payload).map_err(|_| {
                DomainError::new(ErrorCode::IoError, "content read metadata is invalid")
            })?;
        let (_, file) = result.descriptors.remove(0);
        verify_read_only(&file)?;
        Ok(FilesystemFrameworkSource {
            file,
            total_size: metadata.total_size,
        })
    }
}

#[cfg(unix)]
fn verify_read_only(file: &File) -> Result<(), DomainError> {
    let flags = rustix::fs::fcntl_getfl(file)
        .map_err(|_| DomainError::new(ErrorCode::IoError, "content read descriptor is invalid"))?;
    if flags & rustix::fs::OFlags::ACCMODE != rustix::fs::OFlags::RDONLY {
        return Err(DomainError::new(
            ErrorCode::PermissionDenied,
            "content read descriptor is not read-only",
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn verify_read_only(_file: &File) -> Result<(), DomainError> {
    Err(DomainError::new(
        ErrorCode::CapabilityUnavailable,
        "Android framework filesystem adapter is unavailable",
    ))
}

pub fn resolve_filesystem_executor<P: FilesystemPreflightPort>(
    capability: &crate::CapabilitySnapshot,
    preflight: &P,
    call: &contract::FilesystemCall,
) -> Result<Option<AdmittedExecutor>, DomainError> {
    let Some(request) = filesystem_executor_request(capability, preflight, call)? else {
        return Ok(None);
    };
    let mut facts = capability.resolver_facts;
    facts.app_native = capability.context.app_execution_surface;
    facts.generations.app_native = capability.fence.host_generation;
    resolve_executor(
        capability.context.host,
        capability.fence.clone(),
        facts,
        request,
    )
    .map(Some)
}

pub fn filesystem_executor_request<P: FilesystemPreflightPort>(
    capability: &crate::CapabilitySnapshot,
    preflight: &P,
    call: &contract::FilesystemCall,
) -> Result<Option<ExecutorRequest>, DomainError> {
    if capability.context.readiness != contract::RuntimeReadiness::Ready {
        return Err(DomainError::new(
            ErrorCode::CapabilityUnavailable,
            "Runtime is not ready for filesystem execution",
        ));
    }
    let Some((route, target_type, shared_storage)) = filesystem_route(call)? else {
        return Ok(None);
    };
    let mut facts = capability.resolver_facts;
    facts.app_native = capability.context.app_execution_surface;
    facts.generations.app_native = capability.fence.host_generation;
    let (app_preflight, shizuku_preflight) = if capability.context.host
        == contract::RuntimeHost::ApkRuntime
        && target_type == FileTargetType::Path
    {
        if shared_storage {
            if facts.shizuku == contract::CapabilityState::Available {
                (Preflight::Unknown, Preflight::Positive)
            } else {
                (Preflight::Positive, Preflight::Unknown)
            }
        } else {
            let app = preflight.preflight(FilesystemCandidate::App, call)?;
            let shizuku = if app == Preflight::Positive
                || facts.shizuku != contract::CapabilityState::Available
            {
                Preflight::Unknown
            } else {
                preflight.preflight(FilesystemCandidate::Shizuku, call)?
            };
            (app, shizuku)
        }
    } else {
        (Preflight::Unknown, Preflight::Unknown)
    };
    let request = ExecutorRequest::Filesystem {
        route,
        target_type,
        app_preflight,

        shizuku_preflight,
    };
    resolve_executor(
        capability.context.host,
        capability.fence.clone(),
        facts,
        request,
    )?;
    Ok(Some(request))
}

fn filesystem_route(
    call: &contract::FilesystemCall,
) -> Result<Option<(FilesystemRoute, FileTargetType, bool)>, DomainError> {
    use contract::FilesystemCall;
    let (route, targets): (FilesystemRoute, Vec<&FileTarget>) = match call {
        FilesystemCall::Inspect(input) => (FilesystemRoute::InspectOrRead, vec![&input.target]),
        FilesystemCall::Read(input) => match &input.source {
            ReadSource::Target { target } => (FilesystemRoute::InspectOrRead, vec![target]),
            ReadSource::DataRef { .. } => return Ok(None),
        },
        FilesystemCall::Write(input) => {
            let target = match input {
                FilesystemWriteInput::Create { target, .. }
                | FilesystemWriteInput::Replace { target, .. }
                | FilesystemWriteInput::Edit { target, .. } => target,
            };
            (FilesystemRoute::Mutation, vec![target])
        }
        FilesystemCall::Manage(input) => match input {
            FilesystemManageInput::Mkdir { target, .. }
            | FilesystemManageInput::Delete { target, .. } => {
                (FilesystemRoute::Mutation, vec![target])
            }
            FilesystemManageInput::Copy {
                source,
                destination,
                ..
            }
            | FilesystemManageInput::Move {
                source,
                destination,
                ..
            } => (FilesystemRoute::Mutation, vec![source, destination]),
        },
        FilesystemCall::Download(input) => (FilesystemRoute::Mutation, vec![&input.destination]),
        FilesystemCall::Archive(input) => match input {
            FilesystemArchiveInput::List { target, .. } => {
                (FilesystemRoute::InspectOrRead, vec![target])
            }
            FilesystemArchiveInput::Extract {
                target,
                destination,
                ..
            } => (FilesystemRoute::Mutation, vec![target, destination]),
            FilesystemArchiveInput::Create {
                sources,
                destination,
                ..
            } => {
                let mut targets = sources.iter().collect::<Vec<_>>();
                targets.push(destination);
                (FilesystemRoute::Mutation, targets)
            }
        },
    };
    let target_type = targets
        .first()
        .map_or(FileTargetType::Path, |target| target.target_type);
    if targets
        .iter()
        .any(|target| target.target_type != target_type)
    {
        return Err(DomainError::new(
            ErrorCode::Unsupported,
            "filesystem action cannot cross path and content providers",
        ));
    }
    if target_type == FileTargetType::ContentUri
        && (!matches!(call, FilesystemCall::Inspect(_) | FilesystemCall::Read(_)))
    {
        return Err(DomainError::new(
            ErrorCode::Unsupported,
            "content URI supports only inspect and read in protocol v1",
        ));
    }
    let shared_storage = if target_type == FileTargetType::Path {
        let mut namespaces = targets
            .iter()
            .map(|target| is_android_shared_storage_path(&target.value));
        let first = namespaces.next().unwrap_or(false);
        if namespaces.any(|shared| shared != first) {
            return Err(DomainError::new(
                ErrorCode::Unsupported,
                "filesystem action cannot cross Android storage authorities",
            ));
        }
        first
    } else {
        false
    };
    Ok(Some((route, target_type, shared_storage)))
}

fn is_android_shared_storage_path(value: &str) -> bool {
    let path = value.trim_end_matches('/');
    path == "/sdcard"
        || path.starts_with("/sdcard/")
        || path == "/storage/self/primary"
        || path.starts_with("/storage/self/primary/")
        || path
            .strip_prefix("/storage/emulated/")
            .is_some_and(|suffix| !suffix.is_empty())
}

pub fn filesystem_preflight(call: &contract::FilesystemCall) -> Result<Preflight, DomainError> {
    use contract::FilesystemCall;
    let positive = match call {
        FilesystemCall::Inspect(input) => preflight_inspect(&input.target)?,
        FilesystemCall::Read(input) => match &input.source {
            ReadSource::Target { target } => preflight_read_source(target, false)?,
            ReadSource::DataRef { .. } => true,
        },
        FilesystemCall::Write(input) => match input {
            FilesystemWriteInput::Create { target, .. } => preflight_destination(target, false)?,
            FilesystemWriteInput::Replace { target, .. }
            | FilesystemWriteInput::Edit { target, .. } => preflight_replace(target)?,
        },
        FilesystemCall::Manage(input) => match input {
            FilesystemManageInput::Mkdir { target, parents } => preflight_mkdir(target, *parents)?,
            FilesystemManageInput::Delete { target, .. } => preflight_existing_mutation(target)?,
            FilesystemManageInput::Copy {
                source,
                destination,
                overwrite,
                ..
            } => {
                preflight_read_source(source, false)?
                    && preflight_destination(destination, *overwrite)?
            }
            FilesystemManageInput::Move {
                source,
                destination,
                overwrite,
                ..
            } => {
                preflight_existing_mutation(source)?
                    && preflight_destination(destination, *overwrite)?
            }
        },
        FilesystemCall::Download(input) => {
            preflight_destination(&input.destination, input.overwrite)?
        }
        FilesystemCall::Archive(input) => match input {
            FilesystemArchiveInput::List { target, .. } => preflight_read_source(target, false)?,
            FilesystemArchiveInput::Extract {
                target,
                destination,
                overwrite,
            } => {
                preflight_read_source(target, false)?
                    && preflight_destination(destination, *overwrite)?
            }
            FilesystemArchiveInput::Create {
                sources,
                destination,
                overwrite,
                ..
            } => {
                let mut readable = true;
                for source in sources {
                    readable &= preflight_read_source(source, false)?;
                }
                readable && preflight_destination(destination, *overwrite)?
            }
        },
    };
    Ok(if positive {
        Preflight::Positive
    } else {
        Preflight::Negative
    })
}

fn preflight_inspect(target: &FileTarget) -> Result<bool, DomainError> {
    let path = preflight_path(target)?;
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(_) => return Ok(false),
    };
    if metadata.is_dir()
        && File::open(&path)
            .and_then(|directory| directory.metadata())
            .is_err()
    {
        return Ok(false);
    }
    Ok(true)
}

fn preflight_read_source(target: &FileTarget, regular_only: bool) -> Result<bool, DomainError> {
    let path = preflight_path(target)?;
    let file = match OpenOptions::new().read(true).open(&path) {
        Ok(file) => file,
        Err(_) => return Ok(false),
    };
    let metadata = match file.metadata() {
        Ok(metadata) => metadata,
        Err(_) => return Ok(false),
    };
    Ok(!regular_only || metadata.is_file())
}

fn preflight_replace(target: &FileTarget) -> Result<bool, DomainError> {
    let path = preflight_path(target)?;
    if !preflight_read_source(target, true)? || !preflight_mutation_parent(&path)? {
        return Ok(false);
    }
    Ok(preflight_sticky_allows(&path))
}

fn preflight_existing_mutation(target: &FileTarget) -> Result<bool, DomainError> {
    let path = preflight_path(target)?;
    if fs::symlink_metadata(&path).is_err() || !preflight_mutation_parent(&path)? {
        return Ok(false);
    }
    Ok(preflight_sticky_allows(&path))
}

fn preflight_destination(target: &FileTarget, overwrite: bool) -> Result<bool, DomainError> {
    let path = preflight_path(target)?;
    if !preflight_mutation_parent(&path)? {
        return Ok(false);
    }
    match fs::symlink_metadata(&path) {
        Ok(_) if !overwrite => Ok(false),
        Ok(_) => Ok(preflight_sticky_allows(&path)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(_) => Ok(false),
    }
}

fn preflight_mkdir(target: &FileTarget, parents: bool) -> Result<bool, DomainError> {
    let path = preflight_path(target)?;
    match fs::symlink_metadata(&path) {
        Ok(metadata) => Ok(parents && metadata.is_dir()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if parents {
                let Some(existing) = path.ancestors().skip(1).find(|ancestor| ancestor.exists())
                else {
                    return Ok(false);
                };
                preflight_mutation_directory(existing)
            } else {
                preflight_mutation_parent(&path)
            }
        }
        Err(_) => Ok(false),
    }
}

fn preflight_path(target: &FileTarget) -> Result<PathBuf, DomainError> {
    require_path(target)?;
    normalize_absolute_path(&target.value)
}

fn preflight_mutation_parent(path: &Path) -> Result<bool, DomainError> {
    let Some(parent) = path.parent() else {
        return Ok(false);
    };
    preflight_mutation_directory(parent)
}

#[cfg(unix)]
fn preflight_mutation_directory(path: &Path) -> Result<bool, DomainError> {
    let directory = match File::open(path) {
        Ok(directory) => directory,
        Err(_) => return Ok(false),
    };
    if !directory.metadata().is_ok_and(|metadata| metadata.is_dir()) {
        return Ok(false);
    }
    preflight_write_search_access(path)
}

#[cfg(not(unix))]
fn preflight_mutation_directory(path: &Path) -> Result<bool, DomainError> {
    if !fs::metadata(path).is_ok_and(|metadata| metadata.is_dir()) {
        return Ok(false);
    }
    preflight_write_search_access(path)
}

#[cfg(unix)]
fn preflight_write_search_access(path: &Path) -> Result<bool, DomainError> {
    use rustix::fs::{Access, AtFlags, CWD, accessat};
    #[cfg(target_os = "android")]
    let flags = AtFlags::empty();
    #[cfg(not(target_os = "android"))]
    let flags = AtFlags::EACCESS;
    Ok(accessat(CWD, path, Access::WRITE_OK | Access::EXEC_OK, flags).is_ok())
}

#[cfg(not(unix))]
fn preflight_write_search_access(_path: &Path) -> Result<bool, DomainError> {
    Ok(true)
}

#[cfg(unix)]
fn preflight_sticky_allows(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Some(parent) = path.parent() else {
        return false;
    };
    let Ok(parent_metadata) = fs::metadata(parent) else {
        return false;
    };
    if parent_metadata.mode() & 0o1000 == 0 {
        return true;
    }
    let Ok(target_metadata) = fs::symlink_metadata(path) else {
        return false;
    };
    let effective_uid = rustix::process::geteuid().as_raw();
    effective_uid == 0
        || effective_uid == parent_metadata.uid()
        || effective_uid == target_metadata.uid()
}

#[cfg(not(unix))]
fn preflight_sticky_allows(_path: &Path) -> bool {
    true
}

pub struct FilesystemKernel<A> {
    artifacts: A,
}

#[derive(Clone)]
pub struct NativeFilesystemExecutionSurface<
    A,
    C,
    F = UnavailableFilesystemFrameworkPort,
    P = UnavailableFilesystemPrimitivePort,
> {
    canonical_base: PathBuf,
    artifacts: A,
    capabilities: C,
    provider: ProviderToken,
    framework: F,
    primitives: P,
    claims: LocalExecutionClaims,
}

impl<A, C>
    NativeFilesystemExecutionSurface<
        A,
        C,
        UnavailableFilesystemFrameworkPort,
        UnavailableFilesystemPrimitivePort,
    >
{
    pub fn new(
        canonical_base: PathBuf,
        artifacts: A,
        capabilities: C,
        provider: ProviderToken,
    ) -> Self {
        Self {
            canonical_base,
            artifacts,
            capabilities,
            provider,
            framework: UnavailableFilesystemFrameworkPort,
            primitives: UnavailableFilesystemPrimitivePort,
            claims: LocalExecutionClaims::default(),
        }
    }
}

impl<A, C, F, P> NativeFilesystemExecutionSurface<A, C, F, P> {
    pub fn with_framework<N>(self, framework: N) -> NativeFilesystemExecutionSurface<A, C, N, P> {
        NativeFilesystemExecutionSurface {
            canonical_base: self.canonical_base,
            artifacts: self.artifacts,
            capabilities: self.capabilities,
            provider: self.provider,
            framework,
            primitives: self.primitives,
            claims: self.claims,
        }
    }

    pub fn with_primitives<N>(self, primitives: N) -> NativeFilesystemExecutionSurface<A, C, F, N> {
        NativeFilesystemExecutionSurface {
            canonical_base: self.canonical_base,
            artifacts: self.artifacts,
            capabilities: self.capabilities,
            provider: self.provider,
            framework: self.framework,
            primitives,
            claims: self.claims,
        }
    }
}

impl<A, C, F, P> FilesystemPreflightPort for NativeFilesystemExecutionSurface<A, C, F, P>
where
    C: CapabilityPort,
    P: FilesystemPrimitivePort,
{
    fn preflight(
        &self,
        candidate: FilesystemCandidate,
        call: &contract::FilesystemCall,
    ) -> Result<Preflight, DomainError> {
        match (self.provider, candidate) {
            (ProviderToken::AppNative, FilesystemCandidate::App) => filesystem_preflight(call),
            (ProviderToken::AppNative, FilesystemCandidate::Shizuku) => {
                shizuku_filesystem_preflight(&self.primitives, &self.capabilities.current()?, call)
            }
            _ => Ok(Preflight::Unknown),
        }
    }
}

impl<A, C, F, P> ExecutionPort for NativeFilesystemExecutionSurface<A, C, F, P>
where
    A: ArtifactPort + Clone + 'static,
    C: CapabilityPort + Clone + 'static,
    F: FilesystemFrameworkPort + Clone + 'static,
    P: FilesystemPrimitivePort + Clone + 'static,
{
    fn claim_and_start<'a>(
        &'a self,
        execution: AdmittedExecution,
    ) -> PortFuture<'a, Result<ExecutionCompletion, ExecutionFailure>> {
        let claim = match self.claims.claim(execution.execution_id.clone()) {
            Ok(claim) => claim,
            Err(error) => {
                return Box::pin(async move {
                    Err(ExecutionFailure {
                        error,
                        cleanup_verified: true,
                    })
                });
            }
        };
        let claims = self.claims.clone();
        let canonical_base = self.canonical_base.clone();
        let artifacts = self.artifacts.clone();
        let capabilities = self.capabilities.clone();
        let native_provider = self.provider;
        let framework = self.framework.clone();
        let primitives = self.primitives.clone();
        Box::pin(async move {
            let result = execute_filesystem(
                canonical_base,
                artifacts,
                capabilities,
                native_provider,
                framework,
                primitives,
                &execution,
                &claim,
            )
            .await;
            let cleanup_verified = match &result {
                Ok(completion) => completion.cleanup_verified,
                Err(failure) => failure.cleanup_verified,
            };
            claims.finish(&claim, cleanup_verified);
            result
        })
    }

    fn cancel<'a>(
        &'a self,
        execution_id: &'a contract::UuidV4,
    ) -> PortFuture<'a, Result<ExecutionCancelOutcome, DomainError>> {
        Box::pin(self.claims.cancel(execution_id))
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_filesystem<A, C, F, P>(
    canonical_base: PathBuf,
    artifacts: A,
    capabilities: C,
    native_provider: ProviderToken,
    framework: F,
    primitives: P,
    execution: &AdmittedExecution,
    claim: &crate::LocalExecutionClaim,
) -> Result<ExecutionCompletion, ExecutionFailure>
where
    A: ArtifactPort,
    C: CapabilityPort,
    F: FilesystemFrameworkPort,
    P: FilesystemPrimitivePort,
{
    let call = match &execution.payload {
        ExecutionPayload::FilesystemCall(call) => call.clone(),
        _ => {
            return Err(execution_failure(
                ErrorCode::Unsupported,
                "native filesystem surface received a non-filesystem request",
                true,
            ));
        }
    };
    if execution.executor.provider != native_provider
        && execution.executor.provider != ProviderToken::AppFramework
        && execution.executor.provider != ProviderToken::Shizuku
    {
        return Err(execution_failure(
            ErrorCode::StaleAuthority,
            "filesystem execution provider does not match its host surface",
            true,
        ));
    }
    let current = capabilities.current().map_err(|error| ExecutionFailure {
        error,
        cleanup_verified: true,
    })?;
    if !filesystem_executor_is_current(&current, execution, execution.executor.provider) {
        return Err(execution_failure(
            ErrorCode::StaleAuthority,
            "filesystem executor fence or generation is stale",
            true,
        ));
    }
    claim.checkpoint().map_err(|error| ExecutionFailure {
        error,
        cleanup_verified: true,
    })?;
    let task_temp = execution
        .task_id
        .as_ref()
        .map(|task_id| canonical_base.join("tmp").join(task_id.as_str()));
    let kernel = FilesystemKernel::new(artifacts);
    let operation = filesystem_call_operation(&call);
    let execution_result = match execution.executor.provider {
        ProviderToken::AppFramework => {
            execute_framework_filesystem(&kernel, &framework, execution, call, claim)
        }
        ProviderToken::Shizuku => {
            execute_shizuku_filesystem(
                &kernel,
                &primitives,
                execution,
                call,
                task_temp.as_deref(),
                claim,
            )
            .await
        }
        _ => {
            kernel
                .execute_claimed(call, task_temp.as_deref(), claim)
                .await
        }
    };
    let cleanup_verified =
        cleanup_task_temp(task_temp.as_deref()).is_ok() && claim.cleanup_is_verified();
    let result = (|| -> Result<ExecutionOutcome, DomainError> {
        match execution_result {
            Ok(FilesystemExecutionResult::Synchronous(result)) if execution.task_id.is_none() => {
                let encoded_bytes = encoded_len(&result)?;
                Ok(ExecutionOutcome::SynchronousCompleted {
                    result,
                    encoded_bytes,
                })
            }
            Ok(FilesystemExecutionResult::Task(result)) if execution.task_id.is_some() => {
                let encoded_bytes = encoded_len(&result)?;
                Ok(ExecutionOutcome::Completed {
                    result,
                    encoded_bytes,
                })
            }
            Ok(_) => Err(DomainError::new(
                ErrorCode::InternalError,
                "filesystem result kind does not match its admission",
            )),
            Err(error) if error.code == ErrorCode::Cancelled => Ok(ExecutionOutcome::Cancelled {
                error: contract::PublicError {
                    code: ErrorCode::Cancelled,
                    operation,
                    retryable: false,
                    message: None,
                    capability: None,
                    details: None,
                },
                encoded_bytes: crate::RESERVE_FLOOR_BYTES,
            }),
            Err(error) => Err(error),
        }
    })();
    match result {
        Ok(outcome) => Ok(ExecutionCompletion {
            fence: execution_fence(execution),
            capability_generation: execution.executor.capability_generation,
            outcome,
            cleanup_verified,
        }),
        Err(error) => Err(ExecutionFailure {
            error,
            cleanup_verified,
        }),
    }
}

fn filesystem_executor_is_current(
    current: &crate::CapabilitySnapshot,
    execution: &AdmittedExecution,
    provider: ProviderToken,
) -> bool {
    let fence = &execution.executor.fence;
    if current.context.readiness != contract::RuntimeReadiness::Ready
        || current.context.host != execution.executor.host
        || current.fence.runtime_epoch != fence.runtime_epoch
        || current.fence.host_generation != fence.host_generation
        || current.fence.runtime_instance_id != fence.runtime_instance_id
    {
        return false;
    }
    match provider {
        ProviderToken::AppNative => {
            current.context.host == contract::RuntimeHost::ApkRuntime
                && current.context.app_execution_surface == contract::CapabilityState::Available
                && execution.executor.capability_generation == current.fence.host_generation
        }
        ProviderToken::MagiskNative => {
            current.context.host == contract::RuntimeHost::MagiskBackend
                && current.resolver_facts.magisk_native == contract::CapabilityState::Available
                && execution.executor.capability_generation
                    == current.resolver_facts.generations.magisk_native
        }
        ProviderToken::AppFramework => {
            current.resolver_facts.app_framework == contract::CapabilityState::Available
                && execution.executor.capability_generation
                    == current.resolver_facts.generations.app_framework
        }
        ProviderToken::Shizuku => {
            current.context.host == contract::RuntimeHost::ApkRuntime
                && current.resolver_facts.shizuku == contract::CapabilityState::Available
                && execution.executor.capability_generation
                    == current.resolver_facts.generations.shizuku
        }
        _ => false,
    }
}

pub fn shizuku_filesystem_preflight<P: FilesystemPrimitivePort>(
    primitives: &P,
    capability: &crate::CapabilitySnapshot,
    call: &contract::FilesystemCall,
) -> Result<Preflight, DomainError> {
    let execution = AdmittedExecution {
        execution_id: contract::UuidV4::parse(uuid::Uuid::new_v4().hyphenated().to_string())
            .map_err(|_| DomainError::new(ErrorCode::InternalError, "UUID generation failed"))?,
        task_id: None,
        executor: crate::ExecutorRecord {
            host: contract::RuntimeHost::ApkRuntime,
            provider: ProviderToken::Shizuku,
            execution_class: contract::ExecutionClass::Shizuku,
            capability_generation: capability.resolver_facts.generations.shizuku,
            fence: contract::Fence {
                runtime_epoch: capability.fence.runtime_epoch.clone(),
                host_generation: capability.fence.host_generation,
                runtime_instance_id: capability.fence.runtime_instance_id.clone(),
            },
        },
        payload: ExecutionPayload::FilesystemCall(call.clone()),
    };
    let accessible = match call {
        contract::FilesystemCall::Inspect(input) => {
            primitive_preflight_inspect(primitives, &execution, &input.target)?
        }
        contract::FilesystemCall::Read(input) => match &input.source {
            ReadSource::Target { target } => {
                primitive_preflight_read_source(primitives, &execution, target, false)?
            }
            ReadSource::DataRef { .. } => true,
        },
        contract::FilesystemCall::Write(input) => match input {
            FilesystemWriteInput::Create { target, .. } => {
                primitive_preflight_destination(primitives, &execution, target, false)?
            }
            FilesystemWriteInput::Replace { target, .. }
            | FilesystemWriteInput::Edit { target, .. } => {
                primitive_preflight_replace(primitives, &execution, target)?
            }
        },
        contract::FilesystemCall::Manage(input) => match input {
            FilesystemManageInput::Mkdir { target, parents } => {
                primitive_preflight_mkdir(primitives, &execution, target, *parents)?
            }
            FilesystemManageInput::Delete { target, .. } => {
                primitive_preflight_existing_mutation(primitives, &execution, target)?
            }
            FilesystemManageInput::Copy {
                source,
                destination,
                overwrite,
                ..
            } => {
                primitive_preflight_read_source(primitives, &execution, source, false)?
                    && primitive_preflight_destination(
                        primitives,
                        &execution,
                        destination,
                        *overwrite,
                    )?
            }
            FilesystemManageInput::Move {
                source,
                destination,
                overwrite,
                ..
            } => {
                primitive_preflight_existing_mutation(primitives, &execution, source)?
                    && primitive_preflight_destination(
                        primitives,
                        &execution,
                        destination,
                        *overwrite,
                    )?
            }
        },
        contract::FilesystemCall::Download(input) => primitive_preflight_destination(
            primitives,
            &execution,
            &input.destination,
            input.overwrite,
        )?,
        contract::FilesystemCall::Archive(input) => match input {
            FilesystemArchiveInput::List { target, .. } => {
                primitive_preflight_read_source(primitives, &execution, target, false)?
            }
            FilesystemArchiveInput::Extract {
                target,
                destination,
                overwrite,
            } => {
                primitive_preflight_read_source(primitives, &execution, target, true)?
                    && primitive_preflight_destination(
                        primitives,
                        &execution,
                        destination,
                        *overwrite,
                    )?
            }
            FilesystemArchiveInput::Create {
                sources,
                destination,
                overwrite,
                ..
            } => {
                let mut readable = true;
                for source in sources {
                    readable &=
                        primitive_preflight_read_source(primitives, &execution, source, false)?;
                }
                readable
                    && primitive_preflight_destination(
                        primitives,
                        &execution,
                        destination,
                        *overwrite,
                    )?
            }
        },
    };
    Ok(if accessible {
        Preflight::Positive
    } else {
        Preflight::Negative
    })
}

fn primitive_preflight_inspect<P: FilesystemPrimitivePort>(
    primitives: &P,
    execution: &AdmittedExecution,
    target: &FileTarget,
) -> Result<bool, DomainError> {
    let path = preflight_path(target)?;
    match primitives.lstat(execution, &path) {
        Ok(metadata) if remote_file_type(metadata.mode) == FileType::Directory => {
            Ok(primitives.read_directory(execution, &path, 0, 1).is_ok())
        }
        Ok(_) => Ok(true),
        Err(_) => Ok(false),
    }
}

fn primitive_preflight_read_source<P: FilesystemPrimitivePort>(
    primitives: &P,
    execution: &AdmittedExecution,
    target: &FileTarget,
    regular_only: bool,
) -> Result<bool, DomainError> {
    let path = preflight_path(target)?;
    let Ok(metadata) = primitives.lstat(execution, &path) else {
        return Ok(false);
    };
    match remote_file_type(metadata.mode) {
        FileType::File => Ok(primitives
            .open_read(execution, &path)
            .and_then(|file| file.metadata().map_err(fs_error))
            .is_ok_and(|metadata| metadata.is_file())),
        FileType::Directory if !regular_only => {
            Ok(primitives.read_directory(execution, &path, 0, 1).is_ok())
        }
        _ => Ok(false),
    }
}

fn primitive_preflight_replace<P: FilesystemPrimitivePort>(
    primitives: &P,
    execution: &AdmittedExecution,
    target: &FileTarget,
) -> Result<bool, DomainError> {
    let path = preflight_path(target)?;
    if !primitive_preflight_read_source(primitives, execution, target, true)?
        || !primitive_preflight_mutation_parent(primitives, execution, &path)
    {
        return Ok(false);
    }
    Ok(primitive_preflight_sticky_allows(
        primitives, execution, &path,
    ))
}

fn primitive_preflight_existing_mutation<P: FilesystemPrimitivePort>(
    primitives: &P,
    execution: &AdmittedExecution,
    target: &FileTarget,
) -> Result<bool, DomainError> {
    let path = preflight_path(target)?;
    if primitives.lstat(execution, &path).is_err()
        || !primitive_preflight_mutation_parent(primitives, execution, &path)
    {
        return Ok(false);
    }
    Ok(primitive_preflight_sticky_allows(
        primitives, execution, &path,
    ))
}

fn primitive_preflight_destination<P: FilesystemPrimitivePort>(
    primitives: &P,
    execution: &AdmittedExecution,
    target: &FileTarget,
    overwrite: bool,
) -> Result<bool, DomainError> {
    let path = preflight_path(target)?;
    if !primitive_preflight_mutation_parent(primitives, execution, &path) {
        return Ok(false);
    }
    match primitives.lstat(execution, &path) {
        Ok(_) if !overwrite => Ok(false),
        Ok(_) => Ok(primitive_preflight_sticky_allows(
            primitives, execution, &path,
        )),
        Err(error) if error.code == ErrorCode::NotFound => Ok(true),
        Err(_) => Ok(false),
    }
}

fn primitive_preflight_mkdir<P: FilesystemPrimitivePort>(
    primitives: &P,
    execution: &AdmittedExecution,
    target: &FileTarget,
    parents: bool,
) -> Result<bool, DomainError> {
    let path = preflight_path(target)?;
    match primitives.lstat(execution, &path) {
        Ok(metadata) => Ok(parents && remote_file_type(metadata.mode) == FileType::Directory),
        Err(error) if error.code == ErrorCode::NotFound => {
            if !parents {
                return Ok(primitive_preflight_mutation_parent(
                    primitives, execution, &path,
                ));
            }
            for ancestor in path.ancestors().skip(1) {
                match primitives.lstat(execution, ancestor) {
                    Ok(metadata) => {
                        return Ok(remote_file_type(metadata.mode) == FileType::Directory
                            && primitive_preflight_mutation_directory(
                                primitives, execution, ancestor,
                            ));
                    }
                    Err(error) if error.code == ErrorCode::NotFound => continue,
                    Err(_) => return Ok(false),
                }
            }
            Ok(false)
        }
        Err(_) => Ok(false),
    }
}

fn primitive_preflight_mutation_parent<P: FilesystemPrimitivePort>(
    primitives: &P,
    execution: &AdmittedExecution,
    path: &Path,
) -> bool {
    path.parent()
        .is_some_and(|parent| primitive_preflight_mutation_directory(primitives, execution, parent))
}

fn primitive_preflight_mutation_directory<P: FilesystemPrimitivePort>(
    primitives: &P,
    execution: &AdmittedExecution,
    path: &Path,
) -> bool {
    primitives
        .lstat(execution, path)
        .is_ok_and(|metadata| remote_file_type(metadata.mode) == FileType::Directory)
        && primitives.read_directory(execution, path, 0, 1).is_ok()
        && primitives.access_write_search(execution, path).is_ok()
}

fn primitive_preflight_sticky_allows<P: FilesystemPrimitivePort>(
    primitives: &P,
    execution: &AdmittedExecution,
    path: &Path,
) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    let Ok(parent_metadata) = primitives.lstat(execution, parent) else {
        return false;
    };
    if parent_metadata.mode & 0o1000 == 0 {
        return true;
    }
    primitives
        .lstat(execution, path)
        .is_ok_and(|target| parent_metadata.uid == 2_000 || target.uid == 2_000)
}

async fn execute_shizuku_filesystem<A, P>(
    kernel: &FilesystemKernel<A>,
    primitives: &P,
    execution: &AdmittedExecution,
    call: contract::FilesystemCall,
    temporary_directory: Option<&Path>,
    claim: &crate::LocalExecutionClaim,
) -> Result<FilesystemExecutionResult, DomainError>
where
    A: ArtifactPort,
    P: FilesystemPrimitivePort,
{
    cancellation_checkpoint(Some(claim))?;
    match call {
        contract::FilesystemCall::Inspect(input) => {
            validate_inspect_input(&input)?;
            let path = preflight_path(&input.target)?;
            let metadata = primitives.lstat(execution, &path)?;
            let target_type = remote_file_type(metadata.mode);
            let (entries, truncated) = if target_type == FileType::Directory {
                let mut entries = Vec::new();
                let mut truncated = false;
                inspect_primitive_directory(
                    primitives,
                    execution,
                    &path,
                    Path::new(""),
                    1,
                    input.max_depth,
                    input.recursive,
                    input.max_entries as usize,
                    &mut entries,
                    &mut truncated,
                )?;
                (Some(entries), Some(truncated))
            } else {
                (None, None)
            };
            synchronous_result(FilesystemInspectResult {
                target: input.target,
                target_type,
                size: (target_type == FileType::File).then_some(metadata.size),
                modified_at: remote_modified_at(metadata.modified_at_epoch_seconds),
                entries,
                truncated,
            })
        }
        contract::FilesystemCall::Read(input) => {
            validate_read_input(&input)?;
            let ReadSource::Target { target } = &input.source else {
                return Err(DomainError::new(
                    ErrorCode::StaleAuthority,
                    "Shizuku read cannot consume a data artifact",
                ));
            };
            let path = preflight_path(target)?;
            let (file, metadata) = primitive_open_regular(primitives, execution, &path)?;
            cancellation_checkpoint(Some(claim))?;
            synchronous_result(kernel.read_from_reader(
                input,
                Box::new(file),
                regular_size(&metadata),
            )?)
        }
        contract::FilesystemCall::Write(input) => {
            synchronous_result(primitive_write(primitives, execution, input, claim)?)
        }
        contract::FilesystemCall::Manage(input) => {
            synchronous_result(primitive_manage(primitives, execution, input, claim)?)
        }
        contract::FilesystemCall::Download(input) => {
            let temporary_directory = temporary_directory.ok_or_else(|| {
                DomainError::new(
                    ErrorCode::InternalError,
                    "filesystem Task has no temp owner",
                )
            })?;
            primitive_download(primitives, execution, input, temporary_directory, claim)
                .await
                .map(contract::TaskTerminalResult::FilesystemDownload)
                .map(FilesystemExecutionResult::Task)
        }
        contract::FilesystemCall::Archive(input) => match input {
            FilesystemArchiveInput::List { .. } => {
                synchronous_result(primitive_archive_list(primitives, execution, input)?)
            }
            FilesystemArchiveInput::Create {
                sources,
                destination,
                format,
                overwrite,
            } => {
                let temporary_directory = temporary_directory.ok_or_else(|| {
                    DomainError::new(
                        ErrorCode::InternalError,
                        "filesystem Task has no temp owner",
                    )
                })?;
                primitive_archive_create(
                    primitives,
                    execution,
                    sources,
                    destination,
                    format,
                    overwrite,
                    temporary_directory,
                    claim,
                )
                .map(contract::TaskTerminalResult::FilesystemArchive)
                .map(FilesystemExecutionResult::Task)
            }
            FilesystemArchiveInput::Extract {
                target,
                destination,
                overwrite,
            } => {
                let temporary_directory = temporary_directory.ok_or_else(|| {
                    DomainError::new(
                        ErrorCode::InternalError,
                        "filesystem Task has no temp owner",
                    )
                })?;
                primitive_archive_extract(
                    primitives,
                    execution,
                    target,
                    destination,
                    overwrite,
                    temporary_directory,
                    claim,
                )
                .map(contract::TaskTerminalResult::FilesystemArchive)
                .map(FilesystemExecutionResult::Task)
            }
        },
    }
}

fn primitive_write<P: FilesystemPrimitivePort>(
    primitives: &P,
    execution: &AdmittedExecution,
    input: FilesystemWriteInput,
    claim: &crate::LocalExecutionClaim,
) -> Result<FilesystemWriteResult, DomainError> {
    cancellation_checkpoint(Some(claim))?;
    let (target, bytes, create) = match input {
        FilesystemWriteInput::Create {
            target,
            content,
            encoding,
        } => (target, decode_content(&content, encoding)?, true),
        FilesystemWriteInput::Replace {
            target,
            content,
            encoding,
        } => (target, decode_content(&content, encoding)?, false),
        FilesystemWriteInput::Edit {
            target,
            replacements,
        } => {
            require_path(&target)?;
            let path = normalize_absolute_path(&target.value)?;
            reject_virtual_mutation(&path)?;
            let mut original = String::new();
            primitive_open_regular(primitives, execution, &path)?
                .0
                .read_to_string(&mut original)
                .map_err(fs_error)?;
            let edited = apply_replacements(&original, &replacements)?;
            (target, edited.into_bytes(), false)
        }
    };
    require_path(&target)?;
    let path = normalize_absolute_path(&target.value)?;
    reject_virtual_mutation(&path)?;
    primitive_atomic_write(primitives, execution, &path, &bytes, create, claim)?;
    let verified = primitive_read_all(primitives, execution, &path)?;
    if verified != bytes {
        return Err(postcondition_error());
    }
    Ok(FilesystemWriteResult {
        bytes_written: bytes.len() as u64,
        sha256: Some(sha256(&bytes)),
    })
}

fn primitive_atomic_write<P: FilesystemPrimitivePort>(
    primitives: &P,
    execution: &AdmittedExecution,
    path: &Path,
    bytes: &[u8],
    create: bool,
    claim: &crate::LocalExecutionClaim,
) -> Result<(), DomainError> {
    let mut existing = primitive_validate_destination(primitives, execution, path, !create)?;
    match (&existing, create) {
        (Some(_), true) => {
            return Err(DomainError::new(
                ErrorCode::AlreadyExists,
                "filesystem target already exists",
            ));
        }
        (None, false) => {
            return Err(DomainError::new(
                ErrorCode::NotFound,
                "filesystem target does not exist",
            ));
        }
        (Some(metadata), false) if remote_file_type(metadata.mode) != FileType::File => {
            return Err(DomainError::new(
                ErrorCode::Unsupported,
                "filesystem target is not a regular file",
            ));
        }
        _ => {}
    }
    if let Some(metadata) = existing.as_ref() {
        let (file, actual) = primitive_open_regular(primitives, execution, path)?;
        existing = Some(primitive_descriptor_metadata(&file, &actual, metadata)?);
    }
    let temporary = primitive_temporary_sibling(path)?;
    let mut temporary_owned = false;
    let outcome = (|| {
        let mode = existing
            .as_ref()
            .map_or(0o666, |metadata| metadata.mode & 0o7777);
        let mut file = primitives.create_exclusive(execution, &temporary, mode)?;
        temporary_owned = true;
        file.write_all(bytes).map_err(fs_error)?;
        if let Some(metadata) = &existing {
            primitives.apply_metadata(execution, &file, metadata)?;
        }
        file.sync_all().map_err(fs_error)?;
        if let Some(expected) = &existing {
            let actual = file.metadata().map_err(fs_error)?;
            let actual = primitive_descriptor_metadata(&file, &actual, expected)?;
            if !primitive_replacement_metadata_matches(&actual, expected) {
                return Err(postcondition_error());
            }
        }
        cancellation_checkpoint(Some(claim))?;
        primitive_publish_path(
            primitives, execution, &temporary, path, !create, claim, true,
        )?;
        let published = primitives.lstat(execution, path)?;
        if remote_file_type(published.mode) != FileType::File
            || published.size != bytes.len() as u64
            || existing.as_ref().is_some_and(|expected| {
                !primitive_replacement_metadata_matches(&published, expected)
            })
        {
            return Err(postcondition_error());
        }
        Ok(())
    })();
    if outcome.is_err() && temporary_owned && claim.cleanup_is_verified() {
        cleanup_primitive_path(primitives, execution, &temporary, claim);
    }
    outcome
}

fn primitive_manage<P: FilesystemPrimitivePort>(
    primitives: &P,
    execution: &AdmittedExecution,
    input: FilesystemManageInput,
    claim: &crate::LocalExecutionClaim,
) -> Result<FilesystemManageResult, DomainError> {
    cancellation_checkpoint(Some(claim))?;
    match input {
        FilesystemManageInput::Mkdir { target, parents } => {
            require_path(&target)?;
            let path = normalize_absolute_path(&target.value)?;
            primitive_mkdir(primitives, execution, &path, parents, claim)?;
            if primitive_optional_metadata(primitives, execution, &path)?
                .is_none_or(|metadata| remote_file_type(metadata.mode) != FileType::Directory)
            {
                return Err(postcondition_error());
            }
            Ok(FilesystemManageResult {
                operation: ManageOperation::Mkdir,
                completed: True,
                target: Some(target),
                source: None,
                destination: None,
            })
        }
        FilesystemManageInput::Copy {
            source,
            destination,
            recursive,
            overwrite,
        } => {
            let (source_path, destination_path) = path_pair(&source, &destination)?;
            reject_self_destination(&source_path, &destination_path)?;
            primitive_copy_path(
                primitives,
                execution,
                &source_path,
                &destination_path,
                recursive,
                overwrite,
                claim,
            )?;
            if primitive_optional_metadata(primitives, execution, &destination_path)?.is_none() {
                return Err(postcondition_error());
            }
            Ok(FilesystemManageResult {
                operation: ManageOperation::Copy,
                completed: True,
                target: None,
                source: Some(source),
                destination: Some(destination),
            })
        }
        FilesystemManageInput::Move {
            source,
            destination,
            recursive,
            overwrite,
        } => {
            let (source_path, destination_path) = path_pair(&source, &destination)?;
            reject_self_destination(&source_path, &destination_path)?;
            let source_metadata = primitives.lstat(execution, &source_path)?;
            if remote_file_type(source_metadata.mode) == FileType::Directory && !recursive {
                return Err(DomainError::new(
                    ErrorCode::NotEmpty,
                    "recursive move is required for a directory",
                ));
            }
            cancellation_checkpoint(Some(claim))?;
            primitive_publish_path(
                primitives,
                execution,
                &source_path,
                &destination_path,
                overwrite,
                claim,
                true,
            )?;
            if primitive_optional_metadata(primitives, execution, &source_path)?.is_some()
                || primitive_optional_metadata(primitives, execution, &destination_path)?.is_none()
            {
                return Err(postcondition_error());
            }
            Ok(FilesystemManageResult {
                operation: ManageOperation::Move,
                completed: True,
                target: None,
                source: Some(source),
                destination: Some(destination),
            })
        }
        FilesystemManageInput::Delete { target, recursive } => {
            require_path(&target)?;
            let path = normalize_absolute_path(&target.value)?;
            primitive_remove_path(primitives, execution, &path, recursive, Some(claim))?;
            if primitive_optional_metadata(primitives, execution, &path)?.is_some() {
                return Err(postcondition_error());
            }
            Ok(FilesystemManageResult {
                operation: ManageOperation::Delete,
                completed: True,
                target: Some(target),
                source: None,
                destination: None,
            })
        }
    }
}

fn primitive_mkdir<P: FilesystemPrimitivePort>(
    primitives: &P,
    execution: &AdmittedExecution,
    path: &Path,
    parents: bool,
    claim: &crate::LocalExecutionClaim,
) -> Result<(), DomainError> {
    match primitive_optional_metadata(primitives, execution, path)? {
        Some(metadata) if parents && remote_file_type(metadata.mode) == FileType::Directory => {
            return Ok(());
        }
        Some(_) => {
            return Err(DomainError::new(
                ErrorCode::AlreadyExists,
                "filesystem target already exists",
            ));
        }
        None if !parents => {
            return primitive_create_directory(primitives, execution, path, 0o777, Some(claim));
        }
        None => {}
    }
    let mut missing = Vec::new();
    let mut current = path;
    loop {
        match primitive_optional_metadata(primitives, execution, current)? {
            Some(metadata) if remote_file_type(metadata.mode) == FileType::Directory => break,
            Some(_) => {
                return Err(DomainError::new(
                    ErrorCode::NotFound,
                    "filesystem parent is not a directory",
                ));
            }
            None => {
                missing.push(current.to_path_buf());
                current = current.parent().ok_or_else(|| {
                    DomainError::new(ErrorCode::NotFound, "filesystem parent does not exist")
                })?;
            }
        }
    }
    for directory in missing.into_iter().rev() {
        match primitive_create_directory(primitives, execution, &directory, 0o777, Some(claim)) {
            Ok(()) => {}
            Err(error) if error.code == ErrorCode::AlreadyExists => {
                let metadata = primitives.lstat(execution, &directory)?;
                if remote_file_type(metadata.mode) != FileType::Directory {
                    return Err(error);
                }
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn primitive_copy_path<P: FilesystemPrimitivePort>(
    primitives: &P,
    execution: &AdmittedExecution,
    source: &Path,
    destination: &Path,
    recursive: bool,
    overwrite: bool,
    claim: &crate::LocalExecutionClaim,
) -> Result<(), DomainError> {
    cancellation_checkpoint(Some(claim))?;
    let source_metadata = primitives.lstat(execution, source)?;
    let destination_metadata = primitive_optional_metadata(primitives, execution, destination)?;
    if destination_metadata.is_some() && !overwrite {
        return Err(DomainError::new(
            ErrorCode::AlreadyExists,
            "filesystem destination already exists",
        ));
    }
    match remote_file_type(source_metadata.mode) {
        FileType::File => {
            let (mut reader, actual_metadata) =
                primitive_open_regular(primitives, execution, source)?;
            primitive_publish_reader(
                primitives,
                execution,
                &mut reader,
                destination,
                descriptor_mode(&actual_metadata, source_metadata.mode) & 0o777,
                destination_metadata.is_some(),
                claim,
                true,
            )?;
        }
        FileType::Directory => {
            if !recursive {
                return Err(DomainError::new(
                    ErrorCode::NotEmpty,
                    "recursive copy is required for a directory",
                ));
            }
            let publication = primitive_temporary_sibling(destination)?;
            let mut publication_owned = false;
            let copied = (|| {
                primitive_create_directory(primitives, execution, &publication, 0o700, None)?;
                publication_owned = true;
                primitive_copy_directory_contents(
                    primitives,
                    execution,
                    source,
                    &publication,
                    claim,
                )?;
                cancellation_checkpoint(Some(claim))?;
                primitive_publish_path(
                    primitives,
                    execution,
                    &publication,
                    destination,
                    overwrite,
                    claim,
                    true,
                )
            })();
            if copied.is_err() && publication_owned && claim.cleanup_is_verified() {
                cleanup_primitive_path(primitives, execution, &publication, claim);
            }
            copied?;
        }
        FileType::Symlink => {
            let publication = primitive_temporary_sibling(destination)?;
            let mut publication_owned = false;
            let copied = (|| {
                let target = primitives.readlink(execution, source)?;
                primitive_create_symlink(primitives, execution, &target, &publication)?;
                publication_owned = true;
                cancellation_checkpoint(Some(claim))?;
                primitive_publish_path(
                    primitives,
                    execution,
                    &publication,
                    destination,
                    overwrite,
                    claim,
                    true,
                )
            })();
            if copied.is_err() && publication_owned && claim.cleanup_is_verified() {
                cleanup_primitive_path(primitives, execution, &publication, claim);
            }
            copied?;
        }
        FileType::Other => {
            return Err(DomainError::new(
                ErrorCode::Unsupported,
                "special filesystem entry cannot be copied",
            ));
        }
    }
    Ok(())
}

fn primitive_copy_directory_contents<P: FilesystemPrimitivePort>(
    primitives: &P,
    execution: &AdmittedExecution,
    source: &Path,
    destination: &Path,
    claim: &crate::LocalExecutionClaim,
) -> Result<(), DomainError> {
    let mut cookie = 0;
    loop {
        let page = primitives.read_directory(
            execution,
            source,
            cookie,
            PRIMITIVE_DIRECTORY_PAGE_ENTRIES,
        )?;
        for child in page.names {
            cancellation_checkpoint(Some(claim))?;
            let source_child = source.join(&child);
            let destination_child = destination.join(&child);
            let metadata = primitives.lstat(execution, &source_child)?;
            match remote_file_type(metadata.mode) {
                FileType::File => {
                    let (mut reader, actual_metadata) =
                        primitive_open_regular(primitives, execution, &source_child)?;
                    primitive_publish_reader(
                        primitives,
                        execution,
                        &mut reader,
                        &destination_child,
                        descriptor_mode(&actual_metadata, metadata.mode) & 0o777,
                        false,
                        claim,
                        false,
                    )?;
                }
                FileType::Directory => {
                    primitive_create_directory(
                        primitives,
                        execution,
                        &destination_child,
                        0o700,
                        None,
                    )?;
                    primitive_copy_directory_contents(
                        primitives,
                        execution,
                        &source_child,
                        &destination_child,
                        claim,
                    )?;
                }
                FileType::Symlink => {
                    let target = primitives.readlink(execution, &source_child)?;
                    primitive_create_symlink(primitives, execution, &target, &destination_child)?;
                }
                FileType::Other => {
                    return Err(DomainError::new(
                        ErrorCode::Unsupported,
                        "special filesystem entry cannot be copied",
                    ));
                }
            }
        }
        let Some(next_cookie) = page.next_cookie else {
            break;
        };
        cookie = next_cookie;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn primitive_publish_reader<P: FilesystemPrimitivePort, R: Read>(
    primitives: &P,
    execution: &AdmittedExecution,
    reader: &mut R,
    destination: &Path,
    create_mode: u32,
    overwrite: bool,
    claim: &crate::LocalExecutionClaim,
    commit_publication: bool,
) -> Result<u64, DomainError> {
    let temporary = primitive_temporary_sibling(destination)?;
    let mut temporary_owned = false;
    let outcome = (|| {
        let mut output = primitives.create_exclusive(execution, &temporary, create_mode)?;
        temporary_owned = true;
        let written = copy_with_claim(reader, &mut output, claim)?;
        let destination_metadata =
            primitive_validate_destination(primitives, execution, destination, overwrite)?;
        let replacement_metadata = if let Some(metadata) = destination_metadata
            .as_ref()
            .filter(|metadata| remote_file_type(metadata.mode) == FileType::File)
        {
            let (file, actual) = primitive_open_regular(primitives, execution, destination)?;
            let metadata = primitive_descriptor_metadata(&file, &actual, metadata)?;
            primitives.apply_metadata(execution, &output, &metadata)?;
            Some(metadata)
        } else {
            None
        };
        output.sync_all().map_err(fs_error)?;
        if let Some(expected) = &replacement_metadata {
            let actual = output.metadata().map_err(fs_error)?;
            let actual = primitive_descriptor_metadata(&output, &actual, expected)?;
            if !primitive_replacement_metadata_matches(&actual, expected) {
                return Err(postcondition_error());
            }
        }
        cancellation_checkpoint(Some(claim))?;
        primitive_publish_path(
            primitives,
            execution,
            &temporary,
            destination,
            overwrite,
            claim,
            commit_publication,
        )?;
        let published = primitives.lstat(execution, destination)?;
        if remote_file_type(published.mode) != FileType::File
            || published.size != written
            || replacement_metadata.as_ref().is_some_and(|expected| {
                !primitive_replacement_metadata_matches(&published, expected)
            })
        {
            return Err(postcondition_error());
        }
        Ok(written)
    })();
    if outcome.is_err() && temporary_owned && claim.cleanup_is_verified() {
        cleanup_primitive_path(primitives, execution, &temporary, claim);
    }
    outcome
}

fn copy_with_claim<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    claim: &crate::LocalExecutionClaim,
) -> Result<u64, DomainError> {
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1_024];
    loop {
        cancellation_checkpoint(Some(claim))?;
        let read = reader.read(&mut buffer).map_err(fs_error)?;
        if read == 0 {
            break;
        }
        writer.write_all(&buffer[..read]).map_err(fs_error)?;
        total = total.checked_add(read as u64).ok_or_else(|| {
            DomainError::new(ErrorCode::ResourceLimit, "filesystem byte count overflow")
        })?;
    }
    Ok(total)
}

fn primitive_publish_path<P: FilesystemPrimitivePort>(
    primitives: &P,
    execution: &AdmittedExecution,
    source: &Path,
    destination: &Path,
    overwrite: bool,
    cleanup_claim: &crate::LocalExecutionClaim,
    commit_publication: bool,
) -> Result<(), DomainError> {
    let destination_metadata = primitive_optional_metadata(primitives, execution, destination)?;
    let destination_exists = destination_metadata.is_some();
    if destination_exists && !overwrite {
        return Err(DomainError::new(
            ErrorCode::AlreadyExists,
            "filesystem destination already exists",
        ));
    }
    if destination_metadata
        .as_ref()
        .is_some_and(|metadata| remote_file_type(metadata.mode) == FileType::Directory)
        && remote_file_type(primitives.lstat(execution, source)?.mode) != FileType::Directory
    {
        return Err(DomainError::new(
            ErrorCode::Unsupported,
            "filesystem replacement cannot implicitly remove a directory",
        ));
    }
    let publication =
        || primitives.rename_atomic(execution, source, destination, destination_exists);
    if commit_publication {
        cleanup_claim.publish(publication)?;
    } else {
        cancellation_checkpoint(Some(cleanup_claim))?;
        publication()?;
    }
    if let Some(expected) = &destination_metadata {
        let displaced = primitives.lstat(execution, source);
        if !matches!(
            displaced.as_ref(),
            Ok(metadata) if (metadata.device, metadata.inode) == (expected.device, expected.inode)
        ) {
            let rollback = primitives
                .rename_atomic(execution, source, destination, true)
                .and_then(|()| primitive_sync_rename(primitives, execution, source, destination));
            return if rollback.is_ok() {
                Err(DomainError::new(
                    ErrorCode::IoError,
                    "filesystem destination changed before publication",
                ))
            } else {
                cleanup_claim.mark_cleanup_unverified();
                Err(DomainError::new(
                    ErrorCode::IoError,
                    "filesystem replacement identity check and rollback failed",
                ))
            };
        }
    }
    let sync_error = primitive_sync_rename(primitives, execution, source, destination).err();
    if destination_exists
        && let Err(cleanup_error) = primitive_remove_path(primitives, execution, source, true, None)
    {
        let rollback = primitives
            .rename_atomic(execution, source, destination, true)
            .and_then(|()| primitive_sync_rename(primitives, execution, source, destination));
        return if rollback.is_ok() {
            Err(cleanup_error)
        } else {
            cleanup_claim.mark_cleanup_unverified();
            Err(DomainError::new(
                ErrorCode::IoError,
                "filesystem replacement cleanup and rollback failed",
            ))
        };
    }
    sync_error.map_or(Ok(()), Err)
}

fn primitive_sync_rename<P: FilesystemPrimitivePort>(
    primitives: &P,
    execution: &AdmittedExecution,
    source: &Path,
    destination: &Path,
) -> Result<(), DomainError> {
    let source_parent = source
        .parent()
        .ok_or_else(|| DomainError::invalid("filesystem source has no parent"))?;
    let destination_parent = destination
        .parent()
        .ok_or_else(|| DomainError::invalid("filesystem destination has no parent"))?;
    primitives.fsync_directory(execution, source_parent)?;
    if destination_parent != source_parent {
        primitives.fsync_directory(execution, destination_parent)?;
    }
    Ok(())
}

fn primitive_sync_parent<P: FilesystemPrimitivePort>(
    primitives: &P,
    execution: &AdmittedExecution,
    path: &Path,
) -> Result<(), DomainError> {
    primitives.fsync_directory(
        execution,
        path.parent()
            .ok_or_else(|| DomainError::invalid("filesystem path has no parent"))?,
    )
}

fn primitive_create_directory<P: FilesystemPrimitivePort>(
    primitives: &P,
    execution: &AdmittedExecution,
    path: &Path,
    mode: u32,
    claim: Option<&crate::LocalExecutionClaim>,
) -> Result<(), DomainError> {
    publish_with_claim(claim, || primitives.mkdir(execution, path, mode))?;
    primitive_sync_parent(primitives, execution, path)
}

fn primitive_create_symlink<P: FilesystemPrimitivePort>(
    primitives: &P,
    execution: &AdmittedExecution,
    target: &Path,
    destination: &Path,
) -> Result<(), DomainError> {
    primitives.symlink(execution, target, destination)?;
    primitive_sync_parent(primitives, execution, destination)
}

fn cleanup_primitive_path<P: FilesystemPrimitivePort>(
    primitives: &P,
    execution: &AdmittedExecution,
    path: &Path,
    claim: &crate::LocalExecutionClaim,
) {
    let cleanup = match primitive_optional_metadata(primitives, execution, path) {
        Ok(Some(_)) => primitive_remove_path(primitives, execution, path, true, None),
        Ok(None) => Ok(()),
        Err(error) => Err(error),
    };
    if cleanup.is_err() {
        claim.mark_cleanup_unverified();
    }
}

fn primitive_remove_path<P: FilesystemPrimitivePort>(
    primitives: &P,
    execution: &AdmittedExecution,
    path: &Path,
    recursive: bool,
    claim: Option<&crate::LocalExecutionClaim>,
) -> Result<(), DomainError> {
    let metadata = primitives.lstat(execution, path)?;
    if remote_file_type(metadata.mode) == FileType::Directory && recursive {
        loop {
            let page =
                primitives.read_directory(execution, path, 0, PRIMITIVE_DIRECTORY_PAGE_ENTRIES)?;
            if page.names.is_empty() {
                break;
            }
            for child in page.names {
                cancellation_checkpoint(claim)?;
                primitive_remove_path(primitives, execution, &path.join(child), true, claim)?;
            }
        }
    }
    publish_with_claim(claim, || primitives.unlink(execution, path))?;
    primitive_sync_parent(primitives, execution, path)
}

fn primitive_optional_metadata<P: FilesystemPrimitivePort>(
    primitives: &P,
    execution: &AdmittedExecution,
    path: &Path,
) -> Result<Option<FilesystemPrimitiveMetadata>, DomainError> {
    match primitives.lstat(execution, path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.code == ErrorCode::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn primitive_temporary_sibling(path: &Path) -> Result<PathBuf, DomainError> {
    let parent = path
        .parent()
        .ok_or_else(|| DomainError::invalid("filesystem target has no parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| DomainError::invalid("filesystem target has no name"))?
        .to_string_lossy();
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    Ok(parent.join(format!(
        ".{name}.droidbridge-{}-{sequence}.tmp",
        std::process::id()
    )))
}

fn primitive_read_all<P: FilesystemPrimitivePort>(
    primitives: &P,
    execution: &AdmittedExecution,
    path: &Path,
) -> Result<Vec<u8>, DomainError> {
    let (mut file, _) = primitive_open_regular(primitives, execution, path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(fs_error)?;
    Ok(bytes)
}

fn primitive_open_regular<P: FilesystemPrimitivePort>(
    primitives: &P,
    execution: &AdmittedExecution,
    path: &Path,
) -> Result<(File, fs::Metadata), DomainError> {
    let file = primitives.open_read(execution, path)?;
    let metadata = file.metadata().map_err(fs_error)?;
    if !metadata.is_file() {
        return Err(DomainError::new(
            if metadata.is_dir() {
                ErrorCode::InvalidArgument
            } else {
                ErrorCode::Unsupported
            },
            "filesystem source descriptor is not a regular file",
        ));
    }
    Ok((file, metadata))
}

#[cfg(unix)]
fn primitive_descriptor_metadata(
    file: &File,
    metadata: &fs::Metadata,
    _fallback: &FilesystemPrimitiveMetadata,
) -> Result<FilesystemPrimitiveMetadata, DomainError> {
    Ok(FilesystemPrimitiveMetadata {
        device: metadata.dev(),
        inode: metadata.ino(),
        mode: metadata.mode(),
        uid: metadata.uid(),
        gid: metadata.gid(),
        size: metadata.len(),
        modified_at_epoch_seconds: metadata.mtime(),
        selinux_context: descriptor_selinux_context(file)?,
    })
}

fn primitive_replacement_metadata_matches(
    actual: &FilesystemPrimitiveMetadata,
    expected: &FilesystemPrimitiveMetadata,
) -> bool {
    actual.uid == expected.uid
        && actual.gid == expected.gid
        && actual.mode & 0o7777 == expected.mode & 0o7777
        && actual.selinux_context == expected.selinux_context
}

#[cfg(not(unix))]
fn primitive_descriptor_metadata(
    _file: &File,
    metadata: &fs::Metadata,
    fallback: &FilesystemPrimitiveMetadata,
) -> Result<FilesystemPrimitiveMetadata, DomainError> {
    let mut actual = fallback.clone();
    actual.size = metadata.len();
    Ok(actual)
}

#[cfg(any(target_os = "android", target_os = "linux"))]
fn descriptor_selinux_context(file: &File) -> Result<Option<Vec<u8>>, DomainError> {
    use rustix::{fs::fgetxattr, io::Errno};

    let mut value = vec![0_u8; 4_096];
    let length = match fgetxattr(file, "security.selinux", &mut value) {
        Ok(length) => length,
        Err(Errno::NODATA | Errno::NOTSUP) => return Ok(None),
        Err(error) => return Err(rustix_fs_error(error)),
    };
    value.truncate(length);
    Ok(Some(value))
}

#[cfg(all(unix, not(any(target_os = "android", target_os = "linux"))))]
fn descriptor_selinux_context(_file: &File) -> Result<Option<Vec<u8>>, DomainError> {
    Ok(None)
}

#[cfg(unix)]
fn descriptor_mode(metadata: &fs::Metadata, _fallback: u32) -> u32 {
    metadata.mode()
}

#[cfg(not(unix))]
fn descriptor_mode(_metadata: &fs::Metadata, fallback: u32) -> u32 {
    fallback
}

async fn primitive_download<P: FilesystemPrimitivePort>(
    primitives: &P,
    execution: &AdmittedExecution,
    input: FilesystemDownloadInput,
    temporary_directory: &Path,
    claim: &crate::LocalExecutionClaim,
) -> Result<FilesystemDownloadResult, DomainError> {
    cancellation_checkpoint(Some(claim))?;
    require_path(&input.destination)?;
    let destination_path = normalize_absolute_path(&input.destination.value)?;
    reject_virtual_mutation(&destination_path)?;
    primitive_validate_destination(primitives, execution, &destination_path, input.overwrite)?;
    fs::create_dir_all(temporary_directory).map_err(fs_error)?;
    let temporary = temporary_directory.join("download.part");
    let _ = fs::remove_file(&temporary);
    let outcome = async {
        let (size, digest) =
            download_to_temporary(&input.url, input.timeout_ms, &temporary, Some(claim)).await?;
        primitive_validate_destination(primitives, execution, &destination_path, input.overwrite)?;
        let mut source = File::open(&temporary).map_err(fs_error)?;
        let published = primitive_publish_reader(
            primitives,
            execution,
            &mut source,
            &destination_path,
            0o666,
            input.overwrite,
            claim,
            true,
        )?;
        if published != size || primitives.lstat(execution, &destination_path)?.size != size {
            return Err(postcondition_error());
        }
        Ok(FilesystemDownloadResult {
            destination: input.destination,
            size,
            sha256: digest,
        })
    }
    .await;
    if outcome.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    outcome
}

fn primitive_archive_list<P: FilesystemPrimitivePort>(
    primitives: &P,
    execution: &AdmittedExecution,
    input: FilesystemArchiveInput,
) -> Result<FilesystemArchiveListResult, DomainError> {
    let FilesystemArchiveInput::List {
        target,
        max_entries,
    } = input
    else {
        return Err(DomainError::invalid("archive list requires list input"));
    };
    require_path(&target)?;
    if !(1..=5_000).contains(&max_entries) {
        return Err(DomainError::invalid("archive list bound is invalid"));
    }
    let path = normalize_absolute_path(&target.value)?;
    let (mut file, _) = primitive_open_regular(primitives, execution, &path)?;
    let format = detect_archive(&mut file)?;
    file.rewind().map_err(fs_error)?;
    let (entries, truncated) = list_archive(file, format, max_entries as usize)?;
    Ok(FilesystemArchiveListResult {
        operation: ArchiveListOperation::List,
        entries,
        truncated,
    })
}

#[allow(clippy::too_many_arguments)]
fn primitive_archive_create<P: FilesystemPrimitivePort>(
    primitives: &P,
    execution: &AdmittedExecution,
    sources: Vec<FileTarget>,
    destination: FileTarget,
    format: contract::ArchiveFormat,
    overwrite: bool,
    temporary_directory: &Path,
    claim: &crate::LocalExecutionClaim,
) -> Result<FilesystemArchiveTaskResult, DomainError> {
    cancellation_checkpoint(Some(claim))?;
    if sources.is_empty() || sources.len() > 1_000 {
        return Err(DomainError::invalid("archive source count is invalid"));
    }
    require_path(&destination)?;
    let destination_path = normalize_absolute_path(&destination.value)?;
    reject_virtual_mutation(&destination_path)?;
    primitive_validate_destination(primitives, execution, &destination_path, overwrite)?;
    fs::create_dir_all(temporary_directory).map_err(fs_error)?;
    let staged_sources = temporary_directory.join("archive-sources");
    let _ = fs::remove_dir_all(&staged_sources);
    fs::create_dir(&staged_sources).map_err(fs_error)?;
    let mut local_sources = Vec::with_capacity(sources.len());
    for (index, source) in sources.iter().enumerate() {
        cancellation_checkpoint(Some(claim))?;
        require_path(source)?;
        let source_path = normalize_absolute_path(&source.value)?;
        let name = source_path
            .file_name()
            .ok_or_else(|| DomainError::invalid("archive source has no name"))?;
        let holder = staged_sources.join(index.to_string());
        fs::create_dir(&holder).map_err(fs_error)?;
        let local = holder.join(name);
        copy_primitive_to_local(primitives, execution, &source_path, &local, claim)?;
        local_sources.push(local);
    }
    let temporary = temporary_directory.join("archive.part");
    let _ = fs::remove_file(&temporary);
    let entries_archived = match format {
        contract::ArchiveFormat::Zip => create_zip(&local_sources, &temporary)?,
        contract::ArchiveFormat::Tar => create_tar(&local_sources, &temporary, false)?,
        contract::ArchiveFormat::TarGz => create_tar(&local_sources, &temporary, true)?,
    };
    cancellation_checkpoint(Some(claim))?;
    let (expected_size, expected_digest) = hash_file(&temporary, Some(claim))?;
    primitive_validate_destination(primitives, execution, &destination_path, overwrite)?;
    let mut file = File::open(&temporary).map_err(fs_error)?;
    let written = primitive_publish_reader(
        primitives,
        execution,
        &mut file,
        &destination_path,
        0o666,
        overwrite,
        claim,
        true,
    )?;
    let (mut published_file, _) = primitive_open_regular(primitives, execution, &destination_path)?;
    let (verified_size, verified_digest) = hash_reader(&mut published_file, Some(claim))?;
    if written != expected_size
        || verified_size != expected_size
        || verified_digest != expected_digest
    {
        return Err(postcondition_error());
    }
    Ok(FilesystemArchiveTaskResult::Create {
        destination,
        entries_archived,
        bytes_written: written,
        sha256: expected_digest,
    })
}

#[allow(clippy::too_many_arguments)]
fn primitive_archive_extract<P: FilesystemPrimitivePort>(
    primitives: &P,
    execution: &AdmittedExecution,
    target: FileTarget,
    destination: FileTarget,
    overwrite: bool,
    temporary_directory: &Path,
    claim: &crate::LocalExecutionClaim,
) -> Result<FilesystemArchiveTaskResult, DomainError> {
    cancellation_checkpoint(Some(claim))?;
    require_path(&target)?;
    require_path(&destination)?;
    let source_path = normalize_absolute_path(&target.value)?;
    let destination_path = normalize_absolute_path(&destination.value)?;
    reject_virtual_mutation(&destination_path)?;
    primitive_validate_destination(primitives, execution, &destination_path, overwrite)?;
    let (mut file, _) = primitive_open_regular(primitives, execution, &source_path)?;
    let format = detect_archive(&mut file)?;
    file.rewind().map_err(fs_error)?;
    fs::create_dir_all(temporary_directory).map_err(fs_error)?;
    let local_stage = temporary_directory.join("extract.part");
    let _ = fs::remove_dir_all(&local_stage);
    fs::create_dir(&local_stage).map_err(fs_error)?;
    let entries_extracted = extract_archive(file, format, &local_stage)?;
    cancellation_checkpoint(Some(claim))?;
    let publication = primitive_temporary_sibling(&destination_path)?;
    let mut publication_owned = false;
    let published = (|| {
        primitive_create_directory(primitives, execution, &publication, 0o700, None)?;
        publication_owned = true;
        copy_local_directory_to_primitive(
            primitives,
            execution,
            &local_stage,
            &publication,
            claim,
        )?;
        cancellation_checkpoint(Some(claim))?;
        primitive_publish_path(
            primitives,
            execution,
            &publication,
            &destination_path,
            overwrite,
            claim,
            true,
        )
    })();
    if published.is_err() && publication_owned && claim.cleanup_is_verified() {
        cleanup_primitive_path(primitives, execution, &publication, claim);
    }
    published?;
    if primitive_optional_metadata(primitives, execution, &destination_path)?
        .is_none_or(|metadata| remote_file_type(metadata.mode) != FileType::Directory)
    {
        return Err(postcondition_error());
    }
    Ok(FilesystemArchiveTaskResult::Extract {
        destination,
        entries_extracted,
    })
}

fn copy_primitive_to_local<P: FilesystemPrimitivePort>(
    primitives: &P,
    execution: &AdmittedExecution,
    source: &Path,
    destination: &Path,
    claim: &crate::LocalExecutionClaim,
) -> Result<(), DomainError> {
    cancellation_checkpoint(Some(claim))?;
    let metadata = primitives.lstat(execution, source)?;
    match remote_file_type(metadata.mode) {
        FileType::File => {
            let (mut input, _) = primitive_open_regular(primitives, execution, source)?;
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(destination)
                .map_err(fs_error)?;
            copy_with_claim(&mut input, &mut output, claim)?;
            output.sync_all().map_err(fs_error)
        }
        FileType::Directory => {
            fs::create_dir(destination).map_err(fs_error)?;
            let mut cookie = 0;
            loop {
                let page = primitives.read_directory(
                    execution,
                    source,
                    cookie,
                    PRIMITIVE_DIRECTORY_PAGE_ENTRIES,
                )?;
                for child in page.names {
                    copy_primitive_to_local(
                        primitives,
                        execution,
                        &source.join(&child),
                        &destination.join(child),
                        claim,
                    )?;
                }
                let Some(next_cookie) = page.next_cookie else {
                    break;
                };
                cookie = next_cookie;
            }
            sync_directory(destination)
        }
        FileType::Symlink | FileType::Other => Err(DomainError::new(
            ErrorCode::Unsupported,
            "archive creation supports regular files and directories",
        )),
    }
}

fn copy_local_directory_to_primitive<P: FilesystemPrimitivePort>(
    primitives: &P,
    execution: &AdmittedExecution,
    source: &Path,
    destination: &Path,
    claim: &crate::LocalExecutionClaim,
) -> Result<(), DomainError> {
    let mut children = fs::read_dir(source)
        .map_err(fs_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(fs_error)?;
    children.sort_by_key(fs::DirEntry::file_name);
    for child in children {
        cancellation_checkpoint(Some(claim))?;
        let source_child = child.path();
        let destination_child = destination.join(child.file_name());
        let metadata = child.metadata().map_err(fs_error)?;
        if metadata.is_dir() {
            primitive_create_directory(primitives, execution, &destination_child, 0o700, None)?;
            copy_local_directory_to_primitive(
                primitives,
                execution,
                &source_child,
                &destination_child,
                claim,
            )?;
        } else if metadata.is_file() {
            let mut input = File::open(&source_child).map_err(fs_error)?;
            primitive_publish_reader(
                primitives,
                execution,
                &mut input,
                &destination_child,
                0o600,
                false,
                claim,
                false,
            )?;
        } else {
            return Err(DomainError::new(
                ErrorCode::Unsupported,
                "archive extraction produced an unsupported entry",
            ));
        }
    }
    Ok(())
}

/// Copies one local directory tree onto another before publication. Android shared storage
/// synthesizes modes and rejects chmod, so permissions are retained only on filesystems that own
/// real Unix mode bits.
fn copy_local_directory(
    source: &Path,
    destination: &Path,
    claim: Option<&crate::LocalExecutionClaim>,
    preserve_permissions: bool,
) -> Result<(), DomainError> {
    let mut children = fs::read_dir(source)
        .map_err(fs_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(fs_error)?;
    children.sort_by_key(fs::DirEntry::file_name);
    for child in children {
        cancellation_checkpoint(claim)?;
        let source_child = child.path();
        let destination_child = destination.join(child.file_name());
        let metadata = child.metadata().map_err(fs_error)?;
        if metadata.is_dir() {
            fs::create_dir(&destination_child).map_err(fs_error)?;
            if preserve_permissions {
                fs::set_permissions(&destination_child, metadata.permissions())
                    .map_err(fs_error)?;
            }
            copy_local_directory(
                &source_child,
                &destination_child,
                claim,
                preserve_permissions,
            )?;
        } else if metadata.is_file() {
            let mut input = File::open(&source_child).map_err(fs_error)?;
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&destination_child)
                .map_err(fs_error)?;
            match claim {
                Some(claim) => {
                    copy_with_claim(&mut input, &mut output, claim)?;
                }
                None => {
                    std::io::copy(&mut input, &mut output).map_err(fs_error)?;
                }
            }
            if preserve_permissions {
                fs::set_permissions(&destination_child, metadata.permissions())
                    .map_err(fs_error)?;
            }
            output.sync_all().map_err(fs_error)?;
        } else {
            return Err(DomainError::new(
                ErrorCode::Unsupported,
                "archive extraction produced an unsupported entry",
            ));
        }
    }
    sync_directory(destination)
}

fn primitive_validate_destination<P: FilesystemPrimitivePort>(
    primitives: &P,
    execution: &AdmittedExecution,
    destination: &Path,
    overwrite: bool,
) -> Result<Option<FilesystemPrimitiveMetadata>, DomainError> {
    let parent = destination
        .parent()
        .ok_or_else(|| DomainError::invalid("filesystem destination has no parent"))?;
    let parent_metadata = primitives.lstat(execution, parent)?;
    if remote_file_type(parent_metadata.mode) != FileType::Directory {
        return Err(DomainError::new(
            ErrorCode::NotFound,
            "filesystem parent is not a directory",
        ));
    }
    primitives.read_directory(execution, parent, 0, 1)?;
    primitives.access_write_search(execution, parent)?;
    let existing = primitive_optional_metadata(primitives, execution, destination)?;
    if existing.is_some() && !overwrite {
        return Err(DomainError::new(
            ErrorCode::AlreadyExists,
            "filesystem destination already exists",
        ));
    }
    if existing.is_some() && !primitive_preflight_sticky_allows(primitives, execution, destination)
    {
        return Err(DomainError::new(
            ErrorCode::PermissionDenied,
            "sticky-directory ownership rejects filesystem replacement",
        ));
    }
    Ok(existing)
}

#[allow(clippy::too_many_arguments)]
fn inspect_primitive_directory<P: FilesystemPrimitivePort>(
    primitives: &P,
    execution: &AdmittedExecution,
    root: &Path,
    relative: &Path,
    depth: u32,
    max_depth: u32,
    recursive: bool,
    limit: usize,
    output: &mut Vec<FileEntry>,
    truncated: &mut bool,
) -> Result<(), DomainError> {
    let remaining = limit.saturating_sub(output.len());
    let current = if relative.as_os_str().is_empty() {
        root.to_path_buf()
    } else {
        root.join(relative)
    };
    let listing = primitives.read_directory(execution, &current, 0, remaining.saturating_add(1))?;
    let mut children = listing.names;
    children.sort();
    for child in children {
        if output.len() == limit {
            *truncated = true;
            break;
        }
        let child_relative = relative.join(&child);
        let metadata = primitives.lstat(execution, &root.join(&child_relative))?;
        let entry_type = remote_file_type(metadata.mode);
        output.push(FileEntry {
            name: child_relative.to_string_lossy().replace('\\', "/"),
            entry_type,
            size: (entry_type == FileType::File).then_some(metadata.size),
            modified_at: remote_modified_at(metadata.modified_at_epoch_seconds),
        });
        if recursive && depth < max_depth && entry_type == FileType::Directory {
            inspect_primitive_directory(
                primitives,
                execution,
                root,
                &child_relative,
                depth + 1,
                max_depth,
                recursive,
                limit,
                output,
                truncated,
            )?;
        }
        if *truncated {
            break;
        }
    }
    if listing.next_cookie.is_some() {
        *truncated = true;
    }
    Ok(())
}

fn remote_file_type(mode: u32) -> FileType {
    match mode & 0o170000 {
        0o100000 => FileType::File,
        0o040000 => FileType::Directory,
        0o120000 => FileType::Symlink,
        _ => FileType::Other,
    }
}

fn remote_modified_at(epoch_seconds: i64) -> Option<String> {
    chrono::DateTime::<chrono::Utc>::from_timestamp(epoch_seconds, 0)
        .map(|value| value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

fn execute_framework_filesystem<A, F>(
    kernel: &FilesystemKernel<A>,
    framework: &F,
    execution: &AdmittedExecution,
    call: contract::FilesystemCall,
    claim: &crate::LocalExecutionClaim,
) -> Result<FilesystemExecutionResult, DomainError>
where
    A: ArtifactPort,
    F: FilesystemFrameworkPort,
{
    cancellation_checkpoint(Some(claim))?;
    let result = match call {
        contract::FilesystemCall::Inspect(input) => {
            validate_inspect_input(&input)?;
            require_content_uri(&input.target)?;
            serde_json::to_value(framework.inspect(execution, input)?)
        }
        contract::FilesystemCall::Read(input) => {
            let ReadSource::Target { target } = &input.source else {
                return Err(DomainError::new(
                    ErrorCode::StaleAuthority,
                    "framework read cannot consume a data artifact",
                ));
            };
            require_content_uri(target)?;
            let source = framework.open_read(execution, target)?;
            cancellation_checkpoint(Some(claim))?;
            serde_json::to_value(kernel.read_opened(input, source.file, source.total_size)?)
        }
        _ => {
            return Err(DomainError::new(
                ErrorCode::Unsupported,
                "Android framework supports only content inspect and read",
            ));
        }
    }
    .map_err(|_| {
        DomainError::new(
            ErrorCode::InternalError,
            "filesystem result encoding failed",
        )
    })?;
    Ok(FilesystemExecutionResult::Synchronous(result))
}

fn execution_fence(execution: &AdmittedExecution) -> domain::AdmissionFence {
    domain::AdmissionFence {
        runtime_epoch: execution.executor.fence.runtime_epoch.clone(),
        host_generation: execution.executor.fence.host_generation,
        runtime_instance_id: execution.executor.fence.runtime_instance_id.clone(),
    }
}

fn filesystem_call_operation(call: &contract::FilesystemCall) -> String {
    let action = match call {
        contract::FilesystemCall::Inspect(_) => "inspect",
        contract::FilesystemCall::Read(_) => "read",
        contract::FilesystemCall::Write(_) => "write",
        contract::FilesystemCall::Manage(_) => "manage",
        contract::FilesystemCall::Download(_) => "download",
        contract::FilesystemCall::Archive(_) => "archive",
    };
    format!("filesystem.{action}")
}

fn encoded_len(value: &impl serde::Serialize) -> Result<u64, DomainError> {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len() as u64)
        .map_err(|_| {
            DomainError::new(
                ErrorCode::InternalError,
                "filesystem result encoding failed",
            )
        })
}

fn execution_failure(
    code: ErrorCode,
    message: &'static str,
    cleanup_verified: bool,
) -> ExecutionFailure {
    ExecutionFailure {
        error: DomainError::new(code, message),
        cleanup_verified,
    }
}

fn cleanup_task_temp(task_temp: Option<&Path>) -> Result<(), DomainError> {
    let Some(task_temp) = task_temp else {
        return Ok(());
    };
    match fs::remove_dir_all(task_temp) {
        Ok(()) => sync_directory(
            task_temp
                .parent()
                .ok_or_else(|| DomainError::invalid("Task temp has no parent"))?,
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(fs_error(error)),
    }
}

impl<A: ArtifactPort> FilesystemKernel<A> {
    pub const fn new(artifacts: A) -> Self {
        Self { artifacts }
    }

    pub async fn execute(
        &self,
        call: contract::FilesystemCall,
        temporary_directory: Option<&Path>,
    ) -> Result<FilesystemExecutionResult, DomainError> {
        self.execute_inner(call, temporary_directory, None).await
    }

    pub async fn execute_claimed(
        &self,
        call: contract::FilesystemCall,
        temporary_directory: Option<&Path>,
        claim: &crate::LocalExecutionClaim,
    ) -> Result<FilesystemExecutionResult, DomainError> {
        self.execute_inner(call, temporary_directory, Some(claim))
            .await
    }

    async fn execute_inner(
        &self,
        call: contract::FilesystemCall,
        temporary_directory: Option<&Path>,
        claim: Option<&crate::LocalExecutionClaim>,
    ) -> Result<FilesystemExecutionResult, DomainError> {
        cancellation_checkpoint(claim)?;
        match call {
            contract::FilesystemCall::Inspect(input) => synchronous_result(self.inspect(input)?),
            contract::FilesystemCall::Read(input) => synchronous_result(self.read(input)?),
            contract::FilesystemCall::Write(input) => synchronous_result(self.write(input)?),
            contract::FilesystemCall::Manage(input) => synchronous_result(self.manage(input)?),
            contract::FilesystemCall::Download(input) => {
                let temporary = temporary_directory.ok_or_else(|| {
                    DomainError::new(
                        ErrorCode::InternalError,
                        "filesystem Task has no temp owner",
                    )
                })?;
                self.download_inner(input, temporary, claim)
                    .await
                    .map(contract::TaskTerminalResult::FilesystemDownload)
                    .map(FilesystemExecutionResult::Task)
            }
            contract::FilesystemCall::Archive(input) => match input {
                FilesystemArchiveInput::List { .. } => {
                    synchronous_result(self.archive_list(input)?)
                }
                FilesystemArchiveInput::Extract {
                    target,
                    destination,
                    overwrite,
                } => {
                    let temporary = temporary_directory.ok_or_else(|| {
                        DomainError::new(
                            ErrorCode::InternalError,
                            "filesystem Task has no temp owner",
                        )
                    })?;
                    self.archive_extract_inner(target, destination, overwrite, temporary, claim)
                        .map(contract::TaskTerminalResult::FilesystemArchive)
                        .map(FilesystemExecutionResult::Task)
                }
                FilesystemArchiveInput::Create {
                    sources,
                    destination,
                    format,
                    overwrite,
                } => {
                    let temporary = temporary_directory.ok_or_else(|| {
                        DomainError::new(
                            ErrorCode::InternalError,
                            "filesystem Task has no temp owner",
                        )
                    })?;
                    self.archive_create_inner(
                        sources,
                        destination,
                        format,
                        overwrite,
                        temporary,
                        claim,
                    )
                    .map(contract::TaskTerminalResult::FilesystemArchive)
                    .map(FilesystemExecutionResult::Task)
                }
            },
        }
    }

    pub fn inspect(
        &self,
        input: FilesystemInspectInput,
    ) -> Result<FilesystemInspectResult, DomainError> {
        require_path(&input.target)?;
        validate_inspect_input(&input)?;
        let path = normalize_absolute_path(&input.target.value)?;
        let metadata = fs::symlink_metadata(&path).map_err(fs_error)?;
        let target_type = file_type(&metadata);
        let (entries, truncated) = if target_type == FileType::Directory {
            let mut entries = Vec::new();
            let mut truncated = false;
            inspect_directory(
                &path,
                Path::new(""),
                1,
                input.max_depth,
                input.recursive,
                input.max_entries as usize,
                &mut entries,
                &mut truncated,
            )?;
            (Some(entries), Some(truncated))
        } else {
            (None, None)
        };
        Ok(FilesystemInspectResult {
            target: input.target,
            target_type,
            size: regular_size(&metadata),
            modified_at: modified_at(&metadata),
            entries,
            truncated,
        })
    }

    pub fn read(&self, input: FilesystemReadInput) -> Result<FilesystemReadResult, DomainError> {
        validate_read_input(&input)?;
        let (reader, total_size): (Box<dyn ReadSeek>, Option<u64>) = match &input.source {
            ReadSource::Target { target } => {
                require_path(target)?;
                let path = normalize_absolute_path(&target.value)?;
                let file = OpenOptions::new().read(true).open(path).map_err(fs_error)?;
                let metadata = file.metadata().map_err(fs_error)?;
                if metadata.is_dir() {
                    return Err(DomainError::new(
                        ErrorCode::InvalidArgument,
                        "filesystem.read source is a directory",
                    ));
                }
                (Box::new(file), regular_size(&metadata))
            }
            ReadSource::DataRef { data_ref } => {
                if !data_ref.starts_with("dbref:data:") {
                    return Err(DomainError::invalid(
                        "filesystem.read data_ref is not a data artifact",
                    ));
                }
                let bytes = self.artifacts.open(data_ref)?;
                let size = bytes.len() as u64;
                (Box::new(std::io::Cursor::new(bytes)), Some(size))
            }
        };
        self.read_from_reader(input, reader, total_size)
    }

    pub fn read_opened(
        &self,
        input: FilesystemReadInput,
        file: File,
        total_size: Option<u64>,
    ) -> Result<FilesystemReadResult, DomainError> {
        validate_read_input(&input)?;
        let ReadSource::Target { target } = &input.source else {
            return Err(DomainError::invalid(
                "opened filesystem read requires a target source",
            ));
        };
        require_content_uri(target)?;
        self.read_from_reader(input, Box::new(file), total_size)
    }

    fn read_from_reader(
        &self,
        input: FilesystemReadInput,
        mut reader: Box<dyn ReadSeek>,
        total_size: Option<u64>,
    ) -> Result<FilesystemReadResult, DomainError> {
        if reader.seek(SeekFrom::Start(input.offset)).is_err() {
            let mut skipped = 0_u64;
            let mut buffer = [0_u8; 8_192];
            while skipped < input.offset {
                let remaining = input.offset - skipped;
                let requested = min(remaining, buffer.len() as u64) as usize;
                let read = reader.read(&mut buffer[..requested]).map_err(fs_error)?;
                if read == 0 {
                    break;
                }
                skipped += read as u64;
            }
        }
        let capacity = usize::try_from(input.max_bytes)
            .map_err(|_| DomainError::new(ErrorCode::ResourceLimit, "read bound overflow"))?;
        let probe_capacity = capacity
            .checked_add(1)
            .ok_or_else(|| DomainError::new(ErrorCode::ResourceLimit, "read bound overflow"))?;
        let mut selected = Vec::with_capacity(probe_capacity);
        reader
            .take(input.max_bytes + 1)
            .read_to_end(&mut selected)
            .map_err(fs_error)?;
        let has_more_bytes = selected.len() > capacity;
        selected.truncate(capacity);
        let returned_bytes = selected.len() as u64;
        let truncated = has_more_bytes
            || total_size.is_some_and(|size| {
                input
                    .offset
                    .checked_add(returned_bytes)
                    .is_none_or(|end| end < size)
            });
        let sha256 = (input.offset == 0 && !truncated).then(|| sha256(&selected));
        let encoded = match input.encoding {
            DataEncoding::Utf8 => std::str::from_utf8(&selected).map(str::to_owned).ok(),
            DataEncoding::Base64 => Some(BASE64.encode(&selected)),
        };
        let inline = encoded.filter(|data| inline_result_fits(data, returned_bytes, total_size));
        let (data, data_ref) = match inline {
            Some(data) => (Some(data), None),
            None if selected.is_empty() => (Some(String::new()), None),
            None => {
                let published = self.artifacts.publish(&selected)?;
                (None, Some(published.artifact_ref))
            }
        };
        Ok(FilesystemReadResult {
            data,
            data_ref,
            returned_bytes,
            total_size,
            truncated,
            sha256,
        })
    }

    pub fn write(&self, input: FilesystemWriteInput) -> Result<FilesystemWriteResult, DomainError> {
        let (target, bytes, create) = match input {
            FilesystemWriteInput::Create {
                target,
                content,
                encoding,
            } => (target, decode_content(&content, encoding)?, true),
            FilesystemWriteInput::Replace {
                target,
                content,
                encoding,
            } => (target, decode_content(&content, encoding)?, false),
            FilesystemWriteInput::Edit {
                target,
                replacements,
            } => {
                require_path(&target)?;
                let path = normalize_absolute_path(&target.value)?;
                reject_virtual_mutation(&path)?;
                let original = fs::read_to_string(&path).map_err(fs_error)?;
                let edited = apply_replacements(&original, &replacements)?;
                (target, edited.into_bytes(), false)
            }
        };
        require_path(&target)?;
        let path = normalize_absolute_path(&target.value)?;
        reject_virtual_mutation(&path)?;
        atomic_regular_write(&path, &bytes, create)?;
        let verified = fs::read(&path).map_err(fs_error)?;
        if verified != bytes {
            return Err(DomainError::new(
                ErrorCode::IoError,
                "filesystem.write postcondition failed",
            ));
        }
        Ok(FilesystemWriteResult {
            bytes_written: bytes.len() as u64,
            sha256: Some(sha256(&bytes)),
        })
    }

    pub fn manage(
        &self,
        input: FilesystemManageInput,
    ) -> Result<FilesystemManageResult, DomainError> {
        match input {
            FilesystemManageInput::Mkdir { target, parents } => {
                require_path(&target)?;
                let path = normalize_absolute_path(&target.value)?;
                if parents {
                    fs::create_dir_all(&path).map_err(fs_error)?;
                } else {
                    fs::create_dir(&path).map_err(fs_error)?;
                }
                if !path.is_dir() {
                    return Err(postcondition_error());
                }
                sync_parent(&path)?;
                Ok(FilesystemManageResult {
                    operation: ManageOperation::Mkdir,
                    completed: True,
                    target: Some(target),
                    source: None,
                    destination: None,
                })
            }
            FilesystemManageInput::Copy {
                source,
                destination,
                recursive,
                overwrite,
            } => {
                let (source_path, destination_path) = path_pair(&source, &destination)?;
                reject_self_destination(&source_path, &destination_path)?;
                copy_path(&source_path, &destination_path, recursive, overwrite)?;
                if !path_exists_no_follow(&destination_path)? {
                    return Err(postcondition_error());
                }
                Ok(FilesystemManageResult {
                    operation: ManageOperation::Copy,
                    completed: True,
                    target: None,
                    source: Some(source),
                    destination: Some(destination),
                })
            }
            FilesystemManageInput::Move {
                source,
                destination,
                recursive,
                overwrite,
            } => {
                let (source_path, destination_path) = path_pair(&source, &destination)?;
                reject_self_destination(&source_path, &destination_path)?;
                let source_metadata = fs::symlink_metadata(&source_path).map_err(fs_error)?;
                if source_metadata.is_dir() && !recursive {
                    return Err(DomainError::new(
                        ErrorCode::NotEmpty,
                        "recursive move is required for a directory",
                    ));
                }
                if path_exists_no_follow(&destination_path)? && !overwrite {
                    return Err(DomainError::new(
                        ErrorCode::AlreadyExists,
                        "filesystem destination already exists",
                    ));
                }
                publish_renamed_path(&source_path, &destination_path, overwrite)?;
                if path_exists_no_follow(&source_path)?
                    || !path_exists_no_follow(&destination_path)?
                {
                    return Err(postcondition_error());
                }
                Ok(FilesystemManageResult {
                    operation: ManageOperation::Move,
                    completed: True,
                    target: None,
                    source: Some(source),
                    destination: Some(destination),
                })
            }
            FilesystemManageInput::Delete { target, recursive } => {
                require_path(&target)?;
                let path = normalize_absolute_path(&target.value)?;
                remove_path(&path, recursive)?;
                sync_parent(&path)?;
                if fs::symlink_metadata(&path).is_ok() {
                    return Err(postcondition_error());
                }
                Ok(FilesystemManageResult {
                    operation: ManageOperation::Delete,
                    completed: True,
                    target: Some(target),
                    source: None,
                    destination: None,
                })
            }
        }
    }

    pub async fn download(
        &self,
        input: FilesystemDownloadInput,
        temporary_directory: &Path,
    ) -> Result<FilesystemDownloadResult, DomainError> {
        self.download_inner(input, temporary_directory, None).await
    }

    async fn download_inner(
        &self,
        input: FilesystemDownloadInput,
        temporary_directory: &Path,
        claim: Option<&crate::LocalExecutionClaim>,
    ) -> Result<FilesystemDownloadResult, DomainError> {
        cancellation_checkpoint(claim)?;
        require_path(&input.destination)?;
        let destination_path = normalize_absolute_path(&input.destination.value)?;
        existing_parent(&destination_path)?;
        if destination_path.exists() && !input.overwrite {
            return Err(DomainError::new(
                ErrorCode::AlreadyExists,
                "download destination already exists",
            ));
        }
        fs::create_dir_all(temporary_directory).map_err(fs_error)?;
        let temporary = temporary_directory.join("download.part");
        let _ = fs::remove_file(&temporary);
        let outcome = async {
            let (size, digest) =
                download_to_temporary(&input.url, input.timeout_ms, &temporary, claim).await?;
            publish_with_claim(claim, || {
                publish_file(&temporary, &destination_path, input.overwrite)
            })?;
            let metadata = fs::metadata(&destination_path).map_err(fs_error)?;
            if metadata.len() != size {
                return Err(postcondition_error());
            }
            Ok(FilesystemDownloadResult {
                destination: input.destination,
                size,
                sha256: digest,
            })
        }
        .await;
        if outcome.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        outcome
    }

    pub fn archive_list(
        &self,
        input: FilesystemArchiveInput,
    ) -> Result<FilesystemArchiveListResult, DomainError> {
        let FilesystemArchiveInput::List {
            target,
            max_entries,
        } = input
        else {
            return Err(DomainError::invalid("archive list requires list input"));
        };
        require_path(&target)?;
        if !(1..=5_000).contains(&max_entries) {
            return Err(DomainError::invalid("archive list bound is invalid"));
        }
        let path = normalize_absolute_path(&target.value)?;
        let mut file = File::open(path).map_err(fs_error)?;
        let format = detect_archive(&mut file)?;
        file.rewind().map_err(fs_error)?;
        let (entries, truncated) = list_archive(file, format, max_entries as usize)?;
        Ok(FilesystemArchiveListResult {
            operation: ArchiveListOperation::List,
            entries,
            truncated,
        })
    }

    pub fn archive_create(
        &self,
        sources: Vec<FileTarget>,
        destination: FileTarget,
        format: contract::ArchiveFormat,
        overwrite: bool,
        temporary_directory: &Path,
    ) -> Result<FilesystemArchiveTaskResult, DomainError> {
        self.archive_create_inner(
            sources,
            destination,
            format,
            overwrite,
            temporary_directory,
            None,
        )
    }

    fn archive_create_inner(
        &self,
        sources: Vec<FileTarget>,
        destination: FileTarget,
        format: contract::ArchiveFormat,
        overwrite: bool,
        temporary_directory: &Path,
        claim: Option<&crate::LocalExecutionClaim>,
    ) -> Result<FilesystemArchiveTaskResult, DomainError> {
        cancellation_checkpoint(claim)?;
        if sources.is_empty() || sources.len() > 1_000 {
            return Err(DomainError::invalid("archive source count is invalid"));
        }
        require_path(&destination)?;
        let destination_path = normalize_absolute_path(&destination.value)?;
        let mut source_paths = Vec::with_capacity(sources.len());
        for source in &sources {
            require_path(source)?;
            source_paths.push(normalize_absolute_path(&source.value)?);
        }
        fs::create_dir_all(temporary_directory).map_err(fs_error)?;
        let temporary = temporary_directory.join("archive.part");
        let _ = fs::remove_file(&temporary);
        let outcome = (|| {
            let entries_archived = match format {
                contract::ArchiveFormat::Zip => create_zip(&source_paths, &temporary)?,
                contract::ArchiveFormat::Tar => create_tar(&source_paths, &temporary, false)?,
                contract::ArchiveFormat::TarGz => create_tar(&source_paths, &temporary, true)?,
            };
            let (expected_size, expected_digest) = hash_file(&temporary, claim)?;
            publish_with_claim(claim, || {
                publish_file(&temporary, &destination_path, overwrite)
            })?;
            let (verified_size, verified_digest) = hash_file(&destination_path, claim)?;
            if verified_size != expected_size || verified_digest != expected_digest {
                return Err(postcondition_error());
            }
            Ok(FilesystemArchiveTaskResult::Create {
                destination,
                entries_archived,
                bytes_written: expected_size,
                sha256: expected_digest,
            })
        })();
        if outcome.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        outcome
    }

    pub fn archive_extract(
        &self,
        target: FileTarget,
        destination: FileTarget,
        overwrite: bool,
        temporary_directory: &Path,
    ) -> Result<FilesystemArchiveTaskResult, DomainError> {
        self.archive_extract_inner(target, destination, overwrite, temporary_directory, None)
    }

    fn archive_extract_inner(
        &self,
        target: FileTarget,
        destination: FileTarget,
        overwrite: bool,
        temporary_directory: &Path,
        claim: Option<&crate::LocalExecutionClaim>,
    ) -> Result<FilesystemArchiveTaskResult, DomainError> {
        cancellation_checkpoint(claim)?;
        require_path(&target)?;
        require_path(&destination)?;
        let source_path = normalize_absolute_path(&target.value)?;
        let preserve_permissions = !is_android_shared_storage_path(&destination.value);
        let destination_path = normalize_absolute_path(&destination.value)?;
        let mut file = File::open(source_path).map_err(fs_error)?;
        let format = detect_archive(&mut file)?;
        file.rewind().map_err(fs_error)?;
        let stage = temporary_directory.join("extract.part");
        if stage.exists() {
            fs::remove_dir_all(&stage).map_err(fs_error)?;
        }
        fs::create_dir_all(&stage).map_err(fs_error)?;
        let result = extract_archive(file, format, &stage);
        let entries_extracted = match result {
            Ok(count) => count,
            Err(error) => {
                let _ = fs::remove_dir_all(&stage);
                return Err(error);
            }
        };
        if destination_path.exists() && !overwrite {
            fs::remove_dir_all(&stage).map_err(fs_error)?;
            return Err(DomainError::new(
                ErrorCode::AlreadyExists,
                "archive destination already exists",
            ));
        }
        // The stage lives under the Runtime's own temporary directory, which is another filesystem
        // whenever the destination is shared storage, and no directory renames across filesystems.
        // The extracted tree is copied into a sibling of the destination first, so the rename that
        // publishes it stays inside the destination's own directory.
        let publication = temporary_sibling(&destination_path)?;
        let publication_mode = fs::metadata(&stage).map_err(fs_error)?.permissions();
        let mut publication_owned = false;
        let published = (|| {
            fs::create_dir(&publication).map_err(fs_error)?;
            publication_owned = true;
            if preserve_permissions {
                fs::set_permissions(&publication, publication_mode).map_err(fs_error)?;
            }
            copy_local_directory(&stage, &publication, claim, preserve_permissions)?;
            cancellation_checkpoint(claim)?;
            publish_with_claim(claim, || {
                publish_directory(&publication, &destination_path, overwrite)
            })
        })();
        if published.is_err()
            && publication_owned
            && claim.is_none_or(crate::LocalExecutionClaim::cleanup_is_verified)
            && fs::remove_dir_all(&publication).is_err()
            && let Some(claim) = claim
        {
            claim.mark_cleanup_unverified();
        }
        published?;
        if !destination_path.is_dir() {
            return Err(postcondition_error());
        }
        Ok(FilesystemArchiveTaskResult::Extract {
            destination,
            entries_extracted,
        })
    }
}

async fn download_to_temporary(
    url: &str,
    timeout_ms: u64,
    temporary: &Path,
    claim: Option<&crate::LocalExecutionClaim>,
) -> Result<(u64, String), DomainError> {
    if !(1_000..=3_600_000).contains(&timeout_ms) {
        return Err(DomainError::invalid("download timeout is out of bounds"));
    }
    let url =
        reqwest::Url::parse(url).map_err(|_| DomainError::invalid("download URL is invalid"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(DomainError::new(
            ErrorCode::Unsupported,
            "download URL scheme is unsupported",
        ));
    }
    // S-NET-002: a download carries the Runtime's one pinned Rustls configuration. reqwest's
    // default policy is the platform verifier, which refuses genuine public chains and needs a
    // JVM on Android, so neither Runtime host may fall back to it.
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_millis(timeout_ms))
        .tls_backend_preconfigured(crate::network_tls_client_config().as_ref().clone())
        .build()
        .map_err(download_error)?;
    let mut current_url = url;
    let mut redirects = 0_u8;
    let mut response = loop {
        cancellation_checkpoint(claim)?;
        let response = client
            .get(current_url.clone())
            .send()
            .await
            .map_err(download_error)?;
        if !response.status().is_redirection() {
            break response;
        }
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| DomainError::new(ErrorCode::IoError, "download redirect is invalid"))?;
        let next = current_url
            .join(location)
            .map_err(|_| DomainError::new(ErrorCode::IoError, "download redirect is invalid"))?;
        if !matches!(next.scheme(), "http" | "https") {
            return Err(DomainError::new(
                ErrorCode::Unsupported,
                "download redirect URL scheme is unsupported",
            ));
        }
        if response.url() == &next {
            return Err(DomainError::new(
                ErrorCode::IoError,
                "download redirect loop detected",
            ));
        }
        if redirects == 10 {
            return Err(DomainError::new(
                ErrorCode::IoError,
                "download redirect limit exceeded",
            ));
        }
        redirects += 1;
        current_url = next;
    };
    if !response.status().is_success() {
        return Err(DomainError::new(
            ErrorCode::IoError,
            "download HTTP status is not successful",
        ));
    }
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temporary)
        .map_err(fs_error)?;
    let mut digest = Sha256::new();
    let mut size = 0_u64;
    while let Some(chunk) = response.chunk().await.map_err(download_error)? {
        cancellation_checkpoint(claim)?;
        size = size
            .checked_add(chunk.len() as u64)
            .ok_or_else(|| DomainError::new(ErrorCode::ResourceLimit, "download size overflow"))?;
        output.write_all(&chunk).map_err(fs_error)?;
        digest.update(&chunk);
    }
    output.sync_all().map_err(fs_error)?;
    Ok((
        size,
        digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    ))
}

fn synchronous_result<T: serde::Serialize>(
    result: T,
) -> Result<FilesystemExecutionResult, DomainError> {
    serde_json::to_value(result)
        .map(FilesystemExecutionResult::Synchronous)
        .map_err(|_| {
            DomainError::new(
                ErrorCode::InternalError,
                "filesystem result encoding failed",
            )
        })
}

fn cancellation_checkpoint(claim: Option<&crate::LocalExecutionClaim>) -> Result<(), DomainError> {
    claim.map_or(Ok(()), crate::LocalExecutionClaim::checkpoint)
}

fn validate_inspect_input(input: &FilesystemInspectInput) -> Result<(), DomainError> {
    if !(1..=5_000).contains(&input.max_entries)
        || !(1..=16).contains(&input.max_depth)
        || (!input.recursive && input.max_depth != 1)
    {
        return Err(DomainError::invalid(
            "filesystem.inspect input is out of bounds",
        ));
    }
    Ok(())
}

fn validate_read_input(input: &FilesystemReadInput) -> Result<(), DomainError> {
    if !(1..=1_048_576).contains(&input.max_bytes) {
        return Err(DomainError::invalid(
            "filesystem.read input is out of bounds",
        ));
    }
    Ok(())
}

fn publish_with_claim<T>(
    claim: Option<&crate::LocalExecutionClaim>,
    publication: impl FnOnce() -> Result<T, DomainError>,
) -> Result<T, DomainError> {
    match claim {
        Some(claim) => claim.publish(publication),
        None => publication(),
    }
}

trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

#[derive(Clone, Copy)]
enum DetectedArchive {
    Zip,
    Tar,
    TarGz,
}

fn require_path(target: &FileTarget) -> Result<(), DomainError> {
    if target.target_type == FileTargetType::Path {
        Ok(())
    } else {
        Err(DomainError::new(
            ErrorCode::Unsupported,
            "content URI requires the Android framework filesystem adapter",
        ))
    }
}

fn require_content_uri(target: &FileTarget) -> Result<(), DomainError> {
    if target.target_type != FileTargetType::ContentUri || !target.value.starts_with("content://") {
        return Err(DomainError::invalid(
            "Android framework filesystem target is not a content URI",
        ));
    }
    Ok(())
}

pub fn normalize_absolute_path(value: &str) -> Result<PathBuf, DomainError> {
    if value.is_empty() || value.as_bytes().contains(&0) {
        return Err(DomainError::invalid("path is empty or contains NUL"));
    }
    let path = Path::new(value);
    if !path.is_absolute() || value.len() > 4_096 {
        return Err(DomainError::invalid("path must be a bounded absolute path"));
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(Path::new(std::path::MAIN_SEPARATOR_STR)),
            Component::Normal(value) => normalized.push(value),
            Component::CurDir | Component::ParentDir => {
                return Err(DomainError::invalid("path contains an invalid component"));
            }
        }
    }
    Ok(normalized)
}

#[allow(clippy::too_many_arguments)]
fn inspect_directory(
    root: &Path,
    relative: &Path,
    depth: u32,
    max_depth: u32,
    recursive: bool,
    limit: usize,
    output: &mut Vec<FileEntry>,
    truncated: &mut bool,
) -> Result<(), DomainError> {
    let mut children = fs::read_dir(root.join(relative))
        .map_err(fs_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(fs_error)?;
    children.sort_by_key(|entry| entry.file_name());
    for child in children {
        if output.len() == limit {
            *truncated = true;
            break;
        }
        let child_relative = relative.join(child.file_name());
        let metadata = fs::symlink_metadata(child.path()).map_err(fs_error)?;
        let entry_type = file_type(&metadata);
        output.push(FileEntry {
            name: child_relative.to_string_lossy().replace('\\', "/"),
            entry_type,
            size: regular_size(&metadata),
            modified_at: modified_at(&metadata),
        });
        if recursive && depth < max_depth && entry_type == FileType::Directory {
            inspect_directory(
                root,
                &child_relative,
                depth + 1,
                max_depth,
                recursive,
                limit,
                output,
                truncated,
            )?;
        }
        if *truncated {
            break;
        }
    }
    Ok(())
}

fn file_type(metadata: &fs::Metadata) -> FileType {
    let kind = metadata.file_type();
    if kind.is_symlink() {
        FileType::Symlink
    } else if kind.is_file() {
        FileType::File
    } else if kind.is_dir() {
        FileType::Directory
    } else {
        FileType::Other
    }
}

fn regular_size(metadata: &fs::Metadata) -> Option<u64> {
    metadata.file_type().is_file().then_some(metadata.len())
}

fn modified_at(metadata: &fs::Metadata) -> Option<String> {
    let timestamp: chrono::DateTime<chrono::Utc> =
        chrono::DateTime::from(metadata.modified().ok()?);
    Some(timestamp.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

fn inline_result_fits(data: &str, returned_bytes: u64, total_size: Option<u64>) -> bool {
    let result = FilesystemReadResult {
        data: Some(data.to_owned()),
        data_ref: None,
        returned_bytes,
        total_size,
        truncated: false,
        sha256: Some("0".repeat(64)),
    };
    serde_json::to_vec(&result).is_ok_and(|encoded| {
        encoded.len() + MAX_INLINE_ENVELOPE_OVERHEAD <= UI_ENVELOPE_LIMIT_BYTES
    })
}

fn decode_content(content: &str, encoding: DataEncoding) -> Result<Vec<u8>, DomainError> {
    match encoding {
        DataEncoding::Utf8 => Ok(content.as_bytes().to_vec()),
        DataEncoding::Base64 => BASE64
            .decode(content)
            .map_err(|_| DomainError::invalid("filesystem content is not canonical base64")),
    }
}

fn apply_replacements(original: &str, replacements: &[Replacement]) -> Result<String, DomainError> {
    if replacements.is_empty() || replacements.len() > 100 {
        return Err(DomainError::invalid("edit replacement count is invalid"));
    }
    let mut matches = Vec::with_capacity(replacements.len());
    for replacement in replacements {
        if replacement.old.is_empty() {
            return Err(DomainError::invalid("edit match must not be empty"));
        }
        let positions = original
            .match_indices(&replacement.old)
            .map(|(offset, _)| offset)
            .collect::<Vec<_>>();
        if positions.len() != 1 {
            return Err(DomainError::invalid(
                "edit match must occur exactly once in original content",
            ));
        }
        matches.push((
            positions[0],
            positions[0] + replacement.old.len(),
            replacement.new.as_str(),
        ));
    }
    matches.sort_by_key(|value| value.0);
    if matches.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        return Err(DomainError::invalid("edit replacements overlap"));
    }
    let mut output = String::with_capacity(original.len());
    let mut cursor = 0;
    for (start, end, replacement) in matches {
        output.push_str(&original[cursor..start]);
        output.push_str(replacement);
        cursor = end;
    }
    output.push_str(&original[cursor..]);
    Ok(output)
}

fn atomic_regular_write(path: &Path, bytes: &[u8], create: bool) -> Result<(), DomainError> {
    let parent = existing_parent(path)?;
    let existing = match fs::symlink_metadata(path) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(fs_error(error)),
    };
    match (&existing, create) {
        (Some(_), true) => {
            return Err(DomainError::new(
                ErrorCode::AlreadyExists,
                "filesystem target already exists",
            ));
        }
        (None, false) => {
            return Err(DomainError::new(
                ErrorCode::NotFound,
                "filesystem target does not exist",
            ));
        }
        (Some(metadata), false) if !metadata.file_type().is_file() => {
            return Err(DomainError::new(
                ErrorCode::Unsupported,
                "filesystem target is not a regular file",
            ));
        }
        _ => {}
    }
    let temporary = temporary_sibling(path)?;
    let write_result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(fs_error)?;
        file.write_all(bytes).map_err(fs_error)?;
        if let Some(metadata) = &existing {
            preserve_metadata(path, &file, metadata)?;
        }
        file.sync_all().map_err(fs_error)?;
        drop(file);
        if create {
            publish_new_path(&temporary, path)?;
        } else {
            fs::rename(&temporary, path).map_err(fs_error)?;
        }
        sync_directory(&parent)
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}

#[cfg(unix)]
fn preserve_metadata(
    source: &Path,
    target: &File,
    metadata: &fs::Metadata,
) -> Result<(), DomainError> {
    use rustix::fs::{Gid, Mode, Uid, fchmod, fchown};
    use std::os::unix::fs::MetadataExt;
    fchown(
        target,
        Some(Uid::from_raw(metadata.uid())),
        Some(Gid::from_raw(metadata.gid())),
    )
    .map_err(rustix_fs_error)?;
    fchmod(target, Mode::from_raw_mode(metadata.mode() & 0o7777)).map_err(rustix_fs_error)?;
    #[cfg(any(target_os = "android", target_os = "linux"))]
    copy_selinux_context(source, target)?;
    Ok(())
}

#[cfg(not(unix))]
fn preserve_metadata(
    _source: &Path,
    target: &File,
    metadata: &fs::Metadata,
) -> Result<(), DomainError> {
    target
        .set_permissions(metadata.permissions())
        .map_err(fs_error)
}

#[cfg(any(target_os = "android", target_os = "linux"))]
fn copy_selinux_context(source: &Path, target: &File) -> Result<(), DomainError> {
    use rustix::{
        fs::{XattrFlags, fsetxattr, getxattr},
        io::Errno,
    };
    let mut value = vec![0_u8; 4_096];
    let length = match getxattr(source, "security.selinux", &mut value) {
        Ok(length) => length,
        Err(Errno::NODATA | Errno::NOTSUP) => return Ok(()),
        Err(error) => return Err(rustix_fs_error(error)),
    };
    value.truncate(length);
    match fsetxattr(target, "security.selinux", &value, XattrFlags::empty()) {
        Ok(()) => Ok(()),
        // A filesystem that cannot store a label — shared storage is the one on this device —
        // has none to set: the file carries the mount's own label, exactly as every other file
        // there does. Any other refusal is a real one.
        Err(Errno::NOTSUP) => Ok(()),
        Err(error) => Err(rustix_fs_error(error)),
    }
}

fn existing_parent(path: &Path) -> Result<PathBuf, DomainError> {
    let parent = path
        .parent()
        .ok_or_else(|| DomainError::invalid("filesystem target has no parent"))?;
    if !parent.is_dir() {
        return Err(DomainError::new(
            ErrorCode::NotFound,
            "filesystem parent does not exist",
        ));
    }
    Ok(parent.to_path_buf())
}

fn temporary_sibling(path: &Path) -> Result<PathBuf, DomainError> {
    let parent = existing_parent(path)?;
    let name = path
        .file_name()
        .ok_or_else(|| DomainError::invalid("filesystem target has no name"))?
        .to_string_lossy();
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    Ok(parent.join(format!(
        ".{name}.droidbridge-{}-{sequence}.tmp",
        std::process::id()
    )))
}

fn reject_virtual_mutation(path: &Path) -> Result<(), DomainError> {
    #[cfg(unix)]
    if path.starts_with("/proc") || path.starts_with("/sys") {
        return Err(DomainError::new(
            ErrorCode::Unsupported,
            "virtual filesystem mutation has no atomic regular-file semantics",
        ));
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn path_pair(
    source: &FileTarget,
    destination: &FileTarget,
) -> Result<(PathBuf, PathBuf), DomainError> {
    require_path(source)?;
    require_path(destination)?;
    Ok((
        normalize_absolute_path(&source.value)?,
        normalize_absolute_path(&destination.value)?,
    ))
}

fn reject_self_destination(source: &Path, destination: &Path) -> Result<(), DomainError> {
    if destination == source || destination.starts_with(source) {
        Err(DomainError::invalid(
            "filesystem destination cannot be the source or its descendant",
        ))
    } else {
        Ok(())
    }
}

fn copy_path(
    source: &Path,
    destination: &Path,
    recursive: bool,
    overwrite: bool,
) -> Result<(), DomainError> {
    let metadata = fs::symlink_metadata(source).map_err(fs_error)?;
    let destination_metadata = match fs::symlink_metadata(destination) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(fs_error(error)),
    };
    let destination_exists = destination_metadata.is_some();
    if destination_exists && !overwrite {
        return Err(DomainError::new(
            ErrorCode::AlreadyExists,
            "filesystem destination already exists",
        ));
    }
    if destination_metadata
        .as_ref()
        .is_some_and(|metadata| metadata.is_dir())
        && !fs::symlink_metadata(source).map_err(fs_error)?.is_dir()
    {
        return Err(DomainError::new(
            ErrorCode::Unsupported,
            "filesystem replacement cannot implicitly remove a directory",
        ));
    }
    if metadata.file_type().is_symlink() {
        let publication = temporary_sibling(destination)?;
        create_symlink(
            &fs::read_link(source).map_err(fs_error)?,
            &publication,
            source,
        )?;
        if let Err(error) = publish_renamed_path(&publication, destination, overwrite) {
            let _ = remove_path(&publication, true);
            return Err(error);
        }
    } else if metadata.is_dir() {
        if !recursive {
            return Err(DomainError::new(
                ErrorCode::NotEmpty,
                "recursive copy is required for a directory",
            ));
        }
        let publication = temporary_sibling(destination)?;
        let copied = (|| {
            fs::create_dir(&publication).map_err(fs_error)?;
            copy_directory_contents(source, &publication)?;
            publish_renamed_path(&publication, destination, overwrite)
        })();
        if copied.is_err() {
            let _ = remove_path(&publication, true);
        }
        copied?;
    } else if metadata.is_file() {
        let mut source_file = File::open(source).map_err(fs_error)?;
        if !source_file.metadata().map_err(fs_error)?.is_file() {
            return Err(DomainError::new(
                ErrorCode::Unsupported,
                "filesystem copy source descriptor is not a regular file",
            ));
        }
        publish_file_from_reader(&mut source_file, destination, overwrite)?;
    } else {
        return Err(DomainError::new(
            ErrorCode::Unsupported,
            "special filesystem entry cannot be copied",
        ));
    }
    sync_parent(destination)
}

fn copy_directory_contents(source: &Path, destination: &Path) -> Result<(), DomainError> {
    let mut children = fs::read_dir(source)
        .map_err(fs_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(fs_error)?;
    children.sort_by_key(|entry| entry.file_name());
    for child in children {
        let source = child.path();
        let destination = destination.join(child.file_name());
        let metadata = fs::symlink_metadata(&source).map_err(fs_error)?;
        if metadata.file_type().is_symlink() {
            create_symlink(
                &fs::read_link(&source).map_err(fs_error)?,
                &destination,
                &source,
            )?;
        } else if metadata.is_dir() {
            fs::create_dir(&destination).map_err(fs_error)?;
            copy_directory_contents(&source, &destination)?;
        } else if metadata.is_file() {
            fs::copy(&source, &destination).map_err(fs_error)?;
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(&destination)
                .and_then(|file| file.sync_all())
                .map_err(fs_error)?;
        } else {
            return Err(DomainError::new(
                ErrorCode::Unsupported,
                "special filesystem entry cannot be copied",
            ));
        }
    }
    sync_directory(destination)
}

fn remove_path(path: &Path, recursive: bool) -> Result<(), DomainError> {
    let metadata = fs::symlink_metadata(path).map_err(fs_error)?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        if recursive {
            fs::remove_dir_all(path).map_err(fs_error)
        } else {
            fs::remove_dir(path).map_err(|error| {
                if matches!(error.kind(), std::io::ErrorKind::DirectoryNotEmpty) {
                    DomainError::new(ErrorCode::NotEmpty, "filesystem directory is not empty")
                } else {
                    fs_error(error)
                }
            })
        }
    } else {
        fs::remove_file(path).map_err(fs_error)
    }
}

fn path_exists_no_follow(path: &Path) -> Result<bool, DomainError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(fs_error(error)),
    }
}

#[cfg(unix)]
fn create_symlink(target: &Path, destination: &Path, _source: &Path) -> Result<(), DomainError> {
    std::os::unix::fs::symlink(target, destination).map_err(fs_error)
}

#[cfg(windows)]
fn create_symlink(target: &Path, destination: &Path, source: &Path) -> Result<(), DomainError> {
    if fs::metadata(source).is_ok_and(|metadata| metadata.is_dir()) {
        std::os::windows::fs::symlink_dir(target, destination).map_err(fs_error)
    } else {
        std::os::windows::fs::symlink_file(target, destination).map_err(fs_error)
    }
}

fn detect_archive(file: &mut File) -> Result<DetectedArchive, DomainError> {
    let mut prefix = [0_u8; 512];
    let read = file.read(&mut prefix).map_err(fs_error)?;
    if read >= 4 && matches!(&prefix[..4], b"PK\x03\x04" | b"PK\x05\x06" | b"PK\x07\x08") {
        return Ok(DetectedArchive::Zip);
    }
    if read >= 2 && prefix[..2] == [0x1f, 0x8b] {
        return Ok(DetectedArchive::TarGz);
    }
    if read == 512 && valid_tar_header(&prefix) {
        return Ok(DetectedArchive::Tar);
    }
    Err(DomainError::new(
        ErrorCode::Unsupported,
        "archive content has no supported signature",
    ))
}

fn valid_tar_header(header: &[u8; 512]) -> bool {
    let stored = std::str::from_utf8(&header[148..156])
        .ok()
        .and_then(|value| u64::from_str_radix(value.trim_matches(['\0', ' ']), 8).ok());
    let computed = header
        .iter()
        .enumerate()
        .map(|(index, byte)| {
            if (148..156).contains(&index) {
                b' '
            } else {
                *byte
            }
        })
        .map(u64::from)
        .sum::<u64>();
    stored == Some(computed)
}

fn list_archive(
    file: File,
    format: DetectedArchive,
    limit: usize,
) -> Result<(Vec<ArchiveEntry>, bool), DomainError> {
    match format {
        DetectedArchive::Zip => list_zip(file, limit),
        DetectedArchive::Tar => list_tar(tar::Archive::new(file), limit),
        DetectedArchive::TarGz => list_tar(tar::Archive::new(GzDecoder::new(file)), limit),
    }
}

fn list_zip(file: File, limit: usize) -> Result<(Vec<ArchiveEntry>, bool), DomainError> {
    let mut archive = ZipArchive::new(file).map_err(zip_error)?;
    let truncated = archive.len() > limit;
    let mut entries = Vec::with_capacity(min(archive.len(), limit));
    for index in 0..min(archive.len(), limit) {
        let entry = archive.by_index(index).map_err(zip_error)?;
        if entry.encrypted() {
            return Err(DomainError::new(
                ErrorCode::ArchiveEncrypted,
                "encrypted archive is unsupported",
            ));
        }
        let path = safe_archive_path(entry.name())?;
        entries.push(ArchiveEntry {
            path: archive_name(&path, entry.is_dir()),
            entry_type: zip_entry_type(&entry),
            size: (!entry.is_dir()).then_some(entry.size()),
        });
    }
    Ok((entries, truncated))
}

fn list_tar<R: Read>(
    mut archive: tar::Archive<R>,
    limit: usize,
) -> Result<(Vec<ArchiveEntry>, bool), DomainError> {
    let mut entries = Vec::new();
    let mut truncated = false;
    for entry in archive.entries().map_err(archive_corrupt)? {
        let entry = entry.map_err(archive_corrupt)?;
        if entries.len() == limit {
            truncated = true;
            break;
        }
        let path = entry.path().map_err(archive_corrupt)?.into_owned();
        validate_archive_relative(&path)?;
        let entry_type = entry.header().entry_type();
        entries.push(ArchiveEntry {
            path: archive_name(&path, entry_type.is_dir()),
            entry_type: tar_entry_type(entry_type),
            size: entry_type.is_file().then(|| entry.size()),
        });
    }
    Ok((entries, truncated))
}

fn create_zip(sources: &[PathBuf], temporary: &Path) -> Result<u64, DomainError> {
    let file = File::create(temporary).map_err(fs_error)?;
    let mut archive = ZipWriter::new(file);
    let mut count = 0;
    for source in sources {
        let name = source
            .file_name()
            .ok_or_else(|| DomainError::invalid("archive source has no name"))?;
        append_zip(&mut archive, source, Path::new(name), &mut count)?;
    }
    archive
        .finish()
        .map_err(zip_error)?
        .sync_all()
        .map_err(fs_error)?;
    Ok(count)
}

fn append_zip(
    archive: &mut ZipWriter<File>,
    source: &Path,
    relative: &Path,
    count: &mut u64,
) -> Result<(), DomainError> {
    let metadata = fs::symlink_metadata(source).map_err(fs_error)?;
    let name = relative.to_string_lossy().replace('\\', "/");
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    if metadata.is_dir() {
        archive
            .add_directory(format!("{name}/"), options)
            .map_err(zip_error)?;
        *count += 1;
        let mut children = fs::read_dir(source)
            .map_err(fs_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(fs_error)?;
        children.sort_by_key(|entry| entry.file_name());
        for child in children {
            append_zip(
                archive,
                &child.path(),
                &relative.join(child.file_name()),
                count,
            )?;
        }
    } else if metadata.is_file() {
        archive.start_file(name, options).map_err(zip_error)?;
        let mut input = File::open(source).map_err(fs_error)?;
        std::io::copy(&mut input, archive).map_err(fs_error)?;
        *count += 1;
    } else {
        return Err(DomainError::new(
            ErrorCode::Unsupported,
            "archive creation supports regular files and directories",
        ));
    }
    Ok(())
}

fn create_tar(sources: &[PathBuf], temporary: &Path, gzip: bool) -> Result<u64, DomainError> {
    let count = count_archive_entries(sources)?;
    let file = File::create(temporary).map_err(fs_error)?;
    if gzip {
        let encoder = GzEncoder::new(file, Compression::default());
        let mut archive = tar::Builder::new(encoder);
        append_tar_sources(&mut archive, sources)?;
        let encoder = archive.into_inner().map_err(fs_error)?;
        encoder
            .finish()
            .map_err(fs_error)?
            .sync_all()
            .map_err(fs_error)?;
    } else {
        let mut archive = tar::Builder::new(file);
        append_tar_sources(&mut archive, sources)?;
        archive
            .into_inner()
            .map_err(fs_error)?
            .sync_all()
            .map_err(fs_error)?;
    }
    Ok(count)
}

fn append_tar_sources<W: Write>(
    archive: &mut tar::Builder<W>,
    sources: &[PathBuf],
) -> Result<(), DomainError> {
    archive.follow_symlinks(false);
    for source in sources {
        let name = source
            .file_name()
            .ok_or_else(|| DomainError::invalid("archive source has no name"))?;
        if fs::symlink_metadata(source).map_err(fs_error)?.is_dir() {
            archive
                .append_dir_all(Path::new(name), source)
                .map_err(fs_error)?;
        } else {
            archive
                .append_path_with_name(source, Path::new(name))
                .map_err(fs_error)?;
        }
    }
    archive.finish().map_err(fs_error)
}

fn count_archive_entries(sources: &[PathBuf]) -> Result<u64, DomainError> {
    sources.iter().try_fold(0_u64, |total, source| {
        count_path_entries(source).and_then(|count| {
            total.checked_add(count).ok_or_else(|| {
                DomainError::new(ErrorCode::ResourceLimit, "archive entry count overflow")
            })
        })
    })
}

fn count_path_entries(path: &Path) -> Result<u64, DomainError> {
    let metadata = fs::symlink_metadata(path).map_err(fs_error)?;
    if !metadata.is_dir() {
        return Ok(1);
    }
    fs::read_dir(path)
        .map_err(fs_error)?
        .try_fold(1_u64, |total, entry| {
            let count = count_path_entries(&entry.map_err(fs_error)?.path())?;
            total.checked_add(count).ok_or_else(|| {
                DomainError::new(ErrorCode::ResourceLimit, "archive entry count overflow")
            })
        })
}

fn extract_archive(
    file: File,
    format: DetectedArchive,
    destination: &Path,
) -> Result<u64, DomainError> {
    match format {
        DetectedArchive::Zip => extract_zip(file, destination),
        DetectedArchive::Tar => extract_tar(tar::Archive::new(file), destination),
        DetectedArchive::TarGz => extract_tar(tar::Archive::new(GzDecoder::new(file)), destination),
    }
}

fn extract_zip(file: File, destination: &Path) -> Result<u64, DomainError> {
    let mut archive = ZipArchive::new(file).map_err(zip_error)?;
    let extraction = SecureExtractionRoot::open(destination)?;
    let mut count = 0_u64;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(zip_error)?;
        if entry.encrypted() {
            return Err(DomainError::new(
                ErrorCode::ArchiveEncrypted,
                "encrypted archive is unsupported",
            ));
        }
        let relative = safe_archive_path(entry.name())?;
        if entry.is_dir() {
            extraction.create_directory(&relative)?;
        } else if zip_entry_type(&entry) == ArchiveEntryType::File {
            let mut output = extraction.create_file(&relative)?;
            std::io::copy(&mut entry, &mut output).map_err(fs_error)?;
            output.sync_all().map_err(fs_error)?;
        } else {
            return Err(DomainError::new(
                ErrorCode::Unsupported,
                "archive link extraction is unsupported",
            ));
        }
        count += 1;
    }
    Ok(count)
}

fn extract_tar<R: Read>(
    mut archive: tar::Archive<R>,
    destination: &Path,
) -> Result<u64, DomainError> {
    let extraction = SecureExtractionRoot::open(destination)?;
    let mut count = 0_u64;
    for entry in archive.entries().map_err(archive_corrupt)? {
        let mut entry = entry.map_err(archive_corrupt)?;
        let relative = entry.path().map_err(archive_corrupt)?.into_owned();
        validate_archive_relative(&relative)?;
        let kind = entry.header().entry_type();
        if !(kind.is_file() || kind.is_dir()) {
            return Err(DomainError::new(
                ErrorCode::Unsupported,
                "archive link extraction is unsupported",
            ));
        }
        if kind.is_dir() {
            extraction.create_directory(&relative)?;
        } else {
            let mut output = extraction.create_file(&relative)?;
            std::io::copy(&mut entry, &mut output).map_err(fs_error)?;
            output.sync_all().map_err(fs_error)?;
        }
        count += 1;
    }
    Ok(count)
}

#[cfg(unix)]
struct SecureExtractionRoot {
    directory: std::os::fd::OwnedFd,
}

#[cfg(unix)]
impl SecureExtractionRoot {
    fn open(path: &Path) -> Result<Self, DomainError> {
        use rustix::fs::{CWD, Mode, OFlags, openat};
        let directory = openat(
            CWD,
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(secure_extraction_error)?;
        Ok(Self { directory })
    }

    fn create_directory(&self, relative: &Path) -> Result<(), DomainError> {
        use std::os::fd::AsFd;
        let mut current = None;
        for component in relative.components() {
            let Component::Normal(name) = component else {
                return Err(archive_containment_error());
            };
            let parent_fd = current
                .as_ref()
                .map_or_else(|| self.directory.as_fd(), std::os::fd::AsFd::as_fd);
            current = Some(open_or_create_directory_at(parent_fd, name)?);
        }
        Ok(())
    }

    fn create_file(&self, relative: &Path) -> Result<File, DomainError> {
        use rustix::fs::{Mode, OFlags, openat};
        use std::os::fd::AsFd;
        let mut components = relative.components().collect::<Vec<_>>();
        let Some(Component::Normal(name)) = components.pop() else {
            return Err(archive_containment_error());
        };
        let mut current = None;
        for component in components {
            let Component::Normal(directory) = component else {
                return Err(archive_containment_error());
            };
            let parent_fd = current
                .as_ref()
                .map_or_else(|| self.directory.as_fd(), AsFd::as_fd);
            current = Some(open_or_create_directory_at(parent_fd, directory)?);
        }
        let parent_fd = current
            .as_ref()
            .map_or_else(|| self.directory.as_fd(), AsFd::as_fd);
        openat(
            parent_fd,
            name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map(File::from)
        .map_err(secure_extraction_error)
    }
}

#[cfg(unix)]
fn open_or_create_directory_at(
    parent_fd: std::os::fd::BorrowedFd<'_>,
    name: &std::ffi::OsStr,
) -> Result<std::os::fd::OwnedFd, DomainError> {
    use rustix::{
        fs::{Mode, OFlags, mkdirat, openat},
        io::Errno,
    };
    if let Err(error) = mkdirat(parent_fd, name, Mode::from_raw_mode(0o700))
        && error != Errno::EXIST
    {
        return Err(secure_extraction_error(error));
    }
    openat(
        parent_fd,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(secure_extraction_error)
}

#[cfg(unix)]
fn secure_extraction_error(error: rustix::io::Errno) -> DomainError {
    use rustix::io::Errno;
    if matches!(error, Errno::LOOP | Errno::NOTDIR | Errno::EXIST) {
        archive_containment_error()
    } else {
        rustix_fs_error(error)
    }
}

#[cfg(not(unix))]
struct SecureExtractionRoot {
    directory: PathBuf,
}

#[cfg(not(unix))]
impl SecureExtractionRoot {
    fn open(path: &Path) -> Result<Self, DomainError> {
        if !fs::symlink_metadata(path)
            .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
        {
            return Err(archive_containment_error());
        }
        Ok(Self {
            directory: path.to_path_buf(),
        })
    }

    fn create_directory(&self, relative: &Path) -> Result<(), DomainError> {
        let mut current = self.directory.clone();
        for component in relative.components() {
            let Component::Normal(name) = component else {
                return Err(archive_containment_error());
            };
            current.push(name);
            match fs::create_dir(&current) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if !fs::symlink_metadata(&current).is_ok_and(|metadata| {
                        metadata.is_dir() && !metadata.file_type().is_symlink()
                    }) {
                        return Err(archive_containment_error());
                    }
                }
                Err(error) => return Err(fs_error(error)),
            }
        }
        Ok(())
    }

    fn create_file(&self, relative: &Path) -> Result<File, DomainError> {
        let Some(parent) = relative.parent() else {
            return Err(archive_containment_error());
        };
        if !parent.as_os_str().is_empty() {
            self.create_directory(parent)?;
        }
        let target = self.directory.join(relative);
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    archive_containment_error()
                } else {
                    fs_error(error)
                }
            })
    }
}

fn archive_containment_error() -> DomainError {
    DomainError::new(
        ErrorCode::ArchiveCorrupt,
        "archive entry escapes its destination",
    )
}

fn safe_archive_path(value: &str) -> Result<PathBuf, DomainError> {
    let path = PathBuf::from(value.replace('\\', "/"));
    validate_archive_relative(&path)?;
    Ok(path)
}

fn validate_archive_relative(path: &Path) -> Result<(), DomainError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::Prefix(_)
                    | Component::RootDir
                    | Component::CurDir
                    | Component::ParentDir
            )
        })
    {
        Err(DomainError::new(
            ErrorCode::ArchiveCorrupt,
            "archive entry escapes its destination",
        ))
    } else {
        Ok(())
    }
}

fn archive_name(path: &Path, directory: bool) -> String {
    let mut name = path.to_string_lossy().replace('\\', "/");
    if directory && !name.ends_with('/') {
        name.push('/');
    }
    name
}

fn zip_entry_type<R: Read + ?Sized>(entry: &zip::read::ZipFile<'_, R>) -> ArchiveEntryType {
    if entry.is_dir() {
        ArchiveEntryType::Directory
    } else if entry
        .unix_mode()
        .is_some_and(|mode| mode & 0o170000 == 0o120000)
    {
        ArchiveEntryType::Symlink
    } else {
        ArchiveEntryType::File
    }
}

fn tar_entry_type(value: tar::EntryType) -> ArchiveEntryType {
    if value.is_file() {
        ArchiveEntryType::File
    } else if value.is_dir() {
        ArchiveEntryType::Directory
    } else if value.is_symlink() {
        ArchiveEntryType::Symlink
    } else {
        ArchiveEntryType::Other
    }
}

fn publish_file(temporary: &Path, destination: &Path, overwrite: bool) -> Result<(), DomainError> {
    let mut input = File::open(temporary).map_err(fs_error)?;
    publish_file_from_reader(&mut input, destination, overwrite)?;
    fs::remove_file(temporary).map_err(fs_error)
}

fn publish_file_from_reader<R: Read>(
    input: &mut R,
    destination: &Path,
    overwrite: bool,
) -> Result<u64, DomainError> {
    let parent = existing_parent(destination)?;
    let existing = match fs::symlink_metadata(destination) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(fs_error(error)),
    };
    if existing.is_some() && !overwrite {
        return Err(DomainError::new(
            ErrorCode::AlreadyExists,
            "filesystem destination already exists",
        ));
    }
    if existing
        .as_ref()
        .is_some_and(|metadata| !metadata.file_type().is_file())
    {
        return Err(DomainError::new(
            ErrorCode::Unsupported,
            "filesystem file publication cannot replace a non-file",
        ));
    }
    let publication = temporary_sibling(destination)?;
    let outcome = (|| {
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&publication)
            .map_err(fs_error)?;
        let written = std::io::copy(input, &mut output).map_err(fs_error)?;
        if let Some(metadata) = &existing {
            preserve_metadata(destination, &output, metadata)?;
        }
        output.sync_all().map_err(fs_error)?;
        drop(output);
        if existing.is_some() {
            fs::rename(&publication, destination).map_err(fs_error)?;
        } else {
            publish_new_path(&publication, destination)?;
        }
        sync_directory(&parent)?;
        Ok(written)
    })();
    if outcome.is_err() {
        let _ = fs::remove_file(&publication);
    }
    outcome
}

#[cfg(unix)]
fn publish_new_path(temporary: &Path, destination: &Path) -> Result<(), DomainError> {
    use rustix::{
        fs::{CWD, RenameFlags, renameat_with},
        io::Errno,
    };

    renameat_with(CWD, temporary, CWD, destination, RenameFlags::NOREPLACE).map_err(|error| {
        if matches!(error, Errno::XDEV | Errno::NOTSUP | Errno::NOSYS) {
            DomainError::new(
                ErrorCode::Unsupported,
                "filesystem destination does not support exclusive publication",
            )
        } else if error == Errno::EXIST {
            DomainError::new(
                ErrorCode::AlreadyExists,
                "filesystem destination already exists",
            )
        } else {
            rustix_fs_error(error)
        }
    })
}

#[cfg(not(unix))]
fn publish_new_path(temporary: &Path, destination: &Path) -> Result<(), DomainError> {
    fs::hard_link(temporary, destination).map_err(fs_error)?;
    fs::remove_file(temporary).map_err(fs_error)
}

#[cfg(unix)]
fn publish_renamed_path(
    source: &Path,
    destination: &Path,
    overwrite: bool,
) -> Result<(), DomainError> {
    use rustix::{
        fs::{CWD, RenameFlags, renameat_with},
        io::Errno,
    };

    let destination_metadata = match fs::symlink_metadata(destination) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(fs_error(error)),
    };
    let destination_exists = destination_metadata.is_some();
    if destination_exists && !overwrite {
        return Err(DomainError::new(
            ErrorCode::AlreadyExists,
            "filesystem destination already exists",
        ));
    }
    if destination_metadata
        .as_ref()
        .is_some_and(|metadata| metadata.is_dir())
        && !fs::symlink_metadata(source).map_err(fs_error)?.is_dir()
    {
        return Err(DomainError::new(
            ErrorCode::Unsupported,
            "filesystem replacement cannot implicitly remove a directory",
        ));
    }
    let flags = if destination_exists {
        RenameFlags::EXCHANGE
    } else {
        RenameFlags::NOREPLACE
    };
    renameat_with(CWD, source, CWD, destination, flags).map_err(|error| {
        if error == Errno::XDEV {
            DomainError::new(
                ErrorCode::Unsupported,
                "filesystem source and destination are on different filesystems",
            )
        } else if matches!(error, Errno::NOTSUP | Errno::NOSYS)
            // FUSE — shared storage is the one on this device — answers an exchange with EINVAL:
            // what it cannot provide there is the exchange, not the rename.
            || (destination_exists && error == Errno::INVAL)
        {
            DomainError::new(
                ErrorCode::Unsupported,
                "filesystem destination does not support atomic publication",
            )
        } else if error == Errno::EXIST {
            DomainError::new(
                ErrorCode::AlreadyExists,
                "filesystem destination already exists",
            )
        } else {
            rustix_fs_error(error)
        }
    })?;
    if let Some(expected) = &destination_metadata {
        let displaced = fs::symlink_metadata(source);
        if !matches!(
            displaced.as_ref(),
            Ok(metadata) if (metadata.dev(), metadata.ino()) == (expected.dev(), expected.ino())
        ) {
            let rollback = renameat_with(CWD, source, CWD, destination, RenameFlags::EXCHANGE)
                .map_err(rustix_fs_error)
                .and_then(|()| sync_parent(source))
                .and_then(|()| sync_parent(destination));
            return if rollback.is_ok() {
                Err(DomainError::new(
                    ErrorCode::IoError,
                    "filesystem destination changed before publication",
                ))
            } else {
                Err(DomainError::new(
                    ErrorCode::IoError,
                    "filesystem replacement identity check and rollback failed",
                ))
            };
        }
    }
    if destination_exists && let Err(cleanup_error) = remove_path(source, true) {
        let rollback = renameat_with(CWD, source, CWD, destination, RenameFlags::EXCHANGE);
        return if rollback.is_ok() {
            Err(cleanup_error)
        } else {
            Err(DomainError::new(
                ErrorCode::IoError,
                "filesystem replacement cleanup and rollback failed",
            ))
        };
    }
    sync_parent(source)?;
    sync_parent(destination)
}

#[cfg(not(unix))]
fn publish_renamed_path(
    source: &Path,
    destination: &Path,
    overwrite: bool,
) -> Result<(), DomainError> {
    if path_exists_no_follow(destination)? {
        return Err(DomainError::new(
            if overwrite {
                ErrorCode::Unsupported
            } else {
                ErrorCode::AlreadyExists
            },
            "filesystem destination does not support atomic replacement",
        ));
    }
    fs::rename(source, destination).map_err(fs_error)?;
    sync_parent(source)?;
    sync_parent(destination)
}

#[cfg(unix)]
fn publish_directory(stage: &Path, destination: &Path, overwrite: bool) -> Result<(), DomainError> {
    publish_renamed_path(stage, destination, overwrite)
}

#[cfg(not(unix))]
fn publish_directory(stage: &Path, destination: &Path, overwrite: bool) -> Result<(), DomainError> {
    if destination.exists() {
        return Err(DomainError::new(
            if overwrite {
                ErrorCode::Unsupported
            } else {
                ErrorCode::AlreadyExists
            },
            "archive destination does not support atomic directory replacement",
        ));
    }
    fs::rename(stage, destination).map_err(fs_error)?;
    sync_parent(destination)
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn hash_file(
    path: &Path,
    claim: Option<&crate::LocalExecutionClaim>,
) -> Result<(u64, String), DomainError> {
    let mut file = File::open(path).map_err(fs_error)?;
    hash_reader(&mut file, claim)
}

fn hash_reader<R: Read>(
    reader: &mut R,
    claim: Option<&crate::LocalExecutionClaim>,
) -> Result<(u64, String), DomainError> {
    let mut size = 0_u64;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1_024];
    loop {
        cancellation_checkpoint(claim)?;
        let read = reader.read(&mut buffer).map_err(fs_error)?;
        if read == 0 {
            break;
        }
        size = size.checked_add(read as u64).ok_or_else(|| {
            DomainError::new(ErrorCode::ResourceLimit, "filesystem byte count overflow")
        })?;
        digest.update(&buffer[..read]);
    }
    Ok((
        size,
        digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    ))
}

fn fs_error(error: std::io::Error) -> DomainError {
    let code = match error.kind() {
        std::io::ErrorKind::NotFound => ErrorCode::NotFound,
        std::io::ErrorKind::AlreadyExists => ErrorCode::AlreadyExists,
        std::io::ErrorKind::PermissionDenied => ErrorCode::PermissionDenied,
        std::io::ErrorKind::DirectoryNotEmpty => ErrorCode::NotEmpty,
        _ => ErrorCode::IoError,
    };
    DomainError::new(code, "filesystem operation failed")
}

#[cfg(unix)]
fn rustix_fs_error(error: rustix::io::Errno) -> DomainError {
    fs_error(error.into())
}

fn zip_error(_: zip::result::ZipError) -> DomainError {
    DomainError::new(ErrorCode::ArchiveCorrupt, "ZIP archive is corrupt")
}

fn archive_corrupt(_: std::io::Error) -> DomainError {
    DomainError::new(ErrorCode::ArchiveCorrupt, "archive is corrupt")
}

fn download_error(error: reqwest::Error) -> DomainError {
    if error.is_timeout() {
        DomainError::new(ErrorCode::Timeout, "download timed out")
    } else {
        DomainError::new(ErrorCode::IoError, "download transport failed")
    }
}

fn postcondition_error() -> DomainError {
    DomainError::new(
        ErrorCode::IoError,
        "filesystem operation postcondition was not established",
    )
}

fn sync_parent(path: &Path) -> Result<(), DomainError> {
    path.parent().map_or(Ok(()), sync_directory)
}

fn sync_directory(path: &Path) -> Result<(), DomainError> {
    #[cfg(unix)]
    {
        File::open(path)
            .and_then(|directory| directory.sync_all())
            .map_err(fs_error)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn archive_extraction_never_follows_a_destination_symlink() {
        let root = std::env::temp_dir().join(format!(
            "droidbridge-archive-containment-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("stage")).unwrap();
        fs::create_dir_all(root.join("outside")).unwrap();
        symlink(root.join("outside"), root.join("stage/pivot")).unwrap();

        let archive_path = root.join("archive.zip");
        let file = File::create(&archive_path).unwrap();
        let mut archive = ZipWriter::new(file);
        archive
            .start_file(
                "pivot/escape.txt",
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
            )
            .unwrap();
        archive.write_all(b"escape").unwrap();
        archive.finish().unwrap();

        let result = extract_zip(File::open(&archive_path).unwrap(), &root.join("stage"));

        assert_eq!(result.unwrap_err().code, ErrorCode::ArchiveCorrupt);
        assert!(!root.join("outside/escape.txt").exists());
        let _ = fs::remove_dir_all(&root);
    }
}
