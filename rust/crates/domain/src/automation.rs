use crate::DomainError;
use contract::{
    AndroidClipboardInput, AndroidLaunchInput, Automation, AutomationAction, AutomationAndroidCall,
    AutomationCommandCall, AutomationCompatibleCall, AutomationFilesystemCall, AutomationId,
    AutomationNetworkCall, AutomationTrigger, AutomationVisualCall, CommandRunInput,
    ConditionOperator, ExecutionId, FileTarget, FilesystemDownloadInput, FilesystemManageInput,
    NetworkDiagnoseInput, PointTarget, RunAs, ScalarValue, TaskId, VisualInteractInput,
};
use std::collections::BTreeMap;

pub const MAX_AUTOMATION_DEPTH: u32 = 16;
pub const MAX_AUTOMATION_NODES: u32 = 512;
pub const MAX_EXPANDED_VISITS: u64 = 10_000;
pub const AUTOMATION_BUDGET_MS: u64 = 3_600_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AutomationMetrics {
    pub depth: u32,
    pub nodes: u32,
    pub expanded_visits: u64,
}

fn bounded(value: &str, min: usize, max: usize, reason: &'static str) -> Result<(), DomainError> {
    if value.len() < min || value.len() > max || value.contains('\0') {
        Err(DomainError::invalid(reason))
    } else {
        Ok(())
    }
}

fn scalar(value: &ScalarValue) -> Result<(), DomainError> {
    if let ScalarValue::String(value) = value {
        bounded(value, 0, 4096, "Automation scalar string is out of bounds")?;
    }
    Ok(())
}

fn state(state: &BTreeMap<String, ScalarValue>) -> Result<(), DomainError> {
    if state.len() > 64 {
        return Err(DomainError::new(
            contract::ErrorCode::ResourceLimit,
            "Automation state contains more than 64 keys",
        ));
    }
    for (key, value) in state {
        bounded(key, 1, 64, "Automation state key is out of bounds")?;
        scalar(value)?;
    }
    Ok(())
}

fn target(target: &FileTarget) -> Result<(), DomainError> {
    bounded(&target.value, 1, 4096, "file target is out of bounds")
}

fn package(value: &str) -> Result<(), DomainError> {
    bounded(value, 1, 255, "package_name is out of bounds")
}

fn class(value: &str) -> Result<(), DomainError> {
    bounded(value, 1, 512, "class_name is out of bounds")
}

fn validate_manage(input: &FilesystemManageInput) -> Result<(), DomainError> {
    match input {
        FilesystemManageInput::Mkdir { target: value, .. }
        | FilesystemManageInput::Delete { target: value, .. } => target(value),
        FilesystemManageInput::Copy {
            source,
            destination,
            ..
        }
        | FilesystemManageInput::Move {
            source,
            destination,
            ..
        } => {
            target(source)?;
            target(destination)
        }
    }
}

fn validate_download(input: &FilesystemDownloadInput) -> Result<(), DomainError> {
    bounded(&input.url, 1, 4096, "download URL is out of bounds")?;
    target(&input.destination)?;
    if !(1000..=3_600_000).contains(&input.timeout_ms) {
        return Err(DomainError::invalid("download timeout is out of bounds"));
    }
    Ok(())
}

fn validate_command(input: &CommandRunInput) -> Result<(), DomainError> {
    bounded(&input.command, 1, 32_768, "command is out of bounds")?;
    if let Some(cwd) = &input.cwd {
        bounded(cwd, 1, 4096, "cwd is out of bounds")?;
        if !cwd.starts_with('/') {
            return Err(DomainError::invalid("cwd must be absolute"));
        }
    }
    if input
        .stdin
        .as_ref()
        .is_some_and(|value| value.len() > 65_536)
    {
        return Err(DomainError::invalid("stdin is out of bounds"));
    }
    let maximum_timeout = match input.run_as {
        RunAs::App | RunAs::Shell => 150_000,
        RunAs::Root => 3_600_000,
    };
    if !(1000..=maximum_timeout).contains(&input.timeout_ms) {
        return Err(DomainError::invalid("command timeout is out of bounds"));
    }
    if !(1024..=1_048_576).contains(&input.max_output_bytes) {
        return Err(DomainError::invalid(
            "command output limit is out of bounds",
        ));
    }
    Ok(())
}

fn validate_network(input: &NetworkDiagnoseInput) -> Result<(), DomainError> {
    match input {
        NetworkDiagnoseInput::Connectivity {} => Ok(()),
        NetworkDiagnoseInput::Dns { name, .. } => {
            bounded(name, 1, 4096, "DNS name is out of bounds")
        }
        NetworkDiagnoseInput::Tcp {
            host,
            port,
            timeout_ms,
        } => {
            bounded(host, 1, 4096, "TCP host is out of bounds")?;
            if *port == 0 || !(100..=60_000).contains(timeout_ms) {
                return Err(DomainError::invalid("TCP parameters are out of bounds"));
            }
            Ok(())
        }
        NetworkDiagnoseInput::Tls {
            host,
            port,
            server_name,
            timeout_ms,
        } => {
            bounded(host, 1, 4096, "TLS host is out of bounds")?;
            if let Some(server_name) = server_name {
                bounded(server_name, 1, 4096, "TLS server_name is out of bounds")?;
            }
            if *port == 0 || !(100..=60_000).contains(timeout_ms) {
                return Err(DomainError::invalid("TLS parameters are out of bounds"));
            }
            Ok(())
        }
        NetworkDiagnoseInput::Route { destination_ip } => {
            bounded(destination_ip, 1, 64, "route destination is out of bounds")
        }
    }
}

fn validate_point(target: &PointTarget) -> Result<(), DomainError> {
    match target {
        PointTarget::Node { node_ref } => bounded(node_ref, 1, 4096, "node_ref is out of bounds"),
        PointTarget::Coordinate { .. } => Ok(()),
    }
}

fn validate_visual(input: &VisualInteractInput) -> Result<(), DomainError> {
    match input {
        VisualInteractInput::Tap { target } | VisualInteractInput::LongPress { target } => {
            validate_point(target)
        }
        VisualInteractInput::Swipe { duration_ms, .. } => {
            if !(1..=10_000).contains(duration_ms) {
                return Err(DomainError::invalid("swipe duration is out of bounds"));
            }
            Ok(())
        }
        VisualInteractInput::Text { text, node_ref } => {
            bounded(text, 0, 65_536, "visual text is out of bounds")?;
            if let Some(node_ref) = node_ref {
                bounded(node_ref, 1, 4096, "node_ref is out of bounds")?;
            }
            Ok(())
        }
        VisualInteractInput::Key { .. } => Ok(()),
    }
}

fn validate_launch(input: &AndroidLaunchInput) -> Result<(), DomainError> {
    match input {
        AndroidLaunchInput::Package { package_name } => package(package_name),
        AndroidLaunchInput::Component {
            package_name,
            class_name,
        } => {
            package(package_name)?;
            class(class_name)
        }
    }
}

fn validate_clipboard(input: &AndroidClipboardInput) -> Result<(), DomainError> {
    match input {
        AndroidClipboardInput::Write { text } if text.len() > 65_536 => {
            Err(DomainError::invalid("clipboard text is out of bounds"))
        }
        _ => Ok(()),
    }
}

fn validate_call(call: &AutomationCompatibleCall) -> Result<(), DomainError> {
    match call {
        AutomationCompatibleCall::Filesystem {
            call: AutomationFilesystemCall::Manage(input),
        } => validate_manage(input),
        AutomationCompatibleCall::Filesystem {
            call: AutomationFilesystemCall::Download(input),
        } => validate_download(input),
        AutomationCompatibleCall::Command {
            call: AutomationCommandCall::Run(input),
        } => validate_command(input),
        AutomationCompatibleCall::Network {
            call: AutomationNetworkCall::Diagnose(input),
        } => validate_network(input),
        AutomationCompatibleCall::Visual {
            call: AutomationVisualCall::Interact(input),
        } => validate_visual(input),
        AutomationCompatibleCall::Android {
            call: AutomationAndroidCall::Launch(input),
        } => validate_launch(input),
        AutomationCompatibleCall::Android {
            call: AutomationAndroidCall::Clipboard(input),
        } => validate_clipboard(input),
    }
}

fn validate_condition(condition: &contract::AutomationCondition) -> Result<(), DomainError> {
    bounded(
        &condition.key,
        1,
        64,
        "Automation condition key is out of bounds",
    )?;
    match (condition.operator, &condition.value) {
        (ConditionOperator::Exists, None) => Ok(()),
        (ConditionOperator::Exists, Some(_)) => {
            Err(DomainError::invalid("exists condition must omit its value"))
        }
        (_, Some(value)) => scalar(value),
        (_, None) => Err(DomainError::invalid(
            "non-exists condition requires a value",
        )),
    }
}

fn walk(action: &AutomationAction) -> Result<AutomationMetrics, DomainError> {
    let leaf = || AutomationMetrics {
        depth: 1,
        nodes: 1,
        expanded_visits: 1,
    };
    let metrics = match action {
        AutomationAction::Call { call } => {
            validate_call(call)?;
            leaf()
        }
        AutomationAction::Delay { duration_ms } => {
            if !(1..=86_400_000).contains(duration_ms) {
                return Err(DomainError::invalid("Automation delay is out of bounds"));
            }
            leaf()
        }
        AutomationAction::SetState { key, value } => {
            bounded(key, 1, 64, "Automation state key is out of bounds")?;
            scalar(value)?;
            leaf()
        }
        AutomationAction::Sequence { children } => {
            if !(1..=64).contains(&children.len()) {
                return Err(DomainError::invalid(
                    "Automation sequence child count is out of bounds",
                ));
            }
            let mut result = leaf();
            for child in children {
                let child = walk(child)?;
                result.depth =
                    result.depth.max(child.depth.checked_add(1).ok_or_else(|| {
                        DomainError::invalid("Automation depth arithmetic overflow")
                    })?);
                result.nodes = result
                    .nodes
                    .checked_add(child.nodes)
                    .ok_or_else(|| DomainError::invalid("Automation node arithmetic overflow"))?;
                result.expanded_visits = result
                    .expanded_visits
                    .checked_add(child.expanded_visits)
                    .ok_or_else(|| DomainError::invalid("Automation visit arithmetic overflow"))?;
            }
            result
        }
        AutomationAction::Conditional {
            condition,
            then,
            else_action,
        } => {
            validate_condition(condition)?;
            let then_metrics = walk(then)?;
            let else_metrics = else_action
                .as_deref()
                .map(walk)
                .transpose()?
                .unwrap_or(leaf());
            AutomationMetrics {
                depth: 1_u32
                    .checked_add(then_metrics.depth.max(else_metrics.depth))
                    .ok_or_else(|| DomainError::invalid("Automation depth arithmetic overflow"))?,
                nodes: 1_u32
                    .checked_add(then_metrics.nodes)
                    .and_then(|value| {
                        value.checked_add(if else_action.is_some() {
                            else_metrics.nodes
                        } else {
                            0
                        })
                    })
                    .ok_or_else(|| DomainError::invalid("Automation node arithmetic overflow"))?,
                expanded_visits: 1_u64
                    .checked_add(then_metrics.expanded_visits.max(if else_action.is_some() {
                        else_metrics.expanded_visits
                    } else {
                        0
                    }))
                    .ok_or_else(|| DomainError::invalid("Automation visit arithmetic overflow"))?,
            }
        }
        AutomationAction::Repeat {
            count,
            action,
            delay_ms,
        } => {
            if !(1..=1000).contains(count) || *delay_ms > 86_400_000 {
                return Err(DomainError::invalid("Automation repeat is out of bounds"));
            }
            let child = walk(action)?;
            AutomationMetrics {
                depth: child
                    .depth
                    .checked_add(1)
                    .ok_or_else(|| DomainError::invalid("Automation depth arithmetic overflow"))?,
                nodes: child
                    .nodes
                    .checked_add(1)
                    .ok_or_else(|| DomainError::invalid("Automation node arithmetic overflow"))?,
                expanded_visits: child
                    .expanded_visits
                    .checked_mul(u64::from(*count))
                    .and_then(|value| value.checked_add(1))
                    .ok_or_else(|| DomainError::invalid("Automation visit arithmetic overflow"))?,
            }
        }
    };
    if metrics.depth > MAX_AUTOMATION_DEPTH
        || metrics.nodes > MAX_AUTOMATION_NODES
        || metrics.expanded_visits > MAX_EXPANDED_VISITS
    {
        return Err(DomainError::invalid(
            "Automation action tree exceeds structural bounds",
        ));
    }
    Ok(metrics)
}

pub fn validate_automation(automation: &Automation) -> Result<AutomationMetrics, DomainError> {
    bounded(&automation.name, 1, 128, "Automation name is out of bounds")?;
    if automation.revision == 0 {
        return Err(DomainError::invalid("Automation revision must start at 1"));
    }
    state(&automation.state)?;
    match &automation.trigger {
        AutomationTrigger::At { at } => {
            bounded(at, 1, 64, "Automation at instant is out of bounds")?;
        }
        AutomationTrigger::Interval { every_ms } => {
            if !(60_000..=2_592_000_000).contains(every_ms) {
                return Err(DomainError::invalid("Automation interval is out of bounds"));
            }
        }
        AutomationTrigger::Rrule { rrule, timezone } => {
            bounded(rrule, 1, 4096, "Automation RRULE is out of bounds")?;
            bounded(timezone, 1, 255, "Automation timezone is out of bounds")?;
        }
        AutomationTrigger::Event { name, r#match } => {
            if !matches!(name.as_str(), "runtime.ready" | "network.default_changed") {
                return Err(DomainError::invalid("Automation event is not registered"));
            }
            if let Some(values) = r#match {
                if values.len() > 32 {
                    return Err(DomainError::invalid(
                        "Automation event match contains more than 32 fields",
                    ));
                }
                for (key, value) in values {
                    bounded(key, 1, 64, "Automation event key is out of bounds")?;
                    scalar(value)?;
                }
            }
        }
    }
    walk(&automation.action)
}

pub fn evaluate_condition(
    condition: &contract::AutomationCondition,
    values: &BTreeMap<String, ScalarValue>,
) -> bool {
    let observed = values.get(&condition.key);
    match condition.operator {
        ConditionOperator::Exists => observed.is_some(),
        ConditionOperator::Equals => observed
            .zip(condition.value.as_ref())
            .is_some_and(|(observed, expected)| observed == expected),
        ConditionOperator::NotEquals => observed
            .zip(condition.value.as_ref())
            .is_some_and(|(observed, expected)| observed != expected),
        ConditionOperator::GreaterThan => match (observed, condition.value.as_ref()) {
            (Some(ScalarValue::Integer(observed)), Some(ScalarValue::Integer(expected))) => {
                observed > expected
            }
            _ => false,
        },
        ConditionOperator::LessThan => match (observed, condition.value.as_ref()) {
            (Some(ScalarValue::Integer(observed)), Some(ScalarValue::Integer(expected))) => {
                observed < expected
            }
            _ => false,
        },
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExecutionSnapshot {
    pub automation_id: AutomationId,
    pub execution_id: ExecutionId,
    pub task_id: TaskId,
    pub revision: u64,
    pub trigger: AutomationTrigger,
    pub action: AutomationAction,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AdmissionDecision {
    Admitted(Box<ExecutionSnapshot>),
    BusyDropped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeleteDisposition {
    RemovedImmediately,
    Tombstoned,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettlementDisposition {
    Retained,
    PurgedTombstone,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AutomationSlot {
    record: Option<Automation>,
    active: Option<ExecutionSnapshot>,
    deleted_at: Option<String>,
}

impl AutomationSlot {
    pub fn new(automation: Automation) -> Result<Self, DomainError> {
        validate_automation(&automation)?;
        Ok(Self {
            record: Some(automation),
            active: None,
            deleted_at: None,
        })
    }

    pub fn visible(&self) -> Option<&Automation> {
        if self.deleted_at.is_some() {
            None
        } else {
            self.record.as_ref()
        }
    }

    pub fn active(&self) -> Option<&ExecutionSnapshot> {
        self.active.as_ref()
    }

    pub fn admit(
        &mut self,
        execution_id: ExecutionId,
        task_id: TaskId,
    ) -> Result<AdmissionDecision, DomainError> {
        if self.deleted_at.is_some() || self.record.is_none() {
            return Err(DomainError::new(
                contract::ErrorCode::NotFound,
                "Automation is not visible",
            ));
        }
        if self.active.is_some() {
            return Ok(AdmissionDecision::BusyDropped);
        }
        let automation = self.record.as_ref().expect("record checked above");
        let snapshot = ExecutionSnapshot {
            automation_id: automation.automation_id.clone(),
            execution_id,
            task_id,
            revision: automation.revision,
            trigger: automation.trigger.clone(),
            action: automation.action.clone(),
        };
        self.active = Some(snapshot.clone());
        Ok(AdmissionDecision::Admitted(Box::new(snapshot)))
    }

    pub fn update_definition(
        &mut self,
        expected_revision: u64,
        name: String,
        enabled: bool,
        trigger: AutomationTrigger,
        action: AutomationAction,
        updated_at: String,
    ) -> Result<u64, DomainError> {
        if self.deleted_at.is_some() {
            return Err(DomainError::new(
                contract::ErrorCode::NotFound,
                "Automation is deleted",
            ));
        }
        let current = self.record.as_ref().ok_or_else(|| {
            DomainError::new(contract::ErrorCode::NotFound, "Automation is removed")
        })?;
        if current.revision != expected_revision {
            return Err(DomainError::new(
                contract::ErrorCode::RevisionConflict,
                "Automation revision does not match",
            ));
        }
        let revision = current.revision.checked_add(1).ok_or_else(|| {
            DomainError::new(contract::ErrorCode::ResourceLimit, "revision exhausted")
        })?;
        let candidate = Automation {
            automation_id: current.automation_id.clone(),
            name,
            enabled,
            trigger,
            action,
            state: current.state.clone(),
            revision,
            created_at: current.created_at.clone(),
            updated_at,
        };
        validate_automation(&candidate)?;
        self.record = Some(candidate);
        Ok(revision)
    }

    pub fn set_enabled(
        &mut self,
        expected_revision: u64,
        enabled: bool,
        updated_at: String,
    ) -> Result<u64, DomainError> {
        let current = self.visible().ok_or_else(|| {
            DomainError::new(contract::ErrorCode::NotFound, "Automation is not visible")
        })?;
        self.update_definition(
            expected_revision,
            current.name.clone(),
            enabled,
            current.trigger.clone(),
            current.action.clone(),
            updated_at,
        )
    }

    pub fn set_state(
        &mut self,
        automation_id: &AutomationId,
        execution_id: &ExecutionId,
        key: String,
        value: ScalarValue,
    ) -> Result<(), DomainError> {
        bounded(&key, 1, 64, "Automation state key is out of bounds")?;
        scalar(&value)?;
        let active = self.active.as_ref().ok_or_else(|| {
            DomainError::new(
                contract::ErrorCode::StaleAuthority,
                "execution is not active",
            )
        })?;
        if &active.automation_id != automation_id || &active.execution_id != execution_id {
            return Err(DomainError::new(
                contract::ErrorCode::StaleAuthority,
                "execution does not own this Automation state",
            ));
        }
        let record = self.record.as_mut().ok_or_else(|| {
            DomainError::new(contract::ErrorCode::NotFound, "Automation is removed")
        })?;
        if !record.state.contains_key(&key) && record.state.len() == 64 {
            return Err(DomainError::new(
                contract::ErrorCode::ResourceLimit,
                "Automation state capacity is full",
            ));
        }
        record.state.insert(key, value);
        Ok(())
    }

    pub fn delete(
        &mut self,
        expected_revision: u64,
        deleted_at: String,
    ) -> Result<DeleteDisposition, DomainError> {
        let record = self.visible().ok_or_else(|| {
            DomainError::new(contract::ErrorCode::NotFound, "Automation is not visible")
        })?;
        if record.revision != expected_revision {
            return Err(DomainError::new(
                contract::ErrorCode::RevisionConflict,
                "Automation revision does not match",
            ));
        }
        let revision = record.revision.checked_add(1).ok_or_else(|| {
            DomainError::new(contract::ErrorCode::ResourceLimit, "revision exhausted")
        })?;
        if self.active.is_some() {
            self.record.as_mut().expect("visible record").revision = revision;
            self.deleted_at = Some(deleted_at);
            Ok(DeleteDisposition::Tombstoned)
        } else {
            self.record = None;
            self.deleted_at = Some(deleted_at);
            Ok(DeleteDisposition::RemovedImmediately)
        }
    }

    pub fn settle(
        &mut self,
        execution_id: &ExecutionId,
    ) -> Result<SettlementDisposition, DomainError> {
        let active = self.active.as_ref().ok_or_else(|| {
            DomainError::new(
                contract::ErrorCode::StaleAuthority,
                "execution is not active",
            )
        })?;
        if &active.execution_id != execution_id {
            return Err(DomainError::new(
                contract::ErrorCode::StaleAuthority,
                "execution identity does not match",
            ));
        }
        self.active = None;
        if self.deleted_at.is_some() {
            self.record = None;
            Ok(SettlementDisposition::PurgedTombstone)
        } else {
            Ok(SettlementDisposition::Retained)
        }
    }
}
