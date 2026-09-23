use contract::{
    AutomationId, ErrorCode, ExecutionId, PublicError, RequestId, RuntimeHost, TaskId, TaskState,
    TaskTerminalResult, UuidV4,
};
use domain::{
    AndroidRoute, DedupIndex, ExecutorRequest, FilesystemRoute, NetworkRoute, PackageInspectFact,
    Preflight, TaskEvent, TaskLifecycle, VisualRoute,
};
use runtime::{
    AutomationExecutionRecord, AutomationRecord, ExecutionPayload, ExecutorRecord,
    RetainedMutationRecord, RuntimeState, SynchronousExecutionRecord, SynchronousExecutionState,
    TaskOrigin, TaskRecord,
};
use serde::{Deserialize, Serialize};

pub const STORE_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeOwner {
    pub schema_version: u32,
    pub runtime_epoch: UuidV4,
    pub host: RuntimeHost,
    pub host_generation: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeLive {
    pub runtime_epoch: UuidV4,
    pub host: RuntimeHost,
    pub host_generation: u64,
    pub runtime_instance_id: UuidV4,
    pub boot_id: UuidV4,
    pub pid: u32,
    pub start_ticks: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriterFence {
    pub runtime_epoch: UuidV4,
    pub host: RuntimeHost,
    pub host_generation: u64,
    pub runtime_instance_id: UuidV4,
}

impl WriterFence {
    pub fn matches(&self, owner: &RuntimeOwner, live: &RuntimeLive) -> bool {
        owner.schema_version == 1
            && owner.host_generation > 0
            && live.host_generation > 0
            && live.pid > 0
            && live.start_ticks > 0
            && self.runtime_epoch == owner.runtime_epoch
            && self.host == owner.host
            && self.host_generation == owner.host_generation
            && self.runtime_epoch == live.runtime_epoch
            && self.host == live.host
            && self.host_generation == live.host_generation
            && self.runtime_instance_id == live.runtime_instance_id
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReservationRecord {
    pub execution_id: ExecutionId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<RequestId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    pub reserved_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestRecord {
    pub request_id: RequestId,
    pub payload_sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synchronous_execution: Option<StoredSynchronousExecution>,
    /// Retained public result of a Core-only mutation that has no executor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutation_result: Option<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StoredRoute {
    Command {
        run_as: contract::RunAs,
    },
    Filesystem {
        route: StoredFilesystemRoute,
        target_type: contract::FileTargetType,
        app_preflight: StoredPreflight,
        shizuku_preflight: StoredPreflight,
    },
    Network {
        route: StoredNetworkRoute,
    },
    Visual {
        route: StoredVisualRoute,
    },
    Android {
        route: StoredAndroidRoute,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoredFilesystemRoute {
    InspectOrRead,
    Mutation,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoredPreflight {
    Positive,
    Negative,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoredNetworkRoute {
    InspectOrDiagnose,
    ReadOnlyRouteSupplement,
    Capture,
    Inject,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoredVisualRoute {
    Display,
    Transform,
    Hierarchy,
    Image,
    CoordinateInput,
    KeyInput,
    FocusedText,
    AccessibilityNode,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoredPackageInspectFact {
    ExactSuccess,
    VisibilityOrAbsent,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", content = "fact", rename_all = "snake_case")]
pub enum StoredAndroidRoute {
    PackageInspect(StoredPackageInspectFact),
    PackageList,
    PackageForceStop,
    LaunchOrIntent,
    Clipboard,
    Notification,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredTask {
    /// Present exactly for a Task admitted by one public request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<RequestId>,
    pub task_id: TaskId,
    pub execution_id: ExecutionId,
    pub state: TaskState,
    pub cancel_requested: bool,
    pub tool: contract::MotherTool,
    pub action: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waiting_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor: Option<ExecutorRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<StoredRoute>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<ExecutionPayload>,
    /// Present exactly for the container Task of one AutomationExecution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automation_owner: Option<StoredAutomationTaskOwner>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<TaskTerminalResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<PublicError>,
    pub reserved_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredAutomationTaskOwner {
    pub automation_id: AutomationId,
    pub fence: contract::Fence,
}

impl StoredTask {
    /// The admission fence of either Task origin; `None` only for a malformed persisted origin.
    pub fn fence(&self) -> Option<&contract::Fence> {
        self.executor
            .as_ref()
            .map(|executor| &executor.fence)
            .or_else(|| self.automation_owner.as_ref().map(|owner| &owner.fence))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredSynchronousExecution {
    pub execution_id: ExecutionId,
    pub operation: String,
    pub state: SynchronousExecutionState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    pub executor: ExecutorRecord,
    pub route: StoredRoute,
    pub payload: ExecutionPayload,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<PublicError>,
    pub reserved_bytes: u64,
    pub terminal_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredAutomation {
    pub automation: contract::Automation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_due_at: Option<String>,
    #[serde(default)]
    pub deleted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_execution_id: Option<ExecutionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_requested_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredAutomationExecution {
    pub automation_id: AutomationId,
    pub summary: contract::AutomationExecutionSummary,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalState {
    pub schema_version: u32,
    pub store_revision: u64,
    pub request_records: Vec<RequestRecord>,
    pub tasks: Vec<StoredTask>,
    pub automations: Vec<StoredAutomation>,
    pub automation_executions: Vec<StoredAutomationExecution>,
    pub artifact_manifest: Vec<crate::ArtifactRecord>,
    pub reservations: Vec<ReservationRecord>,
}

impl Default for CanonicalState {
    fn default() -> Self {
        Self {
            schema_version: STORE_SCHEMA_VERSION,
            store_revision: 0,
            request_records: Vec::new(),
            tasks: Vec::new(),
            automations: Vec::new(),
            automation_executions: Vec::new(),
            artifact_manifest: Vec::new(),
            reservations: Vec::new(),
        }
    }
}

impl TryFrom<&RuntimeState> for CanonicalState {
    type Error = domain::DomainError;

    fn try_from(value: &RuntimeState) -> Result<Self, Self::Error> {
        let mut request_records = Vec::with_capacity(value.dedup.entries().len());
        for record in value.dedup.entries() {
            let matching_tasks = value
                .tasks
                .iter()
                .filter(|task| task.request_id() == Some(&record.request_id))
                .collect::<Vec<_>>();
            let matching_synchronous = value
                .synchronous_executions
                .iter()
                .filter(|execution| execution.request_id == record.request_id)
                .collect::<Vec<_>>();
            let matching_mutations = value
                .retained_mutations
                .iter()
                .filter(|mutation| mutation.request_id == record.request_id)
                .collect::<Vec<_>>();
            if [
                matching_tasks.len(),
                matching_synchronous.len(),
                matching_mutations.len(),
            ]
            .iter()
            .sum::<usize>()
                > 1
            {
                return Err(invalid_store(
                    "request identity has multiple canonical results",
                ));
            }
            request_records.push(RequestRecord {
                request_id: record.request_id.clone(),
                payload_sha256: record.payload_sha256.clone(),
                expires_at_ms: record.expires_at_ms,
                task_id: matching_tasks.first().map(|task| task.task_id.clone()),
                synchronous_execution: matching_synchronous
                    .first()
                    .map(|execution| StoredSynchronousExecution::from(*execution)),
                mutation_result: matching_mutations
                    .first()
                    .map(|mutation| mutation.result.clone()),
            });
        }
        let retained = |request_id: &RequestId| {
            value
                .dedup
                .entries()
                .iter()
                .any(|record| &record.request_id == request_id)
        };
        if value
            .synchronous_executions
            .iter()
            .any(|execution| !retained(&execution.request_id))
        {
            return Err(invalid_store(
                "synchronous execution has no retained request record",
            ));
        }
        if value
            .retained_mutations
            .iter()
            .any(|mutation| !retained(&mutation.request_id))
        {
            return Err(invalid_store(
                "retained mutation result has no retained request record",
            ));
        }
        let tasks = value.tasks.iter().map(StoredTask::from).collect::<Vec<_>>();
        let reservations = value
            .tasks
            .iter()
            .filter(|task| !task.lifecycle.is_terminal())
            .map(|task| ReservationRecord {
                execution_id: task.execution_id.clone(),
                request_id: task.request_id().cloned(),
                task_id: Some(task.task_id.clone()),
                reserved_bytes: task.reserved_bytes,
            })
            .chain(
                value
                    .synchronous_executions
                    .iter()
                    .filter(|execution| !execution.state.is_terminal())
                    .map(|execution| ReservationRecord {
                        execution_id: execution.execution_id.clone(),
                        request_id: Some(execution.request_id.clone()),
                        task_id: None,
                        reserved_bytes: execution.reserved_bytes,
                    }),
            )
            .collect();
        Ok(Self {
            schema_version: STORE_SCHEMA_VERSION,
            store_revision: value.revision,
            request_records,
            tasks,
            automations: value
                .automations
                .iter()
                .map(|record| StoredAutomation {
                    automation: record.automation.clone(),
                    next_due_at: record.next_due_at.clone(),
                    deleted: record.deleted_at.is_some(),
                    deleted_at: record.deleted_at.clone(),
                    active_execution_id: record.active_execution_id.clone(),
                    run_requested_at: record.run_requested_at.clone(),
                })
                .collect(),
            automation_executions: value
                .automation_executions
                .iter()
                .map(|execution| StoredAutomationExecution {
                    automation_id: execution.automation_id.clone(),
                    summary: execution.summary.clone(),
                })
                .collect(),
            artifact_manifest: Vec::new(),
            reservations,
        })
    }
}

impl StoredTask {
    fn into_runtime(self) -> Result<TaskRecord, domain::DomainError> {
        let valid = self.reserved_bytes >= runtime::RESERVE_FLOOR_BYTES
            && match self.state {
                TaskState::Created | TaskState::Queued => {
                    self.started_at.is_none()
                        && self.ended_at.is_none()
                        && self.result.is_none()
                        && self.error.is_none()
                }
                TaskState::Running => {
                    self.started_at.is_some()
                        && self.ended_at.is_none()
                        && self.result.is_none()
                        && self.error.is_none()
                }
                TaskState::Completed => {
                    self.started_at.is_some()
                        && self.ended_at.is_some()
                        && self.result.is_some()
                        && self.error.is_none()
                }
                TaskState::Failed | TaskState::Interrupted => {
                    self.ended_at.is_some() && self.result.is_none() && self.error.is_some()
                }
                TaskState::Cancelled => {
                    self.cancel_requested
                        && self.ended_at.is_some()
                        && self.result.is_none()
                        && self
                            .error
                            .as_ref()
                            .is_some_and(|error| error.code == ErrorCode::Cancelled)
                }
            };
        if !valid {
            return Err(invalid_store(
                "Task terminal or lifecycle fields are invalid",
            ));
        }
        let mut lifecycle = TaskLifecycle::new();
        match self.state {
            TaskState::Created => {
                if self.cancel_requested {
                    lifecycle.apply(TaskEvent::RequestCancel)?;
                }
            }
            TaskState::Queued => {
                lifecycle.apply(TaskEvent::Queue)?;
                if self.cancel_requested {
                    lifecycle.apply(TaskEvent::RequestCancel)?;
                }
            }
            TaskState::Running => {
                lifecycle.apply(TaskEvent::Queue)?;
                lifecycle.apply(TaskEvent::Start)?;
                if self.cancel_requested {
                    lifecycle.apply(TaskEvent::RequestCancel)?;
                }
            }
            TaskState::Completed => {
                lifecycle.apply(TaskEvent::Queue)?;
                lifecycle.apply(TaskEvent::Start)?;
                if self.cancel_requested {
                    lifecycle.apply(TaskEvent::RequestCancel)?;
                }
                lifecycle.apply(TaskEvent::Complete {
                    postcondition_verified: true,
                    cleanup_verified: true,
                })?;
            }
            TaskState::Failed => {
                if self.cancel_requested {
                    lifecycle.apply(TaskEvent::RequestCancel)?;
                }
                lifecycle.apply(TaskEvent::Fail {
                    cleanup_verified: true,
                })?;
            }
            TaskState::Cancelled => {
                if !self.cancel_requested {
                    return Err(domain::DomainError::new(
                        ErrorCode::IoError,
                        "cancelled Task is missing cancellation history",
                    ));
                }
                lifecycle.apply(TaskEvent::RequestCancel)?;
                lifecycle.apply(TaskEvent::SettleCancellation {
                    cleanup_verified: true,
                })?;
            }
            TaskState::Interrupted => {
                if self.cancel_requested {
                    lifecycle.apply(TaskEvent::RequestCancel)?;
                }
                lifecycle.apply(TaskEvent::HostLost)?;
            }
        }
        let container = self.tool == contract::MotherTool::Automation && self.action == "execution";
        let origin = match (
            self.request_id,
            self.executor,
            self.route,
            self.payload,
            self.automation_owner,
        ) {
            (Some(request_id), Some(executor), Some(route), Some(payload), None) if !container => {
                TaskOrigin::Request {
                    request_id,
                    executor,
                    route: route.into(),
                    payload: Box::new(payload),
                }
            }
            (None, None, None, None, Some(owner)) if container => TaskOrigin::AutomationExecution {
                automation_id: owner.automation_id,
                fence: owner.fence,
            },
            _ => return Err(invalid_store("Task origin fields are inconsistent")),
        };
        Ok(TaskRecord {
            task_id: self.task_id,
            execution_id: self.execution_id,
            lifecycle,
            tool: self.tool,
            action: self.action,
            created_at: self.created_at,
            started_at: self.started_at,
            ended_at: self.ended_at,
            waiting_reason: self.waiting_reason,
            origin,
            result: self.result,
            error: self.error,
            reserved_bytes: self.reserved_bytes,
        })
    }
}

impl StoredSynchronousExecution {
    fn into_runtime(
        self,
        request_id: RequestId,
    ) -> Result<SynchronousExecutionRecord, domain::DomainError> {
        let measured_terminal_bytes = self
            .result
            .as_ref()
            .map(serde_json::to_vec)
            .or_else(|| self.error.as_ref().map(serde_json::to_vec))
            .transpose()
            .map_err(|_| invalid_store("synchronous terminal result is not encodable"))?
            .map_or(0_u64, |bytes| bytes.len() as u64);
        let valid_terminal = self.terminal_bytes > 0
            && self.terminal_bytes <= self.reserved_bytes
            && measured_terminal_bytes <= self.terminal_bytes
            && self.ended_at.is_some();
        let valid = self.reserved_bytes >= runtime::RESERVE_FLOOR_BYTES
            && !self.operation.is_empty()
            && match self.state {
                SynchronousExecutionState::Running => {
                    self.ended_at.is_none()
                        && self.result.is_none()
                        && self.error.is_none()
                        && self.terminal_bytes == 0
                }
                // A result over the retention bound answered its own call and is not kept.
                SynchronousExecutionState::Completed => valid_terminal && self.error.is_none(),
                SynchronousExecutionState::Failed | SynchronousExecutionState::Interrupted => {
                    valid_terminal && self.result.is_none() && self.error.is_some()
                }
            };
        if !valid {
            return Err(invalid_store("synchronous execution state is invalid"));
        }
        Ok(SynchronousExecutionRecord {
            request_id,
            execution_id: self.execution_id,
            operation: self.operation,
            state: self.state,
            ended_at: self.ended_at,
            executor: self.executor,
            route: self.route.into(),
            payload: self.payload,
            result: self.result,
            error: self.error,
            reserved_bytes: self.reserved_bytes,
            terminal_bytes: self.terminal_bytes,
        })
    }
}

impl TryFrom<CanonicalState> for RuntimeState {
    type Error = domain::DomainError;

    fn try_from(value: CanonicalState) -> Result<Self, Self::Error> {
        if value.schema_version != STORE_SCHEMA_VERSION {
            return Err(domain::DomainError::new(
                ErrorCode::ProtocolIncompatible,
                "unsupported store schema",
            ));
        }
        let tasks = value
            .tasks
            .into_iter()
            .map(StoredTask::into_runtime)
            .collect::<Result<Vec<_>, _>>()?;
        let mut dedup = DedupIndex::default();
        let mut synchronous_executions = Vec::new();
        let mut retained_mutations = Vec::new();
        for record in value.request_records {
            let matching_tasks = tasks
                .iter()
                .filter(|task| task.request_id() == Some(&record.request_id))
                .collect::<Vec<_>>();
            if matching_tasks.len() > 1 {
                return Err(invalid_store("request identity has multiple Tasks"));
            }
            if let Some(task_id) = record.task_id.as_ref()
                && !matching_tasks
                    .first()
                    .is_some_and(|task| &task.task_id == task_id)
            {
                return Err(invalid_store("request Task reference is invalid"));
            }
            if record.synchronous_execution.is_some()
                && (record.task_id.is_some() || !matching_tasks.is_empty())
            {
                return Err(invalid_store(
                    "request cannot reference both Task and synchronous execution",
                ));
            }
            if record.mutation_result.is_some()
                && (record.task_id.is_some()
                    || !matching_tasks.is_empty()
                    || record.synchronous_execution.is_some()
                    || record.expires_at_ms.is_none())
            {
                return Err(invalid_store(
                    "retained mutation result must be the only settled request result",
                ));
            }
            if let Some(task) = matching_tasks.first()
                && task.lifecycle.is_terminal() != record.expires_at_ms.is_some()
            {
                return Err(invalid_store(
                    "Task request retention does not match terminal state",
                ));
            }
            if let Some(execution) = record.synchronous_execution.as_ref()
                && execution.state.is_terminal() != record.expires_at_ms.is_some()
            {
                return Err(invalid_store(
                    "synchronous request retention does not match terminal state",
                ));
            }
            dedup.decide_and_reserve(record.request_id.clone(), record.payload_sha256, 0, false)?;
            if let Some(expiry) = record.expires_at_ms {
                dedup.settle(&record.request_id, expiry.saturating_sub(86_400_000))?;
            }
            if let Some(result) = record.mutation_result {
                retained_mutations.push(RetainedMutationRecord {
                    request_id: record.request_id.clone(),
                    result,
                });
            }
            if let Some(execution) = record.synchronous_execution {
                synchronous_executions.push(execution.into_runtime(record.request_id)?);
            }
        }
        let (automations, automation_executions) =
            automation_records(value.automations, value.automation_executions)?;
        let mut execution_ids = std::collections::HashSet::new();
        if tasks
            .iter()
            .map(|task| task.execution_id.as_str())
            .chain(
                synchronous_executions
                    .iter()
                    .map(|execution| execution.execution_id.as_str()),
            )
            .any(|execution_id| !execution_ids.insert(execution_id.to_owned()))
        {
            return Err(invalid_store("duplicate canonical execution identity"));
        }
        validate_reservations(&value.reservations, &tasks, &synchronous_executions)?;
        let reserved_bytes = value
            .reservations
            .iter()
            .try_fold(0_u64, |sum, record| sum.checked_add(record.reserved_bytes));
        let reserved_bytes =
            reserved_bytes.ok_or_else(|| invalid_store("reservation accounting overflow"))?;
        Ok(RuntimeState {
            revision: value.store_revision,
            used_bytes: 0,
            reserved_bytes,
            tasks,
            synchronous_executions,
            dedup,
            automations,
            automation_executions,
            retained_mutations,
        })
    }
}

/// Maps persisted Automation records into Runtime records, rejecting tombstone facts that
/// disagree and executions that name no canonical Automation.
fn automation_records(
    automations: Vec<StoredAutomation>,
    executions: Vec<StoredAutomationExecution>,
) -> Result<(Vec<AutomationRecord>, Vec<AutomationExecutionRecord>), domain::DomainError> {
    let mut identities = std::collections::HashSet::new();
    let mut records = Vec::with_capacity(automations.len());
    for stored in automations {
        if stored.deleted != stored.deleted_at.is_some()
            || (stored.deleted
                && (stored.active_execution_id.is_none() || stored.next_due_at.is_some()))
        {
            return Err(invalid_store(
                "Automation deletion tombstone is inconsistent",
            ));
        }
        if !identities.insert(stored.automation.automation_id.as_str().to_owned()) {
            return Err(invalid_store("duplicate canonical Automation identity"));
        }
        records.push(AutomationRecord {
            automation: stored.automation,
            next_due_at: stored.next_due_at,
            deleted_at: stored.deleted_at,
            active_execution_id: stored.active_execution_id,
            run_requested_at: stored.run_requested_at,
        });
    }
    let executions = executions
        .into_iter()
        .map(|stored| {
            if identities.contains(stored.automation_id.as_str()) {
                Ok(AutomationExecutionRecord {
                    automation_id: stored.automation_id,
                    summary: stored.summary,
                })
            } else {
                Err(invalid_store("Automation execution names no Automation"))
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((records, executions))
}

impl From<&SynchronousExecutionRecord> for StoredSynchronousExecution {
    fn from(value: &SynchronousExecutionRecord) -> Self {
        Self {
            execution_id: value.execution_id.clone(),
            operation: value.operation.clone(),
            state: value.state,
            ended_at: value.ended_at.clone(),
            executor: value.executor.clone(),
            route: value.route.into(),
            payload: value.payload.clone(),
            result: value.result.clone(),
            error: value.error.clone(),
            reserved_bytes: value.reserved_bytes,
            terminal_bytes: value.terminal_bytes,
        }
    }
}

fn validate_reservations(
    reservations: &[ReservationRecord],
    tasks: &[TaskRecord],
    synchronous_executions: &[SynchronousExecutionRecord],
) -> Result<(), domain::DomainError> {
    let mut expected = tasks
        .iter()
        .filter(|task| !task.lifecycle.is_terminal())
        .map(|task| ReservationRecord {
            execution_id: task.execution_id.clone(),
            request_id: task.request_id().cloned(),
            task_id: Some(task.task_id.clone()),
            reserved_bytes: task.reserved_bytes,
        })
        .chain(
            synchronous_executions
                .iter()
                .filter(|execution| !execution.state.is_terminal())
                .map(|execution| ReservationRecord {
                    execution_id: execution.execution_id.clone(),
                    request_id: Some(execution.request_id.clone()),
                    task_id: None,
                    reserved_bytes: execution.reserved_bytes,
                }),
        )
        .collect::<Vec<_>>();
    let mut actual = reservations.to_vec();
    expected.sort_by(|left, right| left.execution_id.as_str().cmp(right.execution_id.as_str()));
    actual.sort_by(|left, right| left.execution_id.as_str().cmp(right.execution_id.as_str()));
    if actual != expected {
        return Err(invalid_store(
            "reservation records do not match non-terminal executions",
        ));
    }
    Ok(())
}

fn invalid_store(message: &'static str) -> domain::DomainError {
    domain::DomainError::new(ErrorCode::IoError, message)
}

impl From<&TaskRecord> for StoredTask {
    fn from(value: &TaskRecord) -> Self {
        Self {
            request_id: value.request_id().cloned(),
            task_id: value.task_id.clone(),
            execution_id: value.execution_id.clone(),
            state: value.state(),
            cancel_requested: value.lifecycle.cancel_requested(),
            tool: value.tool,
            action: value.action.clone(),
            created_at: value.created_at.clone(),
            started_at: value.started_at.clone(),
            ended_at: value.ended_at.clone(),
            waiting_reason: value.waiting_reason.clone(),
            executor: value.executor().cloned(),
            route: match &value.origin {
                TaskOrigin::Request { route, .. } => Some((*route).into()),
                TaskOrigin::AutomationExecution { .. } => None,
            },
            payload: match &value.origin {
                TaskOrigin::Request { payload, .. } => Some((**payload).clone()),
                TaskOrigin::AutomationExecution { .. } => None,
            },
            automation_owner: match &value.origin {
                TaskOrigin::Request { .. } => None,
                TaskOrigin::AutomationExecution {
                    automation_id,
                    fence,
                } => Some(StoredAutomationTaskOwner {
                    automation_id: automation_id.clone(),
                    fence: fence.clone(),
                }),
            },
            result: value.result.clone(),
            error: value.error.clone(),
            reserved_bytes: value.reserved_bytes,
        }
    }
}

impl From<ExecutorRequest> for StoredRoute {
    fn from(value: ExecutorRequest) -> Self {
        match value {
            ExecutorRequest::Command(run_as) => Self::Command { run_as },
            ExecutorRequest::Filesystem {
                route,
                target_type,
                app_preflight,
                shizuku_preflight,
            } => Self::Filesystem {
                route: route.into(),
                target_type,
                app_preflight: app_preflight.into(),
                shizuku_preflight: shizuku_preflight.into(),
            },
            ExecutorRequest::Network(route) => Self::Network {
                route: route.into(),
            },
            ExecutorRequest::Visual(route) => Self::Visual {
                route: route.into(),
            },
            ExecutorRequest::Android(route) => Self::Android {
                route: route.into(),
            },
        }
    }
}

impl From<StoredRoute> for ExecutorRequest {
    fn from(value: StoredRoute) -> Self {
        match value {
            StoredRoute::Command { run_as } => Self::Command(run_as),
            StoredRoute::Filesystem {
                route,
                target_type,
                app_preflight,
                shizuku_preflight,
            } => Self::Filesystem {
                route: route.into(),
                target_type,
                app_preflight: app_preflight.into(),
                shizuku_preflight: shizuku_preflight.into(),
            },
            StoredRoute::Network { route } => Self::Network(route.into()),
            StoredRoute::Visual { route } => Self::Visual(route.into()),
            StoredRoute::Android { route } => Self::Android(route.into()),
        }
    }
}

macro_rules! mirror {
    ($source:ty, $stored:ty, {$($variant:ident),+ $(,)?}) => {
        impl From<$source> for $stored {
            fn from(value: $source) -> Self { match value { $(<$source>::$variant => Self::$variant),+ } }
        }
        impl From<$stored> for $source {
            fn from(value: $stored) -> Self { match value { $(<$stored>::$variant => Self::$variant),+ } }
        }
    };
}

mirror!(FilesystemRoute, StoredFilesystemRoute, { InspectOrRead, Mutation });
mirror!(Preflight, StoredPreflight, { Positive, Negative, Unknown });
mirror!(NetworkRoute, StoredNetworkRoute, { InspectOrDiagnose, ReadOnlyRouteSupplement, Capture, Inject });
mirror!(VisualRoute, StoredVisualRoute, { Display, Transform, Hierarchy, Image, CoordinateInput, KeyInput, FocusedText, AccessibilityNode });
mirror!(PackageInspectFact, StoredPackageInspectFact, { ExactSuccess, VisibilityOrAbsent, Unknown });

impl From<AndroidRoute> for StoredAndroidRoute {
    fn from(value: AndroidRoute) -> Self {
        match value {
            AndroidRoute::PackageInspect(fact) => Self::PackageInspect(fact.into()),
            AndroidRoute::PackageList => Self::PackageList,
            AndroidRoute::PackageForceStop => Self::PackageForceStop,
            AndroidRoute::LaunchOrIntent => Self::LaunchOrIntent,
            AndroidRoute::Clipboard => Self::Clipboard,
            AndroidRoute::Notification => Self::Notification,
        }
    }
}

impl From<StoredAndroidRoute> for AndroidRoute {
    fn from(value: StoredAndroidRoute) -> Self {
        match value {
            StoredAndroidRoute::PackageInspect(fact) => Self::PackageInspect(fact.into()),
            StoredAndroidRoute::PackageList => Self::PackageList,
            StoredAndroidRoute::PackageForceStop => Self::PackageForceStop,
            StoredAndroidRoute::LaunchOrIntent => Self::LaunchOrIntent,
            StoredAndroidRoute::Clipboard => Self::Clipboard,
            StoredAndroidRoute::Notification => Self::Notification,
        }
    }
}
