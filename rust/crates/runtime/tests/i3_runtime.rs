use contract::{
    Automation, AutomationAction, AutomationCommandCall, AutomationCompatibleCall,
    AutomationTaskResult, AutomationTrigger, Availability, CapabilityState, CommandRunInput,
    ErrorCode, FileTarget, FileTargetType, FilesystemCall, FilesystemInspectInput, GrantFacts,
    MotherTool, RunAs, RuntimeHost, RuntimeReadiness, ScalarValue, TaskState, TaskTerminalResult,
    True, UuidV4,
};
use domain::{
    AdmissionFence, CapabilityContext, ExecutorRequest, ProviderGenerations, ResolverFacts,
};
use runtime::{
    AdmittedExecution, ArtifactPort, AutomationEffects, AutomationInterpreter, CapabilityPort,
    CapabilitySnapshot, CompositeExecutionSurface, ExecutionCompletion, ExecutionFailure,
    ExecutionOutcome, ExecutionPayload, ExecutionPort, ExecutorRecord, HostControlPort,
    LocalExecutionClaims, PersistencePort, PortFuture, ProviderToken, RecoveryProof, RuntimeCore,
    RuntimeState, SynchronousAdmission, SynchronousExecutionRecord, SynchronousExecutionState,
    TaskAdmission, TaskAdmissionResult,
    fakes::{FakeArtifacts, FakeCapabilities, FakeExecutions, FakeHostControl, FakePersistence},
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::Semaphore;

type TestCore =
    RuntimeCore<FakePersistence, FakeArtifacts, FakeExecutions, FakeCapabilities, FakeHostControl>;

fn uuid(prefix: u32, value: u64) -> UuidV4 {
    UuidV4::parse(format!("{prefix:08x}-0000-4000-8000-{value:012x}")).unwrap()
}

fn availability(state: CapabilityState) -> Availability {
    Availability {
        state,
        reason: None,
    }
}

fn grants(state: CapabilityState) -> GrantFacts {
    GrantFacts {
        android_local_network: availability(state),
        android_notifications: availability(state),
        android_notification_listener: availability(state),
        automation_exact_alarm: availability(state),
        visual_accessibility: availability(state),
        visual_media_projection_session: availability(state),
        shizuku_shell: availability(state),
        magisk_module: availability(state),
        magisk_root: availability(state),
        magisk_framework: availability(state),
        magisk_launch: availability(state),
        magisk_clipboard: availability(state),
        magisk_notifications: availability(state),
        magisk_wake_alarm: availability(state),
        execution_app_guard: availability(state),
        execution_shell_guard: availability(state),
        execution_root_guard: availability(state),
    }
}

fn fence(instance: u64) -> AdmissionFence {
    AdmissionFence {
        runtime_epoch: uuid(0x1000_0000, 1),
        host_generation: 1,
        runtime_instance_id: uuid(0x1000_0000, instance),
    }
}

fn capability(instance: u64) -> CapabilitySnapshot {
    let state = CapabilityState::Available;
    CapabilitySnapshot {
        grants: grants(state),
        context: CapabilityContext {
            sdk_int: 37,
            host: RuntimeHost::ApkRuntime,
            readiness: RuntimeReadiness::Ready,
            app_execution_surface: state,
        },
        resolver_facts: ResolverFacts {
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
            generations: ProviderGenerations {
                app_native: instance,
                app_framework: instance,
                shizuku: instance,
                magisk_native: instance,
                magisk_framework: instance,
                accessibility: instance,
                media_projection: instance,
                notification_listener: instance,
            },
        },
        fence: fence(instance),
    }
}

fn make_core() -> (
    TestCore,
    FakePersistence,
    FakeArtifacts,
    FakeExecutions,
    FakeCapabilities,
    FakeHostControl,
) {
    let persistence = FakePersistence::default();
    let artifacts = FakeArtifacts::default();
    let executions = FakeExecutions::default();
    let capabilities = FakeCapabilities::new(capability(2));
    let host = FakeHostControl::new(RecoveryProof::Clean).with_capabilities(capabilities.clone());
    let core = RuntimeCore::new(
        persistence.clone(),
        artifacts.clone(),
        executions.clone(),
        capabilities.clone(),
        host.clone(),
    );
    (core, persistence, artifacts, executions, capabilities, host)
}

fn admission(index: u64) -> TaskAdmission {
    TaskAdmission {
        request_id: uuid(0x2000_0000, index),
        payload_sha256: format!("{index:064x}"),
        task_id: uuid(0x3000_0000, index),
        execution_id: uuid(0x4000_0000, index),
        tool: MotherTool::Command,
        action: "run".to_owned(),
        route: ExecutorRequest::Command(RunAs::App),
        payload: ExecutionPayload::OpaqueOperation("command.run".to_owned()),
        created_at: format!("2026-09-06T00:00:{:02}.000Z", index % 60),
        settlement_bound_bytes: 16_384,
        now_ms: index,
    }
}

#[tokio::test]
async fn i3_g02_rejected_duplicate_claim_preserves_the_live_cancellation_owner() {
    let claims = LocalExecutionClaims::default();
    let execution_id = uuid(0x4000_0000, 99);
    let live_claim = claims.claim(execution_id.clone()).unwrap();

    assert!(claims.claim(execution_id.clone()).is_err());

    let cancellation = {
        let claims = claims.clone();
        let execution_id = execution_id.clone();
        tokio::spawn(async move { claims.cancel(&execution_id).await })
    };
    tokio::time::timeout(Duration::from_secs(1), async {
        while !claims.cancel_requested(&execution_id) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cancellation reached the retained live claim");
    claims.finish(&live_claim, true);

    let outcome = tokio::time::timeout(Duration::from_secs(1), cancellation)
        .await
        .expect("cancellation still observes the original live claim")
        .expect("cancellation task completed")
        .unwrap();
    assert_eq!(
        outcome,
        runtime::ExecutionCancelOutcome::Cancelled {
            cleanup_verified: true,
        }
    );
}

fn synchronous_admission(index: u64) -> SynchronousAdmission {
    SynchronousAdmission {
        request_id: uuid(0x2100_0000, index),
        payload_sha256: format!("{index:064x}"),
        execution_id: uuid(0x4100_0000, index),
        operation: "filesystem.manage".to_owned(),
        route: ExecutorRequest::Command(RunAs::App),
        payload: ExecutionPayload::OpaqueOperation("filesystem.manage".to_owned()),
        settlement_bound_bytes: 16_384,
        now_ms: index,
    }
}

#[derive(Clone)]
struct GatedExecutions {
    starts: Arc<AtomicUsize>,
    started: Arc<Semaphore>,
    release: Arc<Semaphore>,
    completion: ExecutionCompletion,
}

impl GatedExecutions {
    fn new(completion: ExecutionCompletion) -> Self {
        Self {
            starts: Arc::new(AtomicUsize::new(0)),
            started: Arc::new(Semaphore::new(0)),
            release: Arc::new(Semaphore::new(0)),
            completion,
        }
    }
}

impl ExecutionPort for GatedExecutions {
    fn claim_and_start<'a>(
        &'a self,
        _execution: AdmittedExecution,
    ) -> PortFuture<'a, Result<ExecutionCompletion, ExecutionFailure>> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        self.started.add_permits(1);
        Box::pin(async move {
            self.release
                .acquire()
                .await
                .expect("test release semaphore stays open")
                .forget();
            Ok(self.completion.clone())
        })
    }

    fn cancel<'a>(
        &'a self,
        _execution_id: &'a UuidV4,
    ) -> PortFuture<'a, Result<runtime::ExecutionCancelOutcome, domain::DomainError>> {
        Box::pin(async {
            Ok(runtime::ExecutionCancelOutcome::Cancelled {
                cleanup_verified: true,
            })
        })
    }
}

#[derive(Clone)]
struct CapabilityChangingPersistence {
    inner: FakePersistence,
    capabilities: FakeCapabilities,
}

impl PersistencePort for CapabilityChangingPersistence {
    fn load(&self) -> Result<RuntimeState, domain::DomainError> {
        self.inner.load()
    }

    fn compare_and_commit(
        &self,
        expected_revision: u64,
        candidate: RuntimeState,
    ) -> Result<(), domain::DomainError> {
        let admitted_synchronous = candidate
            .synchronous_executions
            .iter()
            .any(|record| record.state == SynchronousExecutionState::Running);
        self.inner
            .compare_and_commit(expected_revision, candidate)?;
        if admitted_synchronous {
            self.capabilities.set(capability(99));
        }
        Ok(())
    }
}

fn automation() -> Automation {
    Automation {
        automation_id: uuid(0x5000_0000, 1),
        name: "interpreter".to_owned(),
        enabled: true,
        trigger: AutomationTrigger::Event {
            name: "runtime.ready".to_owned(),
            r#match: None,
        },
        action: AutomationAction::Delay { duration_ms: 1 },
        state: BTreeMap::new(),
        revision: 1,
        created_at: "2026-09-06T00:00:00.000Z".to_owned(),
        updated_at: "2026-09-06T00:00:00.000Z".to_owned(),
    }
}

#[tokio::test]
async fn i3_g01_fake_backed_core_exposes_stable_ports_and_interpreter() {
    let (core, _, artifacts, _, _, _) = make_core();
    assert_eq!(
        core.capability_projection().unwrap().command_app.state,
        CapabilityState::Available
    );
    let metadata = artifacts.publish(b"bounded").unwrap();
    assert_eq!(artifacts.open(&metadata.artifact_ref).unwrap(), b"bounded");
    assert_eq!(metadata.sha256.len(), 64);

    let call = AutomationAction::Call {
        call: AutomationCompatibleCall::Command {
            call: AutomationCommandCall::Run(CommandRunInput {
                command: "true".to_owned(),
                run_as: RunAs::App,
                cwd: None,
                stdin: None,
                timeout_ms: 1000,
                max_output_bytes: 1024,
                as_task: false,
            }),
        },
        on_failure: Default::default(),
    };
    let action = AutomationAction::Sequence {
        children: vec![
            call,
            AutomationAction::SetState {
                key: "complete".to_owned(),
                value: ScalarValue::Boolean(true),
            },
        ],
    };
    /// Records the interpreter's effects so traversal is observable without a Runtime host.
    #[derive(Default)]
    struct RecordingEffects {
        calls: usize,
        delays: Vec<u64>,
        states: Vec<(String, ScalarValue)>,
    }

    impl AutomationEffects for RecordingEffects {
        fn checkpoint(&mut self) -> Result<(), domain::DomainError> {
            Ok(())
        }

        fn call<'a>(
            &'a mut self,
            _call: &'a AutomationCompatibleCall,
        ) -> PortFuture<'a, Result<runtime::CallOutcome, domain::DomainError>> {
            self.calls += 1;
            Box::pin(async { Ok(runtime::CallOutcome::default()) })
        }

        fn set_state<'a>(
            &'a mut self,
            key: &'a str,
            value: &'a ScalarValue,
        ) -> PortFuture<'a, Result<(), domain::DomainError>> {
            self.states.push((key.to_owned(), value.clone()));
            Box::pin(async { Ok(()) })
        }

        fn delay<'a>(
            &'a mut self,
            duration_ms: u64,
        ) -> PortFuture<'a, Result<(), domain::DomainError>> {
            self.delays.push(duration_ms);
            Box::pin(async { Ok(()) })
        }
    }

    let action = AutomationAction::Sequence {
        children: vec![action, automation().action],
    };
    let mut state = BTreeMap::new();
    let mut effects = RecordingEffects::default();
    AutomationInterpreter
        .execute(&action, &mut state, &BTreeMap::new(), &mut effects)
        .await
        .unwrap();
    assert_eq!(effects.calls, 1);
    assert_eq!(effects.delays, vec![1]);
    assert_eq!(
        effects.states,
        vec![("complete".to_owned(), ScalarValue::Boolean(true))]
    );
    assert_eq!(state.get("complete"), Some(&ScalarValue::Boolean(true)));
}

#[tokio::test]
async fn i3_g02_fencing_dedup_cancellation_and_partial_failure_are_deterministic() {
    let (core, _, _, executions, _, _) = make_core();
    let request = admission(1);
    let first = core.admit_task(request.clone()).await.unwrap();
    assert!(matches!(first, TaskAdmissionResult::Admitted(_)));
    assert!(matches!(
        core.admit_task(request.clone()).await.unwrap(),
        TaskAdmissionResult::Replay(_)
    ));
    let mut conflict = request.clone();
    conflict.payload_sha256 = "f".repeat(64);
    assert_eq!(
        core.admit_task(conflict).await.unwrap_err().code,
        ErrorCode::InvalidArgument
    );

    executions.set_cancel_cleanup_verified(false);
    let cancelled = core
        .cancel_task(
            &request.task_id,
            "2026-09-06T00:01:00.000Z".to_owned(),
            60_000,
        )
        .await
        .unwrap();
    assert_eq!(cancelled.state, TaskState::Cancelled);
    assert!(cancelled.cancel_requested);
    assert!(executions.cancelled().is_empty());

    let (core, persistence, _, executions, capabilities, host) = make_core();
    let request = admission(2);
    core.admit_task(request.clone()).await.unwrap();
    executions.push(Ok(ExecutionCompletion {
        fence: fence(2),
        capability_generation: 2,
        outcome: ExecutionOutcome::Completed {
            result: TaskTerminalResult::Automation(AutomationTaskResult {
                automation_id: uuid(0x6000_0000, 1),
                execution_id: request.execution_id.clone(),
                completed: True,
            }),
            encoded_bytes: 1024,
        },
        cleanup_verified: false,
    }));
    let partial = core
        .run_task(
            &request.task_id,
            "2026-09-06T00:00:01.000Z".to_owned(),
            2_000,
        )
        .await
        .unwrap();
    assert_eq!(partial.state, TaskState::Interrupted);
    assert!(partial.result.is_none());
    assert_eq!(partial.error.unwrap().code, ErrorCode::IoError);
    assert_eq!(
        capabilities.current().unwrap().context.readiness,
        RuntimeReadiness::Unavailable
    );
    assert_eq!(
        host.cleanup_reports(),
        vec![(fence(2), request.execution_id)]
    );
    let mut reloaded = persistence.load().unwrap();
    let revision = reloaded.revision;
    reloaded.revision += 1;
    persistence.compare_and_commit(revision, reloaded).unwrap();
    assert_eq!(
        core.admit_task(admission(3)).await.unwrap_err().code,
        ErrorCode::CapabilityUnavailable
    );
}

#[tokio::test]
async fn i3_g03_synchronous_mutation_is_persisted_once_and_terminal_result_is_replayed() {
    let persistence = FakePersistence::default();
    let artifacts = FakeArtifacts::default();
    let executions = FakeExecutions::default();
    let capabilities = FakeCapabilities::new(capability(2));
    let host = FakeHostControl::new(RecoveryProof::Clean).with_capabilities(capabilities.clone());
    let core = RuntimeCore::new(
        persistence.clone(),
        artifacts,
        executions.clone(),
        capabilities,
        host,
    );
    let admission = synchronous_admission(1);
    let expected = serde_json::json!({"moved": true});
    executions.push(Ok(ExecutionCompletion {
        fence: fence(2),
        capability_generation: 2,
        outcome: ExecutionOutcome::SynchronousCompleted {
            result: expected.clone(),
            encoded_bytes: 128,
        },
        cleanup_verified: true,
    }));

    assert_eq!(
        core.run_synchronous(
            admission.clone(),
            "2026-09-06T00:00:02.000Z".to_owned(),
            2_000,
        )
        .await
        .unwrap(),
        expected
    );
    let state = persistence.snapshot();
    assert_eq!(state.synchronous_executions.len(), 1);
    assert_eq!(
        state.synchronous_executions[0].state,
        SynchronousExecutionState::Completed
    );
    assert_eq!(state.reserved_bytes, 0);
    assert_eq!(executions.started().len(), 1);

    assert_eq!(
        core.run_synchronous(
            admission.clone(),
            "2026-09-06T00:00:03.000Z".to_owned(),
            3_000,
        )
        .await
        .unwrap(),
        expected
    );
    assert_eq!(executions.started().len(), 1);

    let mut conflict = admission;
    conflict.payload_sha256 = "f".repeat(64);
    assert_eq!(
        core.run_synchronous(conflict, "2026-09-06T00:00:04.000Z".to_owned(), 4_000,)
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
}

#[tokio::test]
async fn i3_g03_a_result_over_the_retention_bound_answers_once_and_is_never_replayed() {
    let (core, persistence, _, executions, _, _) = make_core();
    let admission = synchronous_admission(9);
    let large = serde_json::json!({"nodes": "n".repeat(10 * 1024)});
    executions.push(Ok(ExecutionCompletion {
        fence: fence(2),
        capability_generation: 2,
        outcome: ExecutionOutcome::SynchronousCompleted {
            result: large.clone(),
            encoded_bytes: 128,
        },
        cleanup_verified: true,
    }));

    // The call that ran the operation receives its whole result.
    assert_eq!(
        core.run_synchronous(
            admission.clone(),
            "2026-09-06T00:00:02.000Z".to_owned(),
            2_000
        )
        .await
        .unwrap(),
        large
    );
    let state = persistence.snapshot();
    let record = &state.synchronous_executions[0];
    assert_eq!(record.state, SynchronousExecutionState::Completed);
    assert_eq!(record.result, None, "the store keeps no copy of it");
    assert!(record.terminal_bytes < 10 * 1024);
    assert_eq!(state.reserved_bytes, 0);

    // A replay is told it cannot be answered again and runs nothing a second time.
    let replay = core
        .run_synchronous(admission, "2026-09-06T00:00:03.000Z".to_owned(), 3_000)
        .await
        .unwrap_err();
    assert_eq!(replay.code, ErrorCode::ResourceLimit);
    assert_eq!(replay.operation, "filesystem.manage");
    assert_eq!(executions.started().len(), 1);
}

#[tokio::test]
async fn i3_g03_synchronous_settlement_measures_the_result_instead_of_executor_accounting() {
    let (core, persistence, _, executions, _, _) = make_core();
    let admission = synchronous_admission(5);
    executions.push(Ok(ExecutionCompletion {
        fence: fence(2),
        capability_generation: 2,
        outcome: ExecutionOutcome::SynchronousCompleted {
            result: serde_json::json!({"value": "x".repeat(20_000)}),
            encoded_bytes: 1,
        },
        cleanup_verified: true,
    }));

    assert_eq!(
        core.run_synchronous(admission, "2026-09-06T00:00:08.000Z".to_owned(), 8_000,)
            .await
            .unwrap_err()
            .code,
        ErrorCode::ResourceLimit
    );
    let state = persistence.snapshot();
    assert_eq!(state.reserved_bytes, 0);
    assert_eq!(
        state.synchronous_executions[0].state,
        SynchronousExecutionState::Failed
    );
    assert!(state.synchronous_executions[0].result.is_none());

    let mut admission = synchronous_admission(6);
    admission.settlement_bound_bytes = 16_384;
    let expected = serde_json::json!({"value": "small"});
    executions.push(Ok(ExecutionCompletion {
        fence: fence(2),
        capability_generation: 2,
        outcome: ExecutionOutcome::SynchronousCompleted {
            result: expected.clone(),
            encoded_bytes: u64::MAX,
        },
        cleanup_verified: true,
    }));
    assert_eq!(
        core.run_synchronous(admission, "2026-09-06T00:00:09.000Z".to_owned(), 9_000,)
            .await
            .unwrap(),
        expected
    );
}

#[tokio::test]
async fn i3_g02_concurrent_synchronous_duplicate_waits_for_the_original_execution() {
    let persistence = FakePersistence::default();
    let capabilities = FakeCapabilities::new(capability(2));
    let executions = GatedExecutions::new(ExecutionCompletion {
        fence: fence(2),
        capability_generation: 2,
        outcome: ExecutionOutcome::SynchronousCompleted {
            result: serde_json::json!({"deleted": true}),
            encoded_bytes: 128,
        },
        cleanup_verified: true,
    });
    let core = Arc::new(RuntimeCore::new(
        persistence.clone(),
        FakeArtifacts::default(),
        executions.clone(),
        capabilities.clone(),
        FakeHostControl::new(RecoveryProof::Clean).with_capabilities(capabilities),
    ));
    let admission = synchronous_admission(2);
    let first = {
        let core = Arc::clone(&core);
        let admission = admission.clone();
        tokio::spawn(async move {
            core.run_synchronous(admission, "2026-09-06T00:00:02.000Z".to_owned(), 2_000)
                .await
        })
    };
    executions
        .started
        .acquire()
        .await
        .expect("test start semaphore stays open")
        .forget();
    let admitted = persistence.snapshot();
    assert_eq!(admitted.synchronous_executions.len(), 1);
    assert_eq!(
        admitted.synchronous_executions[0].state,
        SynchronousExecutionState::Running
    );
    assert_eq!(admitted.reserved_bytes, 16_384);
    let duplicate = {
        let core = Arc::clone(&core);
        tokio::spawn(async move {
            core.run_synchronous(admission, "2026-09-06T00:00:03.000Z".to_owned(), 3_000)
                .await
        })
    };
    tokio::task::yield_now().await;
    assert_eq!(executions.starts.load(Ordering::SeqCst), 1);
    executions.release.add_permits(2);

    let first = tokio::time::timeout(Duration::from_secs(1), first)
        .await
        .expect("original synchronous request settled")
        .unwrap()
        .unwrap();
    let duplicate = tokio::time::timeout(Duration::from_secs(1), duplicate)
        .await
        .expect("duplicate synchronous request observed settlement")
        .unwrap()
        .unwrap();
    assert_eq!(first, serde_json::json!({"deleted": true}));
    assert_eq!(duplicate, first);
    assert_eq!(executions.starts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn i3_g02_synchronous_executor_is_revalidated_after_durable_admission() {
    let persisted = FakePersistence::default();
    let capabilities = FakeCapabilities::new(capability(2));
    let persistence = CapabilityChangingPersistence {
        inner: persisted.clone(),
        capabilities: capabilities.clone(),
    };
    let executions = FakeExecutions::default();
    let core = RuntimeCore::new(
        persistence,
        FakeArtifacts::default(),
        executions.clone(),
        capabilities.clone(),
        FakeHostControl::new(RecoveryProof::Clean).with_capabilities(capabilities),
    );

    assert_eq!(
        core.run_synchronous(
            synchronous_admission(4),
            "2026-09-06T00:00:07.000Z".to_owned(),
            7_000,
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::StaleAuthority
    );
    assert!(executions.started().is_empty());
    assert_eq!(persisted.snapshot().reserved_bytes, 0);
}

#[tokio::test]
async fn i3_g04_old_instance_synchronous_mutation_is_interrupted_and_never_replayed() {
    let admission = synchronous_admission(3);
    let mut state = RuntimeState::default();
    state
        .dedup
        .decide_and_reserve(
            admission.request_id.clone(),
            admission.payload_sha256.clone(),
            admission.now_ms,
            false,
        )
        .unwrap();
    let admitted = domain::resolve_executor(
        RuntimeHost::ApkRuntime,
        fence(2),
        capability(2).resolver_facts,
        admission.route,
    )
    .unwrap();
    state.reserved_bytes = 16_384;
    state
        .synchronous_executions
        .push(SynchronousExecutionRecord {
            request_id: admission.request_id.clone(),
            execution_id: admission.execution_id.clone(),
            operation: admission.operation.clone(),
            state: SynchronousExecutionState::Running,
            ended_at: None,
            executor: ExecutorRecord::from(&admitted),
            route: admission.route,
            payload: admission.payload.clone(),
            result: None,
            error: None,
            reserved_bytes: 16_384,
            terminal_bytes: 0,
        });
    let persistence = FakePersistence::with_state(state);
    let executions = FakeExecutions::default();
    let capabilities = FakeCapabilities::new(capability(9));
    let core = RuntimeCore::new(
        persistence.clone(),
        FakeArtifacts::default(),
        executions.clone(),
        capabilities.clone(),
        FakeHostControl::new(RecoveryProof::Clean).with_capabilities(capabilities),
    );

    assert_eq!(
        core.recover_old_instance(
            &fence(2).runtime_instance_id,
            "2026-09-06T00:00:05.000Z".to_owned(),
            5_000,
        )
        .await
        .unwrap(),
        1
    );
    let recovered = persistence.snapshot();
    assert_eq!(recovered.non_terminal_count(), 0);
    assert_eq!(recovered.reserved_bytes, 0);
    assert_eq!(
        recovered.synchronous_executions[0].state,
        SynchronousExecutionState::Interrupted
    );
    assert_eq!(
        core.run_synchronous(admission, "2026-09-06T00:00:06.000Z".to_owned(), 6_000,)
            .await
            .unwrap_err()
            .code,
        ErrorCode::IoError
    );
    assert!(executions.started().is_empty());
}

#[test]
fn i3_g03_downstream_ports_are_typed_without_host_business_forks() {
    fn assert_port_shapes<P, A, E, C, H>()
    where
        P: PersistencePort,
        A: ArtifactPort,
        E: ExecutionPort,
        C: CapabilityPort,
        H: HostControlPort,
    {
    }
    assert_port_shapes::<
        FakePersistence,
        FakeArtifacts,
        FakeExecutions,
        FakeCapabilities,
        FakeHostControl,
    >();

    let persistence = FakePersistence::default();
    let mut candidate = persistence.load().unwrap();
    candidate.revision = 1;
    persistence.compare_and_commit(0, candidate).unwrap();
    assert_eq!(
        persistence
            .compare_and_commit(0, RuntimeState::default())
            .unwrap_err()
            .code,
        ErrorCode::RevisionConflict
    );
    let host = FakeHostControl::new(RecoveryProof::Clean);
    host.prepare().unwrap();
    host.activate(&fence(2)).unwrap();
    assert_eq!(
        host.recover(&uuid(0x1000_0000, 1)).unwrap(),
        RecoveryProof::Clean
    );
}

#[tokio::test]
async fn i3_g04_full_capacity_preserves_settlement_and_old_instance_results_are_rejected() {
    let (core, persistence, _, executions, _, _) = make_core();
    executions.set_cancel_cleanup_verified(true);
    let mut task_ids = Vec::new();
    for index in 1..=256 {
        let request = admission(index);
        task_ids.push(request.task_id.clone());
        assert!(matches!(
            core.admit_task(request).await.unwrap(),
            TaskAdmissionResult::Admitted(_)
        ));
    }
    assert_eq!(
        core.admit_task(admission(257)).await.unwrap_err().code,
        ErrorCode::ResourceLimit
    );
    for (index, task_id) in task_ids.iter().enumerate() {
        let settled = core
            .cancel_task(
                task_id,
                "2026-09-06T01:00:00.000Z".to_owned(),
                1_000_000 + index as u64,
            )
            .await
            .unwrap();
        assert_eq!(settled.state, TaskState::Cancelled);
    }
    assert_eq!(persistence.snapshot().reserved_bytes, 0);

    let stale = admission(300);
    core.admit_task(stale.clone()).await.unwrap();
    executions.push(Ok(ExecutionCompletion {
        fence: fence(2),
        capability_generation: 1,
        outcome: ExecutionOutcome::Completed {
            result: TaskTerminalResult::Automation(AutomationTaskResult {
                automation_id: uuid(0x6000_0000, 2),
                execution_id: stale.execution_id.clone(),
                completed: True,
            }),
            encoded_bytes: 1024,
        },
        cleanup_verified: true,
    }));
    let result = core
        .run_task(
            &stale.task_id,
            "2026-09-06T02:00:00.000Z".to_owned(),
            2_000_000,
        )
        .await
        .unwrap();
    assert_eq!(result.state, TaskState::Interrupted);
    assert_eq!(result.error.unwrap().code, ErrorCode::IoError);
    assert_eq!(executions.started().len(), 1);

    let (core, _, _, executions, capabilities, _) = make_core();
    let stale_before_start = admission(301);
    core.admit_task(stale_before_start.clone()).await.unwrap();
    capabilities.set(capability(99));
    let result = core
        .run_task(
            &stale_before_start.task_id,
            "2026-09-06T02:00:02.000Z".to_owned(),
            2_000_001,
        )
        .await
        .unwrap();
    assert_eq!(result.state, TaskState::Interrupted);
    assert!(executions.started().is_empty());
}

#[tokio::test]
async fn i3_g05_readiness_blocks_admission_and_start_despite_provider_grants() {
    for readiness in [
        RuntimeReadiness::Initializing,
        RuntimeReadiness::Unavailable,
    ] {
        let (core, persistence, _, executions, capabilities, _) = make_core();
        let queued = admission(1);
        core.admit_task(queued.clone()).await.unwrap();
        let mut unavailable = capability(2);
        unavailable.context.readiness = readiness;
        capabilities.set(unavailable);
        let before = persistence.snapshot();
        assert_eq!(
            core.admit_task(admission(2)).await.unwrap_err().code,
            ErrorCode::CapabilityUnavailable
        );
        assert_eq!(persistence.snapshot().revision, before.revision);
        assert!(matches!(
            core.admit_task(queued.clone()).await.unwrap(),
            TaskAdmissionResult::Replay(_)
        ));
        assert_eq!(core.list_tasks(None, 100, 2).await.unwrap().len(), 1);
        let settled = core
            .run_task(&queued.task_id, "2026-09-06T00:00:01.000Z".to_owned(), 2000)
            .await
            .unwrap();
        assert_eq!(settled.state, TaskState::Failed);
        assert!(settled.started_at.is_none());
        let error = settled.error.unwrap();
        assert_eq!(error.code, ErrorCode::CapabilityUnavailable);
        assert_eq!(error.operation, "command.run");
        assert!(executions.started().is_empty());
        assert_eq!(persistence.snapshot().reserved_bytes, 0);
    }
}

#[tokio::test]
async fn oversized_completion_fails_before_terminal_state_is_selected() {
    let (core, persistence, _, executions, _, _) = make_core();
    let request = admission(1);
    core.admit_task(request.clone()).await.unwrap();
    executions.push(Ok(ExecutionCompletion {
        fence: fence(2),
        capability_generation: 2,
        outcome: ExecutionOutcome::Completed {
            result: TaskTerminalResult::Automation(AutomationTaskResult {
                automation_id: uuid(0x6000_0000, 1),
                execution_id: request.execution_id.clone(),
                completed: True,
            }),
            encoded_bytes: request.settlement_bound_bytes + 1,
        },
        cleanup_verified: true,
    }));
    let settled = core
        .run_task(
            &request.task_id,
            "2026-09-06T00:00:01.000Z".to_owned(),
            2000,
        )
        .await
        .unwrap();
    assert_eq!(settled.state, TaskState::Failed);
    assert!(settled.result.is_none());
    assert_eq!(settled.error.unwrap().code, ErrorCode::ResourceLimit);
    assert_eq!(persistence.snapshot().reserved_bytes, 0);
}

#[tokio::test]
async fn i3_g06_shared_ingress_preserves_protocol_operations_and_task_controls() {
    let (core, _, _, executions, capabilities, _) = make_core();
    let queued = admission(1);
    core.admit_task(queued.clone()).await.unwrap();
    let mut unavailable = capability(2);
    unavailable.context.readiness = RuntimeReadiness::Unavailable;
    capabilities.set(unavailable);
    let request_id = uuid(0x2000_0000, 10);
    let request = serde_json::json!({
        "protocol_version": 1, "request_id": request_id,
        "payload": {"tool": "task_control", "action": "get", "input": {"task_id": queued.task_id}}
    });
    let mut cases = Vec::new();
    let mut unsupported_version = request.clone();
    unsupported_version["protocol_version"] = 2.into();
    cases.push((
        unsupported_version,
        "PROTOCOL_INCOMPATIBLE",
        "contract.request",
        true,
    ));
    let mut bad_input = request.clone();
    bad_input["payload"]["input"]["task_id"] = "invalid".into();
    cases.push((bad_input, "INVALID_ARGUMENT", "task_control.get", true));
    let mut unknown_action = request.clone();
    unknown_action["payload"]["action"] = "retry".into();
    cases.push((unknown_action, "INVALID_ARGUMENT", "contract.request", true));
    let mut bad_id = request.clone();
    bad_id["request_id"] = "invalid".into();
    cases.push((bad_id, "INVALID_ARGUMENT", "task_control.get", false));
    let mut missing_task = request.clone();
    missing_task["payload"]["input"]["task_id"] =
        serde_json::to_value(uuid(0x3000_0000, 99)).unwrap();
    cases.push((missing_task, "NOT_FOUND", "task_control.get", true));
    for (request, code, operation, has_id) in cases {
        let response: serde_json::Value = serde_json::from_slice(
            &runtime::submit_public(
                &core,
                &serde_json::to_vec(&request).unwrap(),
                "2026-09-06T00:00:02.000Z".to_owned(),
                2000,
                true,
                |_| async {
                    panic!("invalid request or Task control reached installed-action adapter")
                },
            )
            .await,
        )
        .unwrap();
        assert_eq!(response["protocol_version"], 1);
        assert_eq!(response["outcome"], "error");
        assert_eq!(response["error"]["code"], code);
        assert_eq!(response["error"]["operation"], operation);
        assert_eq!(response.get("request_id").is_some(), has_id);
        assert!(response.get("result").is_none());
    }
    for action in ["get", "list", "cancel"] {
        let mut control = request.clone();
        control["payload"]["action"] = action.into();
        if action == "list" {
            control["payload"]["input"] = serde_json::json!({});
        }
        let response: serde_json::Value = serde_json::from_slice(
            &runtime::submit_public(
                &core,
                &serde_json::to_vec(&control).unwrap(),
                "2026-09-06T00:00:02.000Z".to_owned(),
                2000,
                true,
                |_| async { panic!("Task control reached installed-action adapter") },
            )
            .await,
        )
        .unwrap();
        assert_eq!(response["outcome"], "success");
        assert_eq!(
            response["request_id"],
            serde_json::to_value(&request_id).unwrap()
        );
        if action == "cancel" {
            assert_eq!(response["result"]["state"], "cancelled");
        }
    }
    assert!(executions.started().is_empty());
    let other = serde_json::json!({"protocol_version": 1, "request_id": request_id,
        "payload": {"tool": "context", "action": "status", "input": {}}});
    let response: serde_json::Value = serde_json::from_slice(
        &runtime::submit_public(
            &core,
            &serde_json::to_vec(&other).unwrap(),
            "2026-09-06T00:00:02.000Z".to_owned(),
            2000,
            true,
            |request| async move {
                assert!(matches!(
                    request.payload,
                    contract::PublicPayload::Context { .. }
                ));
                Err(domain::DomainError::new(
                    ErrorCode::Unsupported,
                    "adapter unavailable",
                ))
            },
        )
        .await,
    )
    .unwrap();
    assert_eq!(response["error"]["operation"], "context.status");
    assert_eq!(response["error"]["code"], "UNSUPPORTED");
    // The Runtime's own reason travels with the code; only a pending host transition is retryable.
    assert_eq!(
        response["error"]["details"]["reason"],
        "adapter unavailable"
    );
    assert_eq!(response["error"]["retryable"], false);

    let command = serde_json::json!({"protocol_version": 1, "request_id": request_id,
        "payload": {"tool": "command", "action": "run", "input": {"command": "id", "run_as": "app"}}});
    let response: serde_json::Value = serde_json::from_slice(
        &runtime::submit_public(
            &core,
            &serde_json::to_vec(&command).unwrap(),
            "2026-09-06T00:00:02.000Z".to_owned(),
            2000,
            false,
            |_| async { panic!("a closed admission reached the installed-action adapter") },
        )
        .await,
    )
    .unwrap();
    assert_eq!(response["error"]["code"], "HOST_TRANSITION_PENDING");
    assert_eq!(response["error"]["retryable"], true);
    assert_eq!(
        response["error"]["details"]["reason"],
        "Runtime host transition is pending"
    );
}

#[tokio::test]
async fn i3_g07_composite_execution_dispatches_only_its_closed_typed_variant() {
    let delegate = FakeExecutions::default();
    let composite = CompositeExecutionSurface::new(delegate.clone());
    let admitted = AdmittedExecution {
        execution_id: uuid(0x4000_0000, 80),
        task_id: None,
        executor: ExecutorRecord {
            host: RuntimeHost::ApkRuntime,
            provider: ProviderToken::AppNative,
            execution_class: contract::ExecutionClass::App,
            capability_generation: 2,
            fence: contract::Fence {
                runtime_epoch: fence(2).runtime_epoch,
                host_generation: 1,
                runtime_instance_id: fence(2).runtime_instance_id,
            },
        },
        payload: ExecutionPayload::FilesystemCall(FilesystemCall::Inspect(
            FilesystemInspectInput {
                target: FileTarget {
                    target_type: FileTargetType::Path,
                    value: "/data/local/tmp".to_owned(),
                },
                recursive: false,
                max_depth: 1,
                max_entries: 1,
            },
        )),
    };
    delegate.push(Ok(ExecutionCompletion {
        fence: fence(2),
        capability_generation: 2,
        outcome: ExecutionOutcome::SynchronousCompleted {
            result: serde_json::json!({"delegated": true}),
            encoded_bytes: 18,
        },
        cleanup_verified: true,
    }));
    let completion = composite.claim_and_start(admitted.clone()).await.unwrap();
    assert_eq!(delegate.started(), vec![admitted.clone()]);
    assert!(matches!(
        completion.outcome,
        ExecutionOutcome::SynchronousCompleted { .. }
    ));

    let unsupported = AdmittedExecution {
        execution_id: uuid(0x4000_0000, 81),
        payload: ExecutionPayload::OpaqueOperation("command.run".to_owned()),
        ..admitted
    };
    let failure = composite.claim_and_start(unsupported).await.unwrap_err();
    assert_eq!(failure.error.code, ErrorCode::Unsupported);
    assert!(failure.cleanup_verified);
    assert_eq!(delegate.started().len(), 1);
}

#[test]
fn cleanup_report_requires_a_shared_readiness_owner() {
    let host = FakeHostControl::new(RecoveryProof::CleanupUnverified);
    assert_eq!(
        host.cleanup_unverified(&fence(2), &uuid(0x4000_0000, 1))
            .unwrap_err()
            .code,
        ErrorCode::InternalError
    );
    assert_eq!(
        host.recover(&uuid(0x1000_0000, 1)).unwrap_err().code,
        ErrorCode::InternalError
    );
    let capabilities = FakeCapabilities::new(capability(2));
    let host = host.with_capabilities(capabilities.clone());
    assert_eq!(
        host.recover(&uuid(0x1000_0000, 1)).unwrap(),
        RecoveryProof::CleanupUnverified
    );
    assert_eq!(
        capabilities.current().unwrap().context.readiness,
        RuntimeReadiness::Unavailable
    );
}
