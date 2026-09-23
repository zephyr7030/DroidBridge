use chrono::{Duration, TimeZone, Utc};
use contract::{ErrorCode, RuntimeHost, UuidV4};
use persistence::*;
use sha2::{Digest, Sha256};
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
    fn new(label: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let number = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "droidbridge-i4-{label}-{}-{number}",
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

fn live(instance: u64) -> RuntimeLive {
    RuntimeLive {
        runtime_epoch: id(1),
        host: RuntimeHost::ApkRuntime,
        host_generation: 1,
        runtime_instance_id: id(instance),
        boot_id: id(3),
        pid: 42,
        start_ticks: 99,
    }
}

fn initialized_store(label: &str) -> (TestDirectory, Arc<StateStore>, LifetimeLease) {
    let directory = TestDirectory::new(label);
    let store = Arc::new(StateStore::new(directory.path().to_path_buf()));
    store
        .initialize(&owner(), &CanonicalState::default())
        .unwrap();
    let lease = store.acquire_lifetime(live(2)).unwrap();
    (directory, store, lease)
}

fn overwrite_json(path: &Path, value: &impl serde::Serialize) {
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}

#[test]
fn i4_g01_crash_injection_keeps_atomic_truth_and_one_writer() {
    for (index, point) in [
        CrashPoint::BeforeTempCreate,
        CrashPoint::AfterTempFsync,
        CrashPoint::AfterRename,
    ]
    .into_iter()
    .enumerate()
    {
        let (_directory, store, lease) = initialized_store(&format!("crash-{index}"));
        let result = store.compare_and_commit_with_crash(
            &lease,
            0,
            |state| {
                state.request_records.push(RequestRecord {
                    request_id: id(10),
                    payload_sha256: "00".repeat(32),
                    expires_at_ms: None,
                    task_id: None,
                    synchronous_execution: None,
                    mutation_result: None,
                });
                Ok(())
            },
            |candidate| candidate == point,
        );
        assert_eq!(result.unwrap_err().code, ErrorCode::IoError);
        let recovered = store.load(&lease).unwrap();
        if point == CrashPoint::AfterRename {
            assert_eq!(recovered.store_revision, 1);
            assert_eq!(recovered.request_records.len(), 1);
        } else {
            assert_eq!(recovered.store_revision, 0);
            assert!(recovered.request_records.is_empty());
        }
    }

    let (_directory, store, lease) = initialized_store("writers");
    let lease = Arc::new(lease);
    let handles = (0_u64..2)
        .map(|index| {
            let store = store.clone();
            let lease = lease.clone();
            std::thread::spawn(move || {
                store.compare_and_commit(&lease, 0, |state| {
                    state.request_records.push(RequestRecord {
                        request_id: id(20 + index),
                        payload_sha256: "11".repeat(32),
                        expires_at_ms: None,
                        task_id: None,
                        synchronous_execution: None,
                        mutation_result: None,
                    });
                    Ok(())
                })
            })
        })
        .collect::<Vec<_>>();
    let results = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter_map(|result| result.as_ref().err())
            .filter(|error| error.code == ErrorCode::RevisionConflict)
            .count(),
        1
    );
    assert_eq!(store.load(&lease).unwrap().store_revision, 1);
}

#[test]
fn i4_g02_stale_generation_and_same_host_old_instance_cannot_mutate() {
    let (_directory, store, lease) = initialized_store("stale-generation");
    let mut changed_owner = owner();
    changed_owner.host_generation = 2;
    overwrite_json(
        &store_base(&store, &_directory).join("runtime-owner.json"),
        &changed_owner,
    );
    let error = store.compare_and_commit(&lease, 0, |_| Ok(())).unwrap_err();
    assert_eq!(error.code, ErrorCode::StaleAuthority);

    let (_directory, store, lease) = initialized_store("stale-instance");
    overwrite_json(
        &store_base(&store, &_directory).join("runtime-live.json"),
        &live(9),
    );
    let error = store.compare_and_commit(&lease, 0, |_| Ok(())).unwrap_err();
    assert_eq!(error.code, ErrorCode::StaleAuthority);
}

fn store_base<'a>(_: &StateStore, directory: &'a TestDirectory) -> &'a Path {
    directory.path()
}

#[derive(Clone, Copy)]
struct UnusedCapabilities;

impl runtime::CapabilityPort for UnusedCapabilities {
    fn current(&self) -> Result<runtime::CapabilitySnapshot, domain::DomainError> {
        Err(domain::DomainError::new(
            ErrorCode::InternalError,
            "capability port must not be used by queued Task cancellation",
        ))
    }
}

fn available() -> contract::Availability {
    contract::Availability {
        state: contract::CapabilityState::Available,
        reason: None,
    }
}

fn ready_capability() -> runtime::CapabilitySnapshot {
    let state = contract::CapabilityState::Available;
    runtime::CapabilitySnapshot {
        grants: contract::GrantFacts {
            android_local_network: available(),
            android_notifications: available(),
            android_notification_listener: available(),
            automation_exact_alarm: available(),
            visual_accessibility: available(),
            visual_media_projection_session: available(),
            shizuku_shell: available(),
            magisk_module: available(),
            magisk_root: available(),
            magisk_framework: available(),
            magisk_launch: available(),
            magisk_clipboard: available(),
            magisk_notifications: available(),
            magisk_wake_alarm: available(),
            execution_app_guard: available(),
            execution_shell_guard: available(),
            execution_root_guard: available(),
        },
        context: domain::CapabilityContext {
            sdk_int: 37,
            host: RuntimeHost::ApkRuntime,
            readiness: contract::RuntimeReadiness::Ready,
            app_execution_surface: state,
        },
        resolver_facts: domain::ResolverFacts {
            app_native: state,
            app_framework: state,
            shizuku: state,
            magisk_native: state,
            magisk_framework: state,
            magisk_launch: state,
            magisk_clipboard: state,
            magisk_notifications: state,
            accessibility: state,
            media_projection: state,
            notification_listener: state,
            generations: domain::ProviderGenerations {
                app_native: 2,
                app_framework: 2,
                shizuku: 2,
                magisk_native: 2,
                magisk_framework: 2,
                accessibility: 2,
                media_projection: 2,
                notification_listener: 2,
            },
        },
        fence: domain::AdmissionFence {
            runtime_epoch: id(1),
            host_generation: 1,
            runtime_instance_id: id(2),
        },
    }
}

#[tokio::test]
async fn i4_g10_synchronous_mutation_round_trips_and_replays_from_the_canonical_request_record() {
    use contract::{ExecutionClass, RunAs};
    use runtime::{
        ExecutionCompletion, ExecutionOutcome, ExecutionPayload, ProviderToken, RecoveryProof,
        RuntimeCore, SynchronousAdmission, SynchronousExecutionState,
        fakes::{FakeArtifacts, FakeCapabilities, FakeExecutions, FakeHostControl},
    };

    let (_directory, store, lease) = initialized_store("synchronous-round-trip");
    let lease = Arc::new(lease);
    let port = JsonPersistencePort::new(store.clone(), lease.clone());
    let capabilities = FakeCapabilities::new(ready_capability());
    let executions = FakeExecutions::default();
    executions.push(Ok(ExecutionCompletion {
        fence: ready_capability().fence,
        capability_generation: 2,
        outcome: ExecutionOutcome::SynchronousCompleted {
            result: serde_json::json!({"mutated": true}),
            encoded_bytes: 1,
        },
        cleanup_verified: true,
    }));
    let core = RuntimeCore::new(
        port.clone(),
        FakeArtifacts::default(),
        executions.clone(),
        capabilities.clone(),
        FakeHostControl::new(RecoveryProof::Clean).with_capabilities(capabilities.clone()),
    );
    let admission = SynchronousAdmission {
        request_id: id(850),
        payload_sha256: "85".repeat(32),
        execution_id: id(851),
        operation: "command.run".to_owned(),
        route: domain::ExecutorRequest::Command(RunAs::App),
        payload: ExecutionPayload::OpaqueOperation("command.run".to_owned()),
        settlement_bound_bytes: runtime::RESERVE_FLOOR_BYTES,
        now_ms: 1_788_825_600_000,
    };

    let expected = serde_json::json!({"mutated": true});
    assert_eq!(
        core.run_synchronous(
            admission.clone(),
            "2026-09-08T00:00:01.000Z".to_owned(),
            1_788_825_601_000,
        )
        .await
        .unwrap(),
        expected
    );
    let canonical = store.load(&lease).unwrap();
    assert!(canonical.reservations.is_empty());
    let request = canonical
        .request_records
        .iter()
        .find(|record| record.request_id == admission.request_id)
        .unwrap();
    assert!(request.task_id.is_none());
    let execution = request.synchronous_execution.as_ref().unwrap();
    assert_eq!(execution.execution_id, admission.execution_id);
    assert_eq!(execution.state, SynchronousExecutionState::Completed);
    assert_eq!(execution.result.as_ref(), Some(&expected));
    assert!(execution.error.is_none());
    assert_eq!(executions.started().len(), 1);

    let restarted_executions = FakeExecutions::default();
    let restarted = RuntimeCore::new(
        port,
        FakeArtifacts::default(),
        restarted_executions.clone(),
        capabilities.clone(),
        FakeHostControl::new(RecoveryProof::Clean).with_capabilities(capabilities),
    );
    assert_eq!(
        restarted
            .run_synchronous(
                admission,
                "2026-09-08T00:00:02.000Z".to_owned(),
                1_788_825_602_000,
            )
            .await
            .unwrap(),
        expected
    );
    assert!(restarted_executions.started().is_empty());
    assert_eq!(execution.executor.execution_class, ExecutionClass::App);
    assert_eq!(execution.executor.provider, ProviderToken::AppNative);
}

#[tokio::test]
async fn i4_g10_a_result_over_the_retention_bound_is_not_rewritten_with_every_later_commit() {
    use contract::{ErrorCode, RunAs};
    use runtime::{
        ExecutionCompletion, ExecutionOutcome, ExecutionPayload, RecoveryProof, RuntimeCore,
        SynchronousAdmission, SynchronousExecutionState,
        fakes::{FakeArtifacts, FakeCapabilities, FakeExecutions, FakeHostControl},
    };

    let (_directory, store, lease) = initialized_store("synchronous-unretained");
    let lease = Arc::new(lease);
    let port = JsonPersistencePort::new(store.clone(), lease.clone());
    let capabilities = FakeCapabilities::new(ready_capability());
    let executions = FakeExecutions::default();
    let large = serde_json::json!({"nodes": "n".repeat(12 * 1024)});
    executions.push(Ok(ExecutionCompletion {
        fence: ready_capability().fence,
        capability_generation: 2,
        outcome: ExecutionOutcome::SynchronousCompleted {
            result: large.clone(),
            encoded_bytes: 1,
        },
        cleanup_verified: true,
    }));
    let core = RuntimeCore::new(
        port.clone(),
        FakeArtifacts::default(),
        executions,
        capabilities.clone(),
        FakeHostControl::new(RecoveryProof::Clean).with_capabilities(capabilities.clone()),
    );
    let admission = SynchronousAdmission {
        request_id: id(860),
        payload_sha256: "86".repeat(32),
        execution_id: id(861),
        operation: "visual.observe".to_owned(),
        route: domain::ExecutorRequest::Command(RunAs::App),
        payload: ExecutionPayload::OpaqueOperation("visual.observe".to_owned()),
        settlement_bound_bytes: 32 * 1024,
        now_ms: 1_788_825_600_000,
    };

    assert_eq!(
        core.run_synchronous(
            admission.clone(),
            "2026-09-08T00:00:01.000Z".to_owned(),
            1_788_825_601_000
        )
        .await
        .unwrap(),
        large
    );
    // The canonical store the real decoder reads back keeps the record, not the result.
    let (canonical, encoded) = store.load_measured(&lease).unwrap();
    assert!(encoded < 8 * 1024, "store is {encoded} bytes");
    let execution = canonical
        .request_records
        .iter()
        .find(|record| record.request_id == admission.request_id)
        .and_then(|record| record.synchronous_execution.as_ref())
        .unwrap();
    assert_eq!(execution.state, SynchronousExecutionState::Completed);
    assert!(execution.result.is_none() && execution.error.is_none());

    let restarted = RuntimeCore::new(
        port,
        FakeArtifacts::default(),
        FakeExecutions::default(),
        capabilities.clone(),
        FakeHostControl::new(RecoveryProof::Clean).with_capabilities(capabilities),
    );
    assert_eq!(
        restarted
            .run_synchronous(
                admission,
                "2026-09-08T00:00:02.000Z".to_owned(),
                1_788_825_602_000
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::ResourceLimit
    );
}

#[tokio::test]
async fn i8_task_g02_cancel_round_trips_through_the_canonical_store() {
    use contract::{ExecutionClass, MotherTool, RunAs, TaskControlCall, TaskGetInput, TaskState};
    use runtime::{
        ExecutionPayload, ExecutorRecord, ProviderToken, RecoveryProof, RuntimeCore,
        fakes::{FakeArtifacts, FakeExecutions, FakeHostControl},
    };

    let (_directory, store, lease) = initialized_store("i8-task-cancel");
    let request_id = id(800);
    let task_id = id(801);
    let execution_id = id(802);
    store
        .compare_and_commit(&lease, 0, |state| {
            state.request_records.push(RequestRecord {
                request_id: request_id.clone(),
                payload_sha256: "88".repeat(32),
                expires_at_ms: None,
                task_id: Some(task_id.clone()),
                synchronous_execution: None,
                mutation_result: None,
            });
            state.tasks.push(StoredTask {
                request_id: Some(request_id.clone()),
                task_id: task_id.clone(),
                execution_id: execution_id.clone(),
                state: TaskState::Queued,
                cancel_requested: false,
                tool: MotherTool::Command,
                action: "run".to_owned(),
                created_at: "2026-09-08T00:00:00.000Z".to_owned(),
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
                reserved_bytes: runtime::RESERVE_FLOOR_BYTES,
                automation_owner: None,
            });
            state.reservations.push(ReservationRecord {
                execution_id: execution_id.clone(),
                request_id: Some(request_id.clone()),
                task_id: Some(task_id.clone()),
                reserved_bytes: runtime::RESERVE_FLOOR_BYTES,
            });
            Ok(())
        })
        .unwrap();

    let lease = Arc::new(lease);
    let port = JsonPersistencePort::new(store.clone(), lease.clone());
    let executions = FakeExecutions::default();
    let core = RuntimeCore::new(
        port.clone(),
        FakeArtifacts::default(),
        executions.clone(),
        UnusedCapabilities,
        FakeHostControl::new(RecoveryProof::Clean),
    );
    let response = core
        .handle_task_control(
            TaskControlCall::Cancel(TaskGetInput {
                task_id: task_id.clone(),
            }),
            "2026-09-08T00:00:01.000Z".to_owned(),
            1_788_825_601_000,
        )
        .await
        .unwrap();
    assert_eq!(response["state"], "cancelled");
    assert!(executions.cancelled().is_empty());

    let canonical = store.load(&lease).unwrap();
    assert_eq!(canonical.request_records.len(), 1);
    assert_eq!(
        canonical.request_records[0].task_id.as_ref(),
        Some(&task_id)
    );
    assert!(canonical.reservations.is_empty());
    assert_eq!(canonical.tasks.len(), 1);
    assert_eq!(canonical.tasks[0].state, TaskState::Cancelled);
    assert!(canonical.tasks[0].cancel_requested);
    assert_eq!(
        canonical.tasks[0].error.as_ref().unwrap().code,
        ErrorCode::Cancelled
    );
    assert_eq!(
        canonical.tasks[0].error.as_ref().unwrap().operation,
        "command.run"
    );

    let restarted = RuntimeCore::new(
        port,
        FakeArtifacts::default(),
        FakeExecutions::default(),
        UnusedCapabilities,
        FakeHostControl::new(RecoveryProof::Clean),
    );
    let durable = restarted
        .handle_task_control(
            TaskControlCall::Get(TaskGetInput { task_id }),
            "2026-09-08T00:00:02.000Z".to_owned(),
            1_788_825_602_000,
        )
        .await
        .unwrap();
    assert_eq!(durable, response);
}

#[tokio::test]
async fn i8_task_g04_real_store_full_retention_replays_then_expiry_releases_admission() {
    use contract::{ExecutionClass, MotherTool, PublicError, RunAs, TaskState};
    use runtime::{
        ExecutionPayload, ExecutorRecord, ProviderToken, RecoveryProof, RuntimeCore, TaskAdmission,
        TaskAdmissionResult,
        fakes::{FakeArtifacts, FakeCapabilities, FakeExecutions, FakeHostControl},
    };

    const BASE_MS: u64 = 1_788_825_600_000;
    const RETENTION_MS: u64 = 86_400_000;
    let (_directory, store, lease) = initialized_store("i8-task-retention");
    store
        .compare_and_commit(&lease, 0, |state| {
            for index in 1_u64..=500 {
                let request_id = id(10_000 + index * 3);
                let task_id = id(10_001 + index * 3);
                let execution_id = id(10_002 + index * 3);
                let queued = index == 500;
                state.request_records.push(RequestRecord {
                    request_id: request_id.clone(),
                    payload_sha256: format!("{index:064x}"),
                    expires_at_ms: (!queued).then_some(BASE_MS + RETENTION_MS),
                    task_id: Some(task_id.clone()),
                    synchronous_execution: None,
                    mutation_result: None,
                });
                state.tasks.push(StoredTask {
                    request_id: Some(request_id.clone()),
                    task_id: task_id.clone(),
                    execution_id: execution_id.clone(),
                    state: if queued {
                        TaskState::Queued
                    } else {
                        TaskState::Cancelled
                    },
                    cancel_requested: !queued,
                    tool: MotherTool::Command,
                    action: "run".to_owned(),
                    created_at: format!("2026-09-08T00:{:02}:{:02}.000Z", index / 60, index % 60),
                    started_at: None,
                    ended_at: (!queued).then(|| "2026-09-08T01:00:00.000Z".to_owned()),
                    waiting_reason: None,
                    executor: Some(ExecutorRecord {
                        host: RuntimeHost::ApkRuntime,
                        provider: ProviderToken::AppNative,
                        execution_class: ExecutionClass::App,
                        capability_generation: 2,
                        fence: contract::Fence {
                            runtime_epoch: id(1),
                            host_generation: 1,
                            runtime_instance_id: id(2),
                        },
                    }),
                    route: Some(StoredRoute::Command { run_as: RunAs::App }),
                    payload: Some(ExecutionPayload::OpaqueOperation("command.run".to_owned())),
                    result: None,
                    error: (!queued).then_some(PublicError {
                        code: ErrorCode::Cancelled,
                        operation: "command.run".to_owned(),
                        retryable: false,
                        message: None,
                        capability: None,
                        details: None,
                    }),
                    reserved_bytes: runtime::RESERVE_FLOOR_BYTES,
                    automation_owner: None,
                });
                if queued {
                    state.reservations.push(ReservationRecord {
                        execution_id,
                        request_id: Some(request_id),
                        task_id: Some(task_id),
                        reserved_bytes: runtime::RESERVE_FLOOR_BYTES,
                    });
                }
            }
            Ok(())
        })
        .unwrap();

    let lease = Arc::new(lease);
    let port = JsonPersistencePort::new(store.clone(), lease.clone());
    let capabilities = FakeCapabilities::new(ready_capability());
    let core = RuntimeCore::new(
        port,
        FakeArtifacts::default(),
        FakeExecutions::default(),
        capabilities.clone(),
        FakeHostControl::new(RecoveryProof::Clean).with_capabilities(capabilities),
    );
    let admission = |index: u64, now_ms: u64| TaskAdmission {
        request_id: id(10_000 + index * 3),
        payload_sha256: format!("{index:064x}"),
        task_id: id(10_001 + index * 3),
        execution_id: id(10_002 + index * 3),
        tool: MotherTool::Command,
        action: "run".to_owned(),
        route: domain::ExecutorRequest::Command(RunAs::App),
        payload: ExecutionPayload::OpaqueOperation("command.run".to_owned()),
        created_at: "2026-09-09T00:00:00.000Z".to_owned(),
        settlement_bound_bytes: runtime::RESERVE_FLOOR_BYTES,
        now_ms,
    };

    let mut replay_admission = admission(1, BASE_MS + 1);
    replay_admission.task_id = id(90_001);
    replay_admission.execution_id = id(88_001);
    let replay = core.admit_task(replay_admission).await.unwrap();
    let TaskAdmissionResult::Replay(replayed) = replay else {
        panic!("retained Task request was admitted again")
    };
    assert_eq!(replayed.task_id, id(10_004));
    assert_eq!(replayed.state, TaskState::Cancelled);
    assert!(replayed.cancel_requested);

    let queued_id = id(11_501);
    let cancelled_at_capacity = core
        .cancel_task(
            &queued_id,
            "2026-09-08T00:00:01.000Z".to_owned(),
            BASE_MS + 2,
        )
        .await
        .unwrap();
    assert_eq!(cancelled_at_capacity.state, TaskState::Cancelled);
    assert!(cancelled_at_capacity.cancel_requested);
    let full = store.load(&lease).unwrap();
    assert!(full.reservations.is_empty());
    assert_eq!(full.tasks.len(), 500);
    assert_eq!(
        core.admit_task(admission(501, BASE_MS + 1))
            .await
            .unwrap_err()
            .code,
        ErrorCode::ResourceLimit
    );

    let after_expiry = BASE_MS + RETENTION_MS + 3;
    let replacement = admission(501, after_expiry);
    assert!(matches!(
        core.admit_task(replacement.clone()).await.unwrap(),
        TaskAdmissionResult::Admitted(_)
    ));
    let cancelled = core
        .cancel_task(
            &replacement.task_id,
            "2026-09-09T00:00:01.000Z".to_owned(),
            after_expiry + 1,
        )
        .await
        .unwrap();
    assert_eq!(cancelled.state, TaskState::Cancelled);
    assert!(cancelled.cancel_requested);
    let canonical = store.load(&lease).unwrap();
    assert_eq!(canonical.tasks.len(), 500);
    assert!(
        canonical
            .tasks
            .iter()
            .all(|task| task.task_id != id(10_004))
    );
    assert!(canonical.tasks.iter().any(|task| {
        task.task_id == replacement.task_id
            && task.state == TaskState::Cancelled
            && task.cancel_requested
    }));
}

struct FakeProcessFacts(bool);

impl ProcessFacts for FakeProcessFacts {
    fn is_same_process(&self, _: u32, _: u64) -> Result<bool, domain::DomainError> {
        Ok(self.0)
    }
}

#[test]
fn i4_g04_dead_owner_takeover_retains_intent_and_rechecks_fresh_recovery_truth() {
    let directory = TestDirectory::new("dead-owner-two-phase");
    let store = StateStore::new(directory.path().to_path_buf());
    let source_owner = RuntimeOwner {
        schema_version: 1,
        runtime_epoch: id(1),
        host: RuntimeHost::MagiskBackend,
        host_generation: 8,
    };
    store
        .initialize(&source_owner, &CanonicalState::default())
        .unwrap();
    let source_live = RuntimeLive {
        runtime_epoch: id(1),
        host: RuntimeHost::MagiskBackend,
        host_generation: 8,
        runtime_instance_id: id(80),
        boot_id: id(3),
        pid: 42,
        start_ticks: 99,
    };
    drop(store.acquire_lifetime(source_live).unwrap());
    let intent = RuntimeTransitionIntent {
        schema_version: 1,
        transition_id: id(81),
        runtime_epoch: id(1),
        from_host: RuntimeHost::MagiskBackend,
        from_generation: 8,
        from_instance_id: id(80),
        target_host: RuntimeHost::ApkRuntime,
        target_generation: 9,
    };
    let target_live = RuntimeLive {
        runtime_epoch: id(1),
        host: RuntimeHost::ApkRuntime,
        host_generation: 9,
        runtime_instance_id: id(82),
        boot_id: id(3),
        pid: 43,
        start_ticks: 100,
    };
    let proof_directory = directory
        .path()
        .join("execution-guards")
        .join(id(3).as_str());
    fs::create_dir_all(&proof_directory).unwrap();
    let proof_identity = GuardIdentity {
        runtime_epoch: id(1),
        runtime_instance_id: id(80),
        execution_id: id(83),
        boot_id: id(3),
    };
    let mut proof = encode_guard_frame(&proof_identity).unwrap();
    proof.extend(
        encode_guard_frame(&GuardStarted {
            pid: 45,
            start_ticks: 102,
        })
        .unwrap(),
    );
    fs::write(
        proof_directory.join(format!("{}.proof", id(83).as_str())),
        &proof,
    )
    .unwrap();
    assert_eq!(
        store
            .begin_dead_owner_takeover(
                &intent,
                target_live.clone(),
                &id(3),
                &FakeProcessFacts(false),
            )
            .err()
            .unwrap()
            .code,
        ErrorCode::IoError
    );
    assert_eq!(store.read_owner().unwrap().host_generation, 8);
    assert!(!directory.path().join("runtime-transition.json").exists());

    proof.extend(
        encode_guard_frame(&GuardClean {
            shell_exit_code: Some(0),
            cause: GuardCleanCause::OwnerLost,
        })
        .unwrap(),
    );
    fs::write(
        proof_directory.join(format!("{}.proof", id(83).as_str())),
        &proof,
    )
    .unwrap();
    let pending = store
        .begin_dead_owner_takeover(
            &intent,
            target_live.clone(),
            &id(3),
            &FakeProcessFacts(false),
        )
        .unwrap();
    assert_eq!(pending.recovery_plan().records().len(), 1);
    assert!(directory.path().join("runtime-transition.json").exists());
    assert_eq!(store.read_owner().unwrap().host_generation, 9);
    fs::remove_file(proof_directory.join(format!("{}.proof", id(83).as_str()))).unwrap();

    let late_identity = GuardIdentity {
        execution_id: id(85),
        ..proof_identity
    };
    let mut late_proof = encode_guard_frame(&late_identity).unwrap();
    late_proof.extend(
        encode_guard_frame(&GuardStarted {
            pid: 46,
            start_ticks: 103,
        })
        .unwrap(),
    );
    late_proof.extend(
        encode_guard_frame(&GuardClean {
            shell_exit_code: Some(0),
            cause: GuardCleanCause::OwnerLost,
        })
        .unwrap(),
    );
    fs::write(
        proof_directory.join(format!("{}.proof", id(85).as_str())),
        late_proof,
    )
    .unwrap();
    assert_eq!(
        store
            .complete_dead_owner_takeover(pending, &FakeProcessFacts(false))
            .err()
            .unwrap()
            .code,
        ErrorCode::HostTransitionPending
    );
    assert!(directory.path().join("runtime-transition.json").exists());
    fs::remove_file(proof_directory.join(format!("{}.proof", id(85).as_str()))).unwrap();

    assert_eq!(
        store
            .resume_dead_owner_takeover(
                &intent,
                target_live.clone(),
                &id(3),
                &FakeProcessFacts(false),
            )
            .err()
            .unwrap()
            .code,
        ErrorCode::StaleAuthority
    );
    let resumed_live = RuntimeLive {
        runtime_instance_id: id(84),
        pid: 44,
        start_ticks: 101,
        ..target_live
    };
    let resumed = store
        .resume_dead_owner_takeover(
            &intent,
            resumed_live.clone(),
            &id(3),
            &FakeProcessFacts(false),
        )
        .unwrap();
    assert_eq!(store.read_owner().unwrap().host_generation, 9);
    assert_eq!(resumed.lease().live(), &resumed_live);
    let active = store
        .complete_dead_owner_takeover(resumed, &FakeProcessFacts(false))
        .unwrap();
    assert_eq!(active.live(), &resumed_live);
    assert!(!directory.path().join("runtime-transition.json").exists());
    store.validate_lease(&active).unwrap();
}

#[test]
fn i4_g05_artifact_and_recovery_ports_preserve_opaque_and_proven_truth() {
    let directory = TestDirectory::new("artifact");
    let state_store = Arc::new(StateStore::new(directory.path().to_path_buf()));
    state_store
        .initialize(&owner(), &CanonicalState::default())
        .unwrap();
    let lease = Arc::new(state_store.acquire_lifetime(live(2)).unwrap());
    let artifacts = ArtifactStore::new(state_store.clone(), lease.clone());
    let record = artifacts
        .publish(ArtifactPublish {
            kind: ArtifactKind::Image,
            id: id(30),
            bytes: b"png-bytes".to_vec(),
            created_at: "2026-01-01T00:00:00.000Z".to_owned(),
            created_at_ms: 1_767_225_600_000,
            expires_at: "2026-01-02T00:00:00.000Z".to_owned(),
            expires_at_ms: 1_767_312_000_000,
            mime: Some("image/png".to_owned()),
            task_id: None,
            request_id: None,
        })
        .unwrap();
    state_store
        .compare_and_commit(&lease, 0, |state| {
            state.artifact_manifest.push(record.clone());
            Ok(())
        })
        .unwrap();
    assert_eq!(record.artifact_ref, format!("dbref:image:{}", id(30)));
    assert_eq!(
        artifacts.metadata(&record.artifact_ref).unwrap(),
        ArtifactMetadata::from(&record)
    );
    assert_eq!(artifacts.open(&record.artifact_ref).unwrap(), b"png-bytes");
    assert!(artifacts.open("dbref:image:../runtime-state.json").is_err());

    let identity = GuardIdentity {
        runtime_epoch: id(1),
        runtime_instance_id: id(2),
        execution_id: id(31),
        boot_id: id(3),
    };
    let started = GuardStarted {
        pid: 71,
        start_ticks: 101,
    };
    let mut proof = encode_guard_frame(&identity).unwrap();
    proof.extend(encode_guard_frame(&started).unwrap());
    assert_eq!(
        classify_guard_proof(
            id(3).as_str(),
            &id(3),
            &identity,
            &proof,
            &FakeProcessFacts(true)
        )
        .unwrap(),
        GuardRecovery::Live {
            pid: 71,
            start_ticks: 101
        }
    );
    assert_eq!(
        classify_guard_proof(
            id(4).as_str(),
            &id(3),
            &identity,
            &[],
            &FakeProcessFacts(false)
        )
        .unwrap(),
        GuardRecovery::Clean { clean: None }
    );
    proof.push(0);
    assert_eq!(
        classify_guard_proof(
            id(3).as_str(),
            &id(3),
            &identity,
            &proof,
            &FakeProcessFacts(false)
        )
        .unwrap(),
        GuardRecovery::Unverified
    );

    let reset_directory = TestDirectory::new("reset");
    let reset_store = StateStore::new(reset_directory.path().to_path_buf());
    let mut populated = CanonicalState::default();
    populated.request_records.push(RequestRecord {
        request_id: id(32),
        payload_sha256: "44".repeat(32),
        expires_at_ms: None,
        task_id: None,
        synchronous_execution: None,
        mutation_result: None,
    });
    reset_store.initialize(&owner(), &populated).unwrap();
    fs::create_dir_all(reset_directory.path().join("artifacts").join("data")).unwrap();
    fs::write(
        reset_directory
            .path()
            .join("artifacts")
            .join("data")
            .join(id(33).as_str()),
        b"owned",
    )
    .unwrap();
    fs::create_dir_all(reset_directory.path().join("diagnostics")).unwrap();
    fs::write(
        reset_directory
            .path()
            .join("diagnostics")
            .join("runtime.json"),
        b"preserved",
    )
    .unwrap();
    let reset_intent = RuntimeResetIntent {
        schema_version: 1,
        reset_id: id(34),
        runtime_epoch: id(1),
        source_host_generation: 1,
        target_host: RuntimeHost::ApkRuntime,
        target_host_generation: 2,
    };
    let reset_lease = reset_store.acquire_lifetime(live(2)).unwrap();
    reset_store
        .record_reset_intent(&reset_lease, &reset_intent, true, true)
        .unwrap();
    drop(reset_lease);
    let reset_owner = reset_store.recover_confirmed_reset(true).unwrap();
    assert_eq!(reset_owner.host_generation, 2);
    assert_eq!(
        read_json::<CanonicalState>(&reset_directory.path().join("runtime-state.json")).unwrap(),
        CanonicalState::default()
    );
    assert!(!reset_directory.path().join("artifacts").exists());
    assert_eq!(
        fs::read(
            reset_directory
                .path()
                .join("diagnostics")
                .join("runtime.json")
        )
        .unwrap(),
        b"preserved"
    );

    let transition_directory = TestDirectory::new("transition");
    let transition_store = StateStore::new(transition_directory.path().to_path_buf());
    transition_store
        .initialize(&owner(), &CanonicalState::default())
        .unwrap();
    let source_lease = transition_store.acquire_lifetime(live(2)).unwrap();
    let transition = RuntimeTransitionIntent {
        schema_version: 1,
        transition_id: id(35),
        runtime_epoch: id(1),
        from_host: RuntimeHost::ApkRuntime,
        from_generation: 1,
        from_instance_id: id(2),
        target_host: RuntimeHost::MagiskBackend,
        target_generation: 2,
    };
    transition_store
        .record_transition_intent(&source_lease, &transition)
        .unwrap();
    drop(source_lease);
    assert_eq!(
        transition_store
            .acquire_lifetime(live(2))
            .err()
            .unwrap()
            .code,
        ErrorCode::HostTransitionPending
    );
    let target_owner = transition_store
        .commit_owner_transition(&transition, &id(3), &FakeProcessFacts(false))
        .unwrap();
    assert_eq!(target_owner.host, RuntimeHost::MagiskBackend);
    assert_eq!(target_owner.host_generation, 2);
    let target_live = RuntimeLive {
        runtime_epoch: id(1),
        host: RuntimeHost::MagiskBackend,
        host_generation: 2,
        runtime_instance_id: id(36),
        boot_id: id(3),
        pid: 43,
        start_ticks: 100,
    };
    transition_store.acquire_lifetime(target_live).unwrap();
}

#[test]
fn i4_g05_artifact_publication_is_exclusive_and_never_replaces_published_bytes() {
    let (directory, state_store, lease) = initialized_store("artifact-exclusive");
    let lease = Arc::new(lease);
    let artifacts = ArtifactStore::new(state_store.clone(), lease.clone());
    let publish = |bytes: &[u8]| ArtifactPublish {
        kind: ArtifactKind::Data,
        id: id(40),
        bytes: bytes.to_vec(),
        created_at: "2026-01-01T00:00:00.000Z".to_owned(),
        created_at_ms: 1_767_225_600_000,
        expires_at: "2026-01-02T00:00:00.000Z".to_owned(),
        expires_at_ms: 1_767_312_000_000,
        mime: None,
        task_id: None,
        request_id: None,
    };
    let record = artifacts.publish(publish(b"original")).unwrap();
    state_store
        .compare_and_commit(&lease, 0, {
            let record = record.clone();
            move |state| {
                state.artifact_manifest.push(record);
                Ok(())
            }
        })
        .unwrap();
    let destination = directory
        .path()
        .join("artifacts")
        .join("data")
        .join(id(40).as_str());

    assert_eq!(
        artifacts.publish(publish(b"replacement")).unwrap_err().code,
        ErrorCode::AlreadyExists
    );
    assert_eq!(fs::read(&destination).unwrap(), b"original");
    assert_eq!(artifacts.open(&record.artifact_ref).unwrap(), b"original");
    assert!(
        !directory
            .path()
            .join("artifacts")
            .join("data")
            .join(format!(".{}.tmp", id(40).as_str()))
            .exists()
    );
}

#[test]
fn i4_g06_realistic_capacity_fixtures_are_exact_and_instrumented() {
    for target in [FIXTURE_512_KIB, FIXTURE_2_MIB, FIXTURE_8_MIB] {
        let first = realistic_store_fixture(target);
        let second = realistic_store_fixture(target);
        assert_eq!(first.len(), target);
        assert_eq!(Sha256::digest(&first), Sha256::digest(&second));
        let decoded = decode_canonical_state(&first).unwrap();
        assert!(!decoded.request_records.is_empty());
        assert!(!decoded.tasks.is_empty());
        assert!(!decoded.automations.is_empty());
        assert!(!decoded.automation_executions.is_empty());
        assert!(!decoded.artifact_manifest.is_empty());
    }
    let (_directory, store, lease) = initialized_store("instrumentation");
    let timing = store.compare_and_commit(&lease, 0, |_| Ok(())).unwrap();
    let phases = [
        timing.lock_wait_ns,
        timing.parse_ns,
        timing.domain_mutation_ns,
        timing.serialize_ns,
        timing.temp_write_fsync_ns,
        timing.rename_directory_fsync_ns,
    ];
    assert_eq!(phases.len(), 6);
    assert!(timing.total_ns >= *phases.iter().max().unwrap());
}

#[test]
fn i4_g07_full_logical_capacity_preserves_terminal_and_recovery_commits() {
    for label in ["terminal-capacity", "recovery-capacity"] {
        let directory = TestDirectory::new(label);
        let store = StateStore::new(directory.path().to_path_buf());
        let mut state = CanonicalState::default();
        state.request_records.push(RequestRecord {
            request_id: id(41),
            payload_sha256: "11".repeat(32),
            expires_at_ms: None,
            task_id: None,
            synchronous_execution: Some(StoredSynchronousExecution {
                execution_id: id(40),
                operation: "command.run".to_owned(),
                state: runtime::SynchronousExecutionState::Running,
                ended_at: None,
                executor: runtime::ExecutorRecord {
                    host: RuntimeHost::ApkRuntime,
                    provider: runtime::ProviderToken::AppNative,
                    execution_class: contract::ExecutionClass::App,
                    capability_generation: 1,
                    fence: contract::Fence {
                        runtime_epoch: id(1),
                        host_generation: 1,
                        runtime_instance_id: id(2),
                    },
                },
                route: StoredRoute::Command {
                    run_as: contract::RunAs::App,
                },
                payload: runtime::ExecutionPayload::OpaqueOperation("command.run".to_owned()),
                result: None,
                error: None,
                reserved_bytes: 0,
                terminal_bytes: 0,
            }),
            mutation_result: None,
        });
        state.reservations.push(ReservationRecord {
            execution_id: id(40),
            request_id: Some(id(41)),
            task_id: None,
            reserved_bytes: 0,
        });
        for _ in 0..4 {
            let encoded = serde_json::to_vec(&state).unwrap().len();
            let reservation = (STORE_LIMIT_BYTES - encoded) as u64;
            state.reservations[0].reserved_bytes = reservation;
            state.request_records[0]
                .synchronous_execution
                .as_mut()
                .unwrap()
                .reserved_bytes = reservation;
        }
        let encoded = serde_json::to_vec(&state).unwrap().len();
        assert_eq!(
            encoded + state.reservations[0].reserved_bytes as usize,
            STORE_LIMIT_BYTES
        );
        store.initialize(&owner(), &state).unwrap();
        let lease = store.acquire_lifetime(live(2)).unwrap();
        let ordinary = store.compare_and_commit(&lease, 0, |candidate| {
            candidate.request_records.push(RequestRecord {
                request_id: id(43),
                payload_sha256: "22".repeat(32),
                expires_at_ms: None,
                task_id: None,
                synchronous_execution: None,
                mutation_result: None,
            });
            Ok(())
        });
        assert_eq!(ordinary.unwrap_err().code, ErrorCode::ResourceLimit);
        store
            .compare_and_commit(&lease, 0, |candidate| {
                candidate.reservations.clear();
                let request = &mut candidate.request_records[0];
                request.expires_at_ms = Some(86_400_000);
                let execution = request.synchronous_execution.as_mut().unwrap();
                execution.state = if label == "terminal-capacity" {
                    runtime::SynchronousExecutionState::Failed
                } else {
                    runtime::SynchronousExecutionState::Interrupted
                };
                execution.ended_at = Some("2026-09-08T00:00:01.000Z".to_owned());
                execution.error = Some(contract::PublicError {
                    code: ErrorCode::IoError,
                    operation: "command.run".to_owned(),
                    retryable: false,
                    message: None,
                    capability: None,
                    details: None,
                });
                execution.terminal_bytes = 256;
                Ok(())
            })
            .unwrap();
        let settled = store.load(&lease).unwrap();
        assert_eq!(settled.store_revision, 1);
        assert!(settled.reservations.is_empty());
    }
}

#[test]
fn i4_g08_same_host_old_instance_is_rejected_by_live_owner_fencing() {
    let (directory, store, lease) = initialized_store("capacity-old-instance");
    overwrite_json(
        &store_base(&store, &directory).join("runtime-live.json"),
        &live(45),
    );
    assert_eq!(
        store
            .compare_and_commit(&lease, 0, |_| Ok(()))
            .unwrap_err()
            .code,
        ErrorCode::StaleAuthority
    );
}

#[test]
fn i4_g09_fault_files_are_bounded_coalesced_and_never_recovery_authority() {
    let directory = TestDirectory::new("faults");
    FaultFileStore::initialize_all_by_apk(directory.path()).unwrap();
    let store = FaultFileStore::new(directory.path(), FaultRole::Runtime);
    let base_time = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    let make = |index: u64, at: chrono::DateTime<Utc>| FaultRecord {
        record_id: id(100 + index),
        at: at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        component: "runtime".to_owned(),
        code: format!("IO_{index}"),
        phase: "commit".to_owned(),
        product_version: "1.0.0".to_owned(),
        boot_id: id(3),
        runtime_instance_id: Some(id(2)),
        execution_id: None,
        exit_code: None,
        signal: None,
        repeat_count: 1,
    };
    let now = u64::try_from(base_time.timestamp_millis()).unwrap();
    store.append(make(0, base_time), now).unwrap();
    store
        .append(make(0, base_time + Duration::seconds(30)), now + 30_000)
        .unwrap();
    assert_eq!(store.read().unwrap().records[0].repeat_count, 2);
    for index in 1_u64..=70 {
        let at = base_time + Duration::seconds(i64::try_from(index * 61).unwrap());
        store
            .append(
                make(index, at),
                u64::try_from(at.timestamp_millis()).unwrap(),
            )
            .unwrap();
    }
    let bounded = store.read().unwrap();
    assert_eq!(bounded.records.len(), FAULT_RECORD_LIMIT);
    let path = directory.path().join("diagnostics").join("runtime.json");
    assert!(fs::metadata(&path).unwrap().len() <= FAULT_FILE_LIMIT_BYTES as u64);

    fs::write(&path, b"corrupt").unwrap();
    let before = fs::read(&path).unwrap();
    assert_eq!(store.read().unwrap_err().code, ErrorCode::IoError);
    assert_eq!(
        store.append(make(99, base_time), now).unwrap_err().code,
        ErrorCode::IoError
    );
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[derive(Clone, Default)]
struct CleanupQuarantine {
    unavailable: Arc<std::sync::atomic::AtomicBool>,
}

impl runtime::HostControlPort for CleanupQuarantine {
    fn cleanup_unverified(
        &self,
        _: &domain::AdmissionFence,
        _: &UuidV4,
    ) -> Result<(), domain::DomainError> {
        self.unavailable
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    fn prepare(&self) -> Result<(), domain::DomainError> {
        Ok(())
    }

    fn activate(&self, _: &domain::AdmissionFence) -> Result<(), domain::DomainError> {
        Ok(())
    }

    fn recover(&self, _: &UuidV4) -> Result<runtime::RecoveryProof, domain::DomainError> {
        self.unavailable
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(runtime::RecoveryProof::CleanupUnverified)
    }
}

#[test]
fn i4_g10_json_round_trip_preserves_terminal_cancel_and_not_host_quarantine() {
    use contract::{ExecutionClass, MotherTool, PublicError, RunAs, TaskState};
    use runtime::{
        ExecutionPayload, ExecutorRecord, HostControlPort, PersistencePort, ProviderToken,
    };

    let directory = TestDirectory::new("terminal-cancel-history");
    let store = Arc::new(StateStore::new(directory.path().to_path_buf()));
    let task_id = id(810);
    let execution_id = id(811);
    let mut state = CanonicalState::default();
    state.tasks.push(StoredTask {
        request_id: Some(id(812)),
        task_id: task_id.clone(),
        execution_id,
        state: TaskState::Failed,
        cancel_requested: true,
        tool: MotherTool::Command,
        action: "run".to_owned(),
        created_at: "2026-09-08T00:00:00.000Z".to_owned(),
        started_at: Some("2026-09-08T00:00:01.000Z".to_owned()),
        ended_at: Some("2026-09-08T00:00:02.000Z".to_owned()),
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
        error: Some(PublicError {
            code: ErrorCode::IoError,
            operation: "command.run".to_owned(),
            retryable: false,
            message: None,
            capability: None,
            details: None,
        }),
        reserved_bytes: runtime::RESERVE_FLOOR_BYTES,
        automation_owner: None,
    });
    store.initialize(&owner(), &state).unwrap();
    let lease = Arc::new(store.acquire_lifetime(live(2)).unwrap());
    let port = JsonPersistencePort::new(store, lease);

    let quarantine = CleanupQuarantine::default();
    assert_eq!(
        quarantine.recover(&id(700)).unwrap(),
        runtime::RecoveryProof::CleanupUnverified
    );
    let loaded = port.load().unwrap();
    assert!(loaded.task(&task_id).unwrap().lifecycle.cancel_requested());
    assert!(
        quarantine
            .unavailable
            .load(std::sync::atomic::Ordering::SeqCst)
    );

    let encoded = serde_json::to_string(&CanonicalState::try_from(&loaded).unwrap()).unwrap();
    assert!(!encoded.contains("cleanup_uncertain"));
}

fn recovery_task(task_id: u64, execution_id: u64, instance_id: u64) -> StoredTask {
    use contract::{ExecutionClass, MotherTool, RunAs, TaskState};
    use runtime::{ExecutionPayload, ExecutorRecord, ProviderToken};

    StoredTask {
        request_id: Some(id(task_id + 100)),
        task_id: id(task_id),
        execution_id: id(execution_id),
        state: TaskState::Created,
        cancel_requested: false,
        tool: MotherTool::Command,
        action: "run".to_owned(),
        created_at: "2026-09-08T00:00:00.000Z".to_owned(),
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

#[test]
fn i4_g10_canonical_store_rejects_completed_task_without_its_required_result() {
    let directory = TestDirectory::new("missing-task-result");
    let store = StateStore::new(directory.path().to_path_buf());
    let mut task = recovery_task(824, 924, 724);
    task.state = contract::TaskState::Completed;
    task.started_at = Some("2026-09-08T00:00:01.000Z".to_owned());
    task.ended_at = Some("2026-09-08T00:00:02.000Z".to_owned());
    let mut state = CanonicalState::default();
    state.tasks.push(task);

    assert_eq!(
        store.initialize(&owner(), &state).unwrap_err().code,
        ErrorCode::IoError
    );
}

#[test]
fn i4_g10_canonical_store_rejects_shared_task_and_synchronous_execution_identity() {
    let directory = TestDirectory::new("duplicate-execution-identity");
    let store = StateStore::new(directory.path().to_path_buf());
    let mut task = recovery_task(825, 925, 725);
    task.state = contract::TaskState::Interrupted;
    task.ended_at = Some("2026-09-08T00:00:02.000Z".to_owned());
    task.error = Some(contract::PublicError {
        code: ErrorCode::IoError,
        operation: "command.run".to_owned(),
        retryable: false,
        message: None,
        capability: None,
        details: None,
    });
    let mut request = recovery_synchronous(826, 925, 726);
    request.expires_at_ms = Some(1_788_912_000_000);
    let synchronous = request.synchronous_execution.as_mut().unwrap();
    synchronous.state = runtime::SynchronousExecutionState::Interrupted;
    synchronous.ended_at = Some("2026-09-08T00:00:02.000Z".to_owned());
    synchronous.error = Some(contract::PublicError {
        code: ErrorCode::IoError,
        operation: "command.run".to_owned(),
        retryable: false,
        message: None,
        capability: None,
        details: None,
    });
    synchronous.terminal_bytes = 256;
    let mut state = CanonicalState::default();
    state.tasks.push(task);
    state.request_records.push(request);

    assert_eq!(
        store.initialize(&owner(), &state).unwrap_err().code,
        ErrorCode::IoError
    );
}

fn recovery_synchronous(request_id: u64, execution_id: u64, instance_id: u64) -> RequestRecord {
    RequestRecord {
        request_id: id(request_id),
        payload_sha256: "99".repeat(32),
        expires_at_ms: None,
        task_id: None,
        synchronous_execution: Some(StoredSynchronousExecution {
            execution_id: id(execution_id),
            operation: "command.run".to_owned(),
            state: runtime::SynchronousExecutionState::Running,
            ended_at: None,
            executor: runtime::ExecutorRecord {
                host: RuntimeHost::ApkRuntime,
                provider: runtime::ProviderToken::AppNative,
                execution_class: contract::ExecutionClass::App,
                capability_generation: 1,
                fence: contract::Fence {
                    runtime_epoch: id(1),
                    host_generation: 1,
                    runtime_instance_id: id(instance_id),
                },
            },
            route: StoredRoute::Command {
                run_as: contract::RunAs::App,
            },
            payload: runtime::ExecutionPayload::OpaqueOperation("command.run".to_owned()),
            result: None,
            error: None,
            reserved_bytes: runtime::RESERVE_FLOOR_BYTES,
            terminal_bytes: 0,
        }),
        mutation_result: None,
    }
}

#[tokio::test]
async fn i4_g03_cleanup_unverified_cannot_settle_a_prior_execution() {
    use runtime::{
        CapabilityPort, RecoveryProof, RuntimeCore, SynchronousExecutionState,
        fakes::{FakeArtifacts, FakeCapabilities, FakeExecutions, FakeHostControl},
    };

    let directory = TestDirectory::new("cleanup-unverified-recovery");
    let store = Arc::new(StateStore::new(directory.path().to_path_buf()));
    let mut state = CanonicalState::default();
    state
        .request_records
        .push(recovery_synchronous(850, 851, 7));
    state.reservations.push(ReservationRecord {
        execution_id: id(851),
        request_id: Some(id(850)),
        task_id: None,
        reserved_bytes: runtime::RESERVE_FLOOR_BYTES,
    });
    store.initialize(&owner(), &state).unwrap();
    let lease = Arc::new(store.acquire_lifetime(live(2)).unwrap());
    let capabilities = FakeCapabilities::new(ready_capability());
    let core = RuntimeCore::new(
        JsonPersistencePort::new(store.clone(), lease.clone()),
        FakeArtifacts::default(),
        FakeExecutions::default(),
        capabilities.clone(),
        FakeHostControl::new(RecoveryProof::CleanupUnverified)
            .with_capabilities(capabilities.clone()),
    );

    assert_eq!(
        core.recover_old_instance(
            &id(7),
            "2026-09-08T00:00:01.000Z".to_owned(),
            1_788_825_601_000,
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::IoError
    );
    let unchanged = store.load(&lease).unwrap();
    assert_eq!(
        unchanged.request_records[0]
            .synchronous_execution
            .as_ref()
            .unwrap()
            .state,
        SynchronousExecutionState::Running
    );
    assert_eq!(unchanged.reservations.len(), 1);
    assert_eq!(
        capabilities.current().unwrap().context.readiness,
        contract::RuntimeReadiness::Unavailable
    );
}

#[tokio::test]
async fn i4_g10_synchronous_recovery_is_durable_and_never_replays_the_external_effect() {
    use contract::RunAs;
    use runtime::{
        ExecutionPayload, RecoveryProof, RuntimeCore, SynchronousAdmission,
        SynchronousExecutionState,
        fakes::{FakeArtifacts, FakeExecutions, FakeHostControl},
    };

    let directory = TestDirectory::new("synchronous-recovery");
    let store = Arc::new(StateStore::new(directory.path().to_path_buf()));
    let request = recovery_synchronous(860, 861, 7);
    let mut state = CanonicalState::default();
    state.request_records.push(request);
    state.reservations.push(ReservationRecord {
        execution_id: id(861),
        request_id: Some(id(860)),
        task_id: None,
        reserved_bytes: runtime::RESERVE_FLOOR_BYTES,
    });
    store.initialize(&owner(), &state).unwrap();
    let lease = Arc::new(store.acquire_lifetime(live(2)).unwrap());
    let port = JsonPersistencePort::new(store.clone(), lease.clone());
    let executions = FakeExecutions::default();
    let core = RuntimeCore::new(
        port.clone(),
        FakeArtifacts::default(),
        executions.clone(),
        UnusedCapabilities,
        FakeHostControl::new(RecoveryProof::Clean),
    );

    assert_eq!(
        core.recover_old_instance(
            &id(7),
            "2026-09-08T00:00:01.000Z".to_owned(),
            1_788_825_601_000,
        )
        .await
        .unwrap(),
        1
    );
    let recovered = store.load(&lease).unwrap();
    assert!(recovered.reservations.is_empty());
    let execution = recovered.request_records[0]
        .synchronous_execution
        .as_ref()
        .unwrap();
    assert_eq!(execution.state, SynchronousExecutionState::Interrupted);
    assert_eq!(execution.error.as_ref().unwrap().code, ErrorCode::IoError);

    let replay = SynchronousAdmission {
        request_id: id(860),
        payload_sha256: "99".repeat(32),
        execution_id: id(861),
        operation: "command.run".to_owned(),
        route: domain::ExecutorRequest::Command(RunAs::App),
        payload: ExecutionPayload::OpaqueOperation("command.run".to_owned()),
        settlement_bound_bytes: runtime::RESERVE_FLOOR_BYTES,
        now_ms: 1_788_825_602_000,
    };
    assert_eq!(
        core.run_synchronous(
            replay,
            "2026-09-08T00:00:02.000Z".to_owned(),
            1_788_825_602_000,
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::IoError
    );
    assert!(executions.started().is_empty());
}

fn write_empty_guard(directory: &TestDirectory, boot_id: &UuidV4, execution_id: &UuidV4) {
    let proof_directory = directory
        .path()
        .join("execution-guards")
        .join(boot_id.as_str());
    fs::create_dir_all(&proof_directory).unwrap();
    fs::write(
        proof_directory.join(format!("{}.proof", execution_id.as_str())),
        [],
    )
    .unwrap();
}

#[test]
fn i4_g03_repeated_recovery_traverses_every_prior_instance_and_orphan_proof() {
    let directory = TestDirectory::new("repeated-recovery");
    let current_boot_id = id(3);
    let old_boot_id = id(4);
    let current_instance_id = id(899);
    let first_task_id = id(820);
    let second_task_id = id(821);
    let first_execution_id = id(920);
    let second_execution_id = id(921);
    let orphan_execution_id = id(922);
    let synchronous_execution_id = id(923);
    let mut state = CanonicalState::default();
    state.tasks.push(recovery_task(820, 920, 720));
    state.tasks.push(recovery_task(821, 921, 721));
    state
        .request_records
        .push(recovery_synchronous(823, 923, 722));
    write_empty_guard(&directory, &old_boot_id, &first_execution_id);
    write_empty_guard(&directory, &current_boot_id, &second_execution_id);
    write_empty_guard(&directory, &old_boot_id, &orphan_execution_id);
    write_empty_guard(&directory, &old_boot_id, &synchronous_execution_id);
    let proofs = GuardProofDirectory::new(directory.path());

    let first = build_guard_recovery_plan(
        &state,
        &current_instance_id,
        &current_boot_id,
        &proofs,
        &FakeProcessFacts(false),
    )
    .unwrap();
    assert_eq!(first.records().len(), 4);
    assert!(first.records().iter().any(|record| {
        record.task_id.as_ref() == Some(&first_task_id)
            && record.execution_id == first_execution_id
            && matches!(record.recovery, GuardRecovery::Clean { clean: None })
    }));
    assert!(first.records().iter().any(|record| {
        record.task_id.as_ref() == Some(&second_task_id)
            && record.execution_id == second_execution_id
            && record.recovery == GuardRecovery::Unverified
    }));
    assert!(first.records().iter().any(|record| {
        record.task_id.is_none()
            && record.execution_id == orphan_execution_id
            && matches!(record.recovery, GuardRecovery::Clean { clean: None })
    }));
    assert!(first.records().iter().any(|record| {
        record.task_id.is_none()
            && record.execution_id == synchronous_execution_id
            && record.runtime_instance_id.as_ref() == Some(&id(722))
            && matches!(record.recovery, GuardRecovery::Clean { clean: None })
    }));

    state.tasks[0].state = contract::TaskState::Interrupted;
    let resumed = build_guard_recovery_plan(
        &state,
        &current_instance_id,
        &current_boot_id,
        &proofs,
        &FakeProcessFacts(false),
    )
    .unwrap();
    assert_eq!(resumed.records().len(), 4);
    assert!(resumed.records().iter().any(|record| {
        record.task_id.as_ref() == Some(&first_task_id) && record.execution_id == first_execution_id
    }));
    assert!(resumed.records().iter().any(|record| {
        record.task_id.as_ref() == Some(&second_task_id)
            && record.recovery == GuardRecovery::Unverified
    }));
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct GuardRecoveryFixture {
    schema_version: u32,
    vectors: Vec<GuardRecoveryVector>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct GuardRecoveryVector {
    name: String,
    records: Vec<GuardRecoveryStateVector>,
    proofs: Vec<GuardRecoveryProofVector>,
    process_live: bool,
    finalization_succeeds: bool,
    expected: GuardRecoveryExpected,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct GuardRecoveryStateVector {
    kind: String,
    execution: u64,
    instance: u64,
    state: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct GuardRecoveryProofVector {
    execution: u64,
    instance: u64,
    boot: String,
    format: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct GuardRecoveryExpected {
    result: String,
    recoveries: Vec<String>,
    cleanup_requirements: Vec<String>,
    settlements: Vec<String>,
    finalizations: Vec<String>,
    prior_instances: Vec<u64>,
    cleanup_verified: bool,
}

#[derive(Clone)]
struct VectorProofs(Vec<GuardProofRecord>);

impl GuardProofReader for VectorProofs {
    fn read_proof(
        &self,
        boot_id: &UuidV4,
        execution_id: &contract::ExecutionId,
    ) -> Result<Option<Vec<u8>>, domain::DomainError> {
        Ok(self
            .0
            .iter()
            .find(|record| {
                &record.containing_boot_id == boot_id && &record.execution_id == execution_id
            })
            .map(|record| record.bytes.clone()))
    }

    fn list_proofs(&self) -> Result<Vec<GuardProofRecord>, domain::DomainError> {
        Ok(self.0.clone())
    }
}

fn guard_recovery_fixture() -> GuardRecoveryFixture {
    serde_json::from_str(include_str!(
        "../../../fixtures/guard-recovery-vectors.json"
    ))
    .unwrap()
}

fn vector_state(vector: &GuardRecoveryVector) -> CanonicalState {
    let mut state = CanonicalState::default();
    for record in &vector.records {
        match record.kind.as_str() {
            "task" => {
                let mut task =
                    recovery_task(record.execution + 10_000, record.execution, record.instance);
                task.state = match record.state.as_str() {
                    "created" => contract::TaskState::Created,
                    "running" => contract::TaskState::Running,
                    "interrupted" => contract::TaskState::Interrupted,
                    other => panic!("unsupported fixture Task state {other}"),
                };
                state.tasks.push(task);
            }
            "synchronous" => {
                let mut request = recovery_synchronous(
                    record.execution + 20_000,
                    record.execution,
                    record.instance,
                );
                request.synchronous_execution.as_mut().unwrap().state = match record.state.as_str()
                {
                    "running" => runtime::SynchronousExecutionState::Running,
                    "interrupted" => runtime::SynchronousExecutionState::Interrupted,
                    other => panic!("unsupported fixture synchronous state {other}"),
                };
                state.request_records.push(request);
            }
            other => panic!("unsupported fixture record kind {other}"),
        }
    }
    state
}

fn vector_proofs(vector: &GuardRecoveryVector) -> VectorProofs {
    VectorProofs(
        vector
            .proofs
            .iter()
            .map(|proof| {
                let containing_boot_id = match proof.boot.as_str() {
                    "current" => id(3),
                    "old" => id(4),
                    other => panic!("unsupported fixture boot {other}"),
                };
                let identity = GuardIdentity {
                    runtime_epoch: id(1),
                    runtime_instance_id: id(proof.instance),
                    execution_id: id(proof.execution),
                    boot_id: containing_boot_id.clone(),
                };
                let started = GuardStarted {
                    pid: 71,
                    start_ticks: 101,
                };
                let mut bytes = match proof.format.as_str() {
                    "empty" => Vec::new(),
                    "started" | "clean" | "partial" => {
                        let mut bytes = encode_guard_frame(&identity).unwrap();
                        if proof.format == "partial" {
                            bytes.extend_from_slice(&[0, 0, 0, 10, b'{']);
                        } else {
                            bytes.extend(encode_guard_frame(&started).unwrap());
                        }
                        bytes
                    }
                    "malformed" => vec![0, 0, 0, 0],
                    other => panic!("unsupported fixture proof format {other}"),
                };
                if proof.format == "clean" {
                    bytes.extend(
                        encode_guard_frame(&GuardClean {
                            shell_exit_code: None,
                            cause: GuardCleanCause::OwnerLost,
                        })
                        .unwrap(),
                    );
                }
                GuardProofRecord {
                    containing_boot_id,
                    execution_id: id(proof.execution),
                    bytes,
                }
            })
            .collect(),
    )
}

struct SequencedProofs {
    reads: std::sync::atomic::AtomicUsize,
    started: GuardProofRecord,
    clean: GuardProofRecord,
}

impl GuardProofReader for SequencedProofs {
    fn read_proof(
        &self,
        _boot_id: &UuidV4,
        _execution_id: &contract::ExecutionId,
    ) -> Result<Option<Vec<u8>>, domain::DomainError> {
        Ok(None)
    }

    fn list_proofs(&self) -> Result<Vec<GuardProofRecord>, domain::DomainError> {
        let read = self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(vec![if read == 0 {
            self.started.clone()
        } else {
            self.clean.clone()
        }])
    }
}

#[test]
fn i4_g03_shared_recovery_wait_reloads_a_live_guard_until_it_is_clean() {
    let current_boot_id = id(3);
    let current_instance_id = id(899);
    let execution_id = id(1_020);
    let identity = GuardIdentity {
        runtime_epoch: id(1),
        runtime_instance_id: id(7_020),
        execution_id: execution_id.clone(),
        boot_id: current_boot_id.clone(),
    };
    let mut started = encode_guard_frame(&identity).unwrap();
    started.extend(
        encode_guard_frame(&GuardStarted {
            pid: 71,
            start_ticks: 101,
        })
        .unwrap(),
    );
    let mut clean = started.clone();
    clean.extend(
        encode_guard_frame(&GuardClean {
            shell_exit_code: None,
            cause: GuardCleanCause::OwnerLost,
        })
        .unwrap(),
    );
    let proofs = SequencedProofs {
        reads: std::sync::atomic::AtomicUsize::new(0),
        started: GuardProofRecord {
            containing_boot_id: current_boot_id.clone(),
            execution_id: execution_id.clone(),
            bytes: started,
        },
        clean: GuardProofRecord {
            containing_boot_id: current_boot_id.clone(),
            execution_id,
            bytes: clean,
        },
    };
    let mut state = CanonicalState::default();
    state.tasks.push(recovery_task(10_020, 1_020, 7_020));

    let plan = await_guard_recovery_plan(
        &state,
        &current_instance_id,
        &current_boot_id,
        &proofs,
        &FakeProcessFacts(true),
    )
    .unwrap();

    assert!(plan.guards_are_clean());
    assert_eq!(proofs.reads.load(std::sync::atomic::Ordering::SeqCst), 2);
}

fn recovery_token(recovery: &GuardRecovery) -> &'static str {
    match recovery {
        GuardRecovery::Clean { .. } => "clean",
        GuardRecovery::Live { .. } => "live",
        GuardRecovery::Unverified => "unverified",
    }
}

fn cleanup_requirement_token(requirement: GuardCleanupRequirement) -> &'static str {
    match requirement {
        GuardCleanupRequirement::NoGuardAction => "no_guard_action",
        GuardCleanupRequirement::AwaitGuardExit => "await_guard_exit",
        GuardCleanupRequirement::Quarantine => "quarantine",
    }
}

fn settlement_token(disposition: GuardSettlementDisposition) -> &'static str {
    match disposition {
        GuardSettlementDisposition::None => "none",
        GuardSettlementDisposition::InterruptTask => "interrupt_task",
        GuardSettlementDisposition::InterruptSynchronous => "interrupt_synchronous",
    }
}

fn finalization_token(disposition: GuardFinalizationDisposition) -> &'static str {
    match disposition {
        GuardFinalizationDisposition::Blocked => "blocked",
        GuardFinalizationDisposition::RemoveProof => "remove_proof",
        GuardFinalizationDisposition::CleanupTaskTemporaryAndRemoveProof => {
            "cleanup_task_temporary_and_remove_proof"
        }
    }
}

#[test]
fn i4_g03_guard_recovery_plan_classifies_every_shared_vector_and_rejects_duplicates() {
    let fixture = guard_recovery_fixture();
    assert_eq!(fixture.schema_version, 1);
    assert_eq!(fixture.vectors.len(), 12);
    for vector in &fixture.vectors {
        let state = vector_state(vector);
        let proofs = vector_proofs(vector);
        let result = build_guard_recovery_plan(
            &state,
            &id(899),
            &id(3),
            &proofs,
            &FakeProcessFacts(vector.process_live),
        );
        if vector.expected.result == "io_error" {
            assert_eq!(
                result.unwrap_err().code,
                ErrorCode::IoError,
                "{}",
                vector.name
            );
            continue;
        }
        let plan = result.unwrap_or_else(|error| panic!("{}: {error:?}", vector.name));
        assert_eq!(
            plan.records()
                .iter()
                .map(|record| cleanup_requirement_token(record.required_cleanup))
                .collect::<Vec<_>>(),
            vector.expected.cleanup_requirements,
            "{}",
            vector.name
        );
        assert_eq!(
            plan.records()
                .iter()
                .map(|record| recovery_token(&record.recovery))
                .collect::<Vec<_>>(),
            vector.expected.recoveries,
            "{}",
            vector.name
        );
        assert_eq!(
            plan.records()
                .iter()
                .map(|record| settlement_token(record.settlement))
                .collect::<Vec<_>>(),
            vector.expected.settlements,
            "{}",
            vector.name
        );
        assert_eq!(
            plan.records()
                .iter()
                .map(|record| finalization_token(record.finalization))
                .collect::<Vec<_>>(),
            vector.expected.finalizations,
            "{}",
            vector.name
        );
        assert_eq!(
            plan.prior_instances(),
            vector
                .expected
                .prior_instances
                .iter()
                .copied()
                .map(id)
                .collect::<Vec<_>>(),
            "{}",
            vector.name
        );
    }
}

fn app_vector_cleanup(plan: &GuardRecoveryPlan, finalization_succeeds: bool) -> bool {
    plan.guards_are_clean()
        && finalization_succeeds
        && plan
            .records()
            .iter()
            .all(|record| record.finalization != GuardFinalizationDisposition::Blocked)
}

fn magisk_vector_cleanup(plan: &GuardRecoveryPlan, finalization_succeeds: bool) -> bool {
    if !plan.guards_are_clean() || !finalization_succeeds {
        return false;
    }
    !plan
        .records()
        .iter()
        .any(|record| record.finalization == GuardFinalizationDisposition::Blocked)
}

#[test]
fn i4_g04_app_and_magisk_consumers_share_recovery_semantics_and_finalization_failure() {
    let fixture = guard_recovery_fixture();
    for vector in &fixture.vectors {
        if vector.expected.result != "plan" {
            continue;
        }
        let plan = build_guard_recovery_plan(
            &vector_state(vector),
            &id(899),
            &id(3),
            &vector_proofs(vector),
            &FakeProcessFacts(vector.process_live),
        )
        .unwrap();
        let app = app_vector_cleanup(&plan, vector.finalization_succeeds);
        let magisk = magisk_vector_cleanup(&plan, vector.finalization_succeeds);
        assert_eq!(app, vector.expected.cleanup_verified, "{}", vector.name);
        assert_eq!(magisk, vector.expected.cleanup_verified, "{}", vector.name);
        assert_eq!(app, magisk, "{}", vector.name);
    }
}

#[test]
fn i4_g11_shared_plan_owner_rejects_duplicate_proof_identity() {
    let fixture = guard_recovery_fixture();
    let vector = fixture
        .vectors
        .iter()
        .find(|vector| vector.name == "old_task")
        .unwrap();
    let state = vector_state(vector);
    let mut proofs = vector_proofs(vector);
    proofs.0.push(proofs.0[0].clone());
    assert_eq!(
        build_guard_recovery_plan(&state, &id(899), &id(3), &proofs, &FakeProcessFacts(false),)
            .unwrap_err()
            .code,
        ErrorCode::IoError
    );
}

/// A capture that settles publishes its artifact under the artifact store's lock while a Runtime
/// commit is being prepared; the Runtime commit rebases across that publication instead of failing
/// with REVISION_CONFLICT, and still refuses when something it owns changed.
#[test]
fn i4_runtime_commits_rebase_across_artifact_publications_only() {
    use runtime::PersistencePort;

    let (_directory, store, lease) = initialized_store("rebase-artifacts");
    let lease = Arc::new(lease);
    let port = JsonPersistencePort::new(store.clone(), lease.clone());
    let artifacts = ArtifactStore::new(store.clone(), lease.clone());
    let publish = |seed| {
        artifacts
            .publish(ArtifactPublish {
                kind: ArtifactKind::Image,
                id: id(seed),
                bytes: b"capture".to_vec(),
                created_at: "2026-01-01T00:00:00.000Z".to_owned(),
                created_at_ms: 1_767_225_600_000,
                expires_at: "2026-01-02T00:00:00.000Z".to_owned(),
                expires_at_ms: 1_767_312_000_000,
                mime: Some("image/png".to_owned()),
                task_id: None,
                request_id: None,
            })
            .unwrap()
    };

    let loaded = port.load().unwrap();
    let first = publish(40);
    let revision = store.load(&lease).unwrap().store_revision;
    store
        .compare_and_commit(&lease, revision, |state| {
            state.artifact_manifest.push(first.clone());
            Ok(())
        })
        .unwrap();
    let mut candidate = loaded.clone();
    candidate.revision = loaded.revision + 1;
    port.compare_and_commit(loaded.revision, candidate).unwrap();
    let after = store.load(&lease).unwrap();
    assert_eq!(after.store_revision, loaded.revision + 2);
    assert_eq!(after.artifact_manifest, vec![first]);

    // A commit whose loaded view is not the revision it names cannot tell what changed in between,
    // so it keeps refusing instead of rebasing.
    let loaded = port.load().unwrap();
    let second = publish(41);
    let revision = store.load(&lease).unwrap().store_revision;
    store
        .compare_and_commit(&lease, revision, |state| {
            state.artifact_manifest.push(second.clone());
            Ok(())
        })
        .unwrap();
    let unaware = JsonPersistencePort::new(store.clone(), lease.clone());
    let mut conflicting = loaded.clone();
    conflicting.revision = loaded.revision + 1;
    let refused = unaware
        .compare_and_commit(loaded.revision, conflicting)
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::RevisionConflict);
}

/// Executions a lost instance left running are counted only while no live Runtime owns the store,
/// and clearing them settles each as interrupted, releases its reservation and leaves a store the
/// Runtime still accepts.
#[test]
fn i4_stranded_executions_are_cleared_only_without_a_live_runtime() {
    use contract::{ExecutionClass, MotherTool, RunAs, TaskState};
    use runtime::{ExecutionPayload, ExecutorRecord, ProviderToken};

    let (directory, store, lease) = initialized_store("stranded");
    let request_id = id(900);
    let task_id = id(901);
    let execution_id = id(902);
    store
        .compare_and_commit(&lease, 0, |state| {
            state.request_records.push(RequestRecord {
                request_id: request_id.clone(),
                payload_sha256: "90".repeat(32),
                expires_at_ms: None,
                task_id: Some(task_id.clone()),
                synchronous_execution: None,
                mutation_result: None,
            });
            state.tasks.push(StoredTask {
                request_id: Some(request_id.clone()),
                task_id: task_id.clone(),
                execution_id: execution_id.clone(),
                state: TaskState::Running,
                cancel_requested: false,
                tool: MotherTool::Command,
                action: "run".to_owned(),
                created_at: "2026-09-08T00:00:00.000Z".to_owned(),
                started_at: Some("2026-09-08T00:00:00.000Z".to_owned()),
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
                reserved_bytes: runtime::RESERVE_FLOOR_BYTES,
                automation_owner: None,
            });
            state.reservations.push(ReservationRecord {
                execution_id: execution_id.clone(),
                request_id: Some(request_id.clone()),
                task_id: Some(task_id.clone()),
                reserved_bytes: runtime::RESERVE_FLOOR_BYTES,
            });
            Ok(())
        })
        .unwrap();

    // The instance holding the live lease still settles its own executions.
    assert_eq!(stranded_execution_count(directory.path()).unwrap(), 0);
    // A different instance holding the lease (a daemon restarting) sees the old one's as stranded.
    drop(lease);
    let other = store.acquire_lifetime(live(4)).unwrap();
    assert_eq!(stranded_execution_count(directory.path()).unwrap(), 1);
    let lease = other;
    assert_eq!(
        clear_stranded_executions(
            directory.path(),
            "2026-09-08T00:01:00.000Z",
            1_788_825_660_000,
            std::time::Duration::ZERO,
        )
        .unwrap_err()
        .code,
        ErrorCode::HostTransitionPending
    );

    drop(lease);
    assert_eq!(stranded_execution_count(directory.path()).unwrap(), 1);
    assert_eq!(
        clear_stranded_executions(
            directory.path(),
            "2026-09-08T00:01:00.000Z",
            1_788_825_660_000,
            std::time::Duration::ZERO,
        )
        .unwrap(),
        1
    );
    assert_eq!(stranded_execution_count(directory.path()).unwrap(), 0);

    let state =
        decode_canonical_state(&fs::read(directory.path().join("runtime-state.json")).unwrap())
            .unwrap();
    let task = state
        .tasks
        .iter()
        .find(|task| task.task_id == task_id)
        .unwrap();
    assert_eq!(task.state, TaskState::Interrupted);
    assert_eq!(task.error.as_ref().unwrap().operation, "command.run");
    assert!(state.reservations.is_empty());
    assert_eq!(
        state.request_records[0].expires_at_ms,
        Some(1_788_825_660_000 + 86_400_000)
    );
    // A new instance can take the store over again.
    store.acquire_lifetime(live(3)).unwrap();
}
