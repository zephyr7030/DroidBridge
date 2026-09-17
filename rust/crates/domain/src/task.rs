use crate::DomainError;
use contract::{ErrorCode, TaskState};

pub const MAX_RUNNING_TASKS: u32 = 64;
pub const MAX_QUEUED_TASKS: u32 = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TaskLifecycle {
    state: TaskState,
    cancel_requested: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskEvent {
    Queue,
    Start,
    Complete {
        postcondition_verified: bool,
        cleanup_verified: bool,
    },
    Fail {
        cleanup_verified: bool,
    },
    RequestCancel,
    SettleCancellation {
        cleanup_verified: bool,
    },
    HostLost,
}

impl Default for TaskLifecycle {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskLifecycle {
    pub const fn new() -> Self {
        Self {
            state: TaskState::Created,
            cancel_requested: false,
        }
    }

    pub const fn state(&self) -> TaskState {
        self.state
    }

    pub const fn cancel_requested(&self) -> bool {
        self.cancel_requested
    }

    pub const fn is_terminal(&self) -> bool {
        matches!(
            self.state,
            TaskState::Completed
                | TaskState::Failed
                | TaskState::Cancelled
                | TaskState::Interrupted
        )
    }

    pub fn apply(&mut self, event: TaskEvent) -> Result<TaskState, DomainError> {
        if self.is_terminal() {
            return match event {
                TaskEvent::RequestCancel => Ok(self.state),
                _ => Err(DomainError::invalid("terminal task transition")),
            };
        }
        match event {
            TaskEvent::Queue if self.state == TaskState::Created => {
                self.state = TaskState::Queued;
            }
            TaskEvent::Start if self.state == TaskState::Queued => {
                self.state = TaskState::Running;
            }
            TaskEvent::Complete {
                postcondition_verified: true,
                cleanup_verified: true,
            } if self.state == TaskState::Running => {
                self.state = TaskState::Completed;
            }
            TaskEvent::Complete {
                postcondition_verified: true,
                cleanup_verified: false,
            } if self.state == TaskState::Running => {
                self.state = TaskState::Interrupted;
            }
            TaskEvent::Complete { .. } => {
                return Err(DomainError::invalid(
                    "completion requires running state and verified postcondition and cleanup",
                ));
            }
            TaskEvent::Fail { cleanup_verified }
                if matches!(
                    self.state,
                    TaskState::Created | TaskState::Queued | TaskState::Running
                ) =>
            {
                self.state = if cleanup_verified {
                    TaskState::Failed
                } else {
                    TaskState::Interrupted
                };
            }
            TaskEvent::RequestCancel => {
                self.cancel_requested = true;
            }
            TaskEvent::SettleCancellation { cleanup_verified } if self.cancel_requested => {
                self.state = if cleanup_verified {
                    TaskState::Cancelled
                } else {
                    TaskState::Interrupted
                };
            }
            TaskEvent::SettleCancellation { .. } => {
                return Err(DomainError::new(
                    ErrorCode::CancelFailed,
                    "cancellation was not requested",
                ));
            }
            TaskEvent::HostLost => {
                self.state = TaskState::Interrupted;
            }
            _ => return Err(DomainError::invalid("invalid task transition")),
        }
        Ok(self.state)
    }
}
