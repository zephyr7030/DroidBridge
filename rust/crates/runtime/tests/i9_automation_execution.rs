//! I9 AutomationExecution admission, container Task, deletion and recovery gates.

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use contract::{
    Availability, CapabilityState, ErrorCode, GrantFacts, PublicError, RuntimeHost,
    RuntimeReadiness, TaskState, UuidV4,
};
use domain::{AdmissionFence, CapabilityContext, ProviderGenerations, ResolverFacts};
use runtime::{
    AutomationAdmission, AutomationExecutionOutcome, CapabilitySnapshot, RecoveryProof,
    RuntimeCore,
    fakes::{FakeArtifacts, FakeCapabilities, FakeExecutions, FakeHostControl, FakePersistence},
};
use serde_json::{Value, json};

type TestCore =
    RuntimeCore<FakePersistence, FakeArtifacts, FakeExecutions, FakeCapabilities, FakeHostControl>;

const SAVED: &str = "2026-09-14T08:00:00.000Z";

fn uuid(value: u64) -> UuidV4 {
    UuidV4::parse(format!("99200000-0000-4000-8000-{value:012x}")).unwrap()
}

fn available() -> Availability {
    Availability {
        state: CapabilityState::Available,
        reason: None,
    }
}

fn capability() -> CapabilitySnapshot {
    let state = CapabilityState::Available;
    CapabilitySnapshot {
        grants: GrantFacts {
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
                app_native: 1,
                app_framework: 1,
                shizuku: 1,
                magisk_native: 1,
                magisk_framework: 1,
                accessibility: 1,
                media_projection: 1,
                notification_listener: 1,
            },
        },
        fence: AdmissionFence {
            runtime_epoch: uuid(1),
            host_generation: 1,
            runtime_instance_id: uuid(2),
        },
    }
}

fn make_core() -> (TestCore, FakePersistence) {
    let persistence = FakePersistence::default();
    let capabilities = FakeCapabilities::new(capability());
    let core = RuntimeCore::new(
        persistence.clone(),
        FakeArtifacts::default(),
        FakeExecutions::default(),
        capabilities.clone(),
        FakeHostControl::new(RecoveryProof::Clean).with_capabilities(capabilities),
    );
    (core, persistence)
}

fn at_minutes(minutes: i64, seconds: i64) -> String {
    let saved = DateTime::parse_from_rfc3339(SAVED)
        .unwrap()
        .with_timezone(&Utc);
    (saved + Duration::minutes(minutes) + Duration::seconds(seconds))
        .to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn millis(timestamp: &str) -> u64 {
    u64::try_from(
        DateTime::parse_from_rfc3339(timestamp)
            .unwrap()
            .timestamp_millis(),
    )
    .unwrap()
}

async fn public(
    core: &TestCore,
    request_id: u64,
    tool: &str,
    action: &str,
    input: Value,
    timestamp: &str,
) -> Value {
    let request = json!({
        "protocol_version": 1,
        "request_id": uuid(0x1000 + request_id),
        "payload": {"tool": tool, "action": action, "input": input},
    });
    serde_json::from_slice(
        &runtime::submit_public(
            core,
            &serde_json::to_vec(&request).unwrap(),
            timestamp.to_owned(),
            millis(timestamp),
            true,
            |_| async { panic!("request escaped the shared Runtime ingress") },
        )
        .await,
    )
    .unwrap()
}

fn result(response: &Value) -> &Value {
    assert_eq!(response["outcome"], "success", "{response}");
    &response["result"]
}

async fn saved_interval(core: &TestCore) -> UuidV4 {
    let saved = public(
        core,
        1,
        "automation",
        "save",
        json!({
            "name": "minutely",
            "enabled": true,
            "trigger": {"type": "interval", "every_ms": 60_000},
            "action": {"type": "delay", "duration_ms": 1},
        }),
        SAVED,
    )
    .await;
    UuidV4::parse(result(&saved)["automation_id"].as_str().unwrap().to_owned()).unwrap()
}

fn admitted(admission: AutomationAdmission) -> runtime::AdmittedAutomationExecution {
    match admission {
        AutomationAdmission::Admitted(execution) => *execution,
        other => panic!("expected admission, got {other:?}"),
    }
}

fn failure(code: ErrorCode) -> PublicError {
    PublicError {
        code,
        operation: "automation.execution".to_owned(),
        retryable: false,
        message: None,
        capability: None,
        details: None,
    }
}

#[tokio::test]
async fn i9_g01_due_admission_creates_one_execution_and_advances_the_due_in_one_commit() {
    let (core, persistence) = make_core();
    let automation_id = saved_interval(&core).await;
    let revision = persistence.snapshot().revision;

    let early = core
        .admit_due_automation(&automation_id, at_minutes(0, 30))
        .await
        .unwrap();
    assert_eq!(early, AutomationAdmission::NotDue);
    assert_eq!(persistence.snapshot().revision, revision);

    // Ten missed minutes admit one execution for the persisted due and coalesce the rest.
    let execution = admitted(
        core.admit_due_automation(&automation_id, at_minutes(10, 30))
            .await
            .unwrap(),
    );
    assert_eq!(execution.summary.triggered_at, at_minutes(1, 0));
    let stored = persistence.snapshot();
    assert_eq!(stored.revision, revision + 1);
    let record = &stored.automations[0];
    assert_eq!(
        record.next_due_at.as_deref(),
        Some(at_minutes(11, 0).as_str())
    );
    assert_eq!(
        record.active_execution_id.as_ref(),
        Some(&execution.summary.execution_id)
    );
    let task = stored.task(&execution.summary.task_id).unwrap();
    assert_eq!(
        (task.tool, task.action.as_str()),
        (contract::MotherTool::Automation, "execution")
    );
    assert_eq!(task.state(), TaskState::Queued);
    assert!(task.request_id().is_none() && task.executor().is_none());

    let task_view = public(
        &core,
        2,
        "task_control",
        "get",
        json!({"task_id": execution.summary.task_id}),
        &at_minutes(10, 31),
    )
    .await;
    assert_eq!(result(&task_view)["action"], "execution");
    assert!(result(&task_view).get("execution_class").is_none());

    // A busy Automation keeps its due and never overlaps.
    let busy = core
        .admit_due_automation(&automation_id, at_minutes(12, 0))
        .await
        .unwrap();
    assert_eq!(busy, AutomationAdmission::BusyDropped);
    let stored = persistence.snapshot();
    assert_eq!(stored.automation_executions.len(), 1);
    assert_eq!(
        stored.automations[0].next_due_at.as_deref(),
        Some(at_minutes(11, 0).as_str())
    );
}

#[tokio::test]
async fn i9_g08_completed_execution_keeps_its_task_result_and_releases_ownership() {
    let (core, persistence) = make_core();
    let automation_id = saved_interval(&core).await;
    let execution = admitted(
        core.admit_due_automation(&automation_id, at_minutes(1, 0))
            .await
            .unwrap(),
    );
    let execution_id = execution.summary.execution_id.clone();
    let running = core
        .start_automation_execution(&execution_id, at_minutes(1, 1))
        .await
        .unwrap();
    assert_eq!(running.state, contract::AutomationExecutionState::Running);
    let settled = core
        .settle_automation_execution(
            &execution_id,
            AutomationExecutionOutcome::Completed,
            at_minutes(1, 2),
        )
        .await
        .unwrap();
    assert_eq!(settled.state, contract::AutomationExecutionState::Completed);
    assert_eq!(settled.error_code, None);

    let task_view = public(
        &core,
        2,
        "task_control",
        "get",
        json!({"task_id": execution.summary.task_id}),
        &at_minutes(1, 3),
    )
    .await;
    assert_eq!(result(&task_view)["state"], "completed");
    assert_eq!(
        result(&task_view)["result"],
        json!({"automation_id": automation_id, "execution_id": execution_id, "completed": true})
    );
    assert_eq!(
        persistence.snapshot().automations[0].active_execution_id,
        None
    );
    let history = public(
        &core,
        3,
        "automation",
        "get",
        json!({"automation_id": automation_id}),
        &at_minutes(1, 3),
    )
    .await;
    assert_eq!(result(&history)["history"][0]["state"], "completed");

    let again = core
        .settle_automation_execution(
            &execution_id,
            AutomationExecutionOutcome::Completed,
            at_minutes(1, 4),
        )
        .await
        .unwrap_err();
    assert_eq!(again.code, ErrorCode::StaleAuthority);
}

#[tokio::test]
async fn i9_g08_deleted_in_flight_automation_is_finalized_once_at_settlement() {
    let (core, persistence) = make_core();
    let automation_id = saved_interval(&core).await;
    let execution = admitted(
        core.admit_due_automation(&automation_id, at_minutes(1, 0))
            .await
            .unwrap(),
    );
    let execution_id = execution.summary.execution_id.clone();
    core.start_automation_execution(&execution_id, at_minutes(1, 1))
        .await
        .unwrap();

    let deleted = public(
        &core,
        2,
        "automation",
        "delete",
        json!({"automation_id": automation_id, "expected_revision": 1}),
        &at_minutes(1, 2),
    )
    .await;
    assert_eq!(result(&deleted)["previous_revision"], 1);
    let stored = persistence.snapshot();
    assert!(stored.automations[0].deleted_at.is_some());
    assert_eq!(stored.automations[0].next_due_at, None);
    let fresh = public(
        &core,
        3,
        "automation",
        "delete",
        json!({"automation_id": automation_id, "expected_revision": 2}),
        &at_minutes(1, 3),
    )
    .await;
    assert_eq!(fresh["error"]["code"], "NOT_FOUND");
    let listed = public(&core, 4, "automation", "list", json!({}), &at_minutes(1, 3)).await;
    assert_eq!(result(&listed)["automations"], json!([]));
    assert_eq!(
        core.admit_due_automation(&automation_id, at_minutes(3, 0))
            .await
            .unwrap(),
        AutomationAdmission::NotDue
    );

    let settled = core
        .settle_automation_execution(
            &execution_id,
            AutomationExecutionOutcome::Failed(failure(ErrorCode::ExecutionFailed)),
            at_minutes(1, 4),
        )
        .await
        .unwrap();
    assert_eq!(settled.error_code.as_deref(), Some("EXECUTION_FAILED"));
    let stored = persistence.snapshot();
    assert!(stored.automations.is_empty());
    assert!(stored.automation_executions.is_empty());
    // The generic Task stays queryable under Task retention.
    assert_eq!(
        stored.task(&execution.summary.task_id).unwrap().state(),
        TaskState::Failed
    );
    assert_eq!(
        core.settle_automation_execution(
            &execution_id,
            AutomationExecutionOutcome::Completed,
            at_minutes(1, 5),
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::NotFound
    );
}

#[tokio::test]
async fn i9_g08_host_loss_interrupts_executions_and_finalizes_their_tombstones() {
    let (core, persistence) = make_core();
    let automation_id = saved_interval(&core).await;
    let execution = admitted(
        core.admit_due_automation(&automation_id, at_minutes(1, 0))
            .await
            .unwrap(),
    );
    public(
        &core,
        2,
        "automation",
        "delete",
        json!({"automation_id": automation_id, "expected_revision": 1}),
        &at_minutes(1, 1),
    )
    .await;

    let interrupted = core
        .recover_old_instance(&uuid(2), at_minutes(2, 0), millis(&at_minutes(2, 0)))
        .await
        .unwrap();
    assert_eq!(interrupted, 1);
    let stored = persistence.snapshot();
    assert!(stored.automations.is_empty());
    assert!(stored.automation_executions.is_empty());
    let task = stored.task(&execution.summary.task_id).unwrap();
    assert_eq!(task.state(), TaskState::Interrupted);
    assert_eq!(task.error.as_ref().unwrap().code, ErrorCode::IoError);
    assert_eq!(stored.reserved_bytes, 0);
}

#[tokio::test]
async fn i9_g08_cancelling_a_queued_container_task_settles_its_execution() {
    let (core, persistence) = make_core();
    let automation_id = saved_interval(&core).await;
    let execution = admitted(
        core.admit_due_automation(&automation_id, at_minutes(1, 0))
            .await
            .unwrap(),
    );
    let cancelled = public(
        &core,
        2,
        "task_control",
        "cancel",
        json!({"task_id": execution.summary.task_id}),
        &at_minutes(1, 1),
    )
    .await;
    assert_eq!(result(&cancelled)["state"], "cancelled");
    assert_eq!(result(&cancelled)["cancel_requested"], true);
    let stored = persistence.snapshot();
    let summary = &stored.automation_executions[0].summary;
    assert_eq!(summary.state, contract::AutomationExecutionState::Cancelled);
    assert_eq!(summary.error_code.as_deref(), Some("CANCELLED"));
    assert_eq!(stored.automations[0].active_execution_id, None);
    assert_eq!(stored.reserved_bytes, 0);
    assert_eq!(
        core.start_automation_execution(&execution.summary.execution_id, at_minutes(1, 2))
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
}

#[tokio::test]
async fn i9_g08_terminal_history_keeps_the_newest_hundred_per_automation() {
    let (core, persistence) = make_core();
    let automation_id = saved_interval(&core).await;
    let mut first_execution = None;
    for minute in 1..=101 {
        let execution = admitted(
            core.admit_due_automation(&automation_id, at_minutes(minute, 0))
                .await
                .unwrap(),
        );
        let execution_id = execution.summary.execution_id.clone();
        first_execution.get_or_insert(execution_id.clone());
        core.start_automation_execution(&execution_id, at_minutes(minute, 1))
            .await
            .unwrap();
        core.settle_automation_execution(
            &execution_id,
            AutomationExecutionOutcome::Completed,
            at_minutes(minute, 2),
        )
        .await
        .unwrap();
    }
    let stored = persistence.snapshot();
    assert_eq!(stored.automation_executions.len(), 100);
    let first_execution = first_execution.unwrap();
    assert!(
        stored
            .automation_executions
            .iter()
            .all(|execution| execution.summary.execution_id != first_execution)
    );
    let history = public(
        &core,
        2,
        "automation",
        "get",
        json!({"automation_id": automation_id, "history_limit": 100}),
        &at_minutes(102, 0),
    )
    .await;
    assert_eq!(result(&history)["history"].as_array().unwrap().len(), 100);
    assert_eq!(
        result(&history)["history"][0]["triggered_at"],
        at_minutes(101, 0)
    );
}
