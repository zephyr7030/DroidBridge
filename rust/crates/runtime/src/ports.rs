use contract::{
    AutomationCompatibleCall, CommandCall, FilesystemCall, GrantFacts, NetworkCall,
    TaskTerminalResult, UuidV4,
};
use domain::{AdmissionFence, CapabilityContext, DomainError, ResolverFacts};
use std::{
    collections::{BTreeMap, btree_map::Entry},
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::Notify;

use crate::RuntimeState;

pub type PortFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait PersistencePort: Send + Sync {
    fn load(&self) -> Result<RuntimeState, DomainError>;
    fn compare_and_commit(
        &self,
        expected_revision: u64,
        candidate: RuntimeState,
    ) -> Result<(), DomainError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactMetadata {
    pub artifact_ref: String,
    pub byte_count: u64,
    pub sha256: String,
    pub mime: Option<String>,
}

pub trait ArtifactPort: Send + Sync {
    /// Publishes bytes as one `data` artifact (S-ART-001).
    fn publish(&self, bytes: &[u8]) -> Result<ArtifactMetadata, DomainError>;
    /// Publishes bytes as one artifact of the named S-ART-001 kind. The kind owns the
    /// S-ART-002 byte limit, so a producer whose bytes can exceed the `data` limit names
    /// its kind instead of publishing under a limit that does not describe it.
    fn publish_as(&self, kind: &str, bytes: &[u8]) -> Result<ArtifactMetadata, DomainError>;
    /// Publishes output owned by an already-admitted execution. The persistence adapter
    /// derives Task/request ownership from this identity rather than trusting caller-supplied
    /// owner fields (S-ART-002).
    fn publish_for_execution(
        &self,
        execution_id: &UuidV4,
        kind: &str,
        bytes: &[u8],
    ) -> Result<ArtifactMetadata, DomainError>;
    fn publish_image_for_execution(
        &self,
        execution_id: &UuidV4,
        mime: &str,
        bytes: &[u8],
    ) -> Result<ArtifactMetadata, DomainError>;
    fn open(&self, artifact_ref: &str) -> Result<Vec<u8>, DomainError>;
    fn metadata(&self, artifact_ref: &str) -> Result<ArtifactMetadata, DomainError>;
    fn delete(&self, artifact_ref: &str) -> Result<(), DomainError>;
}

impl<T: ArtifactPort + ?Sized> ArtifactPort for Arc<T> {
    fn publish(&self, bytes: &[u8]) -> Result<ArtifactMetadata, DomainError> {
        self.as_ref().publish(bytes)
    }

    fn publish_as(&self, kind: &str, bytes: &[u8]) -> Result<ArtifactMetadata, DomainError> {
        self.as_ref().publish_as(kind, bytes)
    }

    fn publish_for_execution(
        &self,
        execution_id: &UuidV4,
        kind: &str,
        bytes: &[u8],
    ) -> Result<ArtifactMetadata, DomainError> {
        self.as_ref()
            .publish_for_execution(execution_id, kind, bytes)
    }

    fn publish_image_for_execution(
        &self,
        execution_id: &UuidV4,
        mime: &str,
        bytes: &[u8],
    ) -> Result<ArtifactMetadata, DomainError> {
        self.as_ref()
            .publish_image_for_execution(execution_id, mime, bytes)
    }

    fn open(&self, artifact_ref: &str) -> Result<Vec<u8>, DomainError> {
        self.as_ref().open(artifact_ref)
    }

    fn metadata(&self, artifact_ref: &str) -> Result<ArtifactMetadata, DomainError> {
        self.as_ref().metadata(artifact_ref)
    }

    fn delete(&self, artifact_ref: &str) -> Result<(), DomainError> {
        self.as_ref().delete(artifact_ref)
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, tag = "type", content = "value")]
pub enum ExecutionPayload {
    OpaqueOperation(String),
    AutomationCall(AutomationCompatibleCall),
    FilesystemCall(FilesystemCall),
    CommandCall(CommandCall),
    NetworkCall(NetworkCall),
    VisualCall(crate::VisualExecutionEnvelope),
    AndroidCall(crate::AndroidExecutionEnvelope),
}

#[derive(Clone, Debug, PartialEq)]
pub struct AdmittedExecution {
    pub execution_id: UuidV4,
    pub task_id: Option<UuidV4>,
    pub executor: crate::ExecutorRecord,
    pub payload: ExecutionPayload,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExecutionOutcome {
    Completed {
        result: TaskTerminalResult,
        encoded_bytes: u64,
    },
    SynchronousCompleted {
        result: serde_json::Value,
        encoded_bytes: u64,
    },
    Failed {
        error: contract::PublicError,
        encoded_bytes: u64,
    },
    Cancelled {
        error: contract::PublicError,
        encoded_bytes: u64,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExecutionCompletion {
    pub fence: AdmissionFence,
    pub capability_generation: u64,
    pub outcome: ExecutionOutcome,
    pub cleanup_verified: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionFailure {
    pub error: DomainError,
    pub cleanup_verified: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionCancelOutcome {
    Cancelled { cleanup_verified: bool },
    CompletionWon,
}

pub trait ExecutionPort: Send + Sync {
    /// Establishes the cancellation-visible execution claim before returning the future.
    /// `cancel` observes that claim, and the future cannot start an effect after clean cancellation.
    fn claim_and_start<'a>(
        &'a self,
        execution: AdmittedExecution,
    ) -> PortFuture<'a, Result<ExecutionCompletion, ExecutionFailure>>;
    fn cancel<'a>(
        &'a self,
        execution_id: &'a UuidV4,
    ) -> PortFuture<'a, Result<ExecutionCancelOutcome, DomainError>>;
}

#[derive(Clone, Default)]
pub struct LocalExecutionClaims {
    entries: Arc<Mutex<BTreeMap<String, Arc<LocalExecutionClaimInner>>>>,
}

struct LocalExecutionClaimInner {
    state: Mutex<LocalExecutionClaimState>,
    cleanup_unverified: AtomicBool,
    finished: Notify,
}

#[derive(Default)]
struct LocalExecutionClaimState {
    cancel_requested: bool,
    publication_committed: bool,
    finished: bool,
    cleanup_verified: bool,
}

#[derive(Clone)]
pub struct LocalExecutionClaim {
    execution_id: UuidV4,
    inner: Arc<LocalExecutionClaimInner>,
}

impl LocalExecutionClaims {
    pub fn claim(&self, execution_id: UuidV4) -> Result<LocalExecutionClaim, DomainError> {
        let inner = Arc::new(LocalExecutionClaimInner {
            state: Mutex::new(LocalExecutionClaimState::default()),
            cleanup_unverified: AtomicBool::new(false),
            finished: Notify::new(),
        });
        let mut entries = self.entries.lock().map_err(|_| {
            DomainError::new(contract::ErrorCode::InternalError, "claim lock failed")
        })?;
        match entries.entry(execution_id.as_str().to_owned()) {
            Entry::Vacant(entry) => {
                entry.insert(Arc::clone(&inner));
            }
            Entry::Occupied(_) => {
                return Err(DomainError::invalid(
                    "execution identity is already claimed",
                ));
            }
        }
        Ok(LocalExecutionClaim {
            execution_id,
            inner,
        })
    }

    pub async fn cancel(
        &self,
        execution_id: &UuidV4,
    ) -> Result<ExecutionCancelOutcome, DomainError> {
        let claim = self
            .entries
            .lock()
            .map_err(|_| DomainError::new(contract::ErrorCode::InternalError, "claim lock failed"))?
            .get(execution_id.as_str())
            .cloned();
        let Some(claim) = claim else {
            return Ok(ExecutionCancelOutcome::CompletionWon);
        };
        let completion_won = {
            let mut state = claim.state.lock().map_err(|_| {
                DomainError::new(
                    contract::ErrorCode::InternalError,
                    "claim state lock failed",
                )
            })?;
            if !state.publication_committed {
                state.cancel_requested = true;
            }
            state.publication_committed
        };
        loop {
            let notified = claim.finished.notified();
            if claim
                .state
                .lock()
                .map_err(|_| {
                    DomainError::new(
                        contract::ErrorCode::InternalError,
                        "claim state lock failed",
                    )
                })?
                .finished
            {
                break;
            }
            notified.await;
        }
        let cleanup_verified = claim
            .state
            .lock()
            .map_err(|_| {
                DomainError::new(
                    contract::ErrorCode::InternalError,
                    "claim state lock failed",
                )
            })?
            .cleanup_verified;
        Ok(if completion_won {
            ExecutionCancelOutcome::CompletionWon
        } else {
            ExecutionCancelOutcome::Cancelled { cleanup_verified }
        })
    }

    /// Records cancellation without awaiting the claim, for a caller that cannot
    /// yield: the process runner already polls the same claim, so this only has to
    /// make the request visible. `false` means there was nothing left to cancel,
    /// which is the same outcome `cancel` reports as `CompletionWon`.
    pub fn request_cancel(&self, execution_id: &UuidV4) -> bool {
        let claim = self
            .entries
            .lock()
            .ok()
            .and_then(|entries| entries.get(execution_id.as_str()).cloned());
        let Some(claim) = claim else {
            return false;
        };
        claim
            .state
            .lock()
            .map(|mut state| {
                if state.publication_committed {
                    false
                } else {
                    state.cancel_requested = true;
                    true
                }
            })
            .unwrap_or(false)
    }

    pub fn finish(&self, claim: &LocalExecutionClaim, cleanup_verified: bool) {
        claim.finish(cleanup_verified);
        if let Ok(mut entries) = self.entries.lock() {
            entries.remove(claim.execution_id.as_str());
        }
    }

    pub fn contains(&self, execution_id: &UuidV4) -> bool {
        self.entries
            .lock()
            .is_ok_and(|entries| entries.contains_key(execution_id.as_str()))
    }

    pub fn cancel_requested(&self, execution_id: &UuidV4) -> bool {
        self.entries
            .lock()
            .ok()
            .and_then(|entries| entries.get(execution_id.as_str()).cloned())
            .and_then(|claim| claim.state.lock().ok().map(|state| state.cancel_requested))
            .unwrap_or(false)
    }
}

impl LocalExecutionClaim {
    pub fn checkpoint(&self) -> Result<(), DomainError> {
        let state = self.inner.state.lock().map_err(|_| {
            DomainError::new(
                contract::ErrorCode::InternalError,
                "claim state lock failed",
            )
        })?;
        if state.cancel_requested {
            Err(DomainError::new(
                contract::ErrorCode::Cancelled,
                "execution was cancelled",
            ))
        } else {
            Ok(())
        }
    }

    pub fn publish<T>(
        &self,
        publication: impl FnOnce() -> Result<T, DomainError>,
    ) -> Result<T, DomainError> {
        let mut state = self.inner.state.lock().map_err(|_| {
            DomainError::new(
                contract::ErrorCode::InternalError,
                "claim state lock failed",
            )
        })?;
        if state.cancel_requested {
            return Err(DomainError::new(
                contract::ErrorCode::Cancelled,
                "execution was cancelled",
            ));
        }
        let result = publication()?;
        state.publication_committed = true;
        Ok(result)
    }

    pub fn finish(&self, cleanup_verified: bool) {
        if let Ok(mut state) = self.inner.state.lock() {
            state.cleanup_verified = cleanup_verified && self.cleanup_is_verified();
            state.finished = true;
        }
        self.inner.finished.notify_waiters();
    }

    pub fn mark_cleanup_unverified(&self) {
        self.inner.cleanup_unverified.store(true, Ordering::Release);
    }

    pub fn cleanup_is_verified(&self) -> bool {
        !self.inner.cleanup_unverified.load(Ordering::Acquire)
    }

    pub fn execution_id(&self) -> &UuidV4 {
        &self.execution_id
    }
}

#[derive(Clone, Debug)]
pub struct CapabilitySnapshot {
    pub grants: GrantFacts,
    pub context: CapabilityContext,
    pub resolver_facts: ResolverFacts,
    pub fence: AdmissionFence,
}

pub trait CapabilityPort: Send + Sync {
    fn current(&self) -> Result<CapabilitySnapshot, DomainError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryProof {
    Clean,
    CleanupUnverified,
}

pub trait HostControlPort: Send + Sync {
    fn cleanup_unverified(
        &self,
        fence: &AdmissionFence,
        execution_id: &UuidV4,
    ) -> Result<(), DomainError>;
    fn prepare(&self) -> Result<(), DomainError>;
    fn activate(&self, fence: &AdmissionFence) -> Result<(), DomainError>;
    fn recover(&self, old_instance_id: &UuidV4) -> Result<RecoveryProof, DomainError>;
}
