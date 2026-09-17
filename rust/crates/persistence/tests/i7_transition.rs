use contract::{ErrorCode, RuntimeHost, UuidV4};
use persistence::{
    CanonicalState, RuntimeLive, RuntimeOwner, RuntimeTransitionIntent, StateStore,
    TransitionRecovery,
};
use std::{
    fs,
    path::{Path, PathBuf},
};

struct NoLiveProcess;

impl persistence::ProcessFacts for NoLiveProcess {
    fn is_same_process(&self, _: u32, _: u64) -> Result<bool, domain::DomainError> {
        Ok(false)
    }
}

fn id(index: u64) -> UuidV4 {
    UuidV4::parse(format!("10000000-0000-4000-8000-{index:012x}")).unwrap()
}

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("droidbridge-i7-{label}-{}", std::process::id(),));
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

fn owner(host: RuntimeHost, generation: u64) -> RuntimeOwner {
    RuntimeOwner {
        schema_version: 1,
        runtime_epoch: id(1),
        host,
        host_generation: generation,
    }
}

fn live(host: RuntimeHost, generation: u64, instance: u64) -> RuntimeLive {
    RuntimeLive {
        runtime_epoch: id(1),
        host,
        host_generation: generation,
        runtime_instance_id: id(instance),
        boot_id: id(2),
        pid: 42,
        start_ticks: 99,
    }
}

fn intent() -> RuntimeTransitionIntent {
    RuntimeTransitionIntent {
        schema_version: 1,
        transition_id: id(4),
        runtime_epoch: id(1),
        from_host: RuntimeHost::ApkRuntime,
        from_generation: 1,
        from_instance_id: id(3),
        target_host: RuntimeHost::MagiskBackend,
        target_generation: 2,
    }
}

fn dead_intent() -> RuntimeTransitionIntent {
    RuntimeTransitionIntent {
        schema_version: 1,
        transition_id: id(8),
        runtime_epoch: id(1),
        from_host: RuntimeHost::MagiskBackend,
        from_generation: 8,
        from_instance_id: id(6),
        target_host: RuntimeHost::ApkRuntime,
        target_generation: 9,
    }
}

#[test]
fn i7_g02_abort_and_finish_remove_only_the_matching_transition() {
    let directory = TestDirectory::new("transition-close");
    let store = StateStore::new(directory.path().to_path_buf());
    store
        .initialize(
            &owner(RuntimeHost::ApkRuntime, 1),
            &CanonicalState::default(),
        )
        .unwrap();
    let source = store
        .acquire_lifetime(live(RuntimeHost::ApkRuntime, 1, 3))
        .unwrap();
    let transition = intent();
    store
        .record_transition_intent(&source, &transition)
        .unwrap();
    store.abort_owner_transition(&source, &transition).unwrap();
    assert!(!directory.path().join("runtime-transition.json").exists());
    store.validate_lease(&source).unwrap();

    store
        .record_transition_intent(&source, &transition)
        .unwrap();
    drop(source);
    store
        .commit_owner_transition(&transition, &id(2), &NoLiveProcess)
        .unwrap();
    let target = store
        .acquire_lifetime(live(RuntimeHost::MagiskBackend, 2, 5))
        .unwrap();
    store.finish_owner_transition(&target, &transition).unwrap();
    assert!(!directory.path().join("runtime-transition.json").exists());
    store.validate_lease(&target).unwrap();
}

#[test]
fn i7_g08_dead_source_takeover_requires_exclusive_lifetime_and_two_phase_completion() {
    let directory = TestDirectory::new("dead-takeover");
    let store = StateStore::new(directory.path().to_path_buf());
    store
        .initialize(
            &owner(RuntimeHost::MagiskBackend, 8),
            &CanonicalState::default(),
        )
        .unwrap();
    let source = store
        .acquire_lifetime(live(RuntimeHost::MagiskBackend, 8, 6))
        .unwrap();
    let target_live = live(RuntimeHost::ApkRuntime, 9, 7);
    let transition = dead_intent();
    assert_eq!(
        store
            .begin_dead_owner_takeover(&transition, target_live.clone(), &id(2), &NoLiveProcess,)
            .err()
            .unwrap()
            .code,
        ErrorCode::HostTransitionPending,
    );
    drop(source);
    let pending = store
        .begin_dead_owner_takeover(&transition, target_live, &id(2), &NoLiveProcess)
        .unwrap();
    assert_eq!(store.read_owner().unwrap().host, RuntimeHost::ApkRuntime);
    assert_eq!(store.read_owner().unwrap().host_generation, 9);
    assert!(directory.path().join("runtime-transition.json").exists());
    let target = store
        .complete_dead_owner_takeover(pending, &NoLiveProcess)
        .unwrap();
    assert!(!directory.path().join("runtime-transition.json").exists());
    store.validate_lease(&target).unwrap();
}

#[test]
fn i7_g02_remote_abort_preserves_owner_and_removes_only_matching_intent() {
    let directory = TestDirectory::new("remote-abort");
    let store = StateStore::new(directory.path().to_path_buf());
    store
        .initialize(
            &owner(RuntimeHost::MagiskBackend, 8),
            &CanonicalState::default(),
        )
        .unwrap();
    let source = store
        .acquire_lifetime(live(RuntimeHost::MagiskBackend, 8, 6))
        .unwrap();
    let transition = dead_intent();
    store.record_remote_transition_intent(&transition).unwrap();
    store.abort_remote_transition_intent(&transition).unwrap();
    assert!(!directory.path().join("runtime-transition.json").exists());
    assert_eq!(store.read_owner(), Ok(owner(RuntimeHost::MagiskBackend, 8)));
    store.validate_lease(&source).unwrap();
}

#[test]
fn i7_g02_committed_transition_is_finished_after_target_reconnects() {
    let directory = TestDirectory::new("committed-reconnect");
    let store = StateStore::new(directory.path().to_path_buf());
    store
        .initialize(
            &owner(RuntimeHost::ApkRuntime, 1),
            &CanonicalState::default(),
        )
        .unwrap();
    let source = store
        .acquire_lifetime(live(RuntimeHost::ApkRuntime, 1, 3))
        .unwrap();
    let transition = intent();
    store
        .record_transition_intent(&source, &transition)
        .unwrap();
    drop(source);
    store
        .commit_owner_transition(&transition, &id(2), &NoLiveProcess)
        .unwrap();
    let target = store
        .acquire_lifetime(live(RuntimeHost::MagiskBackend, 2, 5))
        .unwrap();

    assert!(store.finish_committed_transition(&id(5)).unwrap());
    assert!(!directory.path().join("runtime-transition.json").exists());
    assert!(!store.finish_committed_transition(&id(5)).unwrap());
    store.validate_lease(&target).unwrap();
}

#[test]
fn i7_g02_reconnect_validates_the_existing_live_host_without_a_transition() {
    let directory = TestDirectory::new("live-reconnect");
    let store = StateStore::new(directory.path().to_path_buf());
    store
        .initialize(
            &owner(RuntimeHost::MagiskBackend, 2),
            &CanonicalState::default(),
        )
        .unwrap();
    let target = store
        .acquire_lifetime(live(RuntimeHost::MagiskBackend, 2, 5))
        .unwrap();

    assert_eq!(
        store
            .validate_live_instance(RuntimeHost::MagiskBackend, &id(5))
            .unwrap(),
        owner(RuntimeHost::MagiskBackend, 2),
    );
    assert_eq!(
        store
            .validate_live_instance(RuntimeHost::MagiskBackend, &id(6))
            .unwrap_err()
            .code,
        ErrorCode::StaleAuthority,
    );
    assert_eq!(
        store
            .validate_live_instance(RuntimeHost::ApkRuntime, &id(5))
            .unwrap_err()
            .code,
        ErrorCode::StaleAuthority,
    );
    assert!(!directory.path().join("runtime-transition.json").exists());
    store.validate_lease(&target).unwrap();
}

#[test]
fn i7_g02_transition_observation_distinguishes_source_from_committed_target() {
    let directory = TestDirectory::new("transition-observation");
    let store = StateStore::new(directory.path().to_path_buf());
    store
        .initialize(
            &owner(RuntimeHost::MagiskBackend, 8),
            &CanonicalState::default(),
        )
        .unwrap();
    assert!(store.observe_transition().unwrap().is_none());

    let source = store
        .acquire_lifetime(live(RuntimeHost::MagiskBackend, 8, 6))
        .unwrap();
    let transition = dead_intent();
    store.record_remote_transition_intent(&transition).unwrap();
    assert_eq!(
        store.observe_transition().unwrap(),
        Some((
            TransitionRecovery::RemoveUncommittedIntent,
            transition.clone(),
        )),
    );

    drop(source);
    store
        .commit_owner_transition(&transition, &id(2), &NoLiveProcess)
        .unwrap();
    assert_eq!(
        store.observe_transition().unwrap(),
        Some((TransitionRecovery::ActivateCommittedTarget, transition)),
    );
}

#[test]
fn i7_g02_owner_commit_derives_cleanup_from_canonical_work_not_a_caller_claim() {
    use contract::{ExecutionClass, MotherTool, RunAs, TaskState};
    use persistence::{RequestRecord, ReservationRecord, StoredRoute, StoredTask};
    use runtime::{ExecutionPayload, ExecutorRecord, ProviderToken};

    let directory = TestDirectory::new("transition-proof");
    let store = StateStore::new(directory.path().to_path_buf());
    store
        .initialize(
            &owner(RuntimeHost::ApkRuntime, 1),
            &CanonicalState::default(),
        )
        .unwrap();
    let source = store
        .acquire_lifetime(live(RuntimeHost::ApkRuntime, 1, 3))
        .unwrap();
    let request_id = id(20);
    let task_id = id(21);
    let execution_id = id(22);
    store
        .compare_and_commit(&source, 0, |state| {
            state.request_records.push(RequestRecord {
                request_id: request_id.clone(),
                payload_sha256: "22".repeat(32),
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
                created_at: "2026-09-10T00:00:00.000Z".to_owned(),
                started_at: Some("2026-09-10T00:00:01.000Z".to_owned()),
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
                        runtime_instance_id: id(3),
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
                execution_id,
                request_id: Some(request_id),
                task_id: Some(task_id),
                reserved_bytes: runtime::RESERVE_FLOOR_BYTES,
            });
            Ok(())
        })
        .unwrap();
    let transition = intent();
    store
        .record_transition_intent(&source, &transition)
        .unwrap();
    drop(source);

    assert_eq!(
        store
            .commit_owner_transition(&transition, &id(2), &NoLiveProcess)
            .unwrap_err()
            .code,
        ErrorCode::HostTransitionPending,
    );
    assert_eq!(
        store.read_owner().unwrap(),
        owner(RuntimeHost::ApkRuntime, 1),
    );
    assert!(directory.path().join("runtime-transition.json").exists());
}

#[test]
fn i7_g08_owner_commit_rejects_unverified_guard_proof() {
    let directory = TestDirectory::new("transition-guard-proof");
    let store = StateStore::new(directory.path().to_path_buf());
    store
        .initialize(
            &owner(RuntimeHost::ApkRuntime, 1),
            &CanonicalState::default(),
        )
        .unwrap();
    let source = store
        .acquire_lifetime(live(RuntimeHost::ApkRuntime, 1, 3))
        .unwrap();
    let transition = intent();
    store
        .record_transition_intent(&source, &transition)
        .unwrap();
    let proof_directory = directory
        .path()
        .join("execution-guards")
        .join(id(2).as_str());
    fs::create_dir_all(&proof_directory).unwrap();
    fs::write(
        proof_directory.join(format!("{}.proof", id(20).as_str())),
        [],
    )
    .unwrap();
    drop(source);

    assert_eq!(
        store
            .commit_owner_transition(&transition, &id(2), &NoLiveProcess)
            .unwrap_err()
            .code,
        ErrorCode::HostTransitionPending,
    );
    assert_eq!(
        store.read_owner().unwrap(),
        owner(RuntimeHost::ApkRuntime, 1),
    );
    assert!(directory.path().join("runtime-transition.json").exists());
}

#[test]
fn i7_g08_owner_commit_accepts_guard_written_clean_proof() {
    use persistence::{
        GuardClean, GuardCleanCause, GuardIdentity, GuardStarted, encode_guard_frame,
    };

    let directory = TestDirectory::new("transition-clean-proof");
    let store = StateStore::new(directory.path().to_path_buf());
    store
        .initialize(
            &owner(RuntimeHost::ApkRuntime, 1),
            &CanonicalState::default(),
        )
        .unwrap();
    let source = store
        .acquire_lifetime(live(RuntimeHost::ApkRuntime, 1, 3))
        .unwrap();
    let transition = intent();
    store
        .record_transition_intent(&source, &transition)
        .unwrap();
    let execution_id = id(20);
    let proof_directory = directory
        .path()
        .join("execution-guards")
        .join(id(2).as_str());
    fs::create_dir_all(&proof_directory).unwrap();
    let mut proof = encode_guard_frame(&GuardIdentity {
        runtime_epoch: id(1),
        runtime_instance_id: id(3),
        execution_id: execution_id.clone(),
        boot_id: id(2),
    })
    .unwrap();
    proof.extend(
        encode_guard_frame(&GuardStarted {
            pid: 42,
            start_ticks: 99,
        })
        .unwrap(),
    );
    proof.extend(
        encode_guard_frame(&GuardClean {
            shell_exit_code: None,
            cause: GuardCleanCause::OwnerLost,
        })
        .unwrap(),
    );
    fs::write(
        proof_directory.join(format!("{}.proof", execution_id.as_str())),
        proof,
    )
    .unwrap();
    drop(source);

    let target = store
        .commit_owner_transition(&transition, &id(2), &NoLiveProcess)
        .unwrap();
    assert_eq!(target, owner(RuntimeHost::MagiskBackend, 2));
}
