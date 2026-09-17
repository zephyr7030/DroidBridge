//! AutomationExecution admission, start and settlement over the canonical store (S-LIFE-003,
//! S-AUTO-002, S-PERSIST-006). Each transition is one canonical commit, so an execution and its
//! container Task always advance together.

use crate::{
    ArtifactPort, AutomationRecord, CapabilityPort, CapabilitySnapshot, ExecutionPort,
    HostControlPort, PersistencePort, RESERVE_FLOOR_BYTES, RuntimeCore, RuntimeState,
    STORE_LIMIT_BYTES, TASK_RECORD_BYTES, TaskOrigin, TaskRecord, format_instant,
    next_due_after_admission,
};
use chrono::{DateTime, Utc};
use contract::{
    AutomationAction, AutomationExecutionState, AutomationExecutionSummary, AutomationId,
    AutomationTaskResult, AutomationTrigger, ErrorCode, ExecutionId, MotherTool, PublicError,
    RuntimeReadiness, ScalarValue, TaskState, TaskTerminalResult, True, UuidV4,
};
use domain::{DomainError, MAX_QUEUED_TASKS, TaskEvent, TaskLifecycle};
use std::collections::{BTreeMap, HashSet};

/// The reserved non-callable Task provenance action of an AutomationExecution (R-TASK-003).
pub const AUTOMATION_EXECUTION_ACTION: &str = "execution";
pub const RUNTIME_READY_EVENT: &str = "runtime.ready";
/// Exactly the two registered v1 Automation events (S-AUTO-001).
pub const AUTOMATION_EVENT_NAMES: [&str; 2] =
    [RUNTIME_READY_EVENT, crate::NETWORK_DEFAULT_CHANGED_EVENT];
pub const MAX_NON_TERMINAL_AUTOMATION_EXECUTIONS: usize = 320;
pub const MAX_TERMINAL_EXECUTIONS_PER_AUTOMATION: usize = 100;
pub const MAX_TERMINAL_AUTOMATION_EXECUTIONS: usize = 2_000;
pub const AUTOMATION_EXECUTION_RECORD_BYTES: u64 = 1_024;

/// The immutable definition snapshot an admitted AutomationExecution runs (S-AUTO-002).
#[derive(Clone, Debug, PartialEq)]
pub struct AdmittedAutomationExecution {
    pub automation_id: AutomationId,
    pub revision: u64,
    pub summary: AutomationExecutionSummary,
    pub trigger: AutomationTrigger,
    pub action: AutomationAction,
    pub trigger_facts: BTreeMap<String, ScalarValue>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AutomationAdmission {
    Admitted(Box<AdmittedAutomationExecution>),
    /// The Automation already owns a non-terminal execution; this arrival creates none.
    BusyDropped,
    /// The Automation is not visible, not enabled, has no time due, or is not yet due.
    NotDue,
    /// Capacity or readiness rejected only this admission; nothing was committed, so a time due
    /// stays unchanged (S-LIFE-003).
    Rejected(DomainError),
}

#[derive(Clone, Debug, PartialEq)]
pub enum AutomationExecutionOutcome {
    Completed,
    Failed(PublicError),
    Cancelled(PublicError),
    Interrupted(PublicError),
}

impl<P, A, E, C, H> RuntimeCore<P, A, E, C, H>
where
    P: PersistencePort + 'static,
    A: ArtifactPort + 'static,
    E: ExecutionPort + 'static,
    C: CapabilityPort + 'static,
    H: HostControlPort + 'static,
{
    /// Admits the persisted due occurrence of one time-triggered Automation at `timestamp` and
    /// advances its due in the same commit (S-LIFE-003). A busy Automation keeps its due.
    pub async fn admit_due_automation(
        &self,
        automation_id: &AutomationId,
        timestamp: String,
    ) -> Result<AutomationAdmission, DomainError> {
        let wall_now = timestamp_instant(&timestamp)?;
        self.state_transition(|state, capability| {
            admit_due(state, capability, automation_id, &timestamp, wall_now)
        })
        .await
    }

    pub async fn start_automation_execution(
        &self,
        execution_id: &ExecutionId,
        timestamp: String,
    ) -> Result<AutomationExecutionSummary, DomainError> {
        self.state_transition(|state, _| start(state, execution_id, &timestamp).map(|s| (s, true)))
            .await
    }

    pub async fn settle_automation_execution(
        &self,
        execution_id: &ExecutionId,
        outcome: AutomationExecutionOutcome,
        timestamp: String,
    ) -> Result<AutomationExecutionSummary, DomainError> {
        self.state_transition(|state, _| {
            settle_automation_in_state(state, execution_id, outcome, &timestamp)
                .map(|summary| (summary, true))
        })
        .await
    }

    /// The earliest persisted due every host wake projection arms (S-LIFE-003); busy Automations
    /// are excluded until their execution settles.
    pub async fn earliest_automation_due(&self) -> Result<Option<String>, DomainError> {
        self.read_state(|state| earliest_due(state, &[], None))
            .await
    }

    /// The earliest armable due after one scheduler pass at `timestamp`. The already-expired dues
    /// of `blocked` Automations, whose admission failed for capacity or readiness, are disarmed
    /// until the next canonical change instead of re-arming an immediate wake (S-LIFE-003).
    pub async fn earliest_automation_due_excluding(
        &self,
        timestamp: &str,
        blocked: &[AutomationId],
    ) -> Result<Option<String>, DomainError> {
        let wall_now = timestamp_instant(timestamp)?;
        self.read_state(|state| earliest_due(state, blocked, Some(wall_now)))
            .await
    }

    /// Whether an enabled `network.default_changed` Automation requires the sole S-NET-006
    /// subscription (S-AUTO-001).
    pub async fn requires_network_default_events(&self) -> Result<bool, DomainError> {
        self.read_state(|state| {
            Ok(state.automations.iter().any(|record| {
                record.deleted_at.is_none()
                    && record.automation.enabled
                    && matches!(
                        &record.automation.trigger,
                        AutomationTrigger::Event { name, .. }
                            if name == crate::NETWORK_DEFAULT_CHANGED_EVENT
                    )
            }))
        })
        .await
    }

    /// Time-triggered Automations whose persisted due is at or before `timestamp`.
    pub async fn due_automation_ids(
        &self,
        timestamp: &str,
    ) -> Result<Vec<AutomationId>, DomainError> {
        let wall_now = timestamp_instant(timestamp)?;
        self.read_state(|state| {
            let mut due = Vec::new();
            for record in &state.automations {
                if armable_due(record)?.is_some_and(|instant| instant <= wall_now) {
                    due.push(record.automation.automation_id.clone());
                }
            }
            Ok(due)
        })
        .await
    }

    /// Admits one execution for every enabled Automation of a registered v1 event whose exact
    /// matches accept `facts`, each in its own commit; busy Automations drop the arrival.
    pub async fn admit_event_automations(
        &self,
        event: &str,
        facts: BTreeMap<String, ScalarValue>,
        timestamp: String,
    ) -> Result<Vec<AutomationAdmission>, DomainError> {
        if !AUTOMATION_EVENT_NAMES.contains(&event) {
            return Err(DomainError::invalid(
                "Automation event is not a registered v1 event",
            ));
        }
        let candidates = self
            .read_state(|state| {
                Ok(state
                    .automations
                    .iter()
                    .filter(|record| accepts_event(record, event, &facts))
                    .map(|record| record.automation.automation_id.clone())
                    .collect::<Vec<_>>())
            })
            .await?;
        let mut admissions = Vec::with_capacity(candidates.len());
        for automation_id in candidates {
            admissions.push(
                self.state_transition(|state, capability| {
                    admit_event(state, capability, &automation_id, event, &facts, &timestamp)
                })
                .await?,
            );
        }
        Ok(admissions)
    }
}

fn admit_due(
    state: &mut RuntimeState,
    capability: &CapabilitySnapshot,
    automation_id: &AutomationId,
    timestamp: &str,
    wall_now: DateTime<Utc>,
) -> Result<(AutomationAdmission, bool), DomainError> {
    let Some(index) = state.automations.iter().position(|record| {
        record.deleted_at.is_none() && &record.automation.automation_id == automation_id
    }) else {
        return Ok((AutomationAdmission::NotDue, false));
    };
    let record = &state.automations[index];
    let Some(due_text) = record.next_due_at.clone() else {
        return Ok((AutomationAdmission::NotDue, false));
    };
    if !record.automation.enabled
        || matches!(record.automation.trigger, AutomationTrigger::Event { .. })
    {
        return Ok((AutomationAdmission::NotDue, false));
    }
    let due = stored_instant(&due_text)?;
    if due > wall_now {
        return Ok((AutomationAdmission::NotDue, false));
    }
    if record.active_execution_id.is_some() {
        return Ok((AutomationAdmission::BusyDropped, false));
    }
    // A due the bounded search cannot advance is removed rather than re-admitted (S-LIFE-005).
    let next_due_at = next_due_after_admission(&record.automation.trigger, due, wall_now)
        .ok()
        .flatten()
        .map(format_instant);
    let execution = match create_execution(
        state,
        capability,
        index,
        due_text,
        timestamp,
        BTreeMap::new(),
    ) {
        Ok(execution) => execution,
        Err(error) if admission_rejection(&error) => {
            return Ok((AutomationAdmission::Rejected(error), false));
        }
        Err(error) => return Err(error),
    };
    state.automations[index].next_due_at = next_due_at;
    Ok((AutomationAdmission::Admitted(Box::new(execution)), true))
}

/// Admits one enabled event-triggered Automation whose registered name and exact matches accept
/// `facts`; a busy Automation drops the arrival instead of queueing it (R-AUTO-004).
fn admit_event(
    state: &mut RuntimeState,
    capability: &CapabilitySnapshot,
    automation_id: &AutomationId,
    event: &str,
    facts: &BTreeMap<String, ScalarValue>,
    timestamp: &str,
) -> Result<(AutomationAdmission, bool), DomainError> {
    let Some(index) = state.automations.iter().position(|record| {
        &record.automation.automation_id == automation_id && accepts_event(record, event, facts)
    }) else {
        return Ok((AutomationAdmission::NotDue, false));
    };
    if state.automations[index].active_execution_id.is_some() {
        return Ok((AutomationAdmission::BusyDropped, false));
    }
    match create_execution(
        state,
        capability,
        index,
        timestamp.to_owned(),
        timestamp,
        facts.clone(),
    ) {
        Ok(execution) => Ok((AutomationAdmission::Admitted(Box::new(execution)), true)),
        Err(error) if admission_rejection(&error) => {
            Ok((AutomationAdmission::Rejected(error), false))
        }
        Err(error) => Err(error),
    }
}

/// Failures that reject one admission without faulting the scheduler: a full store or queue, or
/// a Runtime that is not ready.
fn admission_rejection(error: &DomainError) -> bool {
    matches!(
        error.code,
        ErrorCode::ResourceLimit | ErrorCode::CapabilityUnavailable
    )
}

fn accepts_event(
    record: &AutomationRecord,
    event: &str,
    facts: &BTreeMap<String, ScalarValue>,
) -> bool {
    record.deleted_at.is_none()
        && record.automation.enabled
        && matches!(
            &record.automation.trigger,
            AutomationTrigger::Event { name, r#match }
                if name == event
                    && r#match.as_ref().is_none_or(|matches| {
                        matches.iter().all(|(key, value)| facts.get(key) == Some(value))
                    })
        )
}

fn earliest_due(
    state: &RuntimeState,
    blocked: &[AutomationId],
    wall_now: Option<DateTime<Utc>>,
) -> Result<Option<String>, DomainError> {
    let mut earliest = None;
    for record in &state.automations {
        let Some(due) = armable_due(record)? else {
            continue;
        };
        if wall_now.is_some_and(|now| due <= now)
            && blocked.contains(&record.automation.automation_id)
        {
            continue;
        }
        earliest = Some(earliest.map_or(due, |current: DateTime<Utc>| current.min(due)));
    }
    Ok(earliest.map(format_instant))
}

/// The persisted due of a time-triggered Automation that may be armed or admitted now: busy,
/// disabled, deleted and event-triggered records have none (S-LIFE-003, S-AUTO-002).
fn armable_due(record: &AutomationRecord) -> Result<Option<DateTime<Utc>>, DomainError> {
    if record.deleted_at.is_some()
        || !record.automation.enabled
        || record.active_execution_id.is_some()
        || matches!(record.automation.trigger, AutomationTrigger::Event { .. })
    {
        return Ok(None);
    }
    record
        .next_due_at
        .as_deref()
        .map(stored_instant)
        .transpose()
}

/// Creates one queued AutomationExecution with its container Task for the record at `index` and
/// makes it that Automation's single active execution, in the caller's commit (S-AUTO-002).
fn create_execution(
    state: &mut RuntimeState,
    capability: &CapabilitySnapshot,
    index: usize,
    triggered_at: String,
    timestamp: &str,
    trigger_facts: BTreeMap<String, ScalarValue>,
) -> Result<AdmittedAutomationExecution, DomainError> {
    if capability.context.readiness != RuntimeReadiness::Ready {
        return Err(DomainError::new(
            ErrorCode::CapabilityUnavailable,
            "Runtime is not ready for Automation admission",
        ));
    }
    let automation = state.automations[index].automation.clone();
    reserve_admission_capacity(state)?;
    let execution_id = crate::command::new_uuid()?;
    let task_id = crate::command::new_uuid()?;
    let summary = AutomationExecutionSummary {
        execution_id: execution_id.clone(),
        task_id: task_id.clone(),
        state: AutomationExecutionState::Queued,
        triggered_at,
        started_at: None,
        ended_at: None,
        error_code: None,
    };
    let mut lifecycle = TaskLifecycle::new();
    lifecycle.apply(TaskEvent::Queue)?;
    state.tasks.push(TaskRecord {
        task_id,
        execution_id: execution_id.clone(),
        lifecycle,
        tool: MotherTool::Automation,
        action: AUTOMATION_EXECUTION_ACTION.to_owned(),
        created_at: timestamp.to_owned(),
        started_at: None,
        ended_at: None,
        waiting_reason: None,
        origin: TaskOrigin::AutomationExecution {
            automation_id: automation.automation_id.clone(),
            fence: contract::Fence {
                runtime_epoch: capability.fence.runtime_epoch.clone(),
                host_generation: capability.fence.host_generation,
                runtime_instance_id: capability.fence.runtime_instance_id.clone(),
            },
        },
        result: None,
        error: None,
        reserved_bytes: RESERVE_FLOOR_BYTES,
    });
    state
        .automation_executions
        .push(crate::AutomationExecutionRecord {
            automation_id: automation.automation_id.clone(),
            summary: summary.clone(),
        });
    state.automations[index].active_execution_id = Some(execution_id);
    Ok(AdmittedAutomationExecution {
        automation_id: automation.automation_id,
        revision: automation.revision,
        summary,
        trigger: automation.trigger,
        action: automation.action,
        trigger_facts,
    })
}

/// Reserves one queued container Task, one execution summary and its settlement bytes
/// (S-PERSIST-006/007); a full store or queue rejects only this admission.
fn reserve_admission_capacity(state: &mut RuntimeState) -> Result<(), DomainError> {
    let queued = state
        .tasks
        .iter()
        .filter(|task| matches!(task.state(), TaskState::Created | TaskState::Queued))
        .count();
    let non_terminal = state
        .automation_executions
        .iter()
        .filter(|execution| !execution.is_terminal())
        .count();
    if queued >= MAX_QUEUED_TASKS as usize || non_terminal >= MAX_NON_TERMINAL_AUTOMATION_EXECUTIONS
    {
        return Err(DomainError::new(
            ErrorCode::ResourceLimit,
            "Automation admission capacity is full",
        ));
    }
    let growth = TASK_RECORD_BYTES + AUTOMATION_EXECUTION_RECORD_BYTES;
    let total = state
        .total_committed_and_reserved()
        .and_then(|value| value.checked_add(growth))
        .and_then(|value| value.checked_add(RESERVE_FLOOR_BYTES))
        .ok_or_else(|| DomainError::new(ErrorCode::ResourceLimit, "store capacity overflow"))?;
    if total > STORE_LIMIT_BYTES {
        return Err(DomainError::new(
            ErrorCode::ResourceLimit,
            "store settlement capacity is full",
        ));
    }
    state.used_bytes += growth;
    state.reserved_bytes += RESERVE_FLOOR_BYTES;
    Ok(())
}

fn start(
    state: &mut RuntimeState,
    execution_id: &ExecutionId,
    timestamp: &str,
) -> Result<AutomationExecutionSummary, DomainError> {
    let execution_index = execution_index(state, execution_id)?;
    if state.automation_executions[execution_index].summary.state
        != AutomationExecutionState::Queued
    {
        return Err(DomainError::invalid("AutomationExecution is not queued"));
    }
    let task_index = container_task_index(state, execution_id)?;
    if state.tasks[task_index].lifecycle.cancel_requested() {
        return Err(DomainError::new(
            ErrorCode::Cancelled,
            "AutomationExecution cancellation was requested",
        ));
    }
    state.tasks[task_index].lifecycle.apply(TaskEvent::Start)?;
    state.tasks[task_index].started_at = Some(timestamp.to_owned());
    let summary = &mut state.automation_executions[execution_index].summary;
    summary.state = AutomationExecutionState::Running;
    summary.started_at = Some(timestamp.to_owned());
    Ok(summary.clone())
}

/// Settles one non-terminal AutomationExecution with its container Task, releases its
/// reservation and ownership, then finalizes a deletion tombstone or prunes terminal history.
pub(crate) fn settle_automation_in_state(
    state: &mut RuntimeState,
    execution_id: &ExecutionId,
    outcome: AutomationExecutionOutcome,
    timestamp: &str,
) -> Result<AutomationExecutionSummary, DomainError> {
    let execution_index = execution_index(state, execution_id)?;
    if state.automation_executions[execution_index].is_terminal() {
        return Err(DomainError::new(
            ErrorCode::StaleAuthority,
            "AutomationExecution is already terminal",
        ));
    }
    let automation_id = state.automation_executions[execution_index]
        .automation_id
        .clone();
    let task_index = container_task_index(state, execution_id)?;
    let (execution_state, result, error, event) = match outcome {
        AutomationExecutionOutcome::Completed => (
            AutomationExecutionState::Completed,
            Some(TaskTerminalResult::Automation(AutomationTaskResult {
                automation_id: automation_id.clone(),
                execution_id: execution_id.clone(),
                completed: True,
            })),
            None,
            TaskEvent::Complete {
                postcondition_verified: true,
                cleanup_verified: true,
            },
        ),
        AutomationExecutionOutcome::Failed(error) => (
            AutomationExecutionState::Failed,
            None,
            Some(error),
            TaskEvent::Fail {
                cleanup_verified: true,
            },
        ),
        AutomationExecutionOutcome::Cancelled(error) => (
            AutomationExecutionState::Cancelled,
            None,
            Some(error),
            TaskEvent::SettleCancellation {
                cleanup_verified: true,
            },
        ),
        AutomationExecutionOutcome::Interrupted(error) => (
            AutomationExecutionState::Interrupted,
            None,
            Some(error),
            TaskEvent::HostLost,
        ),
    };
    let error_code = error
        .as_ref()
        .map(|error| code_token(error.code))
        .transpose()?;
    {
        let task = &mut state.tasks[task_index];
        if matches!(event, TaskEvent::SettleCancellation { .. }) {
            task.lifecycle.apply(TaskEvent::RequestCancel)?;
        }
        task.lifecycle.apply(event)?;
        task.ended_at = Some(timestamp.to_owned());
        task.result = result;
        task.error = error;
    }
    let reservation = state.tasks[task_index].reserved_bytes;
    state.reserved_bytes = state
        .reserved_bytes
        .checked_sub(reservation)
        .ok_or_else(|| DomainError::new(ErrorCode::InternalError, "reservation underflow"))?;
    state.used_bytes = state
        .used_bytes
        .checked_add(RESERVE_FLOOR_BYTES.min(reservation))
        .ok_or_else(|| DomainError::new(ErrorCode::ResourceLimit, "store size overflow"))?;

    let summary = {
        let summary = &mut state.automation_executions[execution_index].summary;
        summary.state = execution_state;
        summary.ended_at = Some(timestamp.to_owned());
        summary.error_code = error_code;
        summary.clone()
    };
    let record_index = state
        .automations
        .iter()
        .position(|record| record.automation.automation_id == automation_id)
        .ok_or_else(|| {
            DomainError::new(
                ErrorCode::IoError,
                "AutomationExecution names no canonical Automation",
            )
        })?;
    if state.automations[record_index].active_execution_id.as_ref() != Some(execution_id) {
        return Err(DomainError::new(
            ErrorCode::IoError,
            "AutomationExecution does not own its Automation",
        ));
    }
    if state.automations[record_index].deleted_at.is_some() {
        // The last execution of a deleted Automation removes its tombstone, state and history.
        state.automations.remove(record_index);
        let before = state.automation_executions.len();
        state
            .automation_executions
            .retain(|execution| execution.automation_id != automation_id);
        release_execution_bytes(state, before);
    } else {
        state.automations[record_index].active_execution_id = None;
        prune_execution_history(state, &automation_id);
    }
    Ok(summary)
}

/// Interrupts every AutomationExecution whose container Task belongs to a prior Runtime
/// instance (S-PERSIST-005), finalizing tombstones through the ordinary settlement.
pub(crate) fn interrupt_old_instance_automation_executions(
    state: &mut RuntimeState,
    old_instance_id: &UuidV4,
    timestamp: &str,
) -> Result<usize, DomainError> {
    let execution_ids = state
        .tasks
        .iter()
        .filter(|task| {
            !task.lifecycle.is_terminal()
                && matches!(task.origin, TaskOrigin::AutomationExecution { .. })
                && &task.fence().runtime_instance_id == old_instance_id
        })
        .map(|task| task.execution_id.clone())
        .collect::<Vec<_>>();
    for execution_id in &execution_ids {
        settle_automation_in_state(
            state,
            execution_id,
            AutomationExecutionOutcome::Interrupted(PublicError {
                code: ErrorCode::IoError,
                operation: format!("automation.{AUTOMATION_EXECUTION_ACTION}"),
                retryable: false,
                message: None,
                capability: None,
                details: None,
            }),
            timestamp,
        )?;
    }
    Ok(execution_ids.len())
}

/// Removes the oldest terminal summaries of one Automation beyond 100, then globally beyond
/// 2,000, by ascending `(triggered_at, execution_id)` (S-PERSIST-006).
fn prune_execution_history(state: &mut RuntimeState, automation_id: &AutomationId) {
    let mut removed = HashSet::new();
    let mut own = state
        .automation_executions
        .iter()
        .filter(|execution| execution.is_terminal() && &execution.automation_id == automation_id)
        .map(history_key)
        .collect::<Vec<_>>();
    own.sort();
    let excess = own
        .len()
        .saturating_sub(MAX_TERMINAL_EXECUTIONS_PER_AUTOMATION);
    removed.extend(own.into_iter().take(excess).map(|(_, id)| id));

    let mut global = state
        .automation_executions
        .iter()
        .filter(|execution| execution.is_terminal())
        .map(history_key)
        .filter(|(_, id)| !removed.contains(id))
        .collect::<Vec<_>>();
    global.sort();
    let excess = global
        .len()
        .saturating_sub(MAX_TERMINAL_AUTOMATION_EXECUTIONS);
    removed.extend(global.into_iter().take(excess).map(|(_, id)| id));

    if removed.is_empty() {
        return;
    }
    let before = state.automation_executions.len();
    state
        .automation_executions
        .retain(|execution| !removed.contains(execution.summary.execution_id.as_str()));
    release_execution_bytes(state, before);
}

fn history_key(execution: &crate::AutomationExecutionRecord) -> (String, String) {
    (
        execution.summary.triggered_at.clone(),
        execution.summary.execution_id.as_str().to_owned(),
    )
}

fn release_execution_bytes(state: &mut RuntimeState, before: usize) {
    let removed = before.saturating_sub(state.automation_executions.len()) as u64;
    state.used_bytes = state
        .used_bytes
        .saturating_sub(AUTOMATION_EXECUTION_RECORD_BYTES.saturating_mul(removed));
}

fn execution_index(state: &RuntimeState, execution_id: &ExecutionId) -> Result<usize, DomainError> {
    state
        .automation_executions
        .iter()
        .position(|execution| &execution.summary.execution_id == execution_id)
        .ok_or_else(|| DomainError::new(ErrorCode::NotFound, "AutomationExecution not found"))
}

fn container_task_index(
    state: &RuntimeState,
    execution_id: &ExecutionId,
) -> Result<usize, DomainError> {
    state
        .tasks
        .iter()
        .position(|task| {
            &task.execution_id == execution_id
                && matches!(task.origin, TaskOrigin::AutomationExecution { .. })
        })
        .ok_or_else(|| {
            DomainError::new(
                ErrorCode::IoError,
                "AutomationExecution has no container Task",
            )
        })
}

fn code_token(code: ErrorCode) -> Result<String, DomainError> {
    match serde_json::to_value(code) {
        Ok(serde_json::Value::String(token)) => Ok(token),
        _ => Err(DomainError::new(
            ErrorCode::InternalError,
            "error code has no wire token",
        )),
    }
}

fn timestamp_instant(timestamp: &str) -> Result<DateTime<Utc>, DomainError> {
    DateTime::parse_from_rfc3339(timestamp)
        .map(|instant| instant.with_timezone(&Utc))
        .map_err(|_| DomainError::new(ErrorCode::InternalError, "wall-clock instant is invalid"))
}

fn stored_instant(value: &str) -> Result<DateTime<Utc>, DomainError> {
    DateTime::parse_from_rfc3339(value)
        .map(|instant| instant.with_timezone(&Utc))
        .map_err(|_| DomainError::new(ErrorCode::IoError, "persisted Automation due is invalid"))
}
