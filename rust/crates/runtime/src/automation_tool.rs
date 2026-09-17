//! Public `automation` action handlers (R-AUTO-001, R-AUTO-014..018) and the time-trigger due
//! computation owned by S-LIFE-005. Core contributes only generic store transactions; definition
//! revision and deletion rules stay with the Domain `AutomationSlot`.

use crate::{
    ArtifactPort, AutomationExecutionRecord, AutomationRecord, CapabilityPort, ExecutionPort,
    HostControlPort, PersistencePort, RuntimeCore, RuntimeState,
};
use chrono::{DateTime, LocalResult, NaiveDateTime, SecondsFormat, TimeDelta, TimeZone, Utc};
use contract::{
    Automation, AutomationCall, AutomationDeleteInput, AutomationDeleteResult,
    AutomationExecutionSummary, AutomationGetInput, AutomationGetResult, AutomationId,
    AutomationListInput, AutomationListResult, AutomationSaveInput, AutomationSetEnabledInput,
    AutomationSummary, AutomationTrigger, ErrorCode, RequestId, True,
};
use domain::{AutomationSlot, DeleteDisposition, DomainError};
use std::{collections::BTreeMap, str::FromStr};

/// S-PERSIST-006 definition bound; deletion tombstones count toward it.
pub const MAX_AUTOMATION_DEFINITIONS: usize = 256;
/// S-LIFE-005 bound on one RRULE occurrence search from DTSTART.
pub const RRULE_OCCURRENCE_SEARCH_LIMIT: usize = 1_000_000;

pub async fn handle_automation_public<P, A, E, C, H>(
    core: &RuntimeCore<P, A, E, C, H>,
    request_id: RequestId,
    payload_sha256: String,
    call: AutomationCall,
    timestamp: String,
    now_ms: u64,
    business_admission_open: bool,
) -> Result<serde_json::Value, DomainError>
where
    P: PersistencePort + 'static,
    A: ArtifactPort + 'static,
    E: ExecutionPort + 'static,
    C: CapabilityPort + 'static,
    H: HostControlPort + 'static,
{
    match call {
        AutomationCall::List(input) => {
            if !(1..=500).contains(&input.limit) {
                return Err(DomainError::invalid(
                    "Automation list limit is out of bounds",
                ));
            }
            core.read_state(|state| list(state, &input)).await
        }
        AutomationCall::Get(input) => {
            if !(1..=100).contains(&input.history_limit) {
                return Err(DomainError::invalid(
                    "Automation history limit is out of bounds",
                ));
            }
            core.read_state(|state| get(state, &input)).await
        }
        _ if !business_admission_open => Err(DomainError::new(
            ErrorCode::HostTransitionPending,
            "Runtime host transition is pending",
        )),
        AutomationCall::Save(input) => {
            let commit = commit_instant(&timestamp)?;
            core.retained_mutation(request_id, payload_sha256, now_ms, |state| {
                save(state, input, &timestamp, commit)
            })
            .await
        }
        AutomationCall::SetEnabled(input) => {
            let commit = commit_instant(&timestamp)?;
            core.retained_mutation(request_id, payload_sha256, now_ms, |state| {
                set_enabled(state, input, &timestamp, commit)
            })
            .await
        }
        AutomationCall::Delete(input) => {
            core.retained_mutation(request_id, payload_sha256, now_ms, |state| {
                delete(state, input, &timestamp)
            })
            .await
        }
    }
}

fn list(
    state: &RuntimeState,
    input: &AutomationListInput,
) -> Result<serde_json::Value, DomainError> {
    let mut automations = state
        .automations
        .iter()
        .filter(|record| record.deleted_at.is_none())
        .map(|record| AutomationSummary {
            automation_id: record.automation.automation_id.clone(),
            name: record.automation.name.clone(),
            enabled: record.automation.enabled,
            revision: record.automation.revision,
            updated_at: record.automation.updated_at.clone(),
            last_execution: history(state, &record.automation.automation_id)
                .into_iter()
                .next(),
        })
        .collect::<Vec<_>>();
    automations.sort_by(|left, right| {
        right.updated_at.cmp(&left.updated_at).then_with(|| {
            right
                .automation_id
                .as_str()
                .cmp(left.automation_id.as_str())
        })
    });
    automations.truncate(input.limit as usize);
    to_value(&AutomationListResult { automations })
}

fn get(state: &RuntimeState, input: &AutomationGetInput) -> Result<serde_json::Value, DomainError> {
    let index = visible_index(state, &input.automation_id)?;
    let mut history = history(state, &input.automation_id);
    history.truncate(input.history_limit as usize);
    to_value(&AutomationGetResult {
        automation: state.automations[index].automation.clone(),
        history,
    })
}

/// AutomationExecution summaries for one Automation, newest first.
fn history(state: &RuntimeState, automation_id: &AutomationId) -> Vec<AutomationExecutionSummary> {
    let mut summaries = state
        .automation_executions
        .iter()
        .filter(|execution| &execution.automation_id == automation_id)
        .map(|execution| execution.summary.clone())
        .collect::<Vec<_>>();
    summaries.sort_by(|left, right| {
        right
            .triggered_at
            .cmp(&left.triggered_at)
            .then_with(|| right.execution_id.as_str().cmp(left.execution_id.as_str()))
    });
    summaries
}

fn save(
    state: &mut RuntimeState,
    input: AutomationSaveInput,
    timestamp: &str,
    commit: DateTime<Utc>,
) -> Result<serde_json::Value, DomainError> {
    match input {
        AutomationSaveInput::Create(input) => {
            if state.automations.len() >= MAX_AUTOMATION_DEFINITIONS {
                return Err(DomainError::new(
                    ErrorCode::ResourceLimit,
                    "Automation definition capacity is full",
                ));
            }
            let automation = Automation {
                automation_id: crate::command::new_uuid()?,
                name: input.name,
                enabled: input.enabled,
                trigger: input.trigger,
                action: input.action,
                state: BTreeMap::new(),
                revision: 1,
                created_at: timestamp.to_owned(),
                updated_at: timestamp.to_owned(),
            };
            AutomationSlot::new(automation.clone())?;
            let next_due_at = scheduled_due(&automation, None, commit)?;
            state.automations.push(AutomationRecord {
                automation: automation.clone(),
                next_due_at,
                deleted_at: None,
                active_execution_id: None,
            });
            to_value(&automation)
        }
        AutomationSaveInput::Update(input) => {
            let index = visible_index(state, &input.automation_id)?;
            let mut slot = definition_slot(state, index)?;
            slot.update_definition(
                input.expected_revision,
                input.name,
                input.enabled,
                input.trigger,
                input.action,
                timestamp.to_owned(),
            )?;
            apply_definition(&mut state.automations[index], &slot, commit)
        }
    }
}

fn set_enabled(
    state: &mut RuntimeState,
    input: AutomationSetEnabledInput,
    timestamp: &str,
    commit: DateTime<Utc>,
) -> Result<serde_json::Value, DomainError> {
    let index = visible_index(state, &input.automation_id)?;
    let mut slot = definition_slot(state, index)?;
    slot.set_enabled(input.expected_revision, input.enabled, timestamp.to_owned())?;
    apply_definition(&mut state.automations[index], &slot, commit)
}

fn delete(
    state: &mut RuntimeState,
    input: AutomationDeleteInput,
    timestamp: &str,
) -> Result<serde_json::Value, DomainError> {
    let index = visible_index(state, &input.automation_id)?;
    let mut slot = definition_slot(state, index)?;
    match slot.delete(input.expected_revision, timestamp.to_owned())? {
        DeleteDisposition::RemovedImmediately => {
            state.automations.remove(index);
            state
                .automation_executions
                .retain(|execution| execution.automation_id != input.automation_id);
        }
        DeleteDisposition::Tombstoned => {
            let record = &mut state.automations[index];
            record.automation.revision = input
                .expected_revision
                .checked_add(1)
                .ok_or_else(|| DomainError::new(ErrorCode::ResourceLimit, "revision exhausted"))?;
            record.deleted_at = Some(timestamp.to_owned());
            record.next_due_at = None;
        }
    }
    to_value(&AutomationDeleteResult {
        automation_id: input.automation_id,
        deleted: True,
        previous_revision: input.expected_revision,
    })
}

fn visible_index(state: &RuntimeState, automation_id: &AutomationId) -> Result<usize, DomainError> {
    state
        .automations
        .iter()
        .position(|record| {
            record.deleted_at.is_none() && &record.automation.automation_id == automation_id
        })
        .ok_or_else(|| DomainError::new(ErrorCode::NotFound, "Automation not found"))
}

/// Rebuilds the Domain slot for a visible record, including its active execution ownership, so
/// revision and deletion decisions keep one semantic owner.
fn definition_slot(state: &RuntimeState, index: usize) -> Result<AutomationSlot, DomainError> {
    let record = &state.automations[index];
    let mut slot = AutomationSlot::new(record.automation.clone())?;
    if let Some(execution_id) = &record.active_execution_id {
        let task_id = state
            .automation_executions
            .iter()
            .find(|execution| &execution.summary.execution_id == execution_id)
            .map(|execution| execution.summary.task_id.clone())
            .ok_or_else(|| {
                DomainError::new(
                    ErrorCode::IoError,
                    "active Automation execution has no canonical summary",
                )
            })?;
        slot.admit(execution_id.clone(), task_id)?;
    }
    Ok(slot)
}

fn apply_definition(
    record: &mut AutomationRecord,
    slot: &AutomationSlot,
    commit: DateTime<Utc>,
) -> Result<serde_json::Value, DomainError> {
    let updated = slot
        .visible()
        .cloned()
        .ok_or_else(|| DomainError::new(ErrorCode::NotFound, "Automation not found"))?;
    let next_due_at = scheduled_due(&updated, Some(record), commit)?;
    record.automation = updated.clone();
    record.next_due_at = next_due_at;
    to_value(&updated)
}

/// The next_due_at a committed definition carries (S-LIFE-005): preserved when an enabled
/// Automation keeps its trigger and still has a due, recomputed otherwise, removed when disabled.
fn scheduled_due(
    updated: &Automation,
    previous: Option<&AutomationRecord>,
    commit: DateTime<Utc>,
) -> Result<Option<String>, DomainError> {
    if !updated.enabled {
        return Ok(None);
    }
    if let Some(previous) = previous
        && previous.automation.enabled
        && previous.automation.trigger == updated.trigger
        && previous.next_due_at.is_some()
    {
        return Ok(previous.next_due_at.clone());
    }
    Ok(initial_due(&updated.trigger, commit)?.map(format_instant))
}

/// First due of a time trigger committed at `commit`; event triggers have none.
pub fn initial_due(
    trigger: &AutomationTrigger,
    commit: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>, DomainError> {
    match trigger {
        AutomationTrigger::At { at } => {
            let at = parse_instant(at)?;
            if at <= commit {
                return Err(DomainError::invalid(
                    "Automation at instant is not after the commit instant",
                ));
            }
            Ok(Some(at))
        }
        AutomationTrigger::Interval { every_ms } => add_millis(commit, *every_ms).map(Some),
        AutomationTrigger::Rrule { rrule, timezone } => {
            rrule_occurrence_after(rrule, timezone, commit)?
                .map(Some)
                .ok_or_else(|| {
                    DomainError::invalid(
                        "Automation RRULE has no occurrence after the commit instant",
                    )
                })
        }
        AutomationTrigger::Event { .. } => Ok(None),
    }
}

/// The due that replaces `due` once its occurrence is admitted at `wall_now`: `at` has none,
/// interval and RRULE coalesce missed occurrences to the first one strictly after `wall_now`.
/// `None` for an RRULE means it is exhausted or exceeded the occurrence search bound.
pub fn next_due_after_admission(
    trigger: &AutomationTrigger,
    due: DateTime<Utc>,
    wall_now: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>, DomainError> {
    match trigger {
        AutomationTrigger::At { .. } | AutomationTrigger::Event { .. } => Ok(None),
        AutomationTrigger::Interval { every_ms } => {
            let every = i64::try_from(*every_ms)
                .map_err(|_| DomainError::invalid("Automation interval is out of bounds"))?;
            let elapsed = (wall_now - due).num_milliseconds();
            let steps = if elapsed < 0 { 1 } else { elapsed / every + 1 };
            steps
                .checked_mul(every)
                .and_then(TimeDelta::try_milliseconds)
                .and_then(|delta| due.checked_add_signed(delta))
                .map(Some)
                .ok_or_else(|| DomainError::invalid("Automation due arithmetic overflow"))
        }
        AutomationTrigger::Rrule { rrule, timezone } => {
            rrule_occurrence_after(rrule, timezone, wall_now)
        }
    }
}

pub fn format_instant(instant: DateTime<Utc>) -> String {
    instant.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn commit_instant(timestamp: &str) -> Result<DateTime<Utc>, DomainError> {
    DateTime::parse_from_rfc3339(timestamp)
        .map(|instant| instant.with_timezone(&Utc))
        .map_err(|_| DomainError::new(ErrorCode::InternalError, "commit instant is invalid"))
}

fn parse_instant(value: &str) -> Result<DateTime<Utc>, DomainError> {
    DateTime::parse_from_rfc3339(value)
        .map(|instant| instant.with_timezone(&Utc))
        .map_err(|_| DomainError::invalid("Automation instant is not RFC3339"))
}

fn add_millis(instant: DateTime<Utc>, millis: u64) -> Result<DateTime<Utc>, DomainError> {
    i64::try_from(millis)
        .ok()
        .and_then(TimeDelta::try_milliseconds)
        .and_then(|delta| instant.checked_add_signed(delta))
        .ok_or_else(|| DomainError::invalid("Automation due arithmetic overflow"))
}

/// Parses the exact two-line S-LIFE-005 RRULE value in the definition's IANA timezone.
fn rrule_set(rrule: &str, timezone: &str) -> Result<rrule::RRuleSet, DomainError> {
    let invalid = || DomainError::invalid("Automation RRULE is invalid");
    let zone = chrono_tz::Tz::from_str(timezone)
        .map_err(|_| DomainError::invalid("Automation timezone is not an IANA zone"))?;
    let normalized = rrule.replace("\r\n", "\n");
    let [dtstart, rule] = normalized.split('\n').collect::<Vec<_>>()[..] else {
        return Err(invalid());
    };
    let local = dtstart.strip_prefix("DTSTART:").ok_or_else(invalid)?;
    let rule = rule.strip_prefix("RRULE:").ok_or_else(invalid)?;
    let well_formed_local = local.len() == 15
        && local.bytes().enumerate().all(|(index, byte)| {
            if index == 8 {
                byte == b'T'
            } else {
                byte.is_ascii_digit()
            }
        });
    if !well_formed_local || rule.is_empty() || rule.contains('\r') {
        return Err(invalid());
    }
    let naive = NaiveDateTime::parse_from_str(local, "%Y%m%dT%H%M%S").map_err(|_| invalid())?;
    if matches!(zone.from_local_datetime(&naive), LocalResult::None) {
        return Err(DomainError::invalid(
            "Automation RRULE DTSTART does not exist in its timezone",
        ));
    }
    format!("DTSTART;TZID={}:{local}\nRRULE:{rule}", zone.name())
        .parse::<rrule::RRuleSet>()
        .map_err(|_| invalid())
}

fn rrule_occurrence_after(
    rrule: &str,
    timezone: &str,
    instant: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>, DomainError> {
    let set = rrule_set(rrule, timezone)?.limit();
    Ok((&set)
        .into_iter()
        .take(RRULE_OCCURRENCE_SEARCH_LIMIT)
        .map(|occurrence| occurrence.with_timezone(&Utc))
        .find(|occurrence| *occurrence > instant))
}

fn to_value<T: serde::Serialize>(value: &T) -> Result<serde_json::Value, DomainError> {
    serde_json::to_value(value).map_err(|_| {
        DomainError::new(
            ErrorCode::InternalError,
            "Automation result serialization failed",
        )
    })
}

impl AutomationExecutionRecord {
    pub fn is_terminal(&self) -> bool {
        !matches!(
            self.summary.state,
            contract::AutomationExecutionState::Queued
                | contract::AutomationExecutionState::Running
        )
    }
}
