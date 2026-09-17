use crate::{AdmissionFence, DomainError};
use contract::{ErrorCode, RuntimeHost, UuidV4};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeOwner {
    pub runtime_epoch: UuidV4,
    pub host: RuntimeHost,
    pub host_generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeIdentity {
    pub owner: RuntimeOwner,
    pub runtime_instance_id: UuidV4,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct OutstandingWork {
    pub tasks: u32,
    pub automation_executions: u32,
    pub synchronous_executions: u32,
}

impl OutstandingWork {
    pub const fn is_idle(self) -> bool {
        self.tasks == 0 && self.automation_executions == 0 && self.synchronous_executions == 0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostTransition {
    pub transition_id: UuidV4,
    pub runtime_epoch: UuidV4,
    pub from_host: RuntimeHost,
    pub from_generation: u64,
    pub from_instance_id: UuidV4,
    pub target_host: RuntimeHost,
    pub target_generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostState {
    Active(RuntimeIdentity),
    Preparing {
        source: RuntimeIdentity,
        transition: HostTransition,
    },
    Released {
        owner: RuntimeOwner,
        transition: HostTransition,
    },
    Committed {
        owner: RuntimeOwner,
        transition: HostTransition,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostEvent {
    BeginTransition {
        transition_id: UuidV4,
        target_host: RuntimeHost,
        outstanding: OutstandingWork,
    },
    AbortTransition,
    ReleaseSource,
    CommitOwner,
    ActivateTarget {
        runtime_instance_id: UuidV4,
    },
}

impl HostState {
    pub const fn business_admission_open(&self) -> bool {
        matches!(self, Self::Active(_))
    }

    pub fn apply(self, event: HostEvent) -> Result<Self, DomainError> {
        match (self, event) {
            (
                Self::Active(source),
                HostEvent::BeginTransition {
                    transition_id,
                    target_host,
                    outstanding,
                },
            ) => {
                if !outstanding.is_idle() {
                    return Err(DomainError::new(
                        ErrorCode::HostTransitionPending,
                        "runtime host is not idle",
                    ));
                }
                if source.owner.host == target_host {
                    return Err(DomainError::invalid("target host already owns Runtime"));
                }
                let target_generation =
                    source.owner.host_generation.checked_add(1).ok_or_else(|| {
                        DomainError::new(
                            ErrorCode::ResourceLimit,
                            "runtime host generation exhausted",
                        )
                    })?;
                let transition = HostTransition {
                    transition_id,
                    runtime_epoch: source.owner.runtime_epoch.clone(),
                    from_host: source.owner.host,
                    from_generation: source.owner.host_generation,
                    from_instance_id: source.runtime_instance_id.clone(),
                    target_host,
                    target_generation,
                };
                Ok(Self::Preparing { source, transition })
            }
            (Self::Preparing { source, .. }, HostEvent::AbortTransition) => {
                Ok(Self::Active(source))
            }
            (Self::Preparing { source, transition }, HostEvent::ReleaseSource) => {
                Ok(Self::Released {
                    owner: source.owner,
                    transition,
                })
            }
            (Self::Released { owner, transition }, HostEvent::CommitOwner) => {
                let committed = RuntimeOwner {
                    runtime_epoch: owner.runtime_epoch,
                    host: transition.target_host,
                    host_generation: transition.target_generation,
                };
                Ok(Self::Committed {
                    owner: committed,
                    transition,
                })
            }
            (
                Self::Committed { owner, .. },
                HostEvent::ActivateTarget {
                    runtime_instance_id,
                },
            ) => Ok(Self::Active(RuntimeIdentity {
                owner,
                runtime_instance_id,
            })),
            _ => Err(DomainError::invalid("invalid runtime host transition")),
        }
    }

    pub fn validate_fence(&self, candidate: &RuntimeIdentity) -> Result<(), DomainError> {
        match self {
            Self::Active(current) if current == candidate => Ok(()),
            _ => Err(DomainError::new(
                ErrorCode::StaleAuthority,
                "runtime fence does not match the active instance",
            )),
        }
    }

    pub fn validate_admission_fence(&self, candidate: &AdmissionFence) -> Result<(), DomainError> {
        match self {
            Self::Active(current)
                if current.owner.runtime_epoch == candidate.runtime_epoch
                    && current.owner.host_generation == candidate.host_generation
                    && current.runtime_instance_id == candidate.runtime_instance_id =>
            {
                Ok(())
            }
            _ => Err(DomainError::new(
                ErrorCode::StaleAuthority,
                "admission fence does not match the active instance",
            )),
        }
    }
}
