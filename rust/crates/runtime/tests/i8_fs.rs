use contract::{
    ArchiveFormat, Availability, CapabilityState, DataEncoding, FileTarget, FileTargetType,
    FileType, FilesystemArchiveInput, FilesystemArchiveTaskResult, FilesystemCall,
    FilesystemDownloadInput, FilesystemInspectInput, FilesystemManageInput, FilesystemReadInput,
    FilesystemWriteInput, GrantFacts, ManageOperation, ReadSource, Replacement, RuntimeHost,
    RuntimeReadiness, UuidV4,
};
use domain::{
    AdmissionFence, CapabilityContext, Preflight, Provider, ProviderGenerations, ResolverFacts,
};
use runtime::{
    AdmittedExecution, CapabilitySnapshot, CompositeExecutionSurface, ExecutionOutcome,
    ExecutionPayload, ExecutionPort, ExecutorRecord, FilesystemCandidate, FilesystemFrameworkPort,
    FilesystemFrameworkSource, FilesystemKernel, FilesystemPreflightPort,
    FilesystemPrimitiveMetadata, FilesystemPrimitivePort, NativeFilesystemExecutionSurface,
    ProviderToken, RecoveryProof, RuntimeCore,
    fakes::{FakeArtifacts, FakeCapabilities, FakeHostControl, FakePersistence},
    filesystem_preflight, resolve_filesystem_executor,
};
use sha2::Digest as _;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::{
    cell::RefCell,
    fs::{self, OpenOptions},
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("droidbridge-i8-fs-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn target(&self, relative: &str) -> FileTarget {
        FileTarget {
            target_type: FileTargetType::Path,
            value: self.root.join(relative).to_string_lossy().into_owned(),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[derive(Clone)]
struct FixtureFrameworkPort {
    source: PathBuf,
}

impl FilesystemFrameworkPort for FixtureFrameworkPort {
    fn inspect(
        &self,
        _execution: &AdmittedExecution,
        input: FilesystemInspectInput,
    ) -> Result<contract::FilesystemInspectResult, domain::DomainError> {
        Ok(contract::FilesystemInspectResult {
            target: input.target,
            target_type: FileType::File,
            size: Some(6),
            modified_at: None,
            entries: None,
            truncated: None,
        })
    }

    fn open_read(
        &self,
        _execution: &AdmittedExecution,
        _target: &FileTarget,
    ) -> Result<FilesystemFrameworkSource, domain::DomainError> {
        let file = fs::File::open(&self.source).unwrap();
        Ok(FilesystemFrameworkSource {
            total_size: Some(file.metadata().unwrap().len()),
            file,
        })
    }
}

#[tokio::test]
async fn i8_fs_g01_canonical_ingress_dispatches_content_read_to_the_framework_handler() {
    let fixture = Fixture::new("canonical-content");
    let source = fixture.root.join("content.txt");
    fs::write(&source, b"content-bridge").unwrap();
    let artifacts = FakeArtifacts::default();
    let capabilities = FakeCapabilities::new(capability(RuntimeHost::ApkRuntime));
    let filesystem = NativeFilesystemExecutionSurface::new(
        fixture.root.clone(),
        artifacts.clone(),
        capabilities.clone(),
        ProviderToken::AppNative,
    )
    .with_framework(FixtureFrameworkPort { source });
    let core = RuntimeCore::new(
        FakePersistence::default(),
        artifacts,
        CompositeExecutionSurface::new(filesystem),
        capabilities,
        FakeHostControl::new(RecoveryProof::Clean),
    );
    let request = serde_json::json!({
        "protocol_version": 1,
        "request_id": "10000000-0000-4000-8000-000000000001",
        "payload": {
            "tool": "filesystem",
            "action": "read",
            "input": {
                "target": {
                    "type": "content_uri",
                    "value": "content://media/external/downloads/1"
                },
                "offset": 0,
                "max_bytes": 64,
                "encoding": "utf8"
            }
        }
    });
    let encoded = runtime::submit_public(
        &core,
        &serde_json::to_vec(&request).unwrap(),
        "2026-09-12T00:00:00.000Z".to_owned(),
        1_789_171_200_000,
        true,
        |_| async { panic!("filesystem escaped its canonical ingress handler") },
    )
    .await;
    let response: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(response["outcome"], "success");
    assert_eq!(response["result"]["data"], "content-bridge");
    assert_eq!(response["result"]["returned_bytes"], 14);
    assert_eq!(response["result"]["truncated"], false);
}

#[tokio::test]
async fn i8_fs_g01_canonical_ingress_preserves_path_mutation_and_data_ref_semantics() {
    let fixture = Fixture::new("canonical-path");
    let artifacts = FakeArtifacts::default();
    let capabilities = FakeCapabilities::new(capability(RuntimeHost::ApkRuntime));
    let filesystem = NativeFilesystemExecutionSurface::new(
        fixture.root.clone(),
        artifacts.clone(),
        capabilities.clone(),
        ProviderToken::AppNative,
    );
    let core = RuntimeCore::new(
        FakePersistence::default(),
        artifacts,
        CompositeExecutionSurface::new(filesystem),
        capabilities,
        FakeHostControl::new(RecoveryProof::Clean),
    );
    let created_path = fixture.root.join("created.txt");
    let create = serde_json::json!({
        "protocol_version": 1,
        "request_id": "10000000-0000-4000-8000-000000000011",
        "payload": {
            "tool": "filesystem",
            "action": "write",
            "input": {
                "mode": "create",
                "target": {"type": "path", "value": created_path.to_string_lossy()},
                "content": "created-through-ingress",
                "encoding": "utf8"
            }
        }
    });
    let create_response: serde_json::Value = serde_json::from_slice(
        &runtime::submit_public(
            &core,
            &serde_json::to_vec(&create).unwrap(),
            "2026-09-12T00:00:00.000Z".to_owned(),
            1_789_171_200_000,
            true,
            |_| async { panic!("filesystem escaped its canonical ingress handler") },
        )
        .await,
    )
    .unwrap();
    assert_eq!(create_response["outcome"], "success", "{create_response}");
    assert_eq!(fs::read(&created_path).unwrap(), b"created-through-ingress");

    let large_path = fixture.root.join("large.txt");
    fs::write(&large_path, vec![b'x'; 300_000]).unwrap();
    let read = serde_json::json!({
        "protocol_version": 1,
        "request_id": "10000000-0000-4000-8000-000000000012",
        "payload": {
            "tool": "filesystem",
            "action": "read",
            "input": {
                "target": {"type": "path", "value": large_path.to_string_lossy()},
                "offset": 0,
                "max_bytes": 300000,
                "encoding": "utf8"
            }
        }
    });
    let read_response: serde_json::Value = serde_json::from_slice(
        &runtime::submit_public(
            &core,
            &serde_json::to_vec(&read).unwrap(),
            "2026-09-12T00:00:01.000Z".to_owned(),
            1_789_171_201_000,
            true,
            |_| async { panic!("filesystem escaped its canonical ingress handler") },
        )
        .await,
    )
    .unwrap();
    assert_eq!(read_response["outcome"], "success");
    assert!(read_response["result"].get("data").is_none());
    let data_ref = read_response["result"]["data_ref"].as_str().unwrap();

    let read_ref = serde_json::json!({
        "protocol_version": 1,
        "request_id": "10000000-0000-4000-8000-000000000013",
        "payload": {
            "tool": "filesystem",
            "action": "read",
            "input": {
                "data_ref": data_ref,
                "offset": 0,
                "max_bytes": 5,
                "encoding": "utf8"
            }
        }
    });
    let ref_response: serde_json::Value = serde_json::from_slice(
        &runtime::submit_public(
            &core,
            &serde_json::to_vec(&read_ref).unwrap(),
            "2026-09-12T00:00:02.000Z".to_owned(),
            1_789_171_202_000,
            true,
            |_| async { panic!("filesystem escaped its canonical ingress handler") },
        )
        .await,
    )
    .unwrap();
    assert_eq!(ref_response["outcome"], "success");
    assert_eq!(ref_response["result"]["data"], "xxxxx");
    assert_eq!(ref_response["result"]["truncated"], true);
}

#[test]
fn i8_fs_g03_i5_proof_only_filesystem_vertical_is_absent() {
    let vertical = runtime::ApkRuntimeVertical::new(runtime::VerticalEnvironment {
        sdk_int: 35,
        abi: "arm64-v8a".to_owned(),
        timezone: "UTC".to_owned(),
        manufacturer: "fixture".to_owned(),
        model: "fixture".to_owned(),
        device: "fixture".to_owned(),
        build_fingerprint: "fixture".to_owned(),
        version_name: "0.1.0".to_owned(),
        version_code: 1_000,
        runtime_epoch: uuid(31),
        host_generation: 1,
    })
    .unwrap();
    let request: contract::PublicRequest = serde_json::from_value(serde_json::json!({
        "protocol_version": 1,
        "request_id": "10000000-0000-4000-8000-000000000032",
        "payload": {
            "tool": "filesystem",
            "action": "inspect",
            "input": {
                "target": {"type": "path", "value": "/data/local/tmp"},
                "recursive": false,
                "max_depth": 1,
                "max_entries": 10
            }
        }
    }))
    .unwrap();
    assert_eq!(
        vertical.dispatch_installed(request).unwrap_err().code,
        contract::ErrorCode::Unsupported
    );
}

#[tokio::test]
async fn i8_fs_g04_apk_and_magisk_hosts_execute_one_filesystem_semantics_without_fallback() {
    for (host, provider) in [
        (RuntimeHost::ApkRuntime, ProviderToken::AppNative),
        (RuntimeHost::MagiskBackend, ProviderToken::MagiskNative),
    ] {
        let fixture = Fixture::new(match host {
            RuntimeHost::ApkRuntime => "single-app-handler",
            RuntimeHost::MagiskBackend => "single-magisk-handler",
        });
        let source = fixture.root.join("source.txt");
        fs::write(&source, b"same-semantics").unwrap();
        let artifacts = FakeArtifacts::default();
        let capabilities = FakeCapabilities::new(capability(host));
        let filesystem = NativeFilesystemExecutionSurface::new(
            fixture.root.clone(),
            artifacts.clone(),
            capabilities.clone(),
            provider,
        );
        let core = RuntimeCore::new(
            FakePersistence::default(),
            artifacts,
            CompositeExecutionSurface::new(filesystem),
            capabilities,
            FakeHostControl::new(RecoveryProof::Clean),
        );
        let request = serde_json::json!({
            "protocol_version": 1,
            "request_id": match host {
                RuntimeHost::ApkRuntime => "10000000-0000-4000-8000-000000000041",
                RuntimeHost::MagiskBackend => "10000000-0000-4000-8000-000000000042",
            },
            "payload": {
                "tool": "filesystem",
                "action": "read",
                "input": {
                    "target": {"type": "path", "value": source.to_string_lossy()},
                    "offset": 0,
                    "max_bytes": 64,
                    "encoding": "utf8"
                }
            }
        });
        let response: serde_json::Value = serde_json::from_slice(
            &runtime::submit_public(
                &core,
                &serde_json::to_vec(&request).unwrap(),
                "2026-09-12T00:00:00.000Z".to_owned(),
                1_789_171_200_000,
                true,
                |_| async { panic!("filesystem escaped its canonical ingress handler") },
            )
            .await,
        )
        .unwrap();
        assert_eq!(response["outcome"], "success");
        assert_eq!(response["result"]["data"], "same-semantics");
    }
}

#[cfg(windows)]
#[test]
fn i8_fs_g06_architecture_gate_rejects_both_forbidden_filesystem_boundaries() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .unwrap();
    let status = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            "tools/test-architecture-gate.ps1",
        ])
        .current_dir(repository)
        .status()
        .unwrap();
    assert!(status.success());
}

#[derive(Clone, Copy)]
struct FixturePrimitivePort;

impl FilesystemPrimitivePort for FixturePrimitivePort {
    fn lstat(
        &self,
        _execution: &AdmittedExecution,
        path: &std::path::Path,
    ) -> Result<FilesystemPrimitiveMetadata, domain::DomainError> {
        let metadata = fs::symlink_metadata(path).map_err(fixture_fs_error)?;
        #[cfg(unix)]
        let mode = metadata.mode();
        #[cfg(not(unix))]
        let mode = if metadata.is_dir() {
            0o040000
        } else {
            0o100000
        };
        Ok(FilesystemPrimitiveMetadata {
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(not(unix))]
            device: 0,
            #[cfg(unix)]
            inode: metadata.ino(),
            #[cfg(not(unix))]
            inode: 0,
            mode,
            uid: 0,
            gid: 0,
            size: metadata.len(),
            modified_at_epoch_seconds: 0,
            selinux_context: fixture_selinux_context(path)?,
        })
    }

    fn open_read(
        &self,
        _execution: &AdmittedExecution,
        path: &std::path::Path,
    ) -> Result<fs::File, domain::DomainError> {
        #[cfg(windows)]
        if path.is_dir() {
            use std::os::windows::fs::OpenOptionsExt;
            return OpenOptions::new()
                .read(true)
                .custom_flags(0x0200_0000)
                .open(path)
                .map_err(fixture_fs_error);
        }
        fs::File::open(path).map_err(fixture_fs_error)
    }

    fn read_directory(
        &self,
        _execution: &AdmittedExecution,
        path: &Path,
        cookie: u64,
        limit: usize,
    ) -> Result<runtime::FilesystemPrimitiveDirectoryPage, domain::DomainError> {
        if path.to_string_lossy().ends_with(std::path::MAIN_SEPARATOR) {
            return Err(domain::DomainError::new(
                contract::ErrorCode::InvalidArgument,
                "fixture directory path has a trailing separator",
            ));
        }
        let mut names = fs::read_dir(path)
            .map_err(fixture_fs_error)?
            .map(|entry| {
                entry
                    .map(|entry| entry.file_name())
                    .map_err(fixture_fs_error)
            })
            .collect::<Result<Vec<_>, _>>()?;
        names.sort();
        let start = usize::try_from(cookie).map_err(|_| {
            domain::DomainError::new(
                contract::ErrorCode::InvalidArgument,
                "fixture directory cookie is invalid",
            )
        })?;
        if start > names.len() || limit == 0 {
            return Err(domain::DomainError::new(
                contract::ErrorCode::InvalidArgument,
                "fixture directory page is invalid",
            ));
        }
        let end = start.saturating_add(limit).min(names.len());
        Ok(runtime::FilesystemPrimitiveDirectoryPage {
            names: names[start..end].to_vec(),
            next_cookie: (end < names.len()).then_some(end as u64),
        })
    }

    fn access_write_search(
        &self,
        _execution: &AdmittedExecution,
        path: &Path,
    ) -> Result<(), domain::DomainError> {
        if path.is_dir() {
            Ok(())
        } else {
            Err(domain::DomainError::new(
                contract::ErrorCode::PermissionDenied,
                "fixture directory is not writable",
            ))
        }
    }

    fn create_exclusive(
        &self,
        _execution: &AdmittedExecution,
        path: &Path,
        _mode: u32,
    ) -> Result<fs::File, domain::DomainError> {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(fixture_fs_error)
    }

    fn apply_metadata(
        &self,
        _execution: &AdmittedExecution,
        file: &fs::File,
        metadata: &FilesystemPrimitiveMetadata,
    ) -> Result<(), domain::DomainError> {
        #[cfg(unix)]
        file.set_permissions(fs::Permissions::from_mode(metadata.mode & 0o7777))
            .map_err(fixture_fs_error)?;
        #[cfg(not(unix))]
        let _ = (file, metadata);
        Ok(())
    }

    fn rename_atomic(
        &self,
        _execution: &AdmittedExecution,
        source: &Path,
        destination: &Path,
        exchange: bool,
    ) -> Result<(), domain::DomainError> {
        if !exchange {
            return fs::rename(source, destination).map_err(fixture_fs_error);
        }
        let swap = source.with_file_name(".fixture-exchange");
        fs::rename(source, &swap).map_err(fixture_fs_error)?;
        if let Err(error) = fs::rename(destination, source) {
            let _ = fs::rename(&swap, source);
            return Err(fixture_fs_error(error));
        }
        if let Err(error) = fs::rename(&swap, destination) {
            let _ = fs::rename(source, destination);
            let _ = fs::rename(&swap, source);
            return Err(fixture_fs_error(error));
        }
        Ok(())
    }

    fn fsync_directory(
        &self,
        execution: &AdmittedExecution,
        path: &Path,
    ) -> Result<(), domain::DomainError> {
        #[cfg(windows)]
        {
            let _ = (execution, path);
            Ok(())
        }
        #[cfg(not(windows))]
        self.open_read(execution, path)?
            .sync_all()
            .map_err(fixture_fs_error)
    }

    fn mkdir(
        &self,
        _execution: &AdmittedExecution,
        path: &Path,
        _mode: u32,
    ) -> Result<(), domain::DomainError> {
        fs::create_dir(path).map_err(fixture_fs_error)
    }

    fn unlink(
        &self,
        _execution: &AdmittedExecution,
        path: &Path,
    ) -> Result<(), domain::DomainError> {
        let metadata = fs::symlink_metadata(path).map_err(fixture_fs_error)?;
        if metadata.is_dir() {
            fs::remove_dir(path).map_err(fixture_fs_error)
        } else {
            fs::remove_file(path).map_err(fixture_fs_error)
        }
    }

    fn readlink(
        &self,
        _execution: &AdmittedExecution,
        path: &Path,
    ) -> Result<PathBuf, domain::DomainError> {
        fs::read_link(path).map_err(fixture_fs_error)
    }

    fn symlink(
        &self,
        _execution: &AdmittedExecution,
        target: &Path,
        destination: &Path,
    ) -> Result<(), domain::DomainError> {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, destination).map_err(fixture_fs_error)
        }
        #[cfg(not(unix))]
        {
            let _ = (target, destination);
            Err(domain::DomainError::new(
                contract::ErrorCode::Unsupported,
                "fixture symlink is unsupported",
            ))
        }
    }
}

#[cfg(any(target_os = "android", target_os = "linux"))]
fn fixture_selinux_context(path: &Path) -> Result<Option<Vec<u8>>, domain::DomainError> {
    use rustix::{fs::lgetxattr, io::Errno};
    let mut value = vec![0_u8; 4_096];
    let length = match lgetxattr(path, "security.selinux", &mut value) {
        Ok(length) => length,
        Err(Errno::NODATA | Errno::NOTSUP) => return Ok(None),
        Err(error) => {
            return Err(fixture_fs_error(std::io::Error::from_raw_os_error(
                error.raw_os_error(),
            )));
        }
    };
    value.truncate(length);
    Ok(Some(value))
}

#[cfg(not(any(target_os = "android", target_os = "linux")))]
fn fixture_selinux_context(_path: &Path) -> Result<Option<Vec<u8>>, domain::DomainError> {
    Ok(None)
}

fn fixture_fs_error(error: std::io::Error) -> domain::DomainError {
    let code = match error.kind() {
        std::io::ErrorKind::NotFound => contract::ErrorCode::NotFound,
        std::io::ErrorKind::AlreadyExists => contract::ErrorCode::AlreadyExists,
        std::io::ErrorKind::PermissionDenied => contract::ErrorCode::PermissionDenied,
        std::io::ErrorKind::DirectoryNotEmpty => contract::ErrorCode::NotEmpty,
        _ => contract::ErrorCode::IoError,
    };
    domain::DomainError::new(code, "fixture filesystem failure")
}

#[derive(Clone, Copy)]
struct CleanupFailurePrimitivePort;

impl FilesystemPrimitivePort for CleanupFailurePrimitivePort {
    fn lstat(
        &self,
        execution: &AdmittedExecution,
        path: &Path,
    ) -> Result<FilesystemPrimitiveMetadata, domain::DomainError> {
        FixturePrimitivePort.lstat(execution, path)
    }

    fn open_read(
        &self,
        execution: &AdmittedExecution,
        path: &Path,
    ) -> Result<fs::File, domain::DomainError> {
        FixturePrimitivePort.open_read(execution, path)
    }

    fn read_directory(
        &self,
        execution: &AdmittedExecution,
        path: &Path,
        cookie: u64,
        limit: usize,
    ) -> Result<runtime::FilesystemPrimitiveDirectoryPage, domain::DomainError> {
        FixturePrimitivePort.read_directory(execution, path, cookie, limit)
    }

    fn access_write_search(
        &self,
        execution: &AdmittedExecution,
        path: &Path,
    ) -> Result<(), domain::DomainError> {
        FixturePrimitivePort.access_write_search(execution, path)
    }

    fn create_exclusive(
        &self,
        execution: &AdmittedExecution,
        path: &Path,
        mode: u32,
    ) -> Result<fs::File, domain::DomainError> {
        FixturePrimitivePort.create_exclusive(execution, path, mode)
    }

    fn rename_atomic(
        &self,
        _execution: &AdmittedExecution,
        _source: &Path,
        _destination: &Path,
        _exchange: bool,
    ) -> Result<(), domain::DomainError> {
        Err(domain::DomainError::new(
            contract::ErrorCode::IoError,
            "fixture publication failed",
        ))
    }

    fn unlink(
        &self,
        _execution: &AdmittedExecution,
        _path: &Path,
    ) -> Result<(), domain::DomainError> {
        Err(domain::DomainError::new(
            contract::ErrorCode::PermissionDenied,
            "fixture cleanup failed",
        ))
    }
}

#[derive(Clone)]
struct DestinationRacePrimitivePort {
    raced: Arc<AtomicBool>,
}

impl FilesystemPrimitivePort for DestinationRacePrimitivePort {
    fn lstat(
        &self,
        execution: &AdmittedExecution,
        path: &Path,
    ) -> Result<FilesystemPrimitiveMetadata, domain::DomainError> {
        let mut metadata = FixturePrimitivePort.lstat(execution, path)?;
        if path
            .file_name()
            .is_some_and(|name| name == "race-source.txt")
            && fs::read(path).is_ok_and(|bytes| bytes == b"intruder")
        {
            // The race left a foreign object where the destination was. Its identity has to differ
            // from the destination identity the operation captured, but a filesystem may hand the
            // replacement the captured inode back (Windows reuses file ids, and a nearby inode
            // collides with a +1 perturbation), which would make the replacement indistinguishable
            // from an unchanged destination. Report an identity no filesystem hands out instead.
            metadata.inode = u64::MAX;
        }
        Ok(metadata)
    }

    fn open_read(
        &self,
        execution: &AdmittedExecution,
        path: &Path,
    ) -> Result<fs::File, domain::DomainError> {
        FixturePrimitivePort.open_read(execution, path)
    }

    fn rename_atomic(
        &self,
        execution: &AdmittedExecution,
        source: &Path,
        destination: &Path,
        exchange: bool,
    ) -> Result<(), domain::DomainError> {
        if exchange && !self.raced.swap(true, Ordering::SeqCst) {
            fs::remove_file(destination).map_err(fixture_fs_error)?;
            fs::write(destination, b"intruder").map_err(fixture_fs_error)?;
        }
        FixturePrimitivePort.rename_atomic(execution, source, destination, exchange)
    }

    fn fsync_directory(
        &self,
        execution: &AdmittedExecution,
        path: &Path,
    ) -> Result<(), domain::DomainError> {
        FixturePrimitivePort.fsync_directory(execution, path)
    }
}

#[tokio::test]
async fn i8_fs_g02_app_framework_reuses_the_rust_read_kernel_under_the_admission_fence() {
    let fixture = Fixture::new("framework-surface");
    let source = fixture.root.join("provider-bytes");
    fs::write(&source, b"abcdef").unwrap();
    let snapshot = capability(RuntimeHost::ApkRuntime);
    let surface = NativeFilesystemExecutionSurface::new(
        fixture.root.clone(),
        FakeArtifacts::default(),
        FakeCapabilities::new(snapshot.clone()),
        ProviderToken::AppNative,
    )
    .with_framework(FixtureFrameworkPort { source });
    let execution = AdmittedExecution {
        execution_id: uuid(80),
        task_id: None,
        executor: ExecutorRecord {
            host: RuntimeHost::ApkRuntime,
            provider: ProviderToken::AppFramework,
            execution_class: contract::ExecutionClass::AndroidFramework,
            capability_generation: snapshot.resolver_facts.generations.app_framework,
            fence: contract::Fence {
                runtime_epoch: snapshot.fence.runtime_epoch,
                host_generation: snapshot.fence.host_generation,
                runtime_instance_id: snapshot.fence.runtime_instance_id,
            },
        },
        payload: ExecutionPayload::FilesystemCall(FilesystemCall::Read(FilesystemReadInput {
            source: ReadSource::Target {
                target: FileTarget {
                    target_type: FileTargetType::ContentUri,
                    value: "content://authority/document/1".to_owned(),
                },
            },
            offset: 2,
            max_bytes: 3,
            encoding: DataEncoding::Utf8,
        })),
    };

    let completion = surface.claim_and_start(execution).await.unwrap();
    let ExecutionOutcome::SynchronousCompleted { result, .. } = completion.outcome else {
        panic!("framework read did not complete synchronously")
    };
    let result: contract::FilesystemReadResult = serde_json::from_value(result).unwrap();
    assert_eq!(result.data.as_deref(), Some("cde"));
    assert_eq!(result.returned_bytes, 3);
    assert_eq!(result.total_size, Some(6));
    assert!(result.truncated);
}

#[test]
fn i8_fs_g01_unknown_provider_size_still_reports_a_bounded_read_as_truncated() {
    let fixture = Fixture::new("unknown-provider-size");
    let source = fixture.root.join("provider-bytes");
    fs::write(&source, b"abcdef").unwrap();
    let result = FilesystemKernel::new(FakeArtifacts::default())
        .read_opened(
            FilesystemReadInput {
                source: ReadSource::Target {
                    target: FileTarget {
                        target_type: FileTargetType::ContentUri,
                        value: "content://authority/document/unknown-size".to_owned(),
                    },
                },
                offset: 0,
                max_bytes: 3,
                encoding: DataEncoding::Utf8,
            },
            fs::File::open(source).unwrap(),
            None,
        )
        .unwrap();

    assert_eq!(result.data.as_deref(), Some("abc"));
    assert_eq!(result.returned_bytes, 3);
    assert_eq!(result.total_size, None);
    assert!(result.truncated);
    assert_eq!(result.sha256, None);
}

#[tokio::test]
async fn i8_fs_g01_shizuku_primitive_read_reuses_the_admitted_rust_kernel() {
    let fixture = Fixture::new("shizuku-surface");
    let source = fixture.root.join("shell-readable");
    fs::write(&source, b"shell-bytes").unwrap();
    let snapshot = capability(RuntimeHost::ApkRuntime);
    let surface = NativeFilesystemExecutionSurface::new(
        fixture.root.clone(),
        FakeArtifacts::default(),
        FakeCapabilities::new(snapshot.clone()),
        ProviderToken::AppNative,
    )
    .with_primitives(FixturePrimitivePort);
    let call = FilesystemCall::Read(FilesystemReadInput {
        source: ReadSource::Target {
            target: FileTarget {
                target_type: FileTargetType::Path,
                value: source.to_string_lossy().into_owned(),
            },
        },
        offset: 6,
        max_bytes: 5,
        encoding: DataEncoding::Utf8,
    });
    assert_eq!(
        surface
            .preflight(FilesystemCandidate::Shizuku, &call)
            .unwrap(),
        Preflight::Positive
    );
    let execution = AdmittedExecution {
        execution_id: uuid(81),
        task_id: None,
        executor: ExecutorRecord {
            host: RuntimeHost::ApkRuntime,
            provider: ProviderToken::Shizuku,
            execution_class: contract::ExecutionClass::Shizuku,
            capability_generation: snapshot.resolver_facts.generations.shizuku,
            fence: contract::Fence {
                runtime_epoch: snapshot.fence.runtime_epoch,
                host_generation: snapshot.fence.host_generation,
                runtime_instance_id: snapshot.fence.runtime_instance_id,
            },
        },
        payload: ExecutionPayload::FilesystemCall(call),
    };

    let completion = surface.claim_and_start(execution).await.unwrap();
    let ExecutionOutcome::SynchronousCompleted { result, .. } = completion.outcome else {
        panic!("Shizuku read did not complete synchronously")
    };
    let result: contract::FilesystemReadResult = serde_json::from_value(result).unwrap();
    assert_eq!(result.data.as_deref(), Some("bytes"));
    assert_eq!(result.total_size, Some(11));
}

#[tokio::test]
async fn i8_fs_g01_shizuku_write_uses_exclusive_temp_and_atomic_publication() {
    let fixture = Fixture::new("shizuku-write");
    let snapshot = capability(RuntimeHost::ApkRuntime);
    let surface = NativeFilesystemExecutionSurface::new(
        fixture.root.clone(),
        FakeArtifacts::default(),
        FakeCapabilities::new(snapshot.clone()),
        ProviderToken::AppNative,
    )
    .with_primitives(FixturePrimitivePort);
    let target = fixture.target("published.txt");
    let create = FilesystemCall::Write(FilesystemWriteInput::Create {
        target: target.clone(),
        content: "first".to_owned(),
        encoding: DataEncoding::Utf8,
    });
    assert_eq!(
        surface
            .preflight(FilesystemCandidate::Shizuku, &create)
            .unwrap(),
        Preflight::Positive
    );
    let completion = surface
        .claim_and_start(shizuku_execution(&snapshot, 82, None, create.clone()))
        .await
        .unwrap();
    assert!(completion.cleanup_verified);
    assert!(matches!(
        completion.outcome,
        ExecutionOutcome::SynchronousCompleted { .. }
    ));
    assert_eq!(
        fs::read(fixture.root.join("published.txt")).unwrap(),
        b"first"
    );

    let replace = FilesystemCall::Write(FilesystemWriteInput::Replace {
        target: target.clone(),
        content: "second".to_owned(),
        encoding: DataEncoding::Utf8,
    });
    let completion = surface
        .claim_and_start(shizuku_execution(&snapshot, 83, None, replace))
        .await
        .unwrap();
    assert!(completion.cleanup_verified);
    assert_eq!(
        fs::read(fixture.root.join("published.txt")).unwrap(),
        b"second"
    );

    let collision = surface
        .claim_and_start(shizuku_execution(&snapshot, 84, None, create))
        .await
        .unwrap_err();
    assert_eq!(collision.error.code, contract::ErrorCode::AlreadyExists);
    assert!(collision.cleanup_verified);
    assert_eq!(
        fs::read(fixture.root.join("published.txt")).unwrap(),
        b"second"
    );
    assert!(fs::read_dir(&fixture.root).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains("droidbridge")
    }));
}

#[test]
fn i8_fs_g05_preflight_rejects_conflicting_destination_without_mutation() {
    let fixture = Fixture::new("preflight-destination");
    fs::write(fixture.root.join("existing"), b"keep").unwrap();

    let calls = [
        FilesystemCall::Write(FilesystemWriteInput::Create {
            target: fixture.target("existing"),
            content: "replacement".to_owned(),
            encoding: DataEncoding::Utf8,
        }),
        FilesystemCall::Manage(FilesystemManageInput::Mkdir {
            target: fixture.target("existing"),
            parents: false,
        }),
        FilesystemCall::Download(FilesystemDownloadInput {
            url: "https://example.invalid/value".to_owned(),
            destination: fixture.target("existing"),
            overwrite: false,
            timeout_ms: 5_000,
        }),
    ];

    for call in calls {
        assert_eq!(filesystem_preflight(&call).unwrap(), Preflight::Negative);
        assert_eq!(fs::read(fixture.root.join("existing")).unwrap(), b"keep");
    }
}

fn http_response(
    status: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let status = status.to_owned();
    let headers = headers
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect::<Vec<_>>();
    let body = body.to_vec();
    let worker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 2_048];
        let _ = stream.read(&mut request).unwrap();
        write!(stream, "HTTP/1.1 {status}\r\n").unwrap();
        for (name, value) in headers {
            write!(stream, "{name}: {value}\r\n").unwrap();
        }
        write!(
            stream,
            "Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(&body).unwrap();
    });
    (format!("http://{address}/fixture"), worker)
}

#[tokio::test]
async fn i8_fs_g01_download_publishes_only_verified_success_and_preserves_prior_destination() {
    let fixture = Fixture::new("download");
    let kernel = FilesystemKernel::new(FakeArtifacts::default());
    let (url, worker) = http_response("200 OK", &[], b"downloaded");
    let result = kernel
        .download(
            FilesystemDownloadInput {
                url,
                destination: fixture.target("target.bin"),
                overwrite: false,
                timeout_ms: 5_000,
            },
            &fixture.root.join("tmp-success"),
        )
        .await
        .unwrap();
    worker.join().unwrap();
    assert_eq!(result.size, 10);
    assert_eq!(
        fs::read(fixture.root.join("target.bin")).unwrap(),
        b"downloaded"
    );
    assert_eq!(result.sha256.len(), 64);

    #[cfg(unix)]
    fs::set_permissions(
        fixture.root.join("target.bin"),
        fs::Permissions::from_mode(0o640),
    )
    .unwrap();

    let (url, worker) = http_response("500 Internal Server Error", &[], b"failure");
    let failed = kernel
        .download(
            FilesystemDownloadInput {
                url,
                destination: fixture.target("target.bin"),
                overwrite: true,
                timeout_ms: 5_000,
            },
            &fixture.root.join("tmp-failure"),
        )
        .await;
    worker.join().unwrap();
    assert_eq!(failed.unwrap_err().code, contract::ErrorCode::IoError);
    assert_eq!(
        fs::read(fixture.root.join("target.bin")).unwrap(),
        b"downloaded"
    );
    #[cfg(unix)]
    assert_eq!(
        fs::metadata(fixture.root.join("target.bin"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o640
    );
    assert!(!fixture.root.join("tmp-failure/download.part").exists());

    let (url, worker) = http_response("200 OK", &[], b"updated");
    kernel
        .download(
            FilesystemDownloadInput {
                url,
                destination: fixture.target("target.bin"),
                overwrite: true,
                timeout_ms: 5_000,
            },
            &fixture.root.join("tmp-replace"),
        )
        .await
        .unwrap();
    worker.join().unwrap();
    assert_eq!(
        fs::read(fixture.root.join("target.bin")).unwrap(),
        b"updated"
    );
    #[cfg(unix)]
    assert_eq!(
        fs::metadata(fixture.root.join("target.bin"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o640
    );

    let unsupported = kernel
        .download(
            FilesystemDownloadInput {
                url: "file:///data/local/tmp/value".to_owned(),
                destination: fixture.target("other.bin"),
                overwrite: false,
                timeout_ms: 5_000,
            },
            &fixture.root.join("tmp-unsupported"),
        )
        .await;
    assert_eq!(
        unsupported.unwrap_err().code,
        contract::ErrorCode::Unsupported
    );

    let (redirect_url, redirect_worker) = http_response(
        "302 Found",
        &[("Location", "file:///data/local/tmp/forbidden")],
        b"",
    );
    let redirected = kernel
        .download(
            FilesystemDownloadInput {
                url: redirect_url,
                destination: fixture.target("redirected"),
                overwrite: false,
                timeout_ms: 1_000,
            },
            &fixture.root.join("tmp-redirect"),
        )
        .await;
    redirect_worker.join().unwrap();
    assert_eq!(
        redirected.unwrap_err().code,
        contract::ErrorCode::Unsupported
    );
    assert!(!fixture.root.join("redirected").exists());
}

fn uuid(value: u64) -> UuidV4 {
    UuidV4::parse(format!("10000000-0000-4000-8000-{value:012x}")).unwrap()
}

fn shizuku_execution(
    snapshot: &CapabilitySnapshot,
    id: u64,
    task_id: Option<UuidV4>,
    call: FilesystemCall,
) -> AdmittedExecution {
    AdmittedExecution {
        execution_id: uuid(id),
        task_id,
        executor: ExecutorRecord {
            host: RuntimeHost::ApkRuntime,
            provider: ProviderToken::Shizuku,
            execution_class: contract::ExecutionClass::Shizuku,
            capability_generation: snapshot.resolver_facts.generations.shizuku,
            fence: contract::Fence {
                runtime_epoch: snapshot.fence.runtime_epoch.clone(),
                host_generation: snapshot.fence.host_generation,
                runtime_instance_id: snapshot.fence.runtime_instance_id.clone(),
            },
        },
        payload: ExecutionPayload::FilesystemCall(call),
    }
}

fn available() -> Availability {
    Availability {
        state: CapabilityState::Available,
        reason: None,
    }
}

fn capability(host: RuntimeHost) -> CapabilitySnapshot {
    let grant = available();
    CapabilitySnapshot {
        grants: GrantFacts {
            android_local_network: grant.clone(),
            android_notifications: grant.clone(),
            android_notification_listener: grant.clone(),
            automation_exact_alarm: grant.clone(),
            visual_accessibility: grant.clone(),
            visual_media_projection_session: grant.clone(),
            shizuku_shell: grant.clone(),
            magisk_module: grant.clone(),
            magisk_root: grant.clone(),
            magisk_framework: grant.clone(),
            magisk_launch: grant.clone(),
            magisk_clipboard: grant.clone(),
            magisk_notifications: grant.clone(),
            magisk_wake_alarm: grant.clone(),
            execution_app_guard: grant.clone(),
            execution_shell_guard: grant.clone(),
            execution_root_guard: grant,
        },
        context: CapabilityContext {
            sdk_int: 37,
            host,
            readiness: RuntimeReadiness::Ready,
            app_execution_surface: CapabilityState::Available,
        },
        resolver_facts: ResolverFacts {
            app_native: CapabilityState::Available,
            app_framework: CapabilityState::Available,
            shizuku: CapabilityState::Available,
            magisk_native: CapabilityState::Available,
            magisk_framework: CapabilityState::Available,
            magisk_launch: CapabilityState::Available,
            magisk_clipboard: CapabilityState::Available,
            magisk_notifications: CapabilityState::Available,
            accessibility: CapabilityState::Available,
            media_projection: CapabilityState::Available,
            notification_listener: CapabilityState::Available,
            generations: ProviderGenerations {
                app_native: 11,
                app_framework: 12,
                shizuku: 13,
                magisk_native: 14,
                magisk_framework: 15,
                accessibility: 16,
                media_projection: 17,
                notification_listener: 18,
            },
        },
        fence: AdmissionFence {
            runtime_epoch: uuid(1),
            host_generation: 2,
            runtime_instance_id: uuid(2),
        },
    }
}

struct RecordingPreflight {
    app: Preflight,
    shizuku: Preflight,
    calls: RefCell<Vec<FilesystemCandidate>>,
}

impl FilesystemPreflightPort for RecordingPreflight {
    fn preflight(
        &self,
        candidate: FilesystemCandidate,
        _call: &FilesystemCall,
    ) -> Result<Preflight, domain::DomainError> {
        self.calls.borrow_mut().push(candidate);
        Ok(match candidate {
            FilesystemCandidate::App => self.app,
            FilesystemCandidate::Shizuku => self.shizuku,
        })
    }
}

#[test]
fn i8_fs_g02_all_actions_share_one_non_mutating_executor_resolver() {
    let path = FileTarget {
        target_type: FileTargetType::Path,
        value: "/data/local/tmp/value".to_owned(),
    };
    let content = FileTarget {
        target_type: FileTargetType::ContentUri,
        value: "content://authority/document/1".to_owned(),
    };
    let path_calls = vec![
        FilesystemCall::Inspect(FilesystemInspectInput {
            target: path.clone(),
            recursive: false,
            max_depth: 1,
            max_entries: 200,
        }),
        FilesystemCall::Read(FilesystemReadInput {
            source: ReadSource::Target {
                target: path.clone(),
            },
            offset: 0,
            max_bytes: 64,
            encoding: DataEncoding::Utf8,
        }),
        FilesystemCall::Write(FilesystemWriteInput::Create {
            target: path.clone(),
            content: "value".to_owned(),
            encoding: DataEncoding::Utf8,
        }),
        FilesystemCall::Manage(FilesystemManageInput::Delete {
            target: path.clone(),
            recursive: false,
        }),
        FilesystemCall::Download(FilesystemDownloadInput {
            url: "https://example.invalid/file".to_owned(),
            destination: path.clone(),
            overwrite: false,
            timeout_ms: 1_000,
        }),
        FilesystemCall::Archive(FilesystemArchiveInput::List {
            target: path.clone(),
            max_entries: 200,
        }),
    ];

    for call in &path_calls {
        let preflight = RecordingPreflight {
            app: Preflight::Negative,
            shizuku: Preflight::Positive,
            calls: RefCell::new(Vec::new()),
        };
        let executor =
            resolve_filesystem_executor(&capability(RuntimeHost::ApkRuntime), &preflight, call)
                .unwrap()
                .unwrap();
        assert_eq!(executor.provider(), Provider::Shizuku);
        assert_eq!(
            preflight.calls.into_inner(),
            [FilesystemCandidate::App, FilesystemCandidate::Shizuku]
        );

        let preflight = RecordingPreflight {
            app: Preflight::Positive,
            shizuku: Preflight::Positive,
            calls: RefCell::new(Vec::new()),
        };
        let executor =
            resolve_filesystem_executor(&capability(RuntimeHost::ApkRuntime), &preflight, call)
                .unwrap()
                .unwrap();
        assert_eq!(executor.provider(), Provider::AppNative);
        assert_eq!(preflight.calls.into_inner(), [FilesystemCandidate::App]);

        let preflight = RecordingPreflight {
            app: Preflight::Unknown,
            shizuku: Preflight::Unknown,
            calls: RefCell::new(Vec::new()),
        };
        let executor =
            resolve_filesystem_executor(&capability(RuntimeHost::MagiskBackend), &preflight, call)
                .unwrap()
                .unwrap();
        assert_eq!(executor.provider(), Provider::MagiskNative);
        assert!(preflight.calls.into_inner().is_empty());
    }

    let preflight = RecordingPreflight {
        app: Preflight::Positive,
        shizuku: Preflight::Positive,
        calls: RefCell::new(Vec::new()),
    };
    let content_executor = resolve_filesystem_executor(
        &capability(RuntimeHost::MagiskBackend),
        &preflight,
        &FilesystemCall::Inspect(FilesystemInspectInput {
            target: content.clone(),
            recursive: false,
            max_depth: 1,
            max_entries: 200,
        }),
    )
    .unwrap()
    .unwrap();
    assert_eq!(content_executor.provider(), Provider::AppFramework);
    assert!(preflight.calls.into_inner().is_empty());

    let unsupported = resolve_filesystem_executor(
        &capability(RuntimeHost::ApkRuntime),
        &RecordingPreflight {
            app: Preflight::Positive,
            shizuku: Preflight::Positive,
            calls: RefCell::new(Vec::new()),
        },
        &FilesystemCall::Write(FilesystemWriteInput::Create {
            target: content,
            content: "value".to_owned(),
            encoding: DataEncoding::Utf8,
        }),
    )
    .unwrap_err();
    assert_eq!(unsupported.code, contract::ErrorCode::Unsupported);

    let data_ref = FilesystemCall::Read(FilesystemReadInput {
        source: ReadSource::DataRef {
            data_ref: "dbref:data:10000000-0000-4000-8000-000000000001".to_owned(),
        },
        offset: 0,
        max_bytes: 64,
        encoding: DataEncoding::Utf8,
    });
    let no_preflight = RecordingPreflight {
        app: Preflight::Positive,
        shizuku: Preflight::Positive,
        calls: RefCell::new(Vec::new()),
    };
    assert!(
        resolve_filesystem_executor(
            &capability(RuntimeHost::ApkRuntime),
            &no_preflight,
            &data_ref,
        )
        .unwrap()
        .is_none()
    );
    assert!(no_preflight.calls.into_inner().is_empty());
}

#[test]
fn i8_fs_g01_path_inspect_read_write_and_manage_preserve_contract_truth() {
    let fixture = Fixture::new("kernel");
    let artifacts = FakeArtifacts::default();
    let kernel = FilesystemKernel::new(artifacts.clone());

    fs::create_dir(fixture.root.join("tree")).unwrap();
    fs::write(fixture.root.join("tree/b.txt"), b"bravo").unwrap();
    fs::write(fixture.root.join("tree/a.txt"), b"alpha").unwrap();
    fs::write(fixture.root.join("tree/c.txt"), b"charlie").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("a.txt", fixture.root.join("tree/link")).unwrap();
    let inspected = kernel
        .inspect(FilesystemInspectInput {
            target: fixture.target("tree"),
            recursive: false,
            max_depth: 1,
            max_entries: 2,
        })
        .unwrap();
    assert_eq!(inspected.target_type, FileType::Directory);
    assert_eq!(
        inspected
            .entries
            .unwrap()
            .into_iter()
            .map(|entry| entry.name)
            .collect::<Vec<_>>(),
        ["a.txt", "b.txt"]
    );
    assert_eq!(inspected.truncated, Some(true));

    let read = kernel
        .read(FilesystemReadInput {
            source: ReadSource::Target {
                target: fixture.target("tree/a.txt"),
            },
            offset: 1,
            max_bytes: 3,
            encoding: DataEncoding::Utf8,
        })
        .unwrap();
    assert_eq!(read.data.as_deref(), Some("lph"));
    assert_eq!(read.data_ref, None);
    assert_eq!(read.returned_bytes, 3);
    assert_eq!(read.total_size, Some(5));
    assert!(read.truncated);
    assert_eq!(read.sha256, None);

    let created = kernel
        .write(FilesystemWriteInput::Create {
            target: fixture.target("created.txt"),
            content: "created".to_owned(),
            encoding: DataEncoding::Utf8,
        })
        .unwrap();
    assert_eq!(created.bytes_written, 7);
    assert_eq!(
        fs::read(fixture.root.join("created.txt")).unwrap(),
        b"created"
    );

    #[cfg(unix)]
    fs::set_permissions(
        fixture.root.join("created.txt"),
        fs::Permissions::from_mode(0o640),
    )
    .unwrap();
    let replaced = kernel
        .write(FilesystemWriteInput::Replace {
            target: fixture.target("created.txt"),
            content: "replacement".to_owned(),
            encoding: DataEncoding::Utf8,
        })
        .unwrap();
    assert_eq!(replaced.bytes_written, 11);
    #[cfg(unix)]
    assert_eq!(
        fs::metadata(fixture.root.join("created.txt"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o640
    );

    let before = fs::read(fixture.root.join("created.txt")).unwrap();
    let ambiguous = kernel.write(FilesystemWriteInput::Edit {
        target: fixture.target("created.txt"),
        replacements: vec![Replacement {
            old: "e".to_owned(),
            new: "x".to_owned(),
        }],
    });
    assert_eq!(
        ambiguous.unwrap_err().code,
        contract::ErrorCode::InvalidArgument
    );
    assert_eq!(fs::read(fixture.root.join("created.txt")).unwrap(), before);

    let mkdir = kernel
        .manage(FilesystemManageInput::Mkdir {
            target: fixture.target("nested/leaf"),
            parents: true,
        })
        .unwrap();
    assert_eq!(mkdir.operation, ManageOperation::Mkdir);
    assert!(fixture.root.join("nested/leaf").is_dir());
    kernel
        .manage(FilesystemManageInput::Copy {
            source: fixture.target("tree"),
            destination: fixture.target("copied"),
            recursive: true,
            overwrite: false,
        })
        .unwrap();
    assert_eq!(
        fs::read(fixture.root.join("copied/a.txt")).unwrap(),
        b"alpha"
    );
    #[cfg(unix)]
    assert!(
        fs::symlink_metadata(fixture.root.join("copied/link"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let non_empty = kernel.manage(FilesystemManageInput::Delete {
        target: fixture.target("copied"),
        recursive: false,
    });
    assert_eq!(non_empty.unwrap_err().code, contract::ErrorCode::NotEmpty);
    kernel
        .manage(FilesystemManageInput::Delete {
            target: fixture.target("copied"),
            recursive: true,
        })
        .unwrap();
    assert!(!fixture.root.join("copied").exists());

    #[cfg(unix)]
    {
        fs::create_dir(fixture.root.join("replace-copy")).unwrap();
        fs::write(fixture.root.join("replace-copy/stale.txt"), b"stale").unwrap();
        kernel
            .manage(FilesystemManageInput::Copy {
                source: fixture.target("tree"),
                destination: fixture.target("replace-copy"),
                recursive: true,
                overwrite: true,
            })
            .unwrap();
        assert!(!fixture.root.join("replace-copy/stale.txt").exists());
        assert_eq!(
            fs::read(fixture.root.join("replace-copy/a.txt")).unwrap(),
            b"alpha"
        );

        fs::write(fixture.root.join("move-source.txt"), b"source").unwrap();
        fs::write(fixture.root.join("move-destination.txt"), b"destination").unwrap();
        kernel
            .manage(FilesystemManageInput::Move {
                source: fixture.target("move-source.txt"),
                destination: fixture.target("move-destination.txt"),
                recursive: false,
                overwrite: true,
            })
            .unwrap();
        assert!(!fixture.root.join("move-source.txt").exists());
        assert_eq!(
            fs::read(fixture.root.join("move-destination.txt")).unwrap(),
            b"source"
        );
    }
}

#[test]
fn i8_fs_g01_read_uses_bounded_data_artifacts_without_reopening_source() {
    let fixture = Fixture::new("artifact");
    let artifacts = FakeArtifacts::default();
    let kernel = FilesystemKernel::new(artifacts.clone());
    let bytes = vec![0xff; 300_000];
    fs::write(fixture.root.join("binary"), &bytes).unwrap();

    let first = kernel
        .read(FilesystemReadInput {
            source: ReadSource::Target {
                target: fixture.target("binary"),
            },
            offset: 0,
            max_bytes: 300_000,
            encoding: DataEncoding::Base64,
        })
        .unwrap();
    let artifact_ref = first.data_ref.unwrap();
    assert_eq!(first.data, None);
    assert_eq!(first.returned_bytes, 300_000);
    assert_eq!(first.total_size, Some(300_000));
    assert!(!first.truncated);
    assert!(first.sha256.is_some());

    fs::remove_file(fixture.root.join("binary")).unwrap();
    let replay = kernel
        .read(FilesystemReadInput {
            source: ReadSource::DataRef {
                data_ref: artifact_ref,
            },
            offset: 299_999,
            max_bytes: 1,
            encoding: DataEncoding::Base64,
        })
        .unwrap();
    assert_eq!(replay.data.as_deref(), Some("/w=="));
    assert_eq!(replay.returned_bytes, 1);
}

#[test]
fn i8_fs_g05_archive_detection_and_extraction_reject_escape_before_publication() {
    let fixture = Fixture::new("archive");
    let kernel = FilesystemKernel::new(FakeArtifacts::default());
    fs::create_dir(fixture.root.join("source")).unwrap();
    fs::write(fixture.root.join("source/value.txt"), b"value").unwrap();

    let created = kernel
        .archive_create(
            vec![fixture.target("source")],
            fixture.target("bundle.bin"),
            ArchiveFormat::Zip,
            false,
            &fixture.root.join("tmp-create"),
        )
        .unwrap();
    let FilesystemArchiveTaskResult::Create {
        entries_archived,
        bytes_written,
        sha256,
        ..
    } = created
    else {
        panic!("archive create returned the wrong result branch")
    };
    assert_eq!(entries_archived, 2);
    assert!(bytes_written > 0);
    assert_eq!(
        sha256,
        sha2::Sha256::digest(fs::read(fixture.root.join("bundle.bin")).unwrap())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );

    let listed = kernel
        .archive_list(FilesystemArchiveInput::List {
            target: fixture.target("bundle.bin"),
            max_entries: 10,
        })
        .unwrap();
    assert_eq!(
        listed
            .entries
            .iter()
            .map(|entry| entry.path.as_str())
            .collect::<Vec<_>>(),
        ["source/", "source/value.txt"]
    );
    assert!(!listed.truncated);

    let extracted = kernel
        .archive_extract(
            fixture.target("bundle.bin"),
            fixture.target("output"),
            false,
            &fixture.root.join("tmp-extract"),
        )
        .unwrap();
    let FilesystemArchiveTaskResult::Extract {
        entries_extracted, ..
    } = extracted
    else {
        panic!("archive extract returned the wrong result branch")
    };
    assert_eq!(entries_extracted, 2);
    assert_eq!(
        fs::read(fixture.root.join("output/source/value.txt")).unwrap(),
        b"value"
    );

    fs::write(fixture.root.join("corrupt.zip"), b"PK\x03\x04broken").unwrap();
    let corrupt = kernel.archive_list(FilesystemArchiveInput::List {
        target: fixture.target("corrupt.zip"),
        max_entries: 10,
    });
    assert_eq!(
        corrupt.unwrap_err().code,
        contract::ErrorCode::ArchiveCorrupt
    );

    fs::write(fixture.root.join("not-archive.zip"), b"plain bytes").unwrap();
    let unsupported = kernel.archive_list(FilesystemArchiveInput::List {
        target: fixture.target("not-archive.zip"),
        max_entries: 10,
    });
    assert_eq!(
        unsupported.unwrap_err().code,
        contract::ErrorCode::Unsupported
    );
}

#[tokio::test]
async fn i8_fs_g01_shizuku_cleanup_failure_is_not_reported_as_verified() {
    let fixture = Fixture::new("shizuku-cleanup-failure");
    let snapshot = capability(RuntimeHost::ApkRuntime);
    let surface = NativeFilesystemExecutionSurface::new(
        fixture.root.clone(),
        FakeArtifacts::default(),
        FakeCapabilities::new(snapshot.clone()),
        ProviderToken::AppNative,
    )
    .with_primitives(CleanupFailurePrimitivePort);
    let failure = surface
        .claim_and_start(shizuku_execution(
            &snapshot,
            85,
            None,
            FilesystemCall::Write(FilesystemWriteInput::Create {
                target: fixture.target("unpublished.txt"),
                content: "value".to_owned(),
                encoding: DataEncoding::Utf8,
            }),
        ))
        .await
        .unwrap_err();

    assert_eq!(failure.error.code, contract::ErrorCode::IoError);
    assert!(!failure.cleanup_verified);
    assert!(!fixture.root.join("unpublished.txt").exists());
    assert!(fs::read_dir(&fixture.root).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains("droidbridge")
    }));
}

#[test]
fn i8_fs_g01_recursive_copy_and_move_reject_self_descendants_before_mutation() {
    let fixture = Fixture::new("self-descendant");
    let kernel = FilesystemKernel::new(FakeArtifacts::default());
    fs::create_dir(fixture.root.join("tree")).unwrap();
    fs::write(fixture.root.join("tree/value.txt"), b"value").unwrap();

    let copied = kernel.manage(FilesystemManageInput::Copy {
        source: fixture.target("tree"),
        destination: fixture.target("tree/copied"),
        recursive: true,
        overwrite: false,
    });
    assert_eq!(
        copied.unwrap_err().code,
        contract::ErrorCode::InvalidArgument
    );
    assert!(!fixture.root.join("tree/copied").exists());

    fs::create_dir(fixture.root.join("tree/moved")).unwrap();
    fs::write(fixture.root.join("tree/moved/keep.txt"), b"keep").unwrap();
    let moved = kernel.manage(FilesystemManageInput::Move {
        source: fixture.target("tree"),
        destination: fixture.target("tree/moved"),
        recursive: true,
        overwrite: true,
    });
    assert_eq!(
        moved.unwrap_err().code,
        contract::ErrorCode::InvalidArgument
    );
    assert_eq!(
        fs::read(fixture.root.join("tree/moved/keep.txt")).unwrap(),
        b"keep"
    );
}

#[tokio::test]
async fn i8_fs_g01_shizuku_move_cannot_replace_a_directory_without_recursive_delete_authority() {
    let fixture = Fixture::new("shizuku-move-directory-replacement");
    let source = fixture.root.join("source.txt");
    let destination = fixture.root.join("destination");
    fs::write(&source, b"source").unwrap();
    fs::create_dir(&destination).unwrap();
    fs::write(destination.join("keep.txt"), b"keep").unwrap();
    let snapshot = capability(RuntimeHost::ApkRuntime);
    let surface = NativeFilesystemExecutionSurface::new(
        fixture.root.clone(),
        FakeArtifacts::default(),
        FakeCapabilities::new(snapshot.clone()),
        ProviderToken::AppNative,
    )
    .with_primitives(FixturePrimitivePort);

    let failure = surface
        .claim_and_start(shizuku_execution(
            &snapshot,
            86,
            None,
            FilesystemCall::Manage(FilesystemManageInput::Move {
                source: fixture.target("source.txt"),
                destination: fixture.target("destination"),
                recursive: false,
                overwrite: true,
            }),
        ))
        .await
        .unwrap_err();

    assert_eq!(failure.error.code, contract::ErrorCode::Unsupported);
    assert_eq!(fs::read(source).unwrap(), b"source");
    assert_eq!(fs::read(destination.join("keep.txt")).unwrap(), b"keep");
}

#[tokio::test]
async fn i8_fs_g01_shizuku_replacement_rolls_back_when_destination_identity_changes() {
    let fixture = Fixture::new("shizuku-replacement-race");
    let source = fixture.root.join("race-source.txt");
    let destination = fixture.root.join("race-destination.txt");
    fs::write(&source, b"source").unwrap();
    fs::write(&destination, b"destination").unwrap();
    let snapshot = capability(RuntimeHost::ApkRuntime);
    let surface = NativeFilesystemExecutionSurface::new(
        fixture.root.clone(),
        FakeArtifacts::default(),
        FakeCapabilities::new(snapshot.clone()),
        ProviderToken::AppNative,
    )
    .with_primitives(DestinationRacePrimitivePort {
        raced: Arc::new(AtomicBool::new(false)),
    });

    let failure = surface
        .claim_and_start(shizuku_execution(
            &snapshot,
            87,
            None,
            FilesystemCall::Manage(FilesystemManageInput::Move {
                source: fixture.target("race-source.txt"),
                destination: fixture.target("race-destination.txt"),
                recursive: false,
                overwrite: true,
            }),
        ))
        .await
        .unwrap_err();

    assert_eq!(
        failure.error.code,
        contract::ErrorCode::IoError,
        "identity-change rollback reported: {}",
        failure.error.reason
    );
    assert!(failure.cleanup_verified);
    assert_eq!(fs::read(source).unwrap(), b"source");
    assert_eq!(fs::read(destination).unwrap(), b"intruder");
}

#[test]
fn i8_fs_g01_archive_failure_cleans_task_temp_and_preserves_existing_destination() {
    let fixture = Fixture::new("archive-failure");
    let kernel = FilesystemKernel::new(FakeArtifacts::default());
    let create_temp = fixture.root.join("tmp-create");
    let failed_create = kernel.archive_create(
        vec![fixture.target("missing")],
        fixture.target("unused.zip"),
        ArchiveFormat::Zip,
        false,
        &create_temp,
    );
    assert_eq!(
        failed_create.unwrap_err().code,
        contract::ErrorCode::NotFound
    );
    assert!(!create_temp.join("archive.part").exists());

    let malicious = fixture.root.join("escape.zip");
    let file = fs::File::create(&malicious).unwrap();
    let mut archive = zip::ZipWriter::new(file);
    archive
        .start_file("../escape.txt", zip::write::SimpleFileOptions::default())
        .unwrap();
    archive.write_all(b"escape").unwrap();
    archive.finish().unwrap();

    fs::create_dir(fixture.root.join("destination")).unwrap();
    fs::write(fixture.root.join("destination/keep.txt"), b"keep").unwrap();
    let extract_temp = fixture.root.join("tmp-extract");
    let failed_extract = kernel.archive_extract(
        fixture.target("escape.zip"),
        fixture.target("destination"),
        true,
        &extract_temp,
    );
    assert_eq!(
        failed_extract.unwrap_err().code,
        contract::ErrorCode::ArchiveCorrupt
    );
    assert_eq!(
        fs::read(fixture.root.join("destination/keep.txt")).unwrap(),
        b"keep"
    );
    assert!(!fixture.root.join("escape.txt").exists());
    assert!(!extract_temp.join("extract.part").exists());
}

#[cfg(unix)]
fn framework_execution(
    snapshot: &CapabilitySnapshot,
    id: u64,
    call: FilesystemCall,
) -> AdmittedExecution {
    AdmittedExecution {
        execution_id: uuid(id),
        task_id: None,
        executor: ExecutorRecord {
            host: snapshot.context.host,
            provider: ProviderToken::AppFramework,
            execution_class: contract::ExecutionClass::AndroidFramework,
            capability_generation: snapshot.resolver_facts.generations.app_framework,
            fence: contract::Fence {
                runtime_epoch: snapshot.fence.runtime_epoch.clone(),
                host_generation: snapshot.fence.host_generation,
                runtime_instance_id: snapshot.fence.runtime_instance_id.clone(),
            },
        },
        payload: ExecutionPayload::FilesystemCall(call),
    }
}

#[test]
fn i8_fs_g02_magisk_content_route_follows_the_authenticated_app_surface() {
    let call = FilesystemCall::Inspect(FilesystemInspectInput {
        target: FileTarget {
            target_type: FileTargetType::ContentUri,
            value: "content://authority/document/1".to_owned(),
        },
        recursive: false,
        max_depth: 1,
        max_entries: 200,
    });
    let connected = capability(RuntimeHost::MagiskBackend);
    let executor = resolve_filesystem_executor(
        &connected,
        &RecordingPreflight {
            app: Preflight::Unknown,
            shizuku: Preflight::Unknown,
            calls: RefCell::new(Vec::new()),
        },
        &call,
    )
    .unwrap()
    .unwrap();
    assert_eq!(executor.provider(), Provider::AppFramework);
    assert_eq!(
        executor.capability_generation(),
        connected.resolver_facts.generations.app_framework
    );

    let mut detached = connected.clone();
    detached.context.app_execution_surface = CapabilityState::Unavailable;
    detached.resolver_facts.app_framework = CapabilityState::Unavailable;
    let error = resolve_filesystem_executor(
        &detached,
        &RecordingPreflight {
            app: Preflight::Unknown,
            shizuku: Preflight::Unknown,
            calls: RefCell::new(Vec::new()),
        },
        &call,
    )
    .unwrap_err();
    assert_eq!(error.code, contract::ErrorCode::CapabilityUnavailable);
}

#[test]
fn i8_fs_g02_magisk_framework_fact_tracks_the_companion_connection_lifetime() {
    let vertical = runtime::ApkRuntimeVertical::new_for_host(
        runtime::VerticalEnvironment {
            sdk_int: 35,
            abi: "arm64-v8a".to_owned(),
            timezone: "UTC".to_owned(),
            manufacturer: "fixture".to_owned(),
            model: "fixture".to_owned(),
            device: "fixture".to_owned(),
            build_fingerprint: "fixture".to_owned(),
            version_name: "0.1.0".to_owned(),
            version_code: 1000,
            runtime_epoch: uuid(1),
            host_generation: 7,
        },
        RuntimeHost::MagiskBackend,
    )
    .unwrap();
    let port = vertical.capability_port(uuid(2));
    let call = FilesystemCall::Read(FilesystemReadInput {
        source: ReadSource::Target {
            target: FileTarget {
                target_type: FileTargetType::ContentUri,
                value: "content://authority/document/1".to_owned(),
            },
        },
        offset: 0,
        max_bytes: 64,
        encoding: DataEncoding::Utf8,
    });
    let preflight = || RecordingPreflight {
        app: Preflight::Unknown,
        shizuku: Preflight::Unknown,
        calls: RefCell::new(Vec::new()),
    };

    let detached = runtime::CapabilityPort::current(&port).unwrap();
    assert_eq!(
        detached.context.app_execution_surface,
        CapabilityState::Unavailable
    );
    assert_eq!(
        detached.resolver_facts.app_framework,
        CapabilityState::Unavailable
    );
    assert_eq!(
        resolve_filesystem_executor(&detached, &preflight(), &call)
            .unwrap_err()
            .code,
        contract::ErrorCode::CapabilityUnavailable
    );

    vertical
        .set_app_execution_surface(CapabilityState::Available)
        .unwrap();
    let connected = runtime::CapabilityPort::current(&port).unwrap();
    assert_eq!(
        connected.resolver_facts.app_framework,
        CapabilityState::Available
    );
    assert_eq!(connected.resolver_facts.generations.app_framework, 7);
    assert_eq!(
        resolve_filesystem_executor(&connected, &preflight(), &call)
            .unwrap()
            .unwrap()
            .provider(),
        Provider::AppFramework
    );

    vertical
        .set_app_execution_surface(CapabilityState::Unavailable)
        .unwrap();
    assert_eq!(
        resolve_filesystem_executor(
            &runtime::CapabilityPort::current(&port).unwrap(),
            &preflight(),
            &call,
        )
        .unwrap_err()
        .code,
        contract::ErrorCode::CapabilityUnavailable
    );
}

#[cfg(unix)]
#[derive(Clone)]
struct RecordingBridge {
    results:
        Arc<std::sync::Mutex<Vec<Result<runtime::AndroidPrimitiveResult, domain::DomainError>>>>,
    calls: Arc<std::sync::Mutex<Vec<(String, Vec<u8>, UuidV4)>>>,
}

#[cfg(unix)]
impl runtime::AndroidExecutionDispatch for RecordingBridge {
    fn dispatch(
        &self,
        primitive: &str,
        payload: &[u8],
        execution: &AdmittedExecution,
    ) -> Result<runtime::AndroidPrimitiveResult, domain::DomainError> {
        self.calls.lock().unwrap().push((
            primitive.to_owned(),
            payload.to_vec(),
            execution.execution_id.clone(),
        ));
        let mut results = self.results.lock().unwrap();
        if results.is_empty() {
            return Err(domain::DomainError::new(
                contract::ErrorCode::InternalError,
                "bridge dispatch had no prepared result",
            ));
        }
        results.remove(0)
    }
}

#[cfg(unix)]
fn bridge_port(
    results: Vec<Result<runtime::AndroidPrimitiveResult, domain::DomainError>>,
) -> (
    runtime::AndroidFrameworkFilesystemPort<RecordingBridge>,
    RecordingBridge,
) {
    let recorder = RecordingBridge {
        results: Arc::new(std::sync::Mutex::new(results)),
        calls: Arc::new(std::sync::Mutex::new(Vec::new())),
    };
    (
        runtime::AndroidFrameworkFilesystemPort::new(recorder.clone()),
        recorder,
    )
}

#[cfg(unix)]
fn bridge_source(fixture: &Fixture) -> PathBuf {
    let source = fixture.root.join("bridged");
    fs::write(&source, b"abcdef").unwrap();
    source
}

#[cfg(unix)]
fn content_target() -> FileTarget {
    FileTarget {
        target_type: FileTargetType::ContentUri,
        value: "content://authority/document/1".to_owned(),
    }
}

#[cfg(unix)]
fn inspect_input(target: FileTarget) -> FilesystemInspectInput {
    FilesystemInspectInput {
        target,
        recursive: false,
        max_depth: 1,
        max_entries: 200,
    }
}

#[cfg(unix)]
fn bridge_inspection(target: FileTarget) -> contract::FilesystemInspectResult {
    contract::FilesystemInspectResult {
        target,
        target_type: FileType::File,
        size: Some(6),
        modified_at: None,
        entries: None,
        truncated: None,
    }
}

#[cfg(unix)]
#[test]
fn i8_fs_g04_bridge_port_verifies_what_the_authenticated_surface_returned() {
    let fixture = Fixture::new("bridge-port");
    let source = bridge_source(&fixture);
    let target = content_target();
    let execution = framework_execution(
        &capability(RuntimeHost::MagiskBackend),
        91,
        FilesystemCall::Inspect(inspect_input(target.clone())),
    );
    let inspection = bridge_inspection(target.clone());

    let (port, bridge) = bridge_port(vec![Ok(runtime::AndroidPrimitiveResult {
        payload: serde_json::to_vec(&inspection).unwrap(),
        descriptors: Vec::new(),
    })]);
    assert_eq!(
        port.inspect(&execution, inspect_input(target.clone()))
            .unwrap(),
        inspection
    );
    let calls = bridge.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "ContentInspect");
    assert_eq!(
        serde_json::from_slice::<FilesystemInspectInput>(&calls[0].1).unwrap(),
        inspect_input(target.clone())
    );
    assert_eq!(calls[0].2, execution.execution_id);
    drop(calls);

    let (port, _) = bridge_port(vec![Ok(runtime::AndroidPrimitiveResult {
        payload: serde_json::to_vec(&inspection).unwrap(),
        descriptors: vec![("content".to_owned(), fs::File::open(&source).unwrap())],
    })]);
    assert_eq!(
        port.inspect(&execution, inspect_input(target.clone()))
            .unwrap_err()
            .code,
        contract::ErrorCode::IoError
    );

    let (port, _) = bridge_port(vec![Ok(runtime::AndroidPrimitiveResult {
        payload: serde_json::to_vec(&bridge_inspection(FileTarget {
            target_type: FileTargetType::ContentUri,
            value: "content://authority/document/2".to_owned(),
        }))
        .unwrap(),
        descriptors: Vec::new(),
    })]);
    assert_eq!(
        port.inspect(&execution, inspect_input(target.clone()))
            .unwrap_err()
            .code,
        contract::ErrorCode::IoError
    );

    let (port, _) = bridge_port(vec![Ok(runtime::AndroidPrimitiveResult {
        payload: b"{}".to_vec(),
        descriptors: Vec::new(),
    })]);
    assert_eq!(
        port.inspect(&execution, inspect_input(target.clone()))
            .unwrap_err()
            .code,
        contract::ErrorCode::IoError
    );

    let (port, _) = bridge_port(vec![Err(domain::DomainError::new(
        contract::ErrorCode::StaleAuthority,
        "companion fence is stale",
    ))]);
    assert_eq!(
        port.inspect(&execution, inspect_input(target.clone()))
            .unwrap_err()
            .code,
        contract::ErrorCode::StaleAuthority
    );

    let (port, bridge) = bridge_port(vec![Ok(runtime::AndroidPrimitiveResult {
        payload: br#"{"total_size":6}"#.to_vec(),
        descriptors: vec![(
            runtime::CONTENT_READ_ROLE.to_owned(),
            fs::File::open(&source).unwrap(),
        )],
    })]);
    let opened = port.open_read(&execution, &target).unwrap();
    assert_eq!(opened.total_size, Some(6));
    let mut bytes = Vec::new();
    opened.file.take(6).read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"abcdef");
    let calls = bridge.calls.lock().unwrap();
    assert_eq!(calls[0].0, "ContentOpenRead");
    assert_eq!(
        serde_json::from_slice::<FileTarget>(&calls[0].1).unwrap(),
        target
    );
    drop(calls);

    let (port, _) = bridge_port(vec![Ok(runtime::AndroidPrimitiveResult {
        payload: br#"{"total_size":6}"#.to_vec(),
        descriptors: vec![("content_read".to_owned(), fs::File::open(&source).unwrap())],
    })]);
    assert_eq!(
        port.open_read(&execution, &target).unwrap_err().code,
        contract::ErrorCode::IoError
    );

    let (port, _) = bridge_port(vec![Ok(runtime::AndroidPrimitiveResult {
        payload: br#"{"total_size":6}"#.to_vec(),
        descriptors: Vec::new(),
    })]);
    assert_eq!(
        port.open_read(&execution, &target).unwrap_err().code,
        contract::ErrorCode::IoError
    );

    let (port, _) = bridge_port(vec![Ok(runtime::AndroidPrimitiveResult {
        payload: br#"{"total_size":6}"#.to_vec(),
        descriptors: vec![(
            runtime::CONTENT_READ_ROLE.to_owned(),
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(&source)
                .unwrap(),
        )],
    })]);
    assert_eq!(
        port.open_read(&execution, &target).unwrap_err().code,
        contract::ErrorCode::PermissionDenied
    );

    let (port, _) = bridge_port(vec![Ok(runtime::AndroidPrimitiveResult {
        payload: br#"{"unknown":1}"#.to_vec(),
        descriptors: vec![(
            runtime::CONTENT_READ_ROLE.to_owned(),
            fs::File::open(&source).unwrap(),
        )],
    })]);
    assert_eq!(
        port.open_read(&execution, &target).unwrap_err().code,
        contract::ErrorCode::IoError
    );
}

#[cfg(unix)]
#[tokio::test]
async fn i8_fs_g04_bridge_port_serves_an_admitted_content_read_through_the_rust_kernel() {
    let fixture = Fixture::new("bridge-kernel");
    let source = bridge_source(&fixture);
    let snapshot = capability(RuntimeHost::MagiskBackend);
    let (port, _) = bridge_port(vec![Ok(runtime::AndroidPrimitiveResult {
        payload: br#"{"total_size":6}"#.to_vec(),
        descriptors: vec![(
            runtime::CONTENT_READ_ROLE.to_owned(),
            fs::File::open(&source).unwrap(),
        )],
    })]);
    let surface = NativeFilesystemExecutionSurface::new(
        fixture.root.clone(),
        FakeArtifacts::default(),
        FakeCapabilities::new(snapshot.clone()),
        ProviderToken::MagiskNative,
    )
    .with_framework(port);
    let execution = framework_execution(
        &snapshot,
        93,
        FilesystemCall::Read(FilesystemReadInput {
            source: ReadSource::Target {
                target: content_target(),
            },
            offset: 2,
            max_bytes: 3,
            encoding: DataEncoding::Utf8,
        }),
    );

    let completion = surface.claim_and_start(execution).await.unwrap();
    let ExecutionOutcome::SynchronousCompleted { result, .. } = completion.outcome else {
        panic!("companion content read did not complete synchronously")
    };
    let result: contract::FilesystemReadResult = serde_json::from_value(result).unwrap();
    assert_eq!(result.data.as_deref(), Some("cde"));
    assert_eq!(result.returned_bytes, 3);
    assert_eq!(result.total_size, Some(6));
    assert!(result.truncated);
}
