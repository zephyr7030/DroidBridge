use contract::{
    ExecutionClass, MotherTool, PublicError, RequestId, TaskId, TaskSnapshot, TaskState,
    TaskSummary, TaskTerminalResult, UuidV4,
};
use domain::{AdmittedExecutor, DedupIndex, ExecutorRequest, Provider, TaskLifecycle};
use serde::{Deserialize, Serialize};

use crate::ExecutionPayload;

pub const STORE_LIMIT_BYTES: u64 = 8 * 1024 * 1024;
pub const RESERVE_FLOOR_BYTES: u64 = 16 * 1024;
pub const TASK_RECORD_BYTES: u64 = 2048;
pub const SYNCHRONOUS_RECORD_BYTES: u64 = 2048;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderToken {
    AppNative,
    AppFramework,
    Shizuku,
    MagiskNative,
    MagiskFramework,
    Accessibility,
    MediaProjection,
    NotificationListener,
}

impl From<Provider> for ProviderToken {
    fn from(value: Provider) -> Self {
        match value {
            Provider::AppNative => Self::AppNative,
            Provider::AppFramework => Self::AppFramework,
            Provider::Shizuku => Self::Shizuku,
            Provider::MagiskNative => Self::MagiskNative,
            Provider::MagiskFramework => Self::MagiskFramework,
            Provider::Accessibility => Self::Accessibility,
            Provider::MediaProjection => Self::MediaProjection,
            Provider::NotificationListener => Self::NotificationListener,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutorRecord {
    pub host: contract::RuntimeHost,
    pub provider: ProviderToken,
    pub execution_class: contract::ExecutionClass,
    pub capability_generation: u64,
    pub fence: contract::Fence,
}

impl From<&AdmittedExecutor> for ExecutorRecord {
    fn from(value: &AdmittedExecutor) -> Self {
        Self {
            host: value.host(),
            provider: value.provider().into(),
            execution_class: value.execution_class(),
            capability_generation: value.capability_generation(),
            fence: contract::Fence {
                runtime_epoch: value.fence().runtime_epoch.clone(),
                host_generation: value.fence().host_generation,
                runtime_instance_id: value.fence().runtime_instance_id.clone(),
            },
        }
    }
}

/// Where a Task came from. A request Task is executed by one admitted executor; the container
/// Task of an AutomationExecution (R-TASK-003 `automation.execution`) is driven by that execution
/// and has neither a public request nor an executor of its own.
#[derive(Clone, Debug, PartialEq)]
pub enum TaskOrigin {
    Request {
        request_id: RequestId,
        executor: ExecutorRecord,
        route: ExecutorRequest,
        payload: Box<ExecutionPayload>,
    },
    AutomationExecution {
        automation_id: contract::AutomationId,
        fence: contract::Fence,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct TaskRecord {
    pub task_id: TaskId,
    pub execution_id: UuidV4,
    pub lifecycle: TaskLifecycle,
    pub tool: MotherTool,
    pub action: String,
    pub created_at: String,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub waiting_reason: Option<String>,
    pub origin: TaskOrigin,
    pub result: Option<TaskTerminalResult>,
    pub error: Option<PublicError>,
    pub reserved_bytes: u64,
}

impl TaskRecord {
    pub fn state(&self) -> TaskState {
        self.lifecycle.state()
    }

    pub fn request_id(&self) -> Option<&RequestId> {
        match &self.origin {
            TaskOrigin::Request { request_id, .. } => Some(request_id),
            TaskOrigin::AutomationExecution { .. } => None,
        }
    }

    pub fn executor(&self) -> Option<&ExecutorRecord> {
        match &self.origin {
            TaskOrigin::Request { executor, .. } => Some(executor),
            TaskOrigin::AutomationExecution { .. } => None,
        }
    }

    /// The admission fence every Task origin carries.
    pub fn fence(&self) -> &contract::Fence {
        match &self.origin {
            TaskOrigin::Request { executor, .. } => &executor.fence,
            TaskOrigin::AutomationExecution { fence, .. } => fence,
        }
    }

    pub fn summary(&self) -> TaskSummary {
        TaskSummary {
            task_id: self.task_id.clone(),
            state: self.state(),
            tool: self.tool,
            action: self.action.clone(),
            created_at: self.created_at.clone(),
            started_at: self.started_at.clone(),
            ended_at: self.ended_at.clone(),
            waiting_reason: self.waiting_reason.clone(),
        }
    }

    pub fn snapshot(&self) -> TaskSnapshot {
        TaskSnapshot {
            task_id: self.task_id.clone(),
            state: self.state(),
            tool: self.tool,
            action: self.action.clone(),
            created_at: self.created_at.clone(),
            started_at: self.started_at.clone(),
            ended_at: self.ended_at.clone(),
            cancel_requested: self.lifecycle.cancel_requested(),
            waiting_reason: self.waiting_reason.clone(),
            execution_class: self.execution_class(),
            result: self.result.clone(),
            error: self.error.clone(),
        }
    }

    pub fn execution_class(&self) -> Option<ExecutionClass> {
        self.executor().map(|executor| executor.execution_class)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SynchronousExecutionState {
    Running,
    Completed,
    Failed,
    Interrupted,
}

impl SynchronousExecutionState {
    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::Running)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SynchronousExecutionRecord {
    pub request_id: RequestId,
    pub execution_id: UuidV4,
    pub operation: String,
    pub state: SynchronousExecutionState,
    pub ended_at: Option<String>,
    pub executor: ExecutorRecord,
    pub route: ExecutorRequest,
    pub payload: ExecutionPayload,
    /// The result a replay of this request answers with. A completed execution whose result was
    /// larger than [`RETAINED_RESULT_LIMIT_BYTES`] answered its own call and keeps none, so a
    /// replay reports that it cannot be answered again instead of executing a second time.
    pub result: Option<serde_json::Value>,
    pub error: Option<PublicError>,
    pub reserved_bytes: u64,
    pub terminal_bytes: u64,
}

/// The largest synchronous result kept for replay. Every retained result is rewritten with the
/// whole store on each later commit, so observations and long outputs, which a client reissues
/// rather than replays, are answered once and not kept.
pub const RETAINED_RESULT_LIMIT_BYTES: u64 = 8 * 1024;

pub const RETAINED_MUTATION_RECORD_BYTES: u64 = 1024;

/// One canonical Automation record: its definition plus the internal due and deletion-tombstone
/// facts owned by S-LIFE-003/S-LIFE-005 and S-AUTO-002.
#[derive(Clone, Debug, PartialEq)]
pub struct AutomationRecord {
    pub automation: contract::Automation,
    pub next_due_at: Option<String>,
    pub deleted_at: Option<String>,
    pub active_execution_id: Option<contract::ExecutionId>,
    /// A run asked for outside the trigger, admitted by the scheduler's next pass.
    pub run_requested_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AutomationExecutionRecord {
    pub automation_id: contract::AutomationId,
    pub summary: contract::AutomationExecutionSummary,
}

/// The retained public result of a Core-only mutation that has no executor, replayed for the
/// same request_id inside the S-CONTRACT-005 window.
#[derive(Clone, Debug, PartialEq)]
pub struct RetainedMutationRecord {
    pub request_id: RequestId,
    pub result: serde_json::Value,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RuntimeState {
    pub revision: u64,
    pub used_bytes: u64,
    pub reserved_bytes: u64,
    pub tasks: Vec<TaskRecord>,
    pub synchronous_executions: Vec<SynchronousExecutionRecord>,
    pub dedup: DedupIndex,
    pub automations: Vec<AutomationRecord>,
    pub automation_executions: Vec<AutomationExecutionRecord>,
    pub retained_mutations: Vec<RetainedMutationRecord>,
}

impl RuntimeState {
    pub fn task(&self, task_id: &TaskId) -> Option<&TaskRecord> {
        self.tasks.iter().find(|task| &task.task_id == task_id)
    }

    pub fn non_terminal_count(&self) -> usize {
        let tasks = self
            .tasks
            .iter()
            .filter(|task| !task.lifecycle.is_terminal())
            .count();
        let synchronous = self
            .synchronous_executions
            .iter()
            .filter(|execution| !execution.state.is_terminal())
            .count();
        let automations = self
            .automation_executions
            .iter()
            .filter(|execution| {
                matches!(
                    execution.summary.state,
                    contract::AutomationExecutionState::Queued
                        | contract::AutomationExecutionState::Running
                )
            })
            .count();
        tasks + synchronous + automations
    }

    pub fn total_committed_and_reserved(&self) -> Option<u64> {
        self.used_bytes.checked_add(self.reserved_bytes)
    }
}
