use chrono::{DateTime, Duration, SecondsFormat, Utc};
use contract::{
    Availability, CapabilityState, ErrorCode, GrantFacts, MotherTool, RunAs, RuntimeHost,
    RuntimeReadiness, TaskControlCall, TaskGetInput, TaskListInput, TaskState, UuidV4,
};
use domain::{
    AdmissionFence, CapabilityContext, DomainError, ExecutorRequest, ProviderGenerations,
    ResolverFacts, TaskEvent,
};
use runtime::{
    CapabilityPort, CapabilitySnapshot, ExecutionPayload, PersistencePort, RecoveryProof,
    RuntimeCore, TaskAdmission, TaskAdmissionResult,
    fakes::{FakeArtifacts, FakeCapabilities, FakeExecutions, FakeHostControl, FakePersistence},
};
use std::{sync::Arc, time::Duration as StdDuration};

const DAY_MS: u64 = 86_400_000;

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
        runtime_epoch: uuid(0x8100_0000, 1),
        host_generation: 1,
        runtime_instance_id: uuid(0x8100_0000, instance),
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
    FakeExecutions,
    FakeCapabilities,
    FakeHostControl,
) {
    let persistence = FakePersistence::default();
    let executions = FakeExecutions::default();
    let capabilities = FakeCapabilities::new(capability(2));
    let host = FakeHostControl::new(RecoveryProof::Clean).with_capabilities(capabilities.clone());
    let core = RuntimeCore::new(
        persistence.clone(),
        FakeArtifacts::default(),
        executions.clone(),
        capabilities.clone(),
        host.clone(),
    );
    (core, persistence, executions, capabilities, host)
}

async fn public_task_call(
    core: &TestCore,
    request_id: UuidV4,
    action: &str,
    input: serde_json::Value,
    now_ms: u64,
) -> serde_json::Value {
    let request = serde_json::json!({
        "protocol_version": 1,
        "request_id": request_id,
        "payload": {
            "tool": "task_control",
            "action": action,
            "input": input,
        },
    });
    serde_json::from_slice(
        &runtime::submit_public(
            core,
            &serde_json::to_vec(&request).unwrap(),
            "2026-09-08T00:00:01.000Z".to_owned(),
            now_ms,
            true,
            |_| async { panic!("Task control escaped the shared Runtime ingress") },
        )
        .await,
    )
    .unwrap()
}

fn instant(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .unwrap()
        .with_timezone(&Utc)
}

fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn millis(value: DateTime<Utc>) -> u64 {
    value.timestamp_millis().try_into().unwrap()
}

fn admission(index: u64, created_at: DateTime<Utc>, now_ms: u64) -> TaskAdmission {
    TaskAdmission {
        request_id: uuid(0x8200_0000, index),
        payload_sha256: format!("{index:064x}"),
        task_id: uuid(0x8300_0000, index),
        execution_id: uuid(0x8400_0000, index),
        tool: MotherTool::Command,
        action: "run".to_owned(),
        route: ExecutorRequest::Command(RunAs::App),
        payload: ExecutionPayload::OpaqueOperation("command.run".to_owned()),
        created_at: timestamp(created_at),
        settlement_bound_bytes: 16_384,
        now_ms,
    }
}

#[tokio::test]
async fn i8_task_g01_encoded_public_list_get_cancel_share_one_registry_and_exact_projections() {
    let (core, persistence, executions, _, _) = make_core();
    let base = instant("2026-09-08T00:00:00.000Z");
    let older = admission(1, base, millis(base));
    let newer = admission(2, base + Duration::milliseconds(2), millis(base) + 2);
    core.admit_task(newer.clone()).await.unwrap();
    core.admit_task(older.clone()).await.unwrap();

    let listed = public_task_call(
        &core,
        uuid(0x8200_0010, 1),
        "list",
        serde_json::json!({"states": ["queued"], "limit": 1}),
        millis(base) + 1_000,
    )
    .await;
    assert_eq!(listed["outcome"], "success");
    let listed = &listed["result"];
    assert_eq!(listed["tasks"].as_array().unwrap().len(), 1);
    assert_eq!(listed["tasks"][0]["task_id"], newer.task_id.as_str());
    assert_eq!(
        listed["tasks"][0]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        vec!["action", "created_at", "state", "task_id", "tool"]
    );

    let fetched = public_task_call(
        &core,
        uuid(0x8200_0010, 2),
        "get",
        serde_json::json!({"task_id": older.task_id}),
        millis(base) + 1_000,
    )
    .await;
    assert_eq!(fetched["outcome"], "success");
    let fetched = &fetched["result"];
    assert_eq!(fetched["task_id"], older.task_id.as_str());
    assert_eq!(fetched["state"], "queued");
    assert_eq!(fetched["cancel_requested"], false);
    assert!(fetched.get("result").is_none());
    assert!(fetched.get("error").is_none());

    let cancelled = public_task_call(
        &core,
        uuid(0x8200_0010, 3),
        "cancel",
        serde_json::json!({"task_id": older.task_id}),
        millis(base) + 1_000,
    )
    .await;
    assert_eq!(cancelled["outcome"], "success");
    assert_eq!(cancelled["result"]["state"], "cancelled");
    assert_eq!(cancelled["result"]["cancel_requested"], true);
    assert!(executions.cancelled().is_empty());
    assert_eq!(
        persistence
            .snapshot()
            .task(&older.task_id)
            .unwrap()
            .snapshot(),
        serde_json::from_value(cancelled["result"].clone()).unwrap()
    );

    let missing = public_task_call(
        &core,
        uuid(0x8200_0010, 4),
        "get",
        serde_json::json!({"task_id": uuid(0x8300_0000, 99)}),
        millis(base) + 1_000,
    )
    .await;
    assert_eq!(missing["outcome"], "error");
    assert_eq!(missing["error"]["code"], "NOT_FOUND");
    assert_eq!(missing["error"]["operation"], "task_control.get");
}

#[tokio::test]
async fn i8_task_g04_retention_preserves_replay_then_prunes_by_count_and_age() {
    let (core, persistence, executions, _, _) = make_core();
    executions.set_cancel_cleanup_verified(true);
    let base = instant("2026-09-08T00:00:00.000Z");
    let base_ms = millis(base);

    for index in 1..=500 {
        let request = admission(
            index,
            base + Duration::milliseconds(index as i64),
            base_ms + index,
        );
        assert!(matches!(
            core.admit_task(request.clone()).await.unwrap(),
            TaskAdmissionResult::Admitted(_)
        ));
        core.cancel_task(
            &request.task_id,
            timestamp(base + Duration::seconds(1)),
            base_ms + 1_000,
        )
        .await
        .unwrap();
    }
    assert_eq!(persistence.snapshot().tasks.len(), 500);
    assert_eq!(
        core.admit_task(admission(501, base + Duration::seconds(2), base_ms + 2_000))
            .await
            .unwrap_err()
            .code,
        ErrorCode::ResourceLimit
    );

    let after_replay_window = base_ms + DAY_MS + 2_000;
    let newest = admission(
        501,
        base + Duration::days(1) + Duration::milliseconds(1),
        after_replay_window,
    );
    core.admit_task(newest.clone()).await.unwrap();
    core.cancel_task(
        &newest.task_id,
        timestamp(base + Duration::days(1) + Duration::seconds(1)),
        after_replay_window + 1_000,
    )
    .await
    .unwrap();
    let retained = persistence.snapshot();
    assert_eq!(retained.tasks.len(), 500);
    assert!(retained.task(&uuid(0x8300_0000, 1)).is_none());
    assert!(retained.task(&newest.task_id).is_some());

    let after_history_window = base + Duration::days(9);
    let listed = core
        .handle_task_control(
            TaskControlCall::List(TaskListInput {
                states: None,
                limit: 500,
            }),
            timestamp(after_history_window),
            millis(after_history_window),
        )
        .await
        .unwrap();
    assert!(listed["tasks"].as_array().unwrap().is_empty());
    assert!(persistence.snapshot().tasks.is_empty());

    let expired = admission(700, after_history_window, millis(after_history_window));
    core.admit_task(expired.clone()).await.unwrap();
    core.cancel_task(
        &expired.task_id,
        timestamp(after_history_window + Duration::seconds(1)),
        millis(after_history_window) + 1_000,
    )
    .await
    .unwrap();
    let past_terminal_retention = after_history_window + Duration::days(8);
    assert_eq!(
        core.cancel_task(
            &expired.task_id,
            timestamp(past_terminal_retention),
            millis(past_terminal_retention),
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::NotFound
    );
    assert!(persistence.snapshot().tasks.is_empty());
}

#[tokio::test]
async fn i8_task_g02_cancellation_has_one_owner_and_durable_truth() {
    let (core, persistence, executions, _, _) = make_core();
    executions.set_cancel_cleanup_verified(true);
    let base = instant("2026-09-08T00:00:00.000Z");
    let queued = admission(1, base, millis(base));
    core.admit_task(queued.clone()).await.unwrap();

    let cancelled = core
        .handle_task_control(
            TaskControlCall::Cancel(TaskGetInput {
                task_id: queued.task_id.clone(),
            }),
            timestamp(base + Duration::seconds(1)),
            millis(base) + 1_000,
        )
        .await
        .unwrap();
    assert_eq!(cancelled["state"], "cancelled");
    assert_eq!(cancelled["cancel_requested"], true);
    assert_eq!(cancelled["error"]["operation"], "command.run");
    assert_eq!(executions.cancelled().len(), 0);
    assert_eq!(persistence.snapshot().dedup.entries().len(), 1);
    let revision = persistence.snapshot().revision;

    let repeated = core
        .handle_task_control(
            TaskControlCall::Cancel(TaskGetInput {
                task_id: queued.task_id,
            }),
            timestamp(base + Duration::seconds(2)),
            millis(base) + 2_000,
        )
        .await
        .unwrap();
    assert_eq!(repeated, cancelled);
    assert_eq!(persistence.snapshot().revision, revision);
    assert_eq!(executions.cancelled().len(), 0);

    let running = admission(2, base + Duration::seconds(3), millis(base) + 3_000);
    core.admit_task(running.clone()).await.unwrap();
    let mut state = persistence.load().unwrap();
    let expected = state.revision;
    let record = state
        .tasks
        .iter_mut()
        .find(|task| task.task_id == running.task_id)
        .unwrap();
    record.lifecycle.apply(TaskEvent::Start).unwrap();
    record.started_at = Some(timestamp(base + Duration::seconds(4)));
    state.revision += 1;
    persistence.compare_and_commit(expected, state).unwrap();

    executions.set_cancel_error(Some(DomainError::new(
        ErrorCode::IoError,
        "executor rejected cancellation",
    )));
    let failed = core
        .cancel_task(
            &running.task_id,
            timestamp(base + Duration::seconds(5)),
            millis(base) + 5_000,
        )
        .await
        .unwrap_err();
    assert_eq!(failed.code, ErrorCode::CancelFailed);
    let durable = persistence
        .snapshot()
        .task(&running.task_id)
        .unwrap()
        .snapshot();
    assert_eq!(durable.state, TaskState::Running);
    assert!(durable.cancel_requested);
    assert_eq!(executions.cancelled(), vec![running.execution_id]);

    let current = core
        .cancel_task(
            &running.task_id,
            timestamp(base + Duration::seconds(6)),
            millis(base) + 6_000,
        )
        .await
        .unwrap();
    assert_eq!(current, durable);
    assert_eq!(executions.cancelled().len(), 1);

    let (
        uncertain_core,
        uncertain_store,
        uncertain_executions,
        uncertain_capabilities,
        uncertain_host,
    ) = make_core();
    let uncertain = admission(3, base + Duration::seconds(7), millis(base) + 7_000);
    uncertain_core.admit_task(uncertain.clone()).await.unwrap();
    let mut state = uncertain_store.load().unwrap();
    let expected = state.revision;
    let record = state
        .tasks
        .iter_mut()
        .find(|task| task.task_id == uncertain.task_id)
        .unwrap();
    record.lifecycle.apply(TaskEvent::Start).unwrap();
    record.started_at = Some(timestamp(base + Duration::seconds(8)));
    state.revision += 1;
    uncertain_store.compare_and_commit(expected, state).unwrap();
    uncertain_executions.set_cancel_cleanup_verified(false);
    let interrupted = uncertain_core
        .cancel_task(
            &uncertain.task_id,
            timestamp(base + Duration::seconds(9)),
            millis(base) + 9_000,
        )
        .await
        .unwrap();
    assert_eq!(interrupted.state, TaskState::Interrupted);
    assert!(interrupted.cancel_requested);
    assert_eq!(interrupted.error.unwrap().code, ErrorCode::IoError);
    assert_eq!(
        uncertain_host.cleanup_reports(),
        vec![(fence(2), uncertain.execution_id)]
    );
    assert_eq!(
        uncertain_capabilities.current().unwrap().context.readiness,
        RuntimeReadiness::Unavailable
    );

    let (clean_core, clean_store, clean_executions, _, clean_host) = make_core();
    let clean = admission(4, base + Duration::seconds(10), millis(base) + 10_000);
    clean_core.admit_task(clean.clone()).await.unwrap();
    let mut state = clean_store.load().unwrap();
    let expected = state.revision;
    let record = state
        .tasks
        .iter_mut()
        .find(|task| task.task_id == clean.task_id)
        .unwrap();
    record.lifecycle.apply(TaskEvent::Start).unwrap();
    record.started_at = Some(timestamp(base + Duration::seconds(11)));
    state.revision += 1;
    clean_store.compare_and_commit(expected, state).unwrap();
    clean_executions.set_cancel_cleanup_verified(true);
    let stopped = clean_core
        .cancel_task(
            &clean.task_id,
            timestamp(base + Duration::seconds(12)),
            millis(base) + 12_000,
        )
        .await
        .unwrap();
    assert_eq!(stopped.state, TaskState::Cancelled);
    assert!(stopped.cancel_requested);
    assert_eq!(stopped.error.unwrap().operation, "command.run");
    assert_eq!(clean_executions.cancelled(), vec![clean.execution_id]);
    assert!(clean_host.cleanup_reports().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn i8_task_g02_cancel_after_running_commit_cannot_start_effect_after_clean_cancellation() {
    let (core, persistence, executions, _, _) = make_core();
    let core = Arc::new(core);
    executions.set_cancel_cleanup_verified(true);
    let release_effect = executions.pause_before_effect();
    let base = instant("2026-09-08T01:00:00.000Z");
    let request = admission(50, base, millis(base));
    core.admit_task(request.clone()).await.unwrap();
    let runner = {
        let core = Arc::clone(&core);
        let task_id = request.task_id.clone();
        tokio::spawn(async move {
            core.run_task(
                &task_id,
                "2026-09-08T01:00:01.000Z".to_owned(),
                millis(base) + 3_000,
            )
            .await
        })
    };
    tokio::time::timeout(StdDuration::from_secs(1), async {
        while executions.started().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("execution claim was established");
    assert_eq!(
        persistence
            .snapshot()
            .task(&request.task_id)
            .unwrap()
            .state(),
        TaskState::Running
    );

    let canceller = {
        let core = Arc::clone(&core);
        let task_id = request.task_id.clone();
        tokio::spawn(async move {
            core.cancel_task(
                &task_id,
                "2026-09-08T01:00:02.000Z".to_owned(),
                millis(base) + 2_000,
            )
            .await
        })
    };
    tokio::time::timeout(StdDuration::from_secs(1), async {
        while !executions.cancellation_reached_claim(&request.execution_id) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cancellation reached the shared execution claim");
    assert!(
        !canceller.is_finished(),
        "cancelled must wait for the claimed executor to verify cleanup"
    );
    release_effect.add_permits(1);
    let cancelled = canceller
        .await
        .expect("cancellation task completed")
        .unwrap();
    assert_eq!(cancelled.state, TaskState::Cancelled);
    assert!(cancelled.cancel_requested);
    assert!(executions.effects().is_empty());
    let runner_result = tokio::time::timeout(StdDuration::from_secs(1), runner)
        .await
        .expect("claimed execution observed cancellation")
        .unwrap()
        .unwrap();
    assert_eq!(runner_result, cancelled);
    assert!(executions.effects().is_empty());
}

#[tokio::test]
async fn i8_task_g03_admitted_executor_and_host_generation_never_change() {
    let (core, persistence, executions, capabilities, host) = make_core();
    executions.set_cancel_cleanup_verified(true);
    let base = instant("2026-09-08T00:00:00.000Z");
    let request = admission(1, base, millis(base));
    let admitted = match core.admit_task(request.clone()).await.unwrap() {
        TaskAdmissionResult::Admitted(snapshot) => snapshot,
        TaskAdmissionResult::Replay(_) => panic!("unexpected replay"),
    };
    let admitted_executor = persistence
        .snapshot()
        .task(&request.task_id)
        .unwrap()
        .executor()
        .cloned()
        .expect("a request Task keeps its admitted executor");

    let mut replacement = capability(99);
    replacement.fence.host_generation = 2;
    capabilities.set(replacement);
    let fetched = core
        .handle_task_control(
            TaskControlCall::Get(TaskGetInput {
                task_id: request.task_id.clone(),
            }),
            timestamp(base + Duration::seconds(1)),
            millis(base) + 1_000,
        )
        .await
        .unwrap();
    assert_eq!(fetched["execution_class"], "app");
    assert_eq!(
        admitted.execution_class.unwrap(),
        admitted_executor.execution_class
    );

    let interrupted = core
        .run_task(
            &request.task_id,
            timestamp(base + Duration::seconds(2)),
            millis(base) + 3_000,
        )
        .await
        .unwrap();
    assert_eq!(interrupted.state, TaskState::Interrupted);
    assert_eq!(
        interrupted.error.as_ref().unwrap().code,
        ErrorCode::StaleAuthority
    );
    assert!(executions.started().is_empty());
    assert!(host.cleanup_reports().is_empty());
    assert_eq!(
        capabilities.current().unwrap().context.readiness,
        RuntimeReadiness::Ready
    );

    core.cancel_task(
        &request.task_id,
        timestamp(base + Duration::seconds(4)),
        millis(base) + 4_000,
    )
    .await
    .unwrap();
    let settled_executor = persistence
        .snapshot()
        .task(&request.task_id)
        .unwrap()
        .executor()
        .cloned()
        .expect("a request Task keeps its admitted executor");
    assert_eq!(settled_executor, admitted_executor);
    assert_eq!(settled_executor.fence.host_generation, 1);
    assert_eq!(settled_executor.capability_generation, 2);
}
