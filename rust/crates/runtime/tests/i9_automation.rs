//! I9 Automation definition, due-computation and public ingress gates.

use chrono::{DateTime, TimeZone, Utc};
use contract::{
    AutomationTrigger, Availability, CapabilityState, GrantFacts, RuntimeHost, RuntimeReadiness,
    UuidV4,
};
use domain::{AdmissionFence, CapabilityContext, ProviderGenerations, ResolverFacts};
use runtime::{
    CapabilitySnapshot, RecoveryProof, RuntimeCore,
    fakes::{FakeArtifacts, FakeCapabilities, FakeExecutions, FakeHostControl, FakePersistence},
    format_instant, initial_due, next_due_after_admission,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

type TestCore =
    RuntimeCore<FakePersistence, FakeArtifacts, FakeExecutions, FakeCapabilities, FakeHostControl>;

const COMMIT: &str = "2026-09-14T08:00:00.000Z";

fn uuid(value: u64) -> UuidV4 {
    UuidV4::parse(format!("99000000-0000-4000-8000-{value:012x}")).unwrap()
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

fn instant(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .unwrap()
        .with_timezone(&Utc)
}

fn millis(value: &str) -> u64 {
    u64::try_from(instant(value).timestamp_millis()).unwrap()
}

async fn call_at(
    core: &TestCore,
    request_id: u64,
    action: &str,
    input: Value,
    timestamp: &str,
) -> Value {
    let request = json!({
        "protocol_version": 1,
        "request_id": uuid(0x1000 + request_id),
        "payload": {"tool": "automation", "action": action, "input": input},
    });
    serde_json::from_slice(
        &runtime::submit_public(
            core,
            &serde_json::to_vec(&request).unwrap(),
            timestamp.to_owned(),
            millis(timestamp),
            true,
            |_| async { panic!("automation escaped the shared Runtime ingress") },
        )
        .await,
    )
    .unwrap()
}

async fn call(core: &TestCore, request_id: u64, action: &str, input: Value) -> Value {
    call_at(core, request_id, action, input, COMMIT).await
}

fn result(response: &Value) -> &Value {
    assert_eq!(response["outcome"], "success", "{response}");
    &response["result"]
}

fn error_code(response: &Value) -> &str {
    assert_eq!(response["outcome"], "error", "{response}");
    response["error"]["code"].as_str().unwrap()
}

fn definition(trigger: Value) -> Value {
    json!({
        "name": "morning",
        "enabled": true,
        "trigger": trigger,
        "action": {"type": "delay", "duration_ms": 1},
    })
}

#[tokio::test]
async fn i9_g01_save_persists_the_first_due_and_rejects_a_past_at() {
    let (core, persistence) = make_core();
    let saved = call(
        &core,
        1,
        "save",
        definition(json!({"type": "interval", "every_ms": 60_000})),
    )
    .await;
    let automation = result(&saved);
    assert_eq!(automation["revision"], 1);
    assert_eq!(automation["created_at"], COMMIT);
    assert_eq!(automation["state"], json!({}));
    let stored = persistence.snapshot();
    assert_eq!(stored.automations.len(), 1);
    assert_eq!(
        stored.automations[0].next_due_at.as_deref(),
        Some("2026-09-14T08:01:00.000Z")
    );

    let past = call(
        &core,
        2,
        "save",
        definition(json!({"type": "at", "at": "2026-09-14T08:00:00.000Z"})),
    )
    .await;
    assert_eq!(error_code(&past), "INVALID_ARGUMENT");
    assert_eq!(persistence.snapshot().automations.len(), 1);

    let future = call(
        &core,
        3,
        "save",
        definition(json!({"type": "at", "at": "2026-09-14T09:30:00+01:00"})),
    )
    .await;
    let id = result(&future)["automation_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let stored = persistence.snapshot();
    let record = stored
        .automations
        .iter()
        .find(|record| record.automation.automation_id.as_str() == id)
        .unwrap();
    assert_eq!(
        record.next_due_at.as_deref(),
        Some("2026-09-14T08:30:00.000Z")
    );
}

#[tokio::test]
async fn i9_g01_set_enabled_and_saves_recompute_or_preserve_the_due() {
    let (core, persistence) = make_core();
    let saved = call(
        &core,
        1,
        "save",
        definition(json!({"type": "interval", "every_ms": 3_600_000})),
    )
    .await;
    let id = result(&saved)["automation_id"].clone();

    let disabled = call(
        &core,
        2,
        "set_enabled",
        json!({"automation_id": id, "enabled": false, "expected_revision": 1}),
    )
    .await;
    assert_eq!(result(&disabled)["revision"], 2);
    assert_eq!(persistence.snapshot().automations[0].next_due_at, None);

    let enabled = call_at(
        &core,
        3,
        "set_enabled",
        json!({"automation_id": id, "enabled": true, "expected_revision": 2}),
        "2026-09-14T10:00:00.000Z",
    )
    .await;
    assert_eq!(result(&enabled)["updated_at"], "2026-09-14T10:00:00.000Z");
    assert_eq!(
        persistence.snapshot().automations[0].next_due_at.as_deref(),
        Some("2026-09-14T11:00:00.000Z")
    );

    // A rename that keeps the enabled trigger keeps its schedule.
    let mut renamed = definition(json!({"type": "interval", "every_ms": 3_600_000}));
    renamed["automation_id"] = id.clone();
    renamed["expected_revision"] = json!(3);
    renamed["name"] = json!("renamed");
    let renamed = call_at(&core, 4, "save", renamed, "2026-09-14T10:30:00.000Z").await;
    assert_eq!(result(&renamed)["revision"], 4);
    assert_eq!(
        persistence.snapshot().automations[0].next_due_at.as_deref(),
        Some("2026-09-14T11:00:00.000Z")
    );

    let stale = call(
        &core,
        5,
        "set_enabled",
        json!({"automation_id": id, "enabled": false, "expected_revision": 3}),
    )
    .await;
    assert_eq!(error_code(&stale), "REVISION_CONFLICT");
}

#[test]
fn i9_g01_admitted_due_advances_past_now_without_catch_up() {
    let interval = AutomationTrigger::Interval { every_ms: 60_000 };
    let due = instant("2026-09-14T08:00:00.000Z");
    // Ten missed minutes coalesce into the first boundary strictly after the wake.
    assert_eq!(
        next_due_after_admission(&interval, due, instant("2026-09-14T08:10:30.000Z"))
            .unwrap()
            .map(format_instant)
            .as_deref(),
        Some("2026-09-14T08:11:00.000Z")
    );
    assert_eq!(
        next_due_after_admission(&interval, due, instant("2026-09-14T08:02:00.000Z"))
            .unwrap()
            .map(format_instant)
            .as_deref(),
        Some("2026-09-14T08:03:00.000Z")
    );
    let at = AutomationTrigger::At {
        at: "2026-09-14T08:00:00.000Z".to_owned(),
    };
    assert_eq!(next_due_after_admission(&at, due, due).unwrap(), None);

    let daily = AutomationTrigger::Rrule {
        rrule: "DTSTART:20260901T090000\nRRULE:FREQ=DAILY".to_owned(),
        timezone: "Asia/Shanghai".to_owned(),
    };
    assert_eq!(
        next_due_after_admission(
            &daily,
            instant("2026-09-10T01:00:00Z"),
            instant("2026-09-14T03:00:00Z")
        )
        .unwrap()
        .map(format_instant)
        .as_deref(),
        Some("2026-09-15T01:00:00.000Z")
    );
    let counted = AutomationTrigger::Rrule {
        rrule: "DTSTART:20260901T090000\r\nRRULE:FREQ=DAILY;COUNT=2".to_owned(),
        timezone: "Asia/Shanghai".to_owned(),
    };
    assert_eq!(
        next_due_after_admission(
            &counted,
            instant("2026-09-02T01:00:00Z"),
            instant("2026-09-14T03:00:00Z")
        )
        .unwrap(),
        None
    );
}

#[test]
fn i9_g01_rrule_requires_one_local_dtstart_in_its_own_timezone() {
    let commit = Utc.with_ymd_and_hms(2026, 9, 14, 8, 0, 0).unwrap();
    let rrule = |value: &str, timezone: &str| AutomationTrigger::Rrule {
        rrule: value.to_owned(),
        timezone: timezone.to_owned(),
    };
    // 09:00 in Shanghai is 01:00Z, already past on the commit day, so the due is the next day.
    assert_eq!(
        initial_due(
            &rrule("DTSTART:20260901T090000\nRRULE:FREQ=DAILY", "Asia/Shanghai"),
            commit
        )
        .unwrap()
        .map(format_instant)
        .as_deref(),
        Some("2026-09-15T01:00:00.000Z")
    );
    for (value, timezone) in [
        ("RRULE:FREQ=DAILY", "Asia/Shanghai"),
        (
            "DTSTART:20260901T090000Z\nRRULE:FREQ=DAILY",
            "Asia/Shanghai",
        ),
        (
            "DTSTART;TZID=Asia/Shanghai:20260901T090000\nRRULE:FREQ=DAILY",
            "Asia/Shanghai",
        ),
        (
            "DTSTART:20260901T090000\nRRULE:FREQ=DAILY\n",
            "Asia/Shanghai",
        ),
        (
            "DTSTART:20260901T090000\nRRULE:FREQ=DAILY\nEXDATE:20260902T090000",
            "Asia/Shanghai",
        ),
        ("DTSTART:20260901T090000\nRRULE:FREQ=DAILY", "Mars/Olympus"),
        (
            "DTSTART:20260901T090000\nRRULE:FREQ=SOMETIMES",
            "Asia/Shanghai",
        ),
        // 02:30 does not exist in New York when daylight saving starts.
        (
            "DTSTART:20270314T023000\nRRULE:FREQ=YEARLY",
            "America/New_York",
        ),
        (
            "DTSTART:20250101T090000\nRRULE:FREQ=DAILY;COUNT=1",
            "Asia/Shanghai",
        ),
    ] {
        assert_eq!(
            initial_due(&rrule(value, timezone), commit)
                .unwrap_err()
                .code,
            contract::ErrorCode::InvalidArgument,
            "{value:?} in {timezone}"
        );
    }
}

#[tokio::test]
async fn i9_g04_saved_calls_validate_against_the_generated_contract() {
    let (core, persistence) = make_core();
    let call_definition = |action: Value| {
        json!({
            "name": "launcher",
            "enabled": false,
            "trigger": {"type": "event", "name": "runtime.ready"},
            "action": action,
        })
    };
    let accepted = call(
        &core,
        1,
        "save",
        call_definition(json!({
            "type": "call", "tool": "android", "action": "launch",
            "args": {"operation": "package", "package_name": "com.example.app"},
        })),
    )
    .await;
    assert_eq!(
        result(&accepted)["action"]["args"]["package_name"],
        "com.example.app"
    );

    for (index, rejected) in [
        // An argument the public schema does not define.
        json!({
            "type": "call", "tool": "android", "action": "launch",
            "args": {"operation": "package", "package_name": "com.example.app", "flags": 1},
        }),
        // A value outside the public bound.
        json!({
            "type": "call", "tool": "android", "action": "launch",
            "args": {"operation": "package", "package_name": ""},
        }),
        // A public action outside the closed Automation-compatible set.
        json!({
            "type": "call", "tool": "filesystem", "action": "read",
            "args": {"target": {"type": "path", "value": "/sdcard/a"}},
        }),
        json!({
            "type": "call", "tool": "automation", "action": "list", "args": {},
        }),
    ]
    .into_iter()
    .enumerate()
    {
        let response = call(&core, 10 + index as u64, "save", call_definition(rejected)).await;
        assert_eq!(error_code(&response), "INVALID_ARGUMENT", "case {index}");
    }
    assert_eq!(persistence.snapshot().automations.len(), 1);
}

#[tokio::test]
async fn i9_g08_delete_replays_its_original_result_and_removes_the_identity() {
    let (core, persistence) = make_core();
    let saved = call(
        &core,
        1,
        "save",
        definition(json!({"type": "interval", "every_ms": 60_000})),
    )
    .await;
    let id = result(&saved)["automation_id"].clone();

    let stale = call(
        &core,
        2,
        "delete",
        json!({"automation_id": id, "expected_revision": 7}),
    )
    .await;
    assert_eq!(error_code(&stale), "REVISION_CONFLICT");

    let deleted = call(
        &core,
        3,
        "delete",
        json!({"automation_id": id, "expected_revision": 1}),
    )
    .await;
    assert_eq!(
        result(&deleted),
        &json!({"automation_id": id, "deleted": true, "previous_revision": 1})
    );
    assert!(persistence.snapshot().automations.is_empty());

    // The retained request replays its original result after the identity is gone.
    let replay = call(
        &core,
        3,
        "delete",
        json!({"automation_id": id, "expected_revision": 1}),
    )
    .await;
    assert_eq!(result(&replay), result(&deleted));

    let fresh = call(
        &core,
        4,
        "delete",
        json!({"automation_id": id, "expected_revision": 1}),
    )
    .await;
    assert_eq!(error_code(&fresh), "NOT_FOUND");
    let get = call(&core, 5, "get", json!({"automation_id": id})).await;
    assert_eq!(error_code(&get), "NOT_FOUND");
    let listed = call(&core, 6, "list", json!({})).await;
    assert_eq!(result(&listed)["automations"], json!([]));
}

#[tokio::test]
async fn i9_run_request_admits_one_manual_execution_and_keeps_the_due() {
    let (core, persistence) = make_core();
    let saved = call(
        &core,
        1,
        "save",
        definition(json!({"type": "interval", "every_ms": 3_600_000})),
    )
    .await;
    let id = result(&saved)["automation_id"].clone();
    let due = persistence.snapshot().automations[0].next_due_at.clone();

    let requested = call(&core, 2, "run", json!({"automation_id": id})).await;
    assert_eq!(result(&requested)["run_requested"], true);
    let again = call(&core, 3, "run", json!({"automation_id": id})).await;
    assert_eq!(error_code(&again), "ALREADY_EXISTS");

    let requested = core.requested_automation_runs().await.unwrap();
    assert_eq!(requested.len(), 1);
    let admission = core
        .admit_requested_run(&requested[0], COMMIT.to_owned())
        .await
        .unwrap();
    let runtime::AutomationAdmission::Admitted(execution) = admission else {
        panic!("a requested run is admitted: {admission:?}");
    };
    assert_eq!(
        execution.trigger_facts.get("manual"),
        Some(&contract::ScalarValue::Boolean(true))
    );
    let stored = persistence.snapshot();
    assert_eq!(stored.automations[0].run_requested_at, None);
    assert_eq!(stored.automations[0].next_due_at, due);
    assert!(core.requested_automation_runs().await.unwrap().is_empty());

    let busy = call(&core, 4, "run", json!({"automation_id": id})).await;
    assert_eq!(error_code(&busy), "ALREADY_EXISTS");
}

#[tokio::test]
async fn i9_element_steps_validate_their_text_and_wait() {
    let (core, _) = make_core();
    let element = |args: Value| {
        json!({
            "name": "element",
            "enabled": true,
            "trigger": {"type": "interval", "every_ms": 60_000},
            "action": {"type": "call", "tool": "visual", "action": "element", "args": args},
        })
    };
    let saved = call(
        &core,
        1,
        "save",
        element(json!({"operation": "tap", "by": "text", "value": "签到"})),
    )
    .await;
    assert_eq!(
        result(&saved)["action"]["args"]["wait_ms"],
        10_000,
        "the wait defaults to ten seconds"
    );
    for (index, rejected) in [
        json!({"operation": "text", "by": "resource_id", "value": "input"}),
        json!({"operation": "tap", "by": "text", "value": "a", "text": "b"}),
        json!({"operation": "tap", "by": "text", "value": ""}),
        json!({"operation": "tap", "by": "text", "value": "a", "wait_ms": 60_001}),
    ]
    .into_iter()
    .enumerate()
    {
        let response = call(&core, 10 + index as u64, "save", element(rejected)).await;
        assert_eq!(error_code(&response), "INVALID_ARGUMENT", "case {index}");
    }
}

/// Answers each Call with the next scripted outcome and records visits.
struct ScriptedEffects {
    outcomes: Vec<Result<runtime::CallOutcome, domain::DomainError>>,
    states: Vec<(String, contract::ScalarValue)>,
}

impl runtime::AutomationEffects for ScriptedEffects {
    fn checkpoint(&mut self) -> Result<(), domain::DomainError> {
        Ok(())
    }

    fn call<'a>(
        &'a mut self,
        _call: &'a contract::AutomationCompatibleCall,
    ) -> runtime::PortFuture<'a, Result<runtime::CallOutcome, domain::DomainError>> {
        let outcome = self.outcomes.remove(0);
        Box::pin(async move { outcome })
    }

    fn set_state<'a>(
        &'a mut self,
        key: &'a str,
        value: &'a contract::ScalarValue,
    ) -> runtime::PortFuture<'a, Result<(), domain::DomainError>> {
        self.states.push((key.to_owned(), value.clone()));
        Box::pin(async { Ok(()) })
    }

    fn delay<'a>(
        &'a mut self,
        _duration_ms: u64,
    ) -> runtime::PortFuture<'a, Result<(), domain::DomainError>> {
        Box::pin(async { Ok(()) })
    }
}

fn scripted_action(on_failure: &str, condition: Value) -> contract::AutomationAction {
    serde_json::from_value(json!({
        "type": "sequence",
        "children": [
            {"type": "call", "tool": "android", "action": "launch",
             "args": {"operation": "package", "package_name": "com.example"},
             "on_failure": on_failure},
            {"type": "conditional", "condition": condition,
             "then": {"type": "set_state", "key": "branch", "value": "then"},
             "else": {"type": "set_state", "key": "branch", "value": "else"}},
        ],
    }))
    .unwrap()
}

async fn run_scripted(
    action: &contract::AutomationAction,
    outcome: Result<runtime::CallOutcome, domain::DomainError>,
) -> (
    Result<(), domain::DomainError>,
    Vec<(String, contract::ScalarValue)>,
) {
    let mut effects = ScriptedEffects {
        outcomes: vec![outcome],
        states: Vec::new(),
    };
    let run = runtime::AutomationInterpreter
        .execute(action, &mut BTreeMap::new(), &BTreeMap::new(), &mut effects)
        .await;
    (run, effects.states)
}

fn branch(value: &str) -> Vec<(String, contract::ScalarValue)> {
    vec![(
        "branch".to_owned(),
        contract::ScalarValue::String(value.to_owned()),
    )]
}

#[tokio::test]
async fn i9_a_continued_failure_is_readable_as_the_previous_result() {
    let failed = || {
        Err(domain::DomainError::new(
            contract::ErrorCode::NotFound,
            "no element matches",
        ))
    };
    let succeeded =
        json!({"source": "result", "key": "succeeded", "operator": "equals", "value": true});
    let (run, states) =
        run_scripted(&scripted_action("continue", succeeded.clone()), failed()).await;
    assert!(run.is_ok());
    assert_eq!(states, branch("else"));

    let code = json!({"source": "result", "key": "error_code", "operator": "equals", "value": "NOT_FOUND"});
    let (_, states) = run_scripted(&scripted_action("continue", code), failed()).await;
    assert_eq!(states, branch("then"));

    let (_, states) = run_scripted(
        &scripted_action("stop", succeeded.clone()),
        Ok(runtime::CallOutcome::default()),
    )
    .await;
    assert_eq!(states, branch("then"));

    // A failure the step does not continue past ends the run before the condition.
    let (run, states) = run_scripted(&scripted_action("stop", succeeded), failed()).await;
    assert_eq!(run.unwrap_err().code, contract::ErrorCode::NotFound);
    assert!(states.is_empty());

    // A command that exited non-zero is a failed step that still reports its exit code.
    let exit = json!({"source": "result", "key": "exit_code", "operator": "equals", "value": 3});
    let (run, states) = run_scripted(
        &scripted_action("continue", exit),
        Ok(runtime::CallOutcome {
            failure: Some(contract::ErrorCode::ExecutionFailed),
            exit_code: Some(3),
        }),
    )
    .await;
    assert!(run.is_ok());
    assert_eq!(states, branch("then"));
}
