//! I9 canonical-store mapping of Automation records, due truth and retained mutation results.

use contract::{
    Automation, AutomationAction, AutomationExecutionState, AutomationExecutionSummary,
    AutomationTrigger, ErrorCode, UuidV4,
};
use persistence::{CanonicalState, StoredAutomation, StoredAutomationExecution};
use runtime::{AutomationExecutionRecord, AutomationRecord, RetainedMutationRecord, RuntimeState};
use serde_json::json;
use std::collections::BTreeMap;

const NOW_MS: u64 = 1_789_372_800_000;

fn uuid(value: u64) -> UuidV4 {
    UuidV4::parse(format!("99100000-0000-4000-8000-{value:012x}")).unwrap()
}

fn automation(id: u64, revision: u64) -> Automation {
    Automation {
        automation_id: uuid(id),
        name: format!("automation-{id}"),
        enabled: true,
        trigger: AutomationTrigger::Interval { every_ms: 60_000 },
        action: AutomationAction::Delay { duration_ms: 1 },
        state: BTreeMap::new(),
        revision,
        created_at: "2026-09-14T08:00:00.000Z".to_owned(),
        updated_at: "2026-09-14T08:00:00.000Z".to_owned(),
    }
}

fn populated_state() -> RuntimeState {
    let mut state = RuntimeState {
        revision: 3,
        ..RuntimeState::default()
    };
    state.automations.push(AutomationRecord {
        automation: automation(1, 2),
        next_due_at: Some("2026-09-14T08:01:00.000Z".to_owned()),
        deleted_at: None,
        active_execution_id: None,
    });
    // A deleted Automation whose execution is still running keeps its tombstone.
    state.automations.push(AutomationRecord {
        automation: automation(2, 3),
        next_due_at: None,
        deleted_at: Some("2026-09-14T08:05:00.000Z".to_owned()),
        active_execution_id: Some(uuid(20)),
    });
    state.automation_executions.push(AutomationExecutionRecord {
        automation_id: uuid(2),
        summary: AutomationExecutionSummary {
            execution_id: uuid(20),
            task_id: uuid(21),
            state: AutomationExecutionState::Running,
            triggered_at: "2026-09-14T08:04:00.000Z".to_owned(),
            started_at: Some("2026-09-14T08:04:00.010Z".to_owned()),
            ended_at: None,
            error_code: None,
        },
    });
    let request_id = uuid(30);
    state
        .dedup
        .decide_and_reserve(request_id.clone(), "ab".repeat(32), NOW_MS, false)
        .unwrap();
    state.dedup.settle(&request_id, NOW_MS).unwrap();
    state.retained_mutations.push(RetainedMutationRecord {
        request_id,
        result: json!({"automation_id": uuid(3), "deleted": true, "previous_revision": 4}),
    });
    state
}

#[test]
fn i9_g01_canonical_store_round_trips_due_tombstones_and_retained_results() {
    let state = populated_state();
    let canonical = CanonicalState::try_from(&state).unwrap();
    assert_eq!(
        canonical.automations[0].next_due_at.as_deref(),
        Some("2026-09-14T08:01:00.000Z")
    );
    assert!(canonical.automations[1].deleted);
    assert!(
        canonical.request_records[0].mutation_result.is_some()
            && canonical.request_records[0].expires_at_ms.is_some()
    );

    let encoded = serde_json::to_vec(&canonical).unwrap();
    let decoded: CanonicalState = serde_json::from_slice(&encoded).unwrap();
    let restored = RuntimeState::try_from(decoded).unwrap();
    assert_eq!(restored.automations, state.automations);
    assert_eq!(restored.automation_executions, state.automation_executions);
    assert_eq!(restored.retained_mutations, state.retained_mutations);
    assert_eq!(restored.dedup.entries(), state.dedup.entries());
}

#[test]
fn i9_g08_inconsistent_tombstones_and_orphan_executions_are_rejected() {
    let canonical = CanonicalState::try_from(&populated_state()).unwrap();

    let mut tombstone_without_owner = canonical.clone();
    tombstone_without_owner.automations[1].active_execution_id = None;
    assert_eq!(
        RuntimeState::try_from(tombstone_without_owner)
            .unwrap_err()
            .code,
        ErrorCode::IoError
    );

    let mut half_deleted = canonical.clone();
    half_deleted.automations[0] = StoredAutomation {
        deleted: true,
        ..half_deleted.automations[0].clone()
    };
    assert_eq!(
        RuntimeState::try_from(half_deleted).unwrap_err().code,
        ErrorCode::IoError
    );

    let mut orphan = canonical.clone();
    orphan
        .automation_executions
        .push(StoredAutomationExecution {
            automation_id: uuid(99),
            summary: orphan.automation_executions[0].summary.clone(),
        });
    assert_eq!(
        RuntimeState::try_from(orphan).unwrap_err().code,
        ErrorCode::IoError
    );

    let mut duplicated_result = canonical;
    duplicated_result.request_records[0].task_id = Some(uuid(40));
    assert_eq!(
        RuntimeState::try_from(duplicated_result).unwrap_err().code,
        ErrorCode::IoError
    );
}
