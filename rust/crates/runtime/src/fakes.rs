use crate::{
    AdmittedExecution, ArtifactMetadata, ArtifactPort, CapabilityPort, CapabilitySnapshot,
    ExecutionCompletion, ExecutionFailure, ExecutionPort, HostControlPort, PersistencePort,
    PortFuture, RecoveryProof, RuntimeState,
};
use contract::{ErrorCode, UuidV4};
use domain::DomainError;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
};
use tokio::sync::Semaphore;

#[derive(Clone, Default)]
pub struct FakePersistence {
    state: Arc<Mutex<RuntimeState>>,
}

impl FakePersistence {
    pub fn with_state(state: RuntimeState) -> Self {
        Self {
            state: Arc::new(Mutex::new(state)),
        }
    }

    pub fn snapshot(&self) -> RuntimeState {
        self.state.lock().expect("fake store lock").clone()
    }
}

impl PersistencePort for FakePersistence {
    fn load(&self) -> Result<RuntimeState, DomainError> {
        Ok(self.snapshot())
    }

    fn compare_and_commit(
        &self,
        expected_revision: u64,
        candidate: RuntimeState,
    ) -> Result<(), DomainError> {
        let mut state = self.state.lock().expect("fake store lock");
        if state.revision != expected_revision {
            return Err(DomainError::new(
                ErrorCode::RevisionConflict,
                "fake store revision conflict",
            ));
        }
        *state = candidate;
        Ok(())
    }
}

#[derive(Clone, Default)]
pub struct FakeArtifacts {
    artifacts: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    execution_owners: Arc<Mutex<BTreeMap<String, UuidV4>>>,
    mimes: Arc<Mutex<BTreeMap<String, String>>>,
    image_publish_mime: Option<String>,
}

impl FakeArtifacts {
    pub fn with_image_publish_mime(mut self, mime: impl Into<String>) -> Self {
        self.image_publish_mime = Some(mime.into());
        self
    }

    pub fn execution_owner(&self, artifact_ref: &str) -> Option<UuidV4> {
        self.execution_owners
            .lock()
            .expect("fake artifact owner lock")
            .get(artifact_ref)
            .cloned()
    }
}

impl ArtifactPort for FakeArtifacts {
    fn publish(&self, bytes: &[u8]) -> Result<ArtifactMetadata, DomainError> {
        self.publish_as("data", bytes)
    }

    fn publish_as(&self, kind: &str, bytes: &[u8]) -> Result<ArtifactMetadata, DomainError> {
        let mut artifacts = self.artifacts.lock().expect("fake artifact lock");
        let artifact_ref = format!(
            "dbref:{kind}:00000000-0000-4000-8000-{:012x}",
            artifacts.len() + 1
        );
        let sha256 = hex_sha256(bytes);
        artifacts.insert(artifact_ref.clone(), bytes.to_vec());
        Ok(ArtifactMetadata {
            artifact_ref,
            byte_count: bytes.len() as u64,
            sha256,
            mime: (kind == "image").then(|| "image/png".to_owned()),
        })
    }

    fn publish_for_execution(
        &self,
        execution_id: &UuidV4,
        kind: &str,
        bytes: &[u8],
    ) -> Result<ArtifactMetadata, DomainError> {
        let metadata = self.publish_as(kind, bytes)?;
        self.execution_owners
            .lock()
            .expect("fake artifact owner lock")
            .insert(metadata.artifact_ref.clone(), execution_id.clone());
        Ok(metadata)
    }

    fn publish_image_for_execution(
        &self,
        execution_id: &UuidV4,
        mime: &str,
        bytes: &[u8],
    ) -> Result<ArtifactMetadata, DomainError> {
        let actual_mime = self.image_publish_mime.as_deref().unwrap_or(mime);
        let mut metadata = self.publish_for_execution(execution_id, "image", bytes)?;
        metadata.mime = Some(actual_mime.to_owned());
        self.mimes
            .lock()
            .expect("fake artifact MIME lock")
            .insert(metadata.artifact_ref.clone(), actual_mime.to_owned());
        Ok(metadata)
    }

    fn open(&self, artifact_ref: &str) -> Result<Vec<u8>, DomainError> {
        self.artifacts
            .lock()
            .expect("fake artifact lock")
            .get(artifact_ref)
            .cloned()
            .ok_or_else(|| DomainError::new(ErrorCode::NotFound, "artifact not found"))
    }

    fn metadata(&self, artifact_ref: &str) -> Result<ArtifactMetadata, DomainError> {
        let bytes = self.open(artifact_ref)?;
        Ok(ArtifactMetadata {
            artifact_ref: artifact_ref.to_owned(),
            byte_count: bytes.len() as u64,
            sha256: hex_sha256(&bytes),
            mime: self
                .mimes
                .lock()
                .expect("fake artifact MIME lock")
                .get(artifact_ref)
                .cloned(),
        })
    }

    fn delete(&self, artifact_ref: &str) -> Result<(), DomainError> {
        let removed = self
            .artifacts
            .lock()
            .expect("fake artifact lock")
            .remove(artifact_ref);
        if removed.is_none() {
            return Err(DomainError::new(ErrorCode::NotFound, "artifact not found"));
        }
        self.mimes
            .lock()
            .expect("fake artifact MIME lock")
            .remove(artifact_ref);
        self.execution_owners
            .lock()
            .expect("fake artifact owner lock")
            .remove(artifact_ref);
        Ok(())
    }
}

fn hex_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[derive(Clone)]
pub struct FakeCapabilities {
    snapshot: Arc<Mutex<CapabilitySnapshot>>,
}

impl FakeCapabilities {
    pub fn new(snapshot: CapabilitySnapshot) -> Self {
        Self {
            snapshot: Arc::new(Mutex::new(snapshot)),
        }
    }

    pub fn set(&self, snapshot: CapabilitySnapshot) {
        *self.snapshot.lock().expect("fake capability lock") = snapshot;
    }
}

impl CapabilityPort for FakeCapabilities {
    fn current(&self) -> Result<CapabilitySnapshot, DomainError> {
        Ok(self.snapshot.lock().expect("fake capability lock").clone())
    }
}

#[derive(Clone, Default)]
pub struct FakeExecutions {
    outcomes: Arc<Mutex<VecDeque<Result<ExecutionCompletion, ExecutionFailure>>>>,
    started: Arc<Mutex<Vec<AdmittedExecution>>>,
    cancelled: Arc<Mutex<Vec<UuidV4>>>,
    cancel_cleanup_verified: Arc<Mutex<bool>>,
    cancel_error: Arc<Mutex<Option<DomainError>>>,
    claims: crate::LocalExecutionClaims,
    effects: Arc<Mutex<Vec<UuidV4>>>,
    before_effect: Arc<Mutex<Option<Arc<Semaphore>>>>,
}

impl FakeExecutions {
    pub fn push(&self, outcome: Result<ExecutionCompletion, ExecutionFailure>) {
        self.outcomes
            .lock()
            .expect("fake execution outcome lock")
            .push_back(outcome);
    }

    pub fn started(&self) -> Vec<AdmittedExecution> {
        self.started.lock().expect("fake started lock").clone()
    }

    pub fn set_cancel_cleanup_verified(&self, value: bool) {
        *self
            .cancel_cleanup_verified
            .lock()
            .expect("fake cancel lock") = value;
    }

    pub fn set_cancel_error(&self, error: Option<DomainError>) {
        *self.cancel_error.lock().expect("fake cancel error lock") = error;
    }

    pub fn cancelled(&self) -> Vec<UuidV4> {
        self.cancelled.lock().expect("fake cancelled lock").clone()
    }

    pub fn pause_before_effect(&self) -> Arc<Semaphore> {
        let gate = Arc::new(Semaphore::new(0));
        *self.before_effect.lock().expect("fake effect gate lock") = Some(Arc::clone(&gate));
        gate
    }

    pub fn effects(&self) -> Vec<UuidV4> {
        self.effects.lock().expect("fake effects lock").clone()
    }

    pub fn cancellation_reached_claim(&self, execution_id: &UuidV4) -> bool {
        self.claims.cancel_requested(execution_id)
    }
}

impl ExecutionPort for FakeExecutions {
    fn claim_and_start<'a>(
        &'a self,
        execution: AdmittedExecution,
    ) -> PortFuture<'a, Result<ExecutionCompletion, ExecutionFailure>> {
        let execution_id = execution.execution_id.clone();
        let claim = match self.claims.claim(execution_id.clone()) {
            Ok(claim) => claim,
            Err(error) => {
                return Box::pin(async move {
                    Err(ExecutionFailure {
                        error,
                        cleanup_verified: true,
                    })
                });
            }
        };
        self.started
            .lock()
            .expect("fake started lock")
            .push(execution);
        let before_effect = self
            .before_effect
            .lock()
            .expect("fake effect gate lock")
            .clone();
        Box::pin(async move {
            let result = async {
                if let Some(gate) = before_effect {
                    gate.acquire()
                        .await
                        .expect("fake effect gate stays open")
                        .forget();
                }
                claim
                    .publish(|| {
                        self.effects
                            .lock()
                            .expect("fake effects lock")
                            .push(execution_id);
                        Ok(())
                    })
                    .map_err(|error| ExecutionFailure {
                        error,
                        cleanup_verified: *self
                            .cancel_cleanup_verified
                            .lock()
                            .expect("fake cancel lock"),
                    })?;
                self.outcomes
                    .lock()
                    .expect("fake execution outcome lock")
                    .pop_front()
                    .unwrap_or_else(|| {
                        Err(ExecutionFailure {
                            error: DomainError::new(
                                ErrorCode::InternalError,
                                "fake execution has no outcome",
                            ),
                            cleanup_verified: true,
                        })
                    })
            }
            .await;
            let cleanup_verified = match &result {
                Ok(completion) => completion.cleanup_verified,
                Err(failure) => failure.cleanup_verified,
            };
            self.claims.finish(&claim, cleanup_verified);
            result
        })
    }

    fn cancel<'a>(
        &'a self,
        execution_id: &'a UuidV4,
    ) -> PortFuture<'a, Result<crate::ExecutionCancelOutcome, DomainError>> {
        Box::pin(async move {
            self.cancelled
                .lock()
                .expect("fake cancelled lock")
                .push(execution_id.clone());
            if let Some(error) = self
                .cancel_error
                .lock()
                .expect("fake cancel error lock")
                .clone()
            {
                return Err(error);
            }
            if self.claims.contains(execution_id) {
                return self.claims.cancel(execution_id).await;
            }
            Ok(crate::ExecutionCancelOutcome::Cancelled {
                cleanup_verified: *self
                    .cancel_cleanup_verified
                    .lock()
                    .expect("fake cancel lock"),
            })
        })
    }
}

impl crate::FilesystemPreflightPort for FakeExecutions {
    fn preflight(
        &self,
        _candidate: crate::FilesystemCandidate,
        _call: &contract::FilesystemCall,
    ) -> Result<domain::Preflight, DomainError> {
        Ok(domain::Preflight::Positive)
    }
}

#[derive(Clone)]
pub struct FakeHostControl {
    recovery: Arc<Mutex<RecoveryProof>>,
    capabilities: Option<FakeCapabilities>,
    cleanup_reports: Arc<Mutex<Vec<(domain::AdmissionFence, UuidV4)>>>,
    task_activity: Arc<Mutex<Vec<usize>>>,
}

impl FakeHostControl {
    pub fn new(recovery: RecoveryProof) -> Self {
        Self {
            recovery: Arc::new(Mutex::new(recovery)),
            capabilities: None,
            cleanup_reports: Arc::default(),
            task_activity: Arc::default(),
        }
    }

    pub fn with_capabilities(mut self, capabilities: FakeCapabilities) -> Self {
        self.capabilities = Some(capabilities);
        self
    }

    pub fn cleanup_reports(&self) -> Vec<(domain::AdmissionFence, UuidV4)> {
        self.cleanup_reports
            .lock()
            .expect("fake cleanup reports lock")
            .clone()
    }

    pub fn task_activity(&self) -> Vec<usize> {
        self.task_activity
            .lock()
            .expect("fake Task activity lock")
            .clone()
    }

    fn withdraw_readiness(&self) -> Result<(), DomainError> {
        let capabilities = self.capabilities.as_ref().ok_or_else(|| {
            DomainError::new(ErrorCode::InternalError, "fake host has no readiness owner")
        })?;
        capabilities
            .snapshot
            .lock()
            .expect("fake capability lock")
            .context
            .readiness = contract::RuntimeReadiness::Unavailable;
        Ok(())
    }
}

impl HostControlPort for FakeHostControl {
    fn cleanup_unverified(
        &self,
        fence: &domain::AdmissionFence,
        execution_id: &UuidV4,
    ) -> Result<(), DomainError> {
        self.cleanup_reports
            .lock()
            .expect("fake cleanup reports lock")
            .push((fence.clone(), execution_id.clone()));
        self.withdraw_readiness()
    }

    fn prepare(&self) -> Result<(), DomainError> {
        Ok(())
    }

    fn activate(&self, _fence: &domain::AdmissionFence) -> Result<(), DomainError> {
        Ok(())
    }

    fn recover(&self, _old_instance_id: &UuidV4) -> Result<RecoveryProof, DomainError> {
        let proof = *self.recovery.lock().expect("fake recovery lock");
        if proof == RecoveryProof::CleanupUnverified {
            self.withdraw_readiness()?;
        }
        Ok(proof)
    }

    fn task_activity_changed(&self, active_tasks: usize, _canonical_revision: u64) {
        self.task_activity
            .lock()
            .expect("fake Task activity lock")
            .push(active_tasks);
    }
}
