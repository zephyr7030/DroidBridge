//! I9 AutomationExecution run-loop gates: ordinary call resolution, budgets and cancellation.

use chrono::{DateTime, SecondsFormat, TimeZone, Utc};
use contract::{
    AutomationExecutionState, Availability, CapabilityState, ErrorCode, ExecutionClass, GrantFacts,
    RuntimeHost, RuntimeReadiness, ScalarValue, TaskState, UuidV4,
};
use domain::{AdmissionFence, CapabilityContext, DomainError, ProviderGenerations, ResolverFacts};
use runtime::{
    AdmittedAutomationExecution, AutomationAdmission, AutomationClock, CapabilitySnapshot,
    ExecutionCompletion, ExecutionOutcome, ExecutionPayload, PortFuture, RecoveryProof,
    RuntimeCore,
    fakes::{FakeArtifacts, FakeCapabilities, FakeExecutions, FakeHostControl, FakePersistence},
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::Semaphore;

type TestCore =
    RuntimeCore<FakePersistence, FakeArtifacts, FakeExecutions, FakeCapabilities, FakeHostControl>;

const SAVED: &str = "2026-09-14T08:00:00.000Z";
const DUE: &str = "2026-09-14T08:01:00.000Z";
const APP_GENERATION: u64 = 1;

fn uuid(value: u64) -> UuidV4 {
    UuidV4::parse(format!("99300000-0000-4000-8000-{value:012x}")).unwrap()
}

fn available() -> Availability {
    Availability {
        state: CapabilityState::Available,
        reason: None,
    }
}

fn fence() -> AdmissionFence {
    AdmissionFence {
        runtime_epoch: uuid(1),
        host_generation: 1,
        runtime_instance_id: uuid(2),
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
                app_native: APP_GENERATION,
                app_framework: 1,
                shizuku: 1,
                magisk_native: 1,
                magisk_framework: 1,
                accessibility: 1,
                media_projection: 1,
                notification_listener: 1,
            },
        },
        fence: fence(),
    }
}

fn make_core() -> (TestCore, FakePersistence, FakeExecutions) {
    let persistence = FakePersistence::default();
    let executions = FakeExecutions::default();
    let capabilities = FakeCapabilities::new(capability());
    let core = RuntimeCore::new(
        persistence.clone(),
        FakeArtifacts::default(),
        executions.clone(),
        capabilities.clone(),
        FakeHostControl::new(RecoveryProof::Clean).with_capabilities(capabilities),
    );
    (core, persistence, executions)
}

fn millis(timestamp: &str) -> u64 {
    u64::try_from(
        DateTime::parse_from_rfc3339(timestamp)
            .unwrap()
            .timestamp_millis(),
    )
    .unwrap()
}

/// A deterministic CLOCK_BOOTTIME and wall clock. `sleep` advances both clocks by at least
/// `sleep_advance_ms`, and a held gate keeps a sleep pending until the test releases it.
struct FakeClock {
    boot_ms: AtomicU64,
    wall_ms: AtomicU64,
    sleep_advance_ms: AtomicU64,
    hold: Mutex<Option<Arc<Semaphore>>>,
    sleeps: Mutex<Vec<u64>>,
}

impl FakeClock {
    fn at(timestamp: &str) -> Self {
        Self {
            boot_ms: AtomicU64::new(1_000),
            wall_ms: AtomicU64::new(millis(timestamp)),
            sleep_advance_ms: AtomicU64::new(0),
            hold: Mutex::new(None),
            sleeps: Mutex::new(Vec::new()),
        }
    }

    fn sleeps(&self) -> Vec<u64> {
        self.sleeps.lock().unwrap().clone()
    }
}

impl AutomationClock for FakeClock {
    fn boot_millis(&self) -> Result<u64, DomainError> {
        Ok(self.boot_ms.load(Ordering::SeqCst))
    }

    fn sleep<'a>(&'a self, duration_ms: u64) -> PortFuture<'a, Result<(), DomainError>> {
        Box::pin(async move {
            self.sleeps.lock().unwrap().push(duration_ms);
            // A real timer wait yields; so does this one, letting spawned child Tasks progress on
            // the single-threaded test runtime.
            tokio::task::yield_now().await;
            let hold = self.hold.lock().unwrap().clone();
            if let Some(gate) = hold {
                gate.acquire().await.unwrap().forget();
            }
            let advance = duration_ms.max(self.sleep_advance_ms.load(Ordering::SeqCst));
            self.boot_ms.fetch_add(advance, Ordering::SeqCst);
            self.wall_ms.fetch_add(advance, Ordering::SeqCst);
            Ok(())
        })
    }

    fn wall(&self) -> Result<(String, u64), DomainError> {
        let millis = self.wall_ms.load(Ordering::SeqCst);
        let instant = Utc
            .timestamp_millis_opt(i64::try_from(millis).unwrap())
            .unwrap();
        Ok((instant.to_rfc3339_opts(SecondsFormat::Millis, true), millis))
    }
}

async fn public(core: &TestCore, request_id: u64, tool: &str, action: &str, input: Value) -> Value {
    let request = json!({
        "protocol_version": 1,
        "request_id": uuid(0x1000 + request_id),
        "payload": {"tool": tool, "action": action, "input": input},
    });
    serde_json::from_slice(
        &runtime::submit_public(
            core,
            &serde_json::to_vec(&request).unwrap(),
            SAVED.to_owned(),
            millis(SAVED),
            true,
            |_| async { panic!("request escaped the shared Runtime ingress") },
        )
        .await,
    )
    .unwrap()
}

/// Saves one interval Automation with `action` and admits its first due occurrence.
async fn admitted(core: &TestCore, action: Value) -> AdmittedAutomationExecution {
    let saved = public(
        core,
        1,
        "automation",
        "save",
        json!({
            "name": "engine",
            "enabled": true,
            "trigger": {"type": "interval", "every_ms": 60_000},
            "action": action,
        }),
    )
    .await;
    assert_eq!(saved["outcome"], "success", "{saved}");
    let automation_id = UuidV4::parse(
        saved["result"]["automation_id"]
            .as_str()
            .unwrap()
            .to_owned(),
    )
    .unwrap();
    match core
        .admit_due_automation(&automation_id, DUE.to_owned())
        .await
        .unwrap()
    {
        AutomationAdmission::Admitted(execution) => *execution,
        other => panic!("expected admission, got {other:?}"),
    }
}

fn command_call(as_task: bool) -> Value {
    json!({
        "type": "call", "tool": "command", "action": "run",
        "args": {
            "command": "true", "run_as": "app", "timeout_ms": 1_000,
            "max_output_bytes": 1_024, "as_task": as_task,
        },
    })
}

#[tokio::test]
async fn i9_g05_calls_run_through_the_ordinary_public_resolver() {
    let (core, persistence, executions) = make_core();
    let execution = admitted(
        &core,
        json!({"type": "sequence", "children": [
            command_call(false),
            {"type": "set_state", "key": "done", "value": true},
        ]}),
    )
    .await;
    executions.push(Ok(ExecutionCompletion {
        fence: fence(),
        capability_generation: APP_GENERATION,
        outcome: ExecutionOutcome::SynchronousCompleted {
            result: json!({"exit_code": 0}),
            encoded_bytes: 16,
        },
        cleanup_verified: true,
    }));

    let summary = core
        .run_automation_execution(execution, &FakeClock::at(DUE))
        .await
        .unwrap();
    assert_eq!(summary.state, AutomationExecutionState::Completed);

    // The Call was admitted like any public command.run request: same executor resolution,
    // same synchronous execution and retained request record, no Automation-specific path.
    let started = executions.started();
    assert_eq!(started.len(), 1);
    assert!(matches!(
        started[0].payload,
        ExecutionPayload::CommandCall(_)
    ));
    assert_eq!(started[0].executor.execution_class, ExecutionClass::App);
    assert!(started[0].task_id.is_none());
    let stored = persistence.snapshot();
    assert_eq!(stored.synchronous_executions.len(), 1);
    assert_eq!(stored.synchronous_executions[0].operation, "command.run");
    assert_eq!(
        stored.automations[0].automation.state.get("done"),
        Some(&ScalarValue::Boolean(true))
    );
}

#[tokio::test]
async fn i9_g09_elapsed_budget_stops_a_delay_with_timeout() {
    let (core, persistence, _) = make_core();
    let execution = admitted(
        &core,
        json!({"type": "sequence", "children": [
            {"type": "delay", "duration_ms": 86_400_000},
            {"type": "set_state", "key": "after_delay", "value": true},
        ]}),
    )
    .await;
    let clock = FakeClock::at(DUE);
    let summary = core
        .run_automation_execution(execution, &clock)
        .await
        .unwrap();
    assert_eq!(summary.state, AutomationExecutionState::Failed);
    assert_eq!(summary.error_code.as_deref(), Some("TIMEOUT"));
    // The delay waited only for the remaining one-hour budget.
    assert_eq!(clock.sleeps(), vec![3_600_000]);
    let stored = persistence.snapshot();
    assert!(stored.automations[0].automation.state.is_empty());
    assert!(stored.tasks.iter().all(|task| task.lifecycle.is_terminal()));
}

#[tokio::test]
async fn i9_g09_budget_expiry_cancels_the_awaited_child_task_before_settling() {
    let (core, persistence, executions) = make_core();
    executions.set_cancel_cleanup_verified(true);
    let gate = executions.pause_before_effect();
    let execution = admitted(&core, command_call(true)).await;
    let clock = FakeClock::at(DUE);
    clock.sleep_advance_ms.store(1_800_000, Ordering::SeqCst);

    // Once the run loop asks the child to stop, let its paused effect observe the cancellation.
    let releaser = {
        let executions = executions.clone();
        let gate = Arc::clone(&gate);
        tokio::spawn(async move {
            loop {
                if let Some(child) = executions.started().first()
                    && executions.cancellation_reached_claim(&child.execution_id)
                {
                    gate.add_permits(1);
                    return child.task_id.clone().unwrap();
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
    };
    let summary = tokio::time::timeout(
        Duration::from_secs(10),
        core.run_automation_execution(execution, &clock),
    )
    .await
    .expect("the run loop settles after cancelling its child")
    .unwrap();
    let child_task_id = tokio::time::timeout(Duration::from_secs(10), releaser)
        .await
        .expect("the child Task was running when the budget cancelled it")
        .unwrap();

    assert_eq!(summary.state, AutomationExecutionState::Failed);
    assert_eq!(summary.error_code.as_deref(), Some("TIMEOUT"));
    let stored = persistence.snapshot();
    assert_eq!(
        stored.task(&child_task_id).unwrap().state(),
        TaskState::Cancelled
    );
    // No child Task outlives the execution that owned it.
    assert!(stored.tasks.iter().all(|task| task.lifecycle.is_terminal()));
}

#[tokio::test]
async fn i9_g08_cancelling_a_running_execution_stops_before_its_next_action() {
    let (core, persistence, _) = make_core();
    let execution = admitted(
        &core,
        json!({"type": "sequence", "children": [
            {"type": "delay", "duration_ms": 10_000},
            {"type": "set_state", "key": "reached", "value": true},
        ]}),
    )
    .await;
    let task_id = execution.summary.task_id.clone();
    let clock = Arc::new(FakeClock::at(DUE));
    *clock.hold.lock().unwrap() = Some(Arc::new(Semaphore::new(0)));

    let run = {
        let core = core.clone();
        let clock = Arc::clone(&clock);
        tokio::spawn(async move { core.run_automation_execution(execution, &*clock).await })
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        while clock.sleeps().is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the execution reaches its delay");

    let cancelled = public(
        &core,
        2,
        "task_control",
        "cancel",
        json!({"task_id": task_id}),
    )
    .await;
    assert_eq!(cancelled["result"]["cancel_requested"], true, "{cancelled}");

    let summary = tokio::time::timeout(Duration::from_secs(10), run)
        .await
        .expect("cancellation wakes the delayed execution")
        .unwrap()
        .unwrap();
    assert_eq!(summary.state, AutomationExecutionState::Cancelled);
    assert_eq!(summary.error_code.as_deref(), Some("CANCELLED"));
    let stored = persistence.snapshot();
    assert!(stored.automations[0].automation.state.is_empty());
    assert_eq!(stored.task(&task_id).unwrap().state(), TaskState::Cancelled);
}

#[test]
fn i9_g09_boottime_clock_is_only_admitted_on_android_or_linux() {
    let clock = runtime::BoottimeClock;
    if cfg!(any(target_os = "android", target_os = "linux")) {
        assert!(clock.boot_millis().unwrap() > 0);
    } else {
        assert_eq!(
            clock.boot_millis().unwrap_err().code,
            ErrorCode::Unsupported
        );
    }
}
