use crate::{
    ArtifactKind, ArtifactRecord, CanonicalState, RequestRecord, StoredAutomation,
    StoredAutomationExecution, StoredRoute, StoredTask,
};
use contract::{
    Automation, AutomationAction, AutomationExecutionState, AutomationExecutionSummary,
    AutomationTrigger, CommandResult, CommandTerminalState, ExecutionClass, Fence, MotherTool,
    RunAs, ScalarValue, TaskState, TaskTerminalResult, UuidV4,
};
use runtime::{ExecutionPayload, ExecutorRecord, ProviderToken};
use std::collections::BTreeMap;

pub const FIXTURE_512_KIB: usize = 512 * 1024;
pub const FIXTURE_2_MIB: usize = 2 * 1024 * 1024;
pub const FIXTURE_8_MIB: usize = 8 * 1024 * 1024;

pub fn realistic_store_fixture(target_bytes: usize) -> Vec<u8> {
    assert!(matches!(
        target_bytes,
        FIXTURE_512_KIB | FIXTURE_2_MIB | FIXTURE_8_MIB
    ));
    let mut state = realistic_baseline();
    for automation_index in 0_u64..255 {
        state.automations.push(fixture_automation(automation_index));
    }
    assert!(encoded_len(&state) < target_bytes);
    for automation_position in 1..state.automations.len() {
        for key_index in 0_u32..64 {
            state.automations[automation_position]
                .automation
                .state
                .insert(
                    format!("state_{key_index:02}"),
                    ScalarValue::String("x".repeat(4096)),
                );
        }
        let full_length = encoded_len(&state);
        if full_length < target_bytes {
            continue;
        }
        if full_length == target_bytes {
            return finish_fixture(state, target_bytes);
        }
        state.automations[automation_position]
            .automation
            .state
            .clear();
        for key_index in 0_u32..64 {
            let current_length = encoded_len(&state);
            if current_length == target_bytes {
                return finish_fixture(state, target_bytes);
            }
            let key = format!("state_{key_index:02}");
            state.automations[automation_position]
                .automation
                .state
                .insert(key.clone(), ScalarValue::String(String::new()));
            let empty_length = encoded_len(&state);
            if empty_length > target_bytes {
                state.automations[automation_position]
                    .automation
                    .state
                    .remove(&key);
                let padding = target_bytes - current_length;
                let name = &mut state.automations[automation_position].automation.name;
                assert!(name.len() + padding <= 128);
                name.push_str(&"x".repeat(padding));
                return finish_fixture(state, target_bytes);
            }
            let fill = (target_bytes - empty_length).min(4096);
            state.automations[automation_position]
                .automation
                .state
                .insert(key, ScalarValue::String("x".repeat(fill)));
            if encoded_len(&state) == target_bytes {
                return finish_fixture(state, target_bytes);
            }
        }
        panic!("fixture target cannot be represented");
    }
    panic!("fixture target exceeds realistic structure capacity")
}

fn encoded_len(state: &CanonicalState) -> usize {
    serde_json::to_vec(state)
        .expect("fixture must serialize")
        .len()
}

fn finish_fixture(state: CanonicalState, target_bytes: usize) -> Vec<u8> {
    let bytes = serde_json::to_vec(&state).expect("fixture must serialize");
    assert_eq!(bytes.len(), target_bytes);
    let decoded: CanonicalState = serde_json::from_slice(&bytes).expect("fixture must parse");
    assert_eq!(decoded, state);
    bytes
}

fn realistic_baseline() -> CanonicalState {
    let request_id = fixture_id(10_000);
    let task_id = fixture_id(10_001);
    let execution_id = fixture_id(10_002);
    let automation_id = fixture_id(10_003);
    let automation_task_id = fixture_id(10_004);
    let automation_execution_id = fixture_id(10_005);
    let artifact_id = fixture_id(10_006);
    let mut state = CanonicalState::default();
    state.request_records.push(RequestRecord {
        request_id: request_id.clone(),
        payload_sha256: "ab".repeat(32),
        expires_at_ms: Some(1_767_312_000_000),
        task_id: Some(task_id.clone()),
        synchronous_execution: None,
        mutation_result: None,
    });
    state.tasks.push(StoredTask {
        request_id: Some(request_id.clone()),
        task_id: task_id.clone(),
        execution_id,
        state: TaskState::Completed,
        cancel_requested: false,
        tool: MotherTool::Command,
        action: "run".to_owned(),
        created_at: "2026-01-01T00:00:00.000Z".to_owned(),
        started_at: Some("2026-01-01T00:00:00.001Z".to_owned()),
        ended_at: Some("2026-01-01T00:00:00.002Z".to_owned()),
        waiting_reason: None,
        executor: Some(ExecutorRecord {
            host: contract::RuntimeHost::ApkRuntime,
            provider: ProviderToken::AppNative,
            execution_class: ExecutionClass::App,
            capability_generation: 1,
            fence: Fence {
                runtime_epoch: fixture_id(10_007),
                host_generation: 1,
                runtime_instance_id: fixture_id(10_008),
            },
        }),
        route: Some(StoredRoute::Command { run_as: RunAs::App }),
        payload: Some(ExecutionPayload::OpaqueOperation(
            "fixture-command".to_owned(),
        )),
        result: Some(TaskTerminalResult::Command(CommandResult {
            state: CommandTerminalState::Completed,
            failure_code: None,
            exit_code: Some(0),
            requested_run_as: RunAs::App,
            actual_run_as: RunAs::App,
            execution_class: ExecutionClass::App,
            duration_ms: 2,
            stdout: None,
            stdout_ref: Some(format!("dbref:stdout:{artifact_id}")),
            stdout_truncated: false,
            stderr: None,
            stderr_ref: None,
            stderr_truncated: false,
        })),
        error: None,
        reserved_bytes: 16_384,
        automation_owner: None,
    });
    state.automations.push(StoredAutomation {
        automation: Automation {
            automation_id: automation_id.clone(),
            name: "fixture-history".to_owned(),
            enabled: true,
            trigger: AutomationTrigger::Interval { every_ms: 60_000 },
            action: AutomationAction::Delay { duration_ms: 1 },
            state: BTreeMap::new(),
            revision: 1,
            created_at: "2026-01-01T00:00:00.000Z".to_owned(),
            updated_at: "2026-01-01T00:00:00.000Z".to_owned(),
        },
        next_due_at: Some("2026-01-01T00:01:00.000Z".to_owned()),
        deleted: false,
        deleted_at: None,
        active_execution_id: None,
    });
    state.automation_executions.push(StoredAutomationExecution {
        automation_id,
        summary: AutomationExecutionSummary {
            execution_id: automation_execution_id,
            task_id: automation_task_id,
            state: AutomationExecutionState::Completed,
            triggered_at: "2026-01-01T00:00:00.000Z".to_owned(),
            started_at: Some("2026-01-01T00:00:00.001Z".to_owned()),
            ended_at: Some("2026-01-01T00:00:00.002Z".to_owned()),
            error_code: None,
        },
    });
    state.artifact_manifest.push(ArtifactRecord {
        artifact_ref: format!("dbref:stdout:{artifact_id}"),
        kind: ArtifactKind::Stdout,
        size: 64,
        created_at: "2026-01-01T00:00:00.000Z".to_owned(),
        expires_at: "2026-01-02T00:00:00.000Z".to_owned(),
        sha256: Some("cd".repeat(32)),
        mime: None,
        task_id: Some(task_id),
        request_id: Some(request_id),
    });
    state
}

fn fixture_automation(index: u64) -> StoredAutomation {
    let id = fixture_id(index);
    StoredAutomation {
        automation: Automation {
            automation_id: id,
            name: format!("fixture-{index}"),
            enabled: true,
            trigger: AutomationTrigger::Interval { every_ms: 60_000 },
            action: AutomationAction::Delay { duration_ms: 1 },
            state: BTreeMap::new(),
            revision: 1,
            created_at: "2026-01-01T00:00:00.000Z".to_owned(),
            updated_at: "2026-01-01T00:00:00.000Z".to_owned(),
        },
        next_due_at: Some("2026-01-01T00:01:00.000Z".to_owned()),
        deleted: false,
        deleted_at: None,
        active_execution_id: None,
    }
}

fn fixture_id(index: u64) -> UuidV4 {
    UuidV4::parse(format!("00000000-0000-4000-8000-{index:012x}"))
        .expect("fixture UUID must be valid")
}
