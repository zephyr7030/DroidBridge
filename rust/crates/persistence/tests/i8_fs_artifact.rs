use contract::{ErrorCode, ExecutionClass, MotherTool, RunAs, RuntimeHost, TaskState, UuidV4};
use persistence::{
    ArtifactKind, ArtifactPublish, ArtifactStore, CanonicalState, RequestRecord, ReservationRecord,
    RuntimeArtifactPort, RuntimeLive, RuntimeOwner, StateStore, StoredRoute,
    StoredSynchronousExecution, StoredTask,
};
use runtime::{
    ArtifactPort, ExecutionPayload, ExecutorRecord, ProviderToken, RESERVE_FLOOR_BYTES,
    SynchronousExecutionState,
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

fn id(index: u64) -> UuidV4 {
    UuidV4::parse(format!("00000000-0000-4000-8000-{index:012x}")).unwrap()
}

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let number = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "droidbridge-i8-fs-artifact-{}-{number}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn owner() -> RuntimeOwner {
    RuntimeOwner {
        schema_version: 1,
        runtime_epoch: id(1),
        host: RuntimeHost::ApkRuntime,
        host_generation: 1,
    }
}

fn live() -> RuntimeLive {
    RuntimeLive {
        runtime_epoch: id(1),
        host: RuntimeHost::ApkRuntime,
        host_generation: 1,
        runtime_instance_id: id(2),
        boot_id: id(3),
        pid: 42,
        start_ticks: 99,
    }
}

fn command_task(execution_id: UuidV4) -> CanonicalState {
    let request_id = id(20);
    let task_id = id(21);
    let mut state = CanonicalState::default();
    state.request_records.push(RequestRecord {
        request_id: request_id.clone(),
        payload_sha256: "00".repeat(32),
        expires_at_ms: None,
        task_id: Some(task_id.clone()),
        synchronous_execution: None,
        mutation_result: None,
    });
    state.tasks.push(StoredTask {
        request_id: Some(request_id.clone()),
        task_id: task_id.clone(),
        execution_id: execution_id.clone(),
        state: TaskState::Created,
        cancel_requested: false,
        tool: MotherTool::Command,
        action: "run".to_owned(),
        created_at: "2026-09-13T00:00:00.000Z".to_owned(),
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
                runtime_instance_id: id(2),
            },
        }),
        route: Some(StoredRoute::Command { run_as: RunAs::App }),
        payload: Some(ExecutionPayload::OpaqueOperation("command.run".to_owned())),
        result: None,
        error: None,
        reserved_bytes: RESERVE_FLOOR_BYTES,
        automation_owner: None,
    });
    state.reservations.push(ReservationRecord {
        execution_id,
        request_id: Some(request_id),
        task_id: Some(task_id),
        reserved_bytes: RESERVE_FLOOR_BYTES,
    });
    state
}

#[test]
fn i8_cmd_artifact_publication_derives_the_canonical_execution_owner() {
    let directory = TestDirectory::new();
    let store = Arc::new(StateStore::new(directory.path().to_path_buf()));
    let execution_id = id(22);
    store
        .initialize(&owner(), &command_task(execution_id.clone()))
        .unwrap();
    let lease = Arc::new(store.acquire_lifetime(live()).unwrap());
    let artifacts = RuntimeArtifactPort::new(Arc::clone(&store), Arc::clone(&lease));

    let published = artifacts
        .publish_for_execution(&execution_id, "stdout", b"owned output")
        .unwrap();
    let canonical = store.load(&lease).unwrap();
    let record = canonical
        .artifact_manifest
        .iter()
        .find(|record| record.artifact_ref == published.artifact_ref)
        .unwrap();
    assert_eq!(record.kind, ArtifactKind::Stdout);
    assert_eq!(record.task_id.as_ref(), Some(&id(21)));
    assert_eq!(record.request_id.as_ref(), Some(&id(20)));

    assert_eq!(
        artifacts
            .publish_for_execution(&id(23), "stderr", b"stale output")
            .unwrap_err()
            .code,
        ErrorCode::StaleAuthority
    );
    assert_eq!(store.load(&lease).unwrap().artifact_manifest.len(), 1);
}

#[test]
fn i8_cmd_synchronous_artifact_publication_records_only_the_request_owner() {
    let directory = TestDirectory::new();
    let store = Arc::new(StateStore::new(directory.path().to_path_buf()));
    let request_id = id(30);
    let execution_id = id(31);
    let mut state = CanonicalState::default();
    state.request_records.push(RequestRecord {
        request_id: request_id.clone(),
        payload_sha256: "11".repeat(32),
        expires_at_ms: None,
        task_id: None,
        synchronous_execution: Some(StoredSynchronousExecution {
            execution_id: execution_id.clone(),
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
                    runtime_instance_id: id(2),
                },
            },
            route: StoredRoute::Command { run_as: RunAs::App },
            payload: ExecutionPayload::OpaqueOperation("command.run".to_owned()),
            result: None,
            error: None,
            reserved_bytes: RESERVE_FLOOR_BYTES,
            terminal_bytes: 0,
        }),
        mutation_result: None,
    });
    state.reservations.push(ReservationRecord {
        execution_id: execution_id.clone(),
        request_id: Some(request_id.clone()),
        task_id: None,
        reserved_bytes: RESERVE_FLOOR_BYTES,
    });
    store.initialize(&owner(), &state).unwrap();
    let lease = Arc::new(store.acquire_lifetime(live()).unwrap());
    let artifacts = RuntimeArtifactPort::new(Arc::clone(&store), Arc::clone(&lease));

    let published = artifacts
        .publish_for_execution(&execution_id, "stderr", b"owned error")
        .unwrap();
    let canonical = store.load(&lease).unwrap();
    let record = canonical
        .artifact_manifest
        .iter()
        .find(|record| record.artifact_ref == published.artifact_ref)
        .unwrap();
    assert_eq!(record.kind, ArtifactKind::Stderr);
    assert_eq!(record.task_id, None);
    assert_eq!(record.request_id.as_ref(), Some(&request_id));
}

#[test]
fn i8_fs_artifact_delete_is_successful_when_bytes_are_already_absent() {
    let directory = TestDirectory::new();
    let store = Arc::new(StateStore::new(directory.path().to_path_buf()));
    store
        .initialize(&owner(), &CanonicalState::default())
        .unwrap();
    let lease = Arc::new(store.acquire_lifetime(live()).unwrap());
    let artifacts = RuntimeArtifactPort::new(Arc::clone(&store), Arc::clone(&lease));

    let published = artifacts.publish(b"durable data").unwrap();
    let identity = published.artifact_ref.rsplit(':').next().unwrap();
    fs::remove_file(directory.path().join("artifacts/data").join(identity)).unwrap();

    artifacts.delete(&published.artifact_ref).unwrap();

    let canonical = store.load(&lease).unwrap();
    assert!(
        canonical
            .artifact_manifest
            .iter()
            .all(|record| record.artifact_ref != published.artifact_ref)
    );
}

#[test]
fn i8_fs_artifact_publish_evicts_expired_manifest_and_bytes_before_admission() {
    let directory = TestDirectory::new();
    let store = Arc::new(StateStore::new(directory.path().to_path_buf()));
    store
        .initialize(&owner(), &CanonicalState::default())
        .unwrap();
    let lease = Arc::new(store.acquire_lifetime(live()).unwrap());
    let artifact_store = ArtifactStore::new(Arc::clone(&store), Arc::clone(&lease));
    let expired = artifact_store
        .publish(ArtifactPublish {
            kind: ArtifactKind::Data,
            id: id(10),
            bytes: b"expired".to_vec(),
            created_at: "1970-01-01T00:00:00.000Z".to_owned(),
            created_at_ms: 0,
            expires_at: "1970-01-02T00:00:00.000Z".to_owned(),
            expires_at_ms: 86_400_000,
            mime: None,
            task_id: None,
            request_id: None,
        })
        .unwrap();
    store
        .compare_and_commit(&lease, 0, {
            let expired = expired.clone();
            move |state| {
                state.artifact_manifest.push(expired);
                Ok(())
            }
        })
        .unwrap();

    let artifacts = RuntimeArtifactPort::new(Arc::clone(&store), Arc::clone(&lease));
    let published = artifacts.publish(b"current").unwrap();
    let canonical = store.load(&lease).unwrap();
    assert_eq!(canonical.artifact_manifest.len(), 1);
    assert_eq!(
        canonical.artifact_manifest[0].artifact_ref,
        published.artifact_ref
    );
    assert!(
        !directory
            .path()
            .join("artifacts/data")
            .join(id(10).as_str())
            .exists()
    );
}
