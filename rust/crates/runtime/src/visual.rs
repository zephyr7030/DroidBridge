//! Shared Visual semantics and observation/reference ownership (S-VIS-001..003).

use crate::{
    AdmittedExecution, ArtifactPort, CapabilityPort, CapabilitySnapshot, ExecutionCancelOutcome,
    ExecutionCompletion, ExecutionFailure, ExecutionOutcome, ExecutionPayload, ExecutionPort,
    ExecutorRecord, FilesystemPreflightPort, HostControlPort, LocalExecutionClaim,
    LocalExecutionClaims, PersistencePort, PortFuture, RuntimeCore, SynchronousAdmission,
    UI_ENVELOPE_LIMIT_BYTES,
    command::{execution_failure, execution_fence, new_uuid},
    resolve_filesystem_executor,
};
use contract::{
    DisplayGeometry, ErrorCode, FileTarget, FileTargetType, FilesystemCall, FilesystemInspectInput,
    ForegroundFact, ImageFormat, InteractionTarget, PointTarget, Region, RequestId, True, UuidV4,
    VisualCall, VisualInteractFact, VisualInteractInput, VisualInteractResult, VisualNode,
    VisualObserveInput, VisualObserveResult, VisualSource, VisualViewInput, VisualViewResult,
};
use domain::{DomainError, ExecutorRequest, VisualRoute};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

pub const VISUAL_OBSERVATION_LIMIT: usize = 32;
pub const VISUAL_OBSERVATION_TTL_MS: u64 = 300_000;
/// No node refs were wanted; no node refs exist although nodes were wanted.
const NODE_REFS_NOT_REQUESTED: &str = "NODES_NOT_REQUESTED";
const NODE_REFS_UNAVAILABLE: &str = "NODE_REFS_UNAVAILABLE";
pub const VISUAL_MAX_NODES: u32 = 5_000;
pub const VISUAL_MAX_IMAGE_BYTES: usize = 8 * 1_024 * 1_024;
pub const VISUAL_MAX_TEXT_BYTES: usize = 65_536;
pub const VISUAL_MAX_NODE_TEXT_BYTES: usize = 4_096;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualSourceAdmission {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor: Option<ExecutorRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
}

impl VisualSourceAdmission {
    fn resolve(capability: &CapabilitySnapshot, route: VisualRoute) -> Self {
        match crate::resolve_execution(capability, ExecutorRequest::Visual(route)) {
            Ok(executor) => Self {
                executor: Some(ExecutorRecord::from(&executor)),
                unavailable_reason: None,
            },
            Err(error) => Self {
                executor: None,
                unavailable_reason: Some(error_code_token(error.code)),
            },
        }
    }

    fn validate(&self) -> Result<(), DomainError> {
        if self.executor.is_some() == self.unavailable_reason.is_some() {
            return Err(DomainError::new(
                ErrorCode::IoError,
                "visual source admission is invalid",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualExecutionEnvelope {
    pub call: VisualCall,
    pub admitted_at: String,
    pub admitted_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_source: Option<VisualSourceAdmission>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hierarchy_source: Option<VisualSourceAdmission>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_source_executor: Option<ExecutorRecord>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VisualDisplaySnapshot {
    pub display: DisplayGeometry,
    pub display_generation: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum VisualSceneProof {
    Privileged {
        hierarchy_sha256: String,
    },
    Accessibility {
        component_generation: u64,
        window_id: i32,
        scene_revision: u64,
        hierarchy_sha256: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct VisualHierarchySnapshot {
    pub display: VisualDisplaySnapshot,
    pub foreground: Option<ForegroundFact>,
    pub nodes: Vec<VisualNode>,
    pub truncated: bool,
    pub proof: VisualSceneProof,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VisualEncodedImage {
    pub bytes: Vec<u8>,
    pub format: ImageFormat,
    pub width: u32,
    pub height: u32,
    pub captured_display: Option<VisualDisplaySnapshot>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum VisualTransformSource {
    ImmutableArtifact(Vec<u8>),
    Path {
        path: String,
        executor: ExecutorRecord,
    },
    ContentUri(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum VisualInteractionRequest {
    Node {
        observation_id: UuidV4,
        node_ref: String,
        operation: String,
        text: Option<String>,
        display: VisualDisplaySnapshot,
        proof: VisualSceneProof,
    },
    /// A place on the display the caller observed. The observation's own lifetime was settled
    /// before this request existed, so the display identity and the geometry are the whole request.
    Coordinate {
        operation: String,
        from_x: u32,
        from_y: u32,
        to_x: Option<u32>,
        to_y: Option<u32>,
        duration_ms: Option<u64>,
        display: VisualDisplaySnapshot,
    },
    FocusedText {
        text: String,
    },
    Key {
        key_code: i32,
        meta_state: i32,
    },
}

pub trait VisualPrimitivePort: Send + Sync {
    fn display(
        &self,
        execution: &AdmittedExecution,
        claim: &LocalExecutionClaim,
    ) -> Result<VisualDisplaySnapshot, ExecutionFailure>;

    fn capture_image(
        &self,
        execution: &AdmittedExecution,
        display: &VisualDisplaySnapshot,
        claim: &LocalExecutionClaim,
    ) -> Result<VisualEncodedImage, ExecutionFailure>;

    fn observe_hierarchy(
        &self,
        execution: &AdmittedExecution,
        display: &VisualDisplaySnapshot,
        observation_id: &UuidV4,
        max_nodes: u32,
        claim: &LocalExecutionClaim,
    ) -> Result<VisualHierarchySnapshot, ExecutionFailure>;

    fn transform(
        &self,
        execution: &AdmittedExecution,
        source: VisualTransformSource,
        region: Option<Region>,
        claim: &LocalExecutionClaim,
    ) -> Result<VisualEncodedImage, ExecutionFailure>;

    fn interact(
        &self,
        execution: &AdmittedExecution,
        request: VisualInteractionRequest,
        claim: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure>;
}

#[derive(Clone, Copy, Default)]
pub struct UnavailableVisualPrimitivePort;

impl VisualPrimitivePort for UnavailableVisualPrimitivePort {
    fn display(
        &self,
        _execution: &AdmittedExecution,
        _claim: &LocalExecutionClaim,
    ) -> Result<VisualDisplaySnapshot, ExecutionFailure> {
        Err(unavailable_visual())
    }

    fn capture_image(
        &self,
        _execution: &AdmittedExecution,
        _display: &VisualDisplaySnapshot,
        _claim: &LocalExecutionClaim,
    ) -> Result<VisualEncodedImage, ExecutionFailure> {
        Err(unavailable_visual())
    }

    fn observe_hierarchy(
        &self,
        _execution: &AdmittedExecution,
        _display: &VisualDisplaySnapshot,
        _observation_id: &UuidV4,
        _max_nodes: u32,
        _claim: &LocalExecutionClaim,
    ) -> Result<VisualHierarchySnapshot, ExecutionFailure> {
        Err(unavailable_visual())
    }

    fn transform(
        &self,
        _execution: &AdmittedExecution,
        _source: VisualTransformSource,
        _region: Option<Region>,
        _claim: &LocalExecutionClaim,
    ) -> Result<VisualEncodedImage, ExecutionFailure> {
        Err(unavailable_visual())
    }

    fn interact(
        &self,
        _execution: &AdmittedExecution,
        _request: VisualInteractionRequest,
        _claim: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure> {
        Err(unavailable_visual())
    }
}

fn unavailable_visual() -> ExecutionFailure {
    execution_failure(
        ErrorCode::CapabilityUnavailable,
        "visual primitive adapter is unavailable",
        true,
    )
}

#[derive(Clone)]
pub struct NativeVisualExecutionSurface<A, C, P = UnavailableVisualPrimitivePort> {
    artifacts: A,
    capabilities: C,
    primitives: P,
    observations: Arc<Mutex<VisualObservationCache>>,
    claims: LocalExecutionClaims,
}

impl<A, C> NativeVisualExecutionSurface<A, C, UnavailableVisualPrimitivePort> {
    pub fn new(artifacts: A, capabilities: C) -> Self {
        Self {
            artifacts,
            capabilities,
            primitives: UnavailableVisualPrimitivePort,
            observations: Arc::new(Mutex::new(VisualObservationCache::default())),
            claims: LocalExecutionClaims::default(),
        }
    }
}

impl<A, C, P> NativeVisualExecutionSurface<A, C, P> {
    pub fn with_primitives<N>(self, primitives: N) -> NativeVisualExecutionSurface<A, C, N> {
        NativeVisualExecutionSurface {
            artifacts: self.artifacts,
            capabilities: self.capabilities,
            primitives,
            observations: self.observations,
            claims: self.claims,
        }
    }
}

impl<A, C, P> ExecutionPort for NativeVisualExecutionSurface<A, C, P>
where
    A: ArtifactPort + Clone + 'static,
    C: CapabilityPort + Clone + 'static,
    P: VisualPrimitivePort + Clone + 'static,
{
    fn claim_and_start<'a>(
        &'a self,
        execution: AdmittedExecution,
    ) -> PortFuture<'a, Result<ExecutionCompletion, ExecutionFailure>> {
        let claim = match self.claims.claim(execution.execution_id.clone()) {
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
        let artifacts = self.artifacts.clone();
        let capabilities = self.capabilities.clone();
        let primitives = self.primitives.clone();
        let observations = Arc::clone(&self.observations);
        let claims = self.claims.clone();
        Box::pin(async move {
            let result = execute_visual(
                artifacts,
                capabilities,
                primitives,
                observations,
                &execution,
                &claim,
            )
            .await;
            let cleanup_verified = match &result {
                Ok(completion) => completion.cleanup_verified,
                Err(failure) => failure.cleanup_verified,
            };
            claims.finish(&claim, cleanup_verified);
            result
        })
    }

    fn cancel<'a>(
        &'a self,
        execution_id: &'a UuidV4,
    ) -> PortFuture<'a, Result<ExecutionCancelOutcome, DomainError>> {
        Box::pin(self.claims.cancel(execution_id))
    }
}

#[derive(Clone, Debug)]
struct VisualObservationRecord {
    observation_id: UuidV4,
    created_at_ms: u64,
    display: Option<VisualDisplaySnapshot>,
    hierarchy_executor: Option<ExecutorRecord>,
    proof: Option<VisualSceneProof>,
    node_refs: BTreeSet<String>,
    image_ref: Option<String>,
    pins: usize,
}

#[derive(Default)]
struct VisualObservationCache {
    records: BTreeMap<String, VisualObservationRecord>,
    node_owners: BTreeMap<String, String>,
    image_owners: BTreeMap<String, String>,
}

impl VisualObservationCache {
    fn reserve(&mut self, observation_id: UuidV4, now_ms: u64) -> Result<(), DomainError> {
        self.remove_expired(now_ms);
        if self.records.len() >= VISUAL_OBSERVATION_LIMIT {
            let oldest = self
                .records
                .values()
                .filter(|record| record.pins == 0)
                .min_by(|left, right| {
                    (left.created_at_ms, left.observation_id.as_str())
                        .cmp(&(right.created_at_ms, right.observation_id.as_str()))
                })
                .map(|record| record.observation_id.as_str().to_owned())
                .ok_or_else(|| {
                    DomainError::new(
                        ErrorCode::ResourceLimit,
                        "all visual observations are pinned",
                    )
                })?;
            self.remove(&oldest);
        }
        let key = observation_id.as_str().to_owned();
        if self.records.contains_key(&key) {
            return Err(DomainError::new(
                ErrorCode::InternalError,
                "visual observation identity is duplicated",
            ));
        }
        self.records.insert(
            key,
            VisualObservationRecord {
                observation_id,
                created_at_ms: now_ms,
                display: None,
                hierarchy_executor: None,
                proof: None,
                node_refs: BTreeSet::new(),
                image_ref: None,
                pins: 1,
            },
        );
        Ok(())
    }

    fn complete(&mut self, record: VisualObservationRecord) {
        let key = record.observation_id.as_str().to_owned();
        if let Some(previous) = self.records.get(&key) {
            for node_ref in &previous.node_refs {
                self.node_owners.remove(node_ref);
            }
            if let Some(image_ref) = &previous.image_ref {
                self.image_owners.remove(image_ref);
            }
        }
        for node_ref in &record.node_refs {
            self.node_owners.insert(node_ref.clone(), key.clone());
        }
        if let Some(image_ref) = &record.image_ref {
            self.image_owners.insert(image_ref.clone(), key.clone());
        }
        self.records.insert(key, record);
    }

    fn fail_reservation(&mut self, observation_id: &UuidV4) {
        self.remove(observation_id.as_str());
    }

    fn pin_observation(
        &mut self,
        observation_id: &UuidV4,
        now_ms: u64,
    ) -> Result<VisualObservationRecord, DomainError> {
        self.pin_key(observation_id.as_str(), now_ms)
    }

    fn pin_node(
        &mut self,
        node_ref: &str,
        now_ms: u64,
    ) -> Result<VisualObservationRecord, DomainError> {
        let key = self
            .node_owners
            .get(node_ref)
            .cloned()
            .ok_or_else(stale_reference)?;
        self.pin_key(&key, now_ms)
    }

    fn pin_image(&mut self, image_ref: &str, now_ms: u64) -> Option<VisualObservationRecord> {
        let key = self.image_owners.get(image_ref)?.clone();
        self.pin_key(&key, now_ms).ok()
    }

    fn pin_key(&mut self, key: &str, now_ms: u64) -> Result<VisualObservationRecord, DomainError> {
        let expired = self.records.get(key).is_none_or(|record| {
            now_ms.saturating_sub(record.created_at_ms) >= VISUAL_OBSERVATION_TTL_MS
        });
        if expired {
            self.remove(key);
            return Err(stale_reference());
        }
        let record = self.records.get_mut(key).ok_or_else(stale_reference)?;
        record.pins = record.pins.checked_add(1).ok_or_else(|| {
            DomainError::new(ErrorCode::ResourceLimit, "visual observation pin overflow")
        })?;
        Ok(record.clone())
    }

    fn unpin(&mut self, observation_id: &UuidV4) {
        if let Some(record) = self.records.get_mut(observation_id.as_str()) {
            record.pins = record.pins.saturating_sub(1);
        }
    }

    fn remove_expired(&mut self, now_ms: u64) {
        let expired: Vec<_> = self
            .records
            .values()
            .filter(|record| {
                record.pins == 0
                    && now_ms.saturating_sub(record.created_at_ms) >= VISUAL_OBSERVATION_TTL_MS
            })
            .map(|record| record.observation_id.as_str().to_owned())
            .collect();
        for key in expired {
            self.remove(&key);
        }
    }

    fn remove(&mut self, key: &str) {
        if let Some(record) = self.records.remove(key) {
            for node_ref in record.node_refs {
                self.node_owners.remove(&node_ref);
            }
            if let Some(image_ref) = record.image_ref {
                self.image_owners.remove(&image_ref);
            }
        }
    }
}

async fn execute_visual<A, C, P>(
    artifacts: A,
    capabilities: C,
    primitives: P,
    observations: Arc<Mutex<VisualObservationCache>>,
    execution: &AdmittedExecution,
    claim: &LocalExecutionClaim,
) -> Result<ExecutionCompletion, ExecutionFailure>
where
    A: ArtifactPort,
    C: CapabilityPort,
    P: VisualPrimitivePort,
{
    let envelope = match &execution.payload {
        ExecutionPayload::VisualCall(envelope) => envelope.clone(),
        _ => {
            return Err(execution_failure(
                ErrorCode::Unsupported,
                "visual surface received a non-visual request",
                true,
            ));
        }
    };
    validate_visual_input(&envelope.call).map_err(verified_failure)?;
    let current = capabilities.current().map_err(verified_failure)?;
    let route = visual_executor_request(&envelope.call);
    if !executor_is_current(&current, execution, route) {
        return Err(execution_failure(
            ErrorCode::StaleAuthority,
            "visual executor fence or generation is stale",
            true,
        ));
    }
    claim.checkpoint().map_err(verified_failure)?;
    let result = match &envelope.call {
        VisualCall::Observe(input) => execute_observe(
            &artifacts,
            &primitives,
            &observations,
            execution,
            claim,
            &current,
            &envelope,
            input,
        )?,
        VisualCall::View(input) => execute_view(
            &artifacts,
            &primitives,
            &observations,
            execution,
            claim,
            &envelope,
            input,
        )?,
        VisualCall::Interact(input) => execute_interact(
            &primitives,
            &observations,
            execution,
            claim,
            &envelope,
            input,
        )?,
    };
    let encoded_bytes = serde_json::to_vec(&result)
        .map_err(|_| {
            verified_failure(DomainError::new(
                ErrorCode::InternalError,
                "visual result encoding failed",
            ))
        })?
        .len() as u64;
    if encoded_bytes > UI_ENVELOPE_LIMIT_BYTES as u64 {
        return Err(execution_failure(
            ErrorCode::ResourceLimit,
            "visual result exceeds the protocol frame limit",
            true,
        ));
    }
    Ok(ExecutionCompletion {
        fence: execution_fence(execution),
        capability_generation: execution.executor.capability_generation,
        outcome: ExecutionOutcome::SynchronousCompleted {
            result,
            encoded_bytes,
        },
        cleanup_verified: claim.cleanup_is_verified(),
    })
}

#[allow(clippy::too_many_arguments)]
fn execute_observe<A, P>(
    artifacts: &A,
    primitives: &P,
    observations: &Arc<Mutex<VisualObservationCache>>,
    execution: &AdmittedExecution,
    claim: &LocalExecutionClaim,
    current: &CapabilitySnapshot,
    envelope: &VisualExecutionEnvelope,
    input: &VisualObserveInput,
) -> Result<serde_json::Value, ExecutionFailure>
where
    A: ArtifactPort,
    P: VisualPrimitivePort,
{
    let observation_id = execution.execution_id.clone();
    observations
        .lock()
        .map_err(|_| internal_visual_lock())?
        .reserve(observation_id.clone(), envelope.admitted_at_ms)
        .map_err(verified_failure)?;
    let mut published_image_ref = None;
    let result = (|| {
        let display = primitives.display(execution, claim)?;
        validate_display(&display).map_err(verified_failure)?;
        let mut record = VisualObservationRecord {
            observation_id: observation_id.clone(),
            created_at_ms: envelope.admitted_at_ms,
            display: Some(display.clone()),
            hierarchy_executor: None,
            proof: None,
            node_refs: BTreeSet::new(),
            image_ref: None,
            pins: 0,
        };
        let mut result = VisualObserveResult {
            observation_id: observation_id.clone(),
            observed_at: envelope.admitted_at.clone(),
            display: display.display.clone(),
            // Complete once the hierarchy is known: a node-less observation keeps this value, an
            // observation that requested nodes has it replaced below with what its node list resolved to.
            interact: VisualInteractFact {
                coordinate: True,
                node_unavailable_reason: Some(NODE_REFS_NOT_REQUESTED.to_owned()),
                ttl_ms: VISUAL_OBSERVATION_TTL_MS,
            },
            foreground: None,
            image_ref: None,
            image_format: None,
            image_unavailable_reason: None,
            nodes: None,
            nodes_unavailable_reason: None,
            nodes_truncated: None,
        };
        if input.include_image {
            let source = envelope.image_source.as_ref().ok_or_else(|| {
                verified_failure(DomainError::new(
                    ErrorCode::IoError,
                    "visual image source is missing",
                ))
            })?;
            source.validate().map_err(verified_failure)?;
            if let Some(reason) = &source.unavailable_reason {
                result.image_unavailable_reason = Some(reason.clone());
            } else if let Some(source_executor) = &source.executor {
                if !source_is_current(current, source_executor, VisualRoute::Image) {
                    result.image_unavailable_reason = Some("STALE_AUTHORITY".to_owned());
                } else {
                    let source_execution = execution_for_source(execution, source_executor);
                    match primitives.capture_image(&source_execution, &display, claim) {
                        Ok(image) => {
                            if let Err(error) = validate_captured_image(&image, &display) {
                                result.image_unavailable_reason =
                                    Some(error_code_token(error.code));
                            } else {
                                let mime = image_mime(image.format);
                                match claim.publish(|| {
                                    artifacts.publish_image_for_execution(
                                        &execution.execution_id,
                                        mime,
                                        &image.bytes,
                                    )
                                }) {
                                    Ok(metadata)
                                        if metadata
                                            .mime
                                            .as_deref()
                                            .is_none_or(|actual| actual == mime) =>
                                    {
                                        record.image_ref = Some(metadata.artifact_ref.clone());
                                        published_image_ref = Some(metadata.artifact_ref.clone());
                                        result.image_ref = Some(metadata.artifact_ref);
                                        result.image_format = Some(image.format);
                                    }
                                    Ok(metadata) => {
                                        let _ = artifacts.delete(&metadata.artifact_ref);
                                        result.image_unavailable_reason =
                                            Some("IO_ERROR".to_owned());
                                    }
                                    Err(error) if error.code == ErrorCode::Cancelled => {
                                        return Err(verified_failure(error));
                                    }
                                    Err(error) => {
                                        result.image_unavailable_reason =
                                            Some(error_code_token(error.code));
                                    }
                                }
                            }
                        }
                        Err(failure) if failure.cleanup_verified => {
                            result.image_unavailable_reason =
                                Some(error_code_token(failure.error.code));
                        }
                        Err(failure) => return Err(failure),
                    }
                }
            }
        }
        claim.checkpoint().map_err(verified_failure)?;
        if input.include_nodes {
            let source = envelope.hierarchy_source.as_ref().ok_or_else(|| {
                verified_failure(DomainError::new(
                    ErrorCode::IoError,
                    "visual hierarchy source is missing",
                ))
            })?;
            source.validate().map_err(verified_failure)?;
            if let Some(reason) = &source.unavailable_reason {
                result.nodes_unavailable_reason = Some(reason.clone());
            } else if let Some(source_executor) = &source.executor {
                if !source_is_current(current, source_executor, VisualRoute::Hierarchy) {
                    result.nodes_unavailable_reason = Some("STALE_AUTHORITY".to_owned());
                } else {
                    let source_execution = execution_for_source(execution, source_executor);
                    match primitives.observe_hierarchy(
                        &source_execution,
                        &display,
                        &observation_id,
                        input.max_nodes,
                        claim,
                    ) {
                        Ok(hierarchy) => {
                            if let Err(error) =
                                validate_hierarchy(&hierarchy, &display, source_executor.provider)
                            {
                                result.nodes_unavailable_reason =
                                    Some(error_code_token(error.code));
                            } else {
                                record.hierarchy_executor = Some(source_executor.clone());
                                record.proof = Some(hierarchy.proof.clone());
                                record.node_refs = hierarchy
                                    .nodes
                                    .iter()
                                    .filter_map(|node| node.node_ref.clone())
                                    .collect();
                                result.foreground = hierarchy.foreground;
                                result.nodes = Some(hierarchy.nodes);
                                result.nodes_truncated = Some(hierarchy.truncated);
                            }
                        }
                        Err(failure) if failure.cleanup_verified => {
                            result.nodes_unavailable_reason =
                                Some(error_code_token(failure.error.code));
                        }
                        Err(failure) => return Err(failure),
                    }
                }
            }
        }
        result.interact = interact_fact(input, &record, &result);
        observations
            .lock()
            .map_err(|_| internal_visual_lock())?
            .complete(record);
        serde_json::to_value(result).map_err(|_| {
            verified_failure(DomainError::new(
                ErrorCode::InternalError,
                "visual observe encoding failed",
            ))
        })
    })();
    if result.is_err() {
        let artifact_cleanup = published_image_ref
            .as_deref()
            .map_or(Ok(()), |artifact_ref| artifacts.delete(artifact_ref));
        let reservation_cleanup = observations
            .lock()
            .map(|mut cache| cache.fail_reservation(&observation_id))
            .map_err(|_| internal_visual_lock());
        if let Err(error) = artifact_cleanup {
            return Err(ExecutionFailure {
                error,
                cleanup_verified: false,
            });
        }
        if reservation_cleanup.is_err() {
            return Err(ExecutionFailure {
                error: DomainError::new(
                    ErrorCode::InternalError,
                    "visual observation reservation cleanup failed",
                ),
                cleanup_verified: false,
            });
        }
    }
    result
}

fn execute_view<A, P>(
    artifacts: &A,
    primitives: &P,
    observations: &Arc<Mutex<VisualObservationCache>>,
    execution: &AdmittedExecution,
    claim: &LocalExecutionClaim,
    envelope: &VisualExecutionEnvelope,
    input: &VisualViewInput,
) -> Result<serde_json::Value, ExecutionFailure>
where
    A: ArtifactPort,
    P: VisualPrimitivePort,
{
    let mut pinned = None;
    let source = match &input.source {
        VisualSource::ImageRef { image_ref } => {
            pinned = observations
                .lock()
                .map_err(|_| internal_visual_lock())?
                .pin_image(image_ref, envelope.admitted_at_ms);
            artifacts
                .metadata(image_ref)
                .map_err(verified_failure)
                .and_then(|metadata| {
                    if metadata
                        .mime
                        .as_deref()
                        .is_none_or(|mime| !matches!(mime, "image/heic" | "image/png"))
                    {
                        return Err(verified_failure(DomainError::invalid(
                            "visual image_ref is not an image artifact",
                        )));
                    }
                    artifacts
                        .open(image_ref)
                        .map(VisualTransformSource::ImmutableArtifact)
                        .map_err(verified_failure)
                })
        }
        VisualSource::Path { path } => envelope
            .path_source_executor
            .clone()
            .map(|executor| VisualTransformSource::Path {
                path: path.clone(),
                executor,
            })
            .ok_or_else(|| {
                verified_failure(DomainError::new(
                    ErrorCode::IoError,
                    "visual path source admission is missing",
                ))
            }),
        VisualSource::ContentUri { content_uri } => {
            Ok(VisualTransformSource::ContentUri(content_uri.clone()))
        }
    };
    let source = match source {
        Ok(source) => source,
        Err(error) => {
            if let Some(record) = pinned {
                unpin_record(observations, &record)?;
            }
            return Err(error);
        }
    };
    let transformed = primitives.transform(execution, source, input.region.clone(), claim);
    if let Some(record) = pinned {
        observations
            .lock()
            .map_err(|_| internal_visual_lock())?
            .unpin(&record.observation_id);
    }
    let transformed = transformed?;
    validate_transformed_image(&transformed).map_err(verified_failure)?;
    let mime = image_mime(transformed.format);
    let metadata = claim
        .publish(|| {
            artifacts.publish_image_for_execution(&execution.execution_id, mime, &transformed.bytes)
        })
        .map_err(verified_failure)?;
    if metadata.mime.as_deref() != Some(mime) {
        return match artifacts.delete(&metadata.artifact_ref) {
            Ok(()) => Err(verified_failure(DomainError::new(
                ErrorCode::IoError,
                "visual image artifact MIME is invalid",
            ))),
            Err(error) => Err(ExecutionFailure {
                error,
                cleanup_verified: false,
            }),
        };
    }
    serde_json::to_value(VisualViewResult {
        image_ref: metadata.artifact_ref,
        width: transformed.width,
        height: transformed.height,
        format: transformed.format,
    })
    .map_err(|_| {
        verified_failure(DomainError::new(
            ErrorCode::InternalError,
            "visual view encoding failed",
        ))
    })
}

fn execute_interact<P>(
    primitives: &P,
    observations: &Arc<Mutex<VisualObservationCache>>,
    execution: &AdmittedExecution,
    claim: &LocalExecutionClaim,
    envelope: &VisualExecutionEnvelope,
    input: &VisualInteractInput,
) -> Result<serde_json::Value, ExecutionFailure>
where
    P: VisualPrimitivePort,
{
    let (request, target, pinned) = match input {
        VisualInteractInput::Tap { target } => interaction_target(
            observations,
            execution,
            envelope.admitted_at_ms,
            target,
            "tap",
        )?,
        VisualInteractInput::LongPress { target } => interaction_target(
            observations,
            execution,
            envelope.admitted_at_ms,
            target,
            "long_press",
        )?,
        VisualInteractInput::Swipe {
            observation_id,
            from_x,
            from_y,
            to_x,
            to_y,
            duration_ms,
        } => {
            let record = pin_observation(observations, observation_id, envelope.admitted_at_ms)?;
            let built = (|| {
                let display = record
                    .display
                    .clone()
                    .ok_or_else(|| verified_failure(stale_reference()))?;
                validate_point(&display.display, *from_x, *from_y).map_err(verified_failure)?;
                validate_point(&display.display, *to_x, *to_y).map_err(verified_failure)?;
                Ok::<_, ExecutionFailure>(VisualInteractionRequest::Coordinate {
                    operation: "swipe".to_owned(),
                    from_x: *from_x,
                    from_y: *from_y,
                    to_x: Some(*to_x),
                    to_y: Some(*to_y),
                    duration_ms: Some(*duration_ms),
                    display,
                })
            })();
            let request = match built {
                Ok(request) => request,
                Err(error) => {
                    unpin_record(observations, &record)?;
                    return Err(error);
                }
            };
            (request, InteractionTarget::Coordinate, Some(record))
        }
        VisualInteractInput::Text {
            text,
            node_ref: Some(node_ref),
        } => {
            let record = observations
                .lock()
                .map_err(|_| internal_visual_lock())?
                .pin_node(node_ref, envelope.admitted_at_ms)
                .map_err(verified_failure)?;
            let built = (|| {
                require_observation_executor(&record, execution)?;
                let display = record
                    .display
                    .clone()
                    .ok_or_else(|| verified_failure(stale_reference()))?;
                let proof = record
                    .proof
                    .clone()
                    .ok_or_else(|| verified_failure(stale_reference()))?;
                Ok::<_, ExecutionFailure>(VisualInteractionRequest::Node {
                    observation_id: record.observation_id.clone(),
                    node_ref: node_ref.clone(),
                    operation: "text".to_owned(),
                    text: Some(text.clone()),
                    display,
                    proof,
                })
            })();
            let request = match built {
                Ok(request) => request,
                Err(error) => {
                    unpin_record(observations, &record)?;
                    return Err(error);
                }
            };
            (request, InteractionTarget::Node, Some(record))
        }
        VisualInteractInput::Text {
            text,
            node_ref: None,
        } => (
            VisualInteractionRequest::FocusedText { text: text.clone() },
            InteractionTarget::Focused,
            None,
        ),
        VisualInteractInput::Key {
            key_code,
            meta_state,
        } => (
            VisualInteractionRequest::Key {
                key_code: *key_code,
                meta_state: *meta_state,
            },
            InteractionTarget::Focused,
            None,
        ),
    };
    let delivered = primitives.interact(execution, request, claim);
    if let Some(record) = pinned {
        unpin_record(observations, &record)?;
    }
    delivered?;
    serde_json::to_value(VisualInteractResult {
        delivered: True,
        operation: visual_interaction_operation(input).to_owned(),
        target,
        execution_class: Some(execution.executor.execution_class),
    })
    .map_err(|_| {
        verified_failure(DomainError::new(
            ErrorCode::InternalError,
            "visual interaction encoding failed",
        ))
    })
}

fn interaction_target(
    observations: &Arc<Mutex<VisualObservationCache>>,
    execution: &AdmittedExecution,
    now_ms: u64,
    target: &PointTarget,
    operation: &str,
) -> Result<
    (
        VisualInteractionRequest,
        InteractionTarget,
        Option<VisualObservationRecord>,
    ),
    ExecutionFailure,
> {
    match target {
        PointTarget::Node { node_ref } => {
            let record = observations
                .lock()
                .map_err(|_| internal_visual_lock())?
                .pin_node(node_ref, now_ms)
                .map_err(verified_failure)?;
            let built = (|| {
                require_observation_executor(&record, execution)?;
                let display = record
                    .display
                    .clone()
                    .ok_or_else(|| verified_failure(stale_reference()))?;
                let proof = record
                    .proof
                    .clone()
                    .ok_or_else(|| verified_failure(stale_reference()))?;
                Ok::<_, ExecutionFailure>(VisualInteractionRequest::Node {
                    observation_id: record.observation_id.clone(),
                    node_ref: node_ref.clone(),
                    operation: operation.to_owned(),
                    text: None,
                    display,
                    proof,
                })
            })();
            match built {
                Ok(request) => Ok((request, InteractionTarget::Node, Some(record))),
                Err(error) => {
                    unpin_record(observations, &record)?;
                    Err(error)
                }
            }
        }
        PointTarget::Coordinate {
            observation_id,
            x,
            y,
        } => {
            let record = pin_observation(observations, observation_id, now_ms)?;
            // A coordinate is addressed against the display the caller saw, not against the scene: the
            // observation's own lifetime and the display identity are its whole freshness contract.
            let built = (|| {
                let display = record
                    .display
                    .clone()
                    .ok_or_else(|| verified_failure(stale_reference()))?;
                validate_point(&display.display, *x, *y).map_err(verified_failure)?;
                Ok::<_, ExecutionFailure>(VisualInteractionRequest::Coordinate {
                    operation: operation.to_owned(),
                    from_x: *x,
                    from_y: *y,
                    to_x: None,
                    to_y: None,
                    duration_ms: None,
                    display,
                })
            })();
            match built {
                Ok(request) => Ok((request, InteractionTarget::Coordinate, Some(record))),
                Err(error) => {
                    unpin_record(observations, &record)?;
                    Err(error)
                }
            }
        }
    }
}

fn pin_observation(
    observations: &Arc<Mutex<VisualObservationCache>>,
    observation_id: &UuidV4,
    now_ms: u64,
) -> Result<VisualObservationRecord, ExecutionFailure> {
    observations
        .lock()
        .map_err(|_| internal_visual_lock())?
        .pin_observation(observation_id, now_ms)
        .map_err(verified_failure)
}

fn unpin_record(
    observations: &Arc<Mutex<VisualObservationCache>>,
    record: &VisualObservationRecord,
) -> Result<(), ExecutionFailure> {
    observations
        .lock()
        .map_err(|_| internal_visual_lock())?
        .unpin(&record.observation_id);
    Ok(())
}

/// What the caller can address in an observation, stated at the observation itself so a caller does not
/// discover the precondition through a rejected interaction.
fn interact_fact(
    input: &VisualObserveInput,
    record: &VisualObservationRecord,
    result: &VisualObserveResult,
) -> VisualInteractFact {
    let node_unavailable_reason = if !record.node_refs.is_empty() {
        None
    } else if let Some(reason) = &result.nodes_unavailable_reason {
        Some(reason.clone())
    } else if input.include_nodes {
        Some(NODE_REFS_UNAVAILABLE.to_owned())
    } else {
        Some(NODE_REFS_NOT_REQUESTED.to_owned())
    };
    VisualInteractFact {
        coordinate: True,
        node_unavailable_reason,
        ttl_ms: VISUAL_OBSERVATION_TTL_MS,
    }
}

fn require_observation_executor(
    record: &VisualObservationRecord,
    execution: &AdmittedExecution,
) -> Result<(), ExecutionFailure> {
    if record.hierarchy_executor.as_ref() != Some(&execution.executor) {
        return Err(verified_failure(stale_reference()));
    }
    Ok(())
}

pub async fn handle_visual_public<P, A, E, C, H>(
    core: &RuntimeCore<P, A, E, C, H>,
    request_id: RequestId,
    payload_sha256: String,
    call: VisualCall,
    timestamp: String,
    now_ms: u64,
) -> Result<serde_json::Value, DomainError>
where
    P: PersistencePort + 'static,
    A: ArtifactPort + Clone + 'static,
    E: ExecutionPort + FilesystemPreflightPort + 'static,
    C: CapabilityPort + 'static,
    H: HostControlPort + 'static,
{
    validate_visual_input(&call)?;
    let capability = core.capability_snapshot()?;
    let (image_source, hierarchy_source) = match &call {
        VisualCall::Observe(input) => (
            input
                .include_image
                .then(|| VisualSourceAdmission::resolve(&capability, VisualRoute::Image)),
            input
                .include_nodes
                .then(|| VisualSourceAdmission::resolve(&capability, VisualRoute::Hierarchy)),
        ),
        _ => (None, None),
    };
    let path_source_executor = match &call {
        VisualCall::View(VisualViewInput {
            source: VisualSource::Path { path },
            ..
        }) => {
            let source_call = FilesystemCall::Inspect(FilesystemInspectInput {
                target: FileTarget {
                    target_type: FileTargetType::Path,
                    value: path.clone(),
                },
                recursive: false,
                max_depth: 1,
                max_entries: 200,
            });
            Some(
                resolve_filesystem_executor(&capability, core.execution_port(), &source_call)?
                    .map(|executor| ExecutorRecord::from(&executor))
                    .ok_or_else(|| {
                        DomainError::new(
                            ErrorCode::Unsupported,
                            "visual path source has no filesystem executor",
                        )
                    })?,
            )
        }
        _ => None,
    };
    let execution_id = new_uuid()?;
    let route = visual_executor_request(&call);
    core.run_synchronous(
        SynchronousAdmission {
            request_id,
            payload_sha256,
            execution_id,
            operation: format!("visual.{}", visual_action(&call)),
            route: ExecutorRequest::Visual(route),
            payload: ExecutionPayload::VisualCall(VisualExecutionEnvelope {
                call,
                admitted_at: timestamp.clone(),
                admitted_at_ms: now_ms,
                image_source,
                hierarchy_source,
                path_source_executor,
            }),
            settlement_bound_bytes: UI_ENVELOPE_LIMIT_BYTES as u64,
            now_ms,
        },
        timestamp,
        now_ms,
    )
    .await
    .map_err(|error| DomainError::new(error.code, "visual execution failed"))
}

fn visual_executor_request(call: &VisualCall) -> VisualRoute {
    match call {
        VisualCall::Observe(_) => VisualRoute::Display,
        VisualCall::View(_) => VisualRoute::Transform,
        VisualCall::Interact(
            VisualInteractInput::Tap {
                target: PointTarget::Node { .. },
            }
            | VisualInteractInput::LongPress {
                target: PointTarget::Node { .. },
            }
            | VisualInteractInput::Text {
                node_ref: Some(_), ..
            },
        ) => VisualRoute::AccessibilityNode,
        VisualCall::Interact(
            VisualInteractInput::Tap {
                target: PointTarget::Coordinate { .. },
            }
            | VisualInteractInput::LongPress {
                target: PointTarget::Coordinate { .. },
            }
            | VisualInteractInput::Swipe { .. },
        ) => VisualRoute::CoordinateInput,
        VisualCall::Interact(VisualInteractInput::Text { node_ref: None, .. }) => {
            VisualRoute::FocusedText
        }
        VisualCall::Interact(VisualInteractInput::Key { .. }) => VisualRoute::KeyInput,
    }
}

fn visual_action(call: &VisualCall) -> &'static str {
    match call {
        VisualCall::Observe(_) => "observe",
        VisualCall::View(_) => "view",
        VisualCall::Interact(_) => "interact",
    }
}

fn visual_interaction_operation(input: &VisualInteractInput) -> &'static str {
    match input {
        VisualInteractInput::Tap { .. } => "tap",
        VisualInteractInput::LongPress { .. } => "long_press",
        VisualInteractInput::Swipe { .. } => "swipe",
        VisualInteractInput::Text { .. } => "text",
        VisualInteractInput::Key { .. } => "key",
    }
}

fn executor_is_current(
    current: &CapabilitySnapshot,
    execution: &AdmittedExecution,
    route: VisualRoute,
) -> bool {
    crate::resolve_execution(current, ExecutorRequest::Visual(route))
        .map(|executor| ExecutorRecord::from(&executor) == execution.executor)
        .unwrap_or(false)
}

fn source_is_current(
    current: &CapabilitySnapshot,
    executor: &ExecutorRecord,
    route: VisualRoute,
) -> bool {
    crate::resolve_execution(current, ExecutorRequest::Visual(route))
        .map(|value| ExecutorRecord::from(&value) == *executor)
        .unwrap_or(false)
}

fn execution_for_source(
    execution: &AdmittedExecution,
    executor: &ExecutorRecord,
) -> AdmittedExecution {
    AdmittedExecution {
        execution_id: execution.execution_id.clone(),
        task_id: execution.task_id.clone(),
        executor: executor.clone(),
        payload: execution.payload.clone(),
    }
}

fn validate_visual_input(call: &VisualCall) -> Result<(), DomainError> {
    match call {
        VisualCall::Observe(input)
            if input.include_nodes && !(1..=VISUAL_MAX_NODES).contains(&input.max_nodes) =>
        {
            Err(DomainError::invalid("visual max_nodes is out of bounds"))
        }
        VisualCall::View(VisualViewInput {
            region: Some(region),
            ..
        }) if region.width == 0 || region.height == 0 => Err(DomainError::invalid(
            "visual region dimensions must be positive",
        )),
        VisualCall::Interact(VisualInteractInput::Swipe { duration_ms, .. })
            if !(1..=10_000).contains(duration_ms) =>
        {
            Err(DomainError::invalid(
                "visual swipe duration is out of bounds",
            ))
        }
        VisualCall::Interact(VisualInteractInput::Text { text, .. })
            if text.len() > VISUAL_MAX_TEXT_BYTES =>
        {
            Err(DomainError::invalid("visual text is out of bounds"))
        }
        _ => Ok(()),
    }
}

fn validate_display(snapshot: &VisualDisplaySnapshot) -> Result<(), DomainError> {
    if snapshot.display_generation == 0
        || snapshot.display.width == 0
        || snapshot.display.height == 0
        || snapshot.display.width > 16_384
        || snapshot.display.height > 16_384
        || !matches!(snapshot.display.rotation, 0 | 90 | 180 | 270)
        || snapshot.display.density_dpi == Some(0)
    {
        return Err(DomainError::new(
            ErrorCode::IoError,
            "visual display snapshot is invalid",
        ));
    }
    Ok(())
}

fn validate_hierarchy(
    hierarchy: &VisualHierarchySnapshot,
    admitted: &VisualDisplaySnapshot,
    provider: crate::ProviderToken,
) -> Result<(), DomainError> {
    validate_display(&hierarchy.display)?;
    if hierarchy.display != *admitted {
        return Err(DomainError::new(
            ErrorCode::StaleAuthority,
            "visual hierarchy display changed",
        ));
    }
    if hierarchy.nodes.len() > VISUAL_MAX_NODES as usize {
        return Err(DomainError::new(
            ErrorCode::ResourceLimit,
            "visual hierarchy exceeds its bound",
        ));
    }
    if let Some(foreground) = &hierarchy.foreground {
        for value in [&foreground.package, &foreground.activity]
            .into_iter()
            .flatten()
        {
            if value.len() > 512 {
                return Err(DomainError::new(
                    ErrorCode::IoError,
                    "visual foreground fact is invalid",
                ));
            }
        }
    }
    let mut refs = BTreeSet::new();
    for node in &hierarchy.nodes {
        validate_node(node)?;
        if let Some(node_ref) = &node.node_ref
            && (provider != crate::ProviderToken::Accessibility
                || node_ref.is_empty()
                || node_ref.len() > 4_096
                || !refs.insert(node_ref))
        {
            return Err(DomainError::new(
                ErrorCode::IoError,
                "visual node reference is invalid",
            ));
        }
    }
    match (&hierarchy.proof, provider) {
        (
            VisualSceneProof::Accessibility {
                component_generation,
                scene_revision,
                hierarchy_sha256,
                ..
            },
            crate::ProviderToken::Accessibility,
        ) if *component_generation > 0 && *scene_revision > 0 && valid_sha256(hierarchy_sha256) => {
        }
        (
            VisualSceneProof::Privileged { hierarchy_sha256 },
            crate::ProviderToken::Shizuku | crate::ProviderToken::MagiskNative,
        ) if valid_sha256(hierarchy_sha256) && refs.is_empty() => {}
        _ => {
            return Err(DomainError::new(
                ErrorCode::IoError,
                "visual hierarchy proof is invalid",
            ));
        }
    }
    Ok(())
}

fn validate_node(node: &VisualNode) -> Result<(), DomainError> {
    for value in [
        &node.text,
        &node.content_description,
        &node.resource_id,
        &node.class_name,
        &node.package_name,
    ]
    .into_iter()
    .flatten()
    {
        if value.len() > VISUAL_MAX_NODE_TEXT_BYTES {
            return Err(DomainError::new(
                ErrorCode::IoError,
                "visual node text is invalid",
            ));
        }
    }
    Ok(())
}

fn validate_captured_image(
    image: &VisualEncodedImage,
    admitted: &VisualDisplaySnapshot,
) -> Result<(), DomainError> {
    if image.captured_display.as_ref() != Some(admitted)
        || image.width != admitted.display.width
        || image.height != admitted.display.height
    {
        return Err(DomainError::new(
            ErrorCode::StaleAuthority,
            "visual image display changed",
        ));
    }
    validate_encoded_image(image)
}

fn validate_transformed_image(image: &VisualEncodedImage) -> Result<(), DomainError> {
    if image.captured_display.is_some()
        || image.width == 0
        || image.height == 0
        || image.width > 16_384
        || image.height > 16_384
    {
        return Err(DomainError::new(
            ErrorCode::IoError,
            "visual transform result is invalid",
        ));
    }
    validate_encoded_image(image)
}

fn validate_encoded_image(image: &VisualEncodedImage) -> Result<(), DomainError> {
    if image.bytes.is_empty() || image.bytes.len() > VISUAL_MAX_IMAGE_BYTES {
        return Err(DomainError::new(
            ErrorCode::ResourceLimit,
            "visual image exceeds its bound",
        ));
    }
    let valid = match image.format {
        ImageFormat::Png => image.bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        ImageFormat::Heic => image.bytes.len() >= 12 && &image.bytes[4..8] == b"ftyp",
    };
    if !valid {
        return Err(DomainError::new(
            ErrorCode::IoError,
            "visual image bytes do not match their format",
        ));
    }
    Ok(())
}

fn validate_point(display: &DisplayGeometry, x: u32, y: u32) -> Result<(), DomainError> {
    if x >= display.width || y >= display.height {
        return Err(DomainError::invalid(
            "visual coordinate lies outside the observation",
        ));
    }
    Ok(())
}

const fn image_mime(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Heic => "image/heic",
        ImageFormat::Png => "image/png",
    }
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn error_code_token(code: ErrorCode) -> String {
    serde_json::to_value(code)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "INTERNAL_ERROR".to_owned())
}

fn stale_reference() -> DomainError {
    DomainError::new(ErrorCode::StaleReference, "visual reference is stale")
}

fn verified_failure(error: DomainError) -> ExecutionFailure {
    ExecutionFailure {
        error,
        cleanup_verified: true,
    }
}

fn internal_visual_lock() -> ExecutionFailure {
    verified_failure(DomainError::new(
        ErrorCode::InternalError,
        "visual observation lock failed",
    ))
}

/// Parses the bounded `uiautomator dump` vocabulary without turning XML attributes into
/// actionable identity. Unknown attributes are ignored; malformed XML/attributes fail the
/// entire hierarchy part so a partial tree is never published as complete truth.
pub fn parse_privileged_hierarchy(
    xml: &[u8],
    max_nodes: u32,
    display: VisualDisplaySnapshot,
) -> Result<VisualHierarchySnapshot, DomainError> {
    if xml.is_empty() || xml.len() > 8 * 1_024 * 1_024 {
        return Err(DomainError::new(
            ErrorCode::ResourceLimit,
            "privileged hierarchy exceeds its bound",
        ));
    }
    if !(1..=VISUAL_MAX_NODES).contains(&max_nodes) {
        return Err(DomainError::invalid("visual max_nodes is out of bounds"));
    }
    let text = std::str::from_utf8(xml)
        .map_err(|_| DomainError::new(ErrorCode::IoError, "privileged hierarchy is not UTF-8"))?;
    if text.contains("<!DOCTYPE")
        || text.contains("<!ENTITY")
        || !text.contains("<hierarchy")
        || !text.contains("</hierarchy>")
    {
        return Err(DomainError::new(
            ErrorCode::IoError,
            "privileged hierarchy document is invalid",
        ));
    }
    validate_privileged_xml_structure(text)?;
    let mut cursor = 0;
    let mut nodes = Vec::new();
    let mut node_count = 0usize;
    while let Some(relative) = text[cursor..].find("<node") {
        let start = cursor + relative;
        let boundary = text.as_bytes().get(start + 5).copied();
        if !boundary.is_some_and(|byte| byte.is_ascii_whitespace() || byte == b'/' || byte == b'>')
        {
            cursor = start + 5;
            continue;
        }
        let end = xml_tag_end(text, start + 5)?;
        let attributes = parse_xml_attributes(&text[start + 5..end])?;
        let node = privileged_node(&attributes)?;
        node_count = node_count.checked_add(1).ok_or_else(|| {
            DomainError::new(
                ErrorCode::ResourceLimit,
                "privileged hierarchy node overflow",
            )
        })?;
        if nodes.len() < max_nodes as usize {
            nodes.push(node);
        }
        cursor = end + 1;
    }
    if node_count == 0 || text.matches("</node>").count() > node_count {
        return Err(DomainError::new(
            ErrorCode::IoError,
            "privileged hierarchy has invalid node structure",
        ));
    }
    let foreground = nodes
        .iter()
        .find_map(|node| node.package_name.clone())
        .map(|package| ForegroundFact {
            package: Some(package),
            activity: None,
        });
    Ok(VisualHierarchySnapshot {
        display,
        foreground,
        nodes,
        truncated: node_count > max_nodes as usize,
        proof: VisualSceneProof::Privileged {
            hierarchy_sha256: Sha256::digest(xml)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        },
    })
}

fn validate_privileged_xml_structure(text: &str) -> Result<(), DomainError> {
    let mut cursor = 0usize;
    let mut stack: Vec<&str> = Vec::new();
    let mut root_seen = false;
    let mut declaration_seen = false;
    while cursor < text.len() {
        let Some(relative) = text[cursor..].find('<') else {
            if !text[cursor..].trim().is_empty() {
                return Err(invalid_xml_structure());
            }
            break;
        };
        let start = cursor + relative;
        if !text[cursor..start].trim().is_empty() {
            return Err(invalid_xml_structure());
        }
        if text[start..].starts_with("<?xml") {
            if declaration_seen || root_seen || !stack.is_empty() {
                return Err(invalid_xml_structure());
            }
            let end = text[start + 5..]
                .find("?>")
                .map(|relative| start + 5 + relative + 2)
                .ok_or_else(invalid_xml_structure)?;
            declaration_seen = true;
            cursor = end;
            continue;
        }
        if text[start..].starts_with("<!") || text[start..].starts_with("<?") {
            return Err(invalid_xml_structure());
        }
        let end = xml_tag_end(text, start + 1)?;
        let raw = text[start + 1..end].trim();
        if let Some(closing) = raw.strip_prefix('/') {
            let name = closing.trim();
            if name.contains(char::is_whitespace) || stack.pop() != Some(name) {
                return Err(invalid_xml_structure());
            }
        } else {
            let self_closing = raw.ends_with('/');
            let body = raw.strip_suffix('/').unwrap_or(raw).trim_end();
            let name_end = body.find(char::is_whitespace).unwrap_or(body.len());
            let name = &body[..name_end];
            parse_xml_attributes(&body[name_end..])?;
            match name {
                "hierarchy" if !root_seen && stack.is_empty() => root_seen = true,
                "node" if stack.first() == Some(&"hierarchy") => {}
                _ => return Err(invalid_xml_structure()),
            }
            if !self_closing {
                stack.push(name);
            } else if name == "hierarchy" {
                return Err(invalid_xml_structure());
            }
        }
        cursor = end + 1;
    }
    if !root_seen || !stack.is_empty() {
        return Err(invalid_xml_structure());
    }
    Ok(())
}

fn invalid_xml_structure() -> DomainError {
    DomainError::new(
        ErrorCode::IoError,
        "privileged hierarchy has invalid XML structure",
    )
}

fn xml_tag_end(text: &str, mut cursor: usize) -> Result<usize, DomainError> {
    let bytes = text.as_bytes();
    let mut quote = None;
    while let Some(&byte) = bytes.get(cursor) {
        match (quote, byte) {
            (None, b'\'' | b'"') => quote = Some(byte),
            (Some(open), close) if open == close => quote = None,
            (None, b'>') => return Ok(cursor),
            _ => {}
        }
        cursor += 1;
    }
    Err(DomainError::new(
        ErrorCode::IoError,
        "privileged hierarchy has an unterminated node",
    ))
}

fn parse_xml_attributes(value: &str) -> Result<BTreeMap<String, String>, DomainError> {
    let bytes = value.as_bytes();
    let mut cursor = 0;
    let mut attributes = BTreeMap::new();
    while cursor < bytes.len() {
        while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        if cursor >= bytes.len() || bytes[cursor] == b'/' {
            break;
        }
        let key_start = cursor;
        while bytes
            .get(cursor)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':'))
        {
            cursor += 1;
        }
        if cursor == key_start {
            return Err(invalid_xml_attribute());
        }
        let key = &value[key_start..cursor];
        while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        if bytes.get(cursor) != Some(&b'=') {
            return Err(invalid_xml_attribute());
        }
        cursor += 1;
        while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        let quote = *bytes.get(cursor).ok_or_else(invalid_xml_attribute)?;
        if !matches!(quote, b'\'' | b'"') {
            return Err(invalid_xml_attribute());
        }
        cursor += 1;
        let value_start = cursor;
        while bytes.get(cursor).is_some_and(|byte| *byte != quote) {
            if bytes[cursor] == b'<' {
                return Err(invalid_xml_attribute());
            }
            cursor += 1;
        }
        if bytes.get(cursor) != Some(&quote) {
            return Err(invalid_xml_attribute());
        }
        let decoded = decode_xml_attribute(&value[value_start..cursor])?;
        if attributes.insert(key.to_owned(), decoded).is_some() {
            return Err(invalid_xml_attribute());
        }
        cursor += 1;
    }
    Ok(attributes)
}

fn decode_xml_attribute(value: &str) -> Result<String, DomainError> {
    let mut decoded = String::with_capacity(value.len());
    let mut cursor = 0;
    while let Some(relative) = value[cursor..].find('&') {
        let start = cursor + relative;
        decoded.push_str(&value[cursor..start]);
        let end = value[start..]
            .find(';')
            .map(|relative| start + relative)
            .ok_or_else(invalid_xml_attribute)?;
        let entity = &value[start + 1..end];
        let character = match entity {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            numeric if numeric.starts_with("#x") => u32::from_str_radix(&numeric[2..], 16)
                .ok()
                .and_then(char::from_u32)
                .ok_or_else(invalid_xml_attribute)?,
            numeric if numeric.starts_with('#') => numeric[1..]
                .parse::<u32>()
                .ok()
                .and_then(char::from_u32)
                .ok_or_else(invalid_xml_attribute)?,
            _ => return Err(invalid_xml_attribute()),
        };
        decoded.push(character);
        cursor = end + 1;
    }
    decoded.push_str(&value[cursor..]);
    Ok(decoded)
}

fn privileged_node(attributes: &BTreeMap<String, String>) -> Result<VisualNode, DomainError> {
    let bounds = attributes
        .get("bounds")
        .ok_or_else(invalid_xml_attribute)
        .and_then(|value| parse_bounds(value))?;
    Ok(VisualNode {
        node_ref: None,
        text: optional_nonempty(attributes, "text"),
        content_description: optional_nonempty(attributes, "content-desc"),
        resource_id: optional_nonempty(attributes, "resource-id"),
        class_name: optional_nonempty(attributes, "class"),
        package_name: optional_nonempty(attributes, "package"),
        bounds,
        checkable: optional_bool(attributes, "checkable")?,
        checked: optional_bool(attributes, "checked")?,
        clickable: optional_bool(attributes, "clickable")?,
        enabled: optional_bool(attributes, "enabled")?,
        focusable: optional_bool(attributes, "focusable")?,
        focused: optional_bool(attributes, "focused")?,
        scrollable: optional_bool(attributes, "scrollable")?,
        long_clickable: optional_bool(attributes, "long-clickable")?,
        password: optional_bool(attributes, "password")?,
        selected: optional_bool(attributes, "selected")?,
        editable: None,
    })
}

fn optional_nonempty(attributes: &BTreeMap<String, String>, key: &str) -> Option<String> {
    attributes
        .get(key)
        .filter(|value| !value.is_empty())
        .cloned()
}

fn optional_bool(
    attributes: &BTreeMap<String, String>,
    key: &str,
) -> Result<Option<bool>, DomainError> {
    attributes
        .get(key)
        .map(|value| match value.as_str() {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => Err(invalid_xml_attribute()),
        })
        .transpose()
}

fn parse_bounds(value: &str) -> Result<contract::NodeBounds, DomainError> {
    let normalized = value
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .ok_or_else(invalid_xml_attribute)?;
    let (first, second) = normalized
        .split_once("][")
        .ok_or_else(invalid_xml_attribute)?;
    let (left, top) = parse_coordinate_pair(first)?;
    let (right, bottom) = parse_coordinate_pair(second)?;
    // An empty rect (right < left or bottom < top) is how Android reports a node with no visible
    // area; it is observed device data, not a malformed attribute.
    Ok(contract::NodeBounds {
        left,
        top,
        right,
        bottom,
    })
}

fn parse_coordinate_pair(value: &str) -> Result<(i32, i32), DomainError> {
    let (first, second) = value.split_once(',').ok_or_else(invalid_xml_attribute)?;
    let first = first.parse().map_err(|_| invalid_xml_attribute())?;
    let second = second.parse().map_err(|_| invalid_xml_attribute())?;
    Ok((first, second))
}

fn invalid_xml_attribute() -> DomainError {
    DomainError::new(
        ErrorCode::IoError,
        "privileged hierarchy attribute is invalid",
    )
}
