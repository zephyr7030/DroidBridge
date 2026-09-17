use contract::{ErrorCode, ExecutionId, UuidV4};
use domain::DomainError;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

pub const GUARD_PROOF_LIMIT_BYTES: usize = 4096;
pub const GUARD_FRAME_LIMIT_BYTES: usize = 2048;
pub const GUARD_PROOF_COUNT_LIMIT: usize = 384;
pub const GUARD_PROOF_TOTAL_LIMIT_BYTES: u64 = 1_572_864;
const GUARD_RECOVERY_WAIT_MILLIS: u64 = 5_000;
const GUARD_RECOVERY_POLL_MILLIS: u64 = 20;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuardIdentity {
    pub runtime_epoch: UuidV4,
    pub runtime_instance_id: UuidV4,
    pub execution_id: ExecutionId,
    pub boot_id: UuidV4,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuardStarted {
    pub pid: u32,
    pub start_ticks: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuardCleanCause {
    Exited,
    Cancelled,
    Timeout,
    OwnerLost,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuardClean {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell_exit_code: Option<i32>,
    pub cause: GuardCleanCause,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GuardRecovery {
    Clean { clean: Option<GuardClean> },
    Live { pid: u32, start_ticks: u64 },
    Unverified,
}

pub trait ProcessFacts: Send + Sync {
    fn is_same_process(&self, pid: u32, start_ticks: u64) -> Result<bool, DomainError>;
}

pub trait GuardProofReader: Send + Sync {
    fn read_proof(
        &self,
        boot_id: &UuidV4,
        execution_id: &ExecutionId,
    ) -> Result<Option<Vec<u8>>, DomainError>;

    fn list_proofs(&self) -> Result<Vec<GuardProofRecord>, DomainError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GuardProofRecord {
    pub containing_boot_id: UuidV4,
    pub execution_id: ExecutionId,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct GuardProofDirectory {
    root: PathBuf,
}

impl GuardProofDirectory {
    pub fn new(canonical_base: &Path) -> Self {
        Self {
            root: canonical_base.join("execution-guards"),
        }
    }
}

impl GuardProofReader for GuardProofDirectory {
    fn read_proof(
        &self,
        boot_id: &UuidV4,
        execution_id: &ExecutionId,
    ) -> Result<Option<Vec<u8>>, DomainError> {
        let path = self
            .root
            .join(boot_id.as_str())
            .join(format!("{}.proof", execution_id.as_str()));
        let metadata = match std::fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(crate::io_error(error)),
        };
        if !metadata.is_file() {
            return Err(DomainError::new(
                ErrorCode::IoError,
                "guard proof path is not a regular file",
            ));
        }
        if metadata.len() > GUARD_PROOF_LIMIT_BYTES as u64 {
            return Ok(Some(vec![0_u8; GUARD_PROOF_LIMIT_BYTES + 1]));
        }
        std::fs::read(path).map(Some).map_err(crate::io_error)
    }

    fn list_proofs(&self) -> Result<Vec<GuardProofRecord>, DomainError> {
        let boot_directories = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(crate::io_error(error)),
        };
        let mut records = Vec::new();
        let mut total_bytes = 0_u64;
        for boot_entry in boot_directories {
            let boot_entry = boot_entry.map_err(crate::io_error)?;
            if !boot_entry.file_type().map_err(crate::io_error)?.is_dir() {
                return Err(DomainError::new(
                    ErrorCode::IoError,
                    "guard proof root contains a non-directory entry",
                ));
            }
            let boot_name = boot_entry.file_name().into_string().map_err(|_| {
                DomainError::new(ErrorCode::IoError, "guard boot directory is corrupt")
            })?;
            let containing_boot_id = UuidV4::parse(boot_name).map_err(|_| {
                DomainError::new(ErrorCode::IoError, "guard boot directory is corrupt")
            })?;
            for proof_entry in std::fs::read_dir(boot_entry.path()).map_err(crate::io_error)? {
                let proof_entry = proof_entry.map_err(crate::io_error)?;
                if !proof_entry.file_type().map_err(crate::io_error)?.is_file() {
                    return Err(DomainError::new(
                        ErrorCode::IoError,
                        "guard proof directory contains a non-file entry",
                    ));
                }
                let proof_name = proof_entry.file_name().into_string().map_err(|_| {
                    DomainError::new(ErrorCode::IoError, "guard proof name is corrupt")
                })?;
                let execution_name = proof_name.strip_suffix(".proof").ok_or_else(|| {
                    DomainError::new(ErrorCode::IoError, "guard proof name is corrupt")
                })?;
                let execution_id = UuidV4::parse(execution_name.to_owned()).map_err(|_| {
                    DomainError::new(ErrorCode::IoError, "guard proof name is corrupt")
                })?;
                let metadata = proof_entry.metadata().map_err(crate::io_error)?;
                total_bytes = total_bytes.checked_add(metadata.len()).ok_or_else(|| {
                    DomainError::new(ErrorCode::IoError, "guard proof byte count overflow")
                })?;
                if records.len() >= GUARD_PROOF_COUNT_LIMIT
                    || total_bytes > GUARD_PROOF_TOTAL_LIMIT_BYTES
                {
                    return Err(DomainError::new(
                        ErrorCode::ResourceLimit,
                        "guard proof storage exceeds its recovery limit",
                    ));
                }
                let bytes = if metadata.len() > GUARD_PROOF_LIMIT_BYTES as u64 {
                    vec![0_u8; GUARD_PROOF_LIMIT_BYTES + 1]
                } else {
                    std::fs::read(proof_entry.path()).map_err(crate::io_error)?
                };
                records.push(GuardProofRecord {
                    containing_boot_id: containing_boot_id.clone(),
                    execution_id,
                    bytes,
                });
            }
        }
        records.sort_by(|left, right| {
            left.containing_boot_id
                .as_str()
                .cmp(right.containing_boot_id.as_str())
                .then_with(|| left.execution_id.as_str().cmp(right.execution_id.as_str()))
        });
        Ok(records)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GuardCleanupRequirement {
    NoGuardAction,
    AwaitGuardExit,
    Quarantine,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GuardSettlementDisposition {
    None,
    InterruptTask,
    InterruptSynchronous,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GuardFinalizationDisposition {
    Blocked,
    RemoveProof,
    CleanupTaskTemporaryAndRemoveProof,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CleanGuardRecord {
    pub task_id: Option<contract::TaskId>,
    pub execution_id: ExecutionId,
    pub containing_boot_id: Option<UuidV4>,
    pub runtime_instance_id: Option<UuidV4>,
    pub recovery: GuardRecovery,
    pub required_cleanup: GuardCleanupRequirement,
    pub settlement: GuardSettlementDisposition,
    pub finalization: GuardFinalizationDisposition,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GuardRecoveryPlan {
    prior_runtime_instance_ids: Vec<UuidV4>,
    records: Vec<CleanGuardRecord>,
}

impl GuardRecoveryPlan {
    pub fn prior_instances(&self) -> &[UuidV4] {
        &self.prior_runtime_instance_ids
    }

    pub fn records(&self) -> &[CleanGuardRecord] {
        &self.records
    }

    pub fn guards_are_clean(&self) -> bool {
        self.records
            .iter()
            .all(|record| matches!(record.recovery, GuardRecovery::Clean { .. }))
    }

    pub fn has_live_guard(&self) -> bool {
        self.records
            .iter()
            .any(|record| matches!(record.recovery, GuardRecovery::Live { .. }))
    }
}

pub fn await_guard_recovery_plan(
    state: &crate::CanonicalState,
    current_instance_id: &UuidV4,
    current_boot_id: &UuidV4,
    proofs: &dyn GuardProofReader,
    process_facts: &dyn ProcessFacts,
) -> Result<GuardRecoveryPlan, DomainError> {
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_millis(GUARD_RECOVERY_WAIT_MILLIS);
    loop {
        let plan = build_guard_recovery_plan(
            state,
            current_instance_id,
            current_boot_id,
            proofs,
            process_facts,
        )?;
        let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) else {
            return Ok(plan);
        };
        if !plan.has_live_guard() {
            return Ok(plan);
        }
        std::thread::sleep(
            remaining.min(std::time::Duration::from_millis(GUARD_RECOVERY_POLL_MILLIS)),
        );
    }
}

pub fn build_guard_recovery_plan(
    state: &crate::CanonicalState,
    current_instance_id: &UuidV4,
    current_boot_id: &UuidV4,
    proofs: &dyn GuardProofReader,
    process_facts: &dyn ProcessFacts,
) -> Result<GuardRecoveryPlan, DomainError> {
    ensure_unique_persisted_execution_ids(state)?;
    if state.tasks.iter().any(|task| task.fence().is_none()) {
        return Err(DomainError::new(
            ErrorCode::IoError,
            "persisted Task has no admission fence",
        ));
    }

    let mut prior_runtime_instance_ids = state
        .tasks
        .iter()
        .filter(|task| {
            is_non_terminal_task(task.state)
                && task_fence(task).runtime_instance_id != *current_instance_id
        })
        .map(|task| task_fence(task).runtime_instance_id.clone())
        .chain(
            state
                .request_records
                .iter()
                .filter_map(|request| request.synchronous_execution.as_ref())
                .filter(|execution| {
                    execution.state == runtime::SynchronousExecutionState::Running
                        && execution.executor.fence.runtime_instance_id != *current_instance_id
                })
                .map(|execution| execution.executor.fence.runtime_instance_id.clone()),
        )
        .collect::<Vec<_>>();
    prior_runtime_instance_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    prior_runtime_instance_ids.dedup();

    let mut proof_records = proofs.list_proofs()?;
    proof_records.sort_by(|left, right| {
        left.execution_id
            .as_str()
            .cmp(right.execution_id.as_str())
            .then_with(|| {
                left.containing_boot_id
                    .as_str()
                    .cmp(right.containing_boot_id.as_str())
            })
    });
    let mut proof_execution_ids = BTreeSet::new();
    for proof in &proof_records {
        if !proof_execution_ids.insert(proof.execution_id.as_str().to_owned()) {
            return Err(duplicate_execution_identity());
        }
    }

    let mut result = Vec::new();
    let mut executions_with_proofs = BTreeSet::new();

    for proof in proof_records {
        let task = state
            .tasks
            .iter()
            .find(|task| task.execution_id == proof.execution_id);
        let synchronous = state
            .request_records
            .iter()
            .filter_map(|request| request.synchronous_execution.as_ref())
            .find(|execution| execution.execution_id == proof.execution_id);
        let task_is_current =
            task.is_some_and(|task| task_fence(task).runtime_instance_id == *current_instance_id);
        let synchronous_is_current = synchronous.is_some_and(|execution| {
            execution.executor.fence.runtime_instance_id == *current_instance_id
        });
        let identity = proof_identity(&proof.bytes);
        let identity_is_current = identity.as_ref().is_some_and(|identity| {
            identity.runtime_instance_id == *current_instance_id
                && identity.boot_id == *current_boot_id
                && proof.containing_boot_id == *current_boot_id
        });
        if task_is_current
            || synchronous_is_current
            || (task.is_none() && synchronous.is_none() && identity_is_current)
        {
            continue;
        }

        executions_with_proofs.insert(proof.execution_id.as_str().to_owned());
        let expected = task
            .map(|task| GuardIdentity {
                runtime_epoch: task_fence(task).runtime_epoch.clone(),
                runtime_instance_id: task_fence(task).runtime_instance_id.clone(),
                execution_id: task.execution_id.clone(),
                boot_id: proof.containing_boot_id.clone(),
            })
            .or_else(|| {
                synchronous.map(|execution| GuardIdentity {
                    runtime_epoch: execution.executor.fence.runtime_epoch.clone(),
                    runtime_instance_id: execution.executor.fence.runtime_instance_id.clone(),
                    execution_id: execution.execution_id.clone(),
                    boot_id: proof.containing_boot_id.clone(),
                })
            })
            .or_else(|| identity.clone());
        let recovery = if proof.containing_boot_id != *current_boot_id {
            GuardRecovery::Clean { clean: None }
        } else if let Some(expected) = expected.as_ref() {
            classify_guard_proof(
                proof.containing_boot_id.as_str(),
                current_boot_id,
                expected,
                &proof.bytes,
                process_facts,
            )?
        } else {
            GuardRecovery::Unverified
        };
        result.push(CleanGuardRecord {
            task_id: task.map(|task| task.task_id.clone()),
            execution_id: proof.execution_id,
            containing_boot_id: Some(proof.containing_boot_id),
            runtime_instance_id: task
                .map(|task| task_fence(task).runtime_instance_id.clone())
                .or_else(|| {
                    synchronous
                        .map(|execution| execution.executor.fence.runtime_instance_id.clone())
                })
                .or_else(|| identity.map(|identity| identity.runtime_instance_id)),
            required_cleanup: cleanup_requirement(&recovery),
            settlement: settlement_disposition(task, synchronous),
            finalization: finalization_disposition(task, &recovery, true),
            recovery,
        });
    }

    // An AutomationExecution container Task spawns no guarded process, so it owns no guard record.
    for task in state.tasks.iter().filter(|task| {
        task.automation_owner.is_none()
            && matches!(
                task.state,
                contract::TaskState::Created
                    | contract::TaskState::Queued
                    | contract::TaskState::Running
            )
            && task_fence(task).runtime_instance_id != *current_instance_id
            && !executions_with_proofs.contains(task.execution_id.as_str())
    }) {
        result.push(CleanGuardRecord {
            task_id: Some(task.task_id.clone()),
            execution_id: task.execution_id.clone(),
            containing_boot_id: None,
            runtime_instance_id: Some(task_fence(task).runtime_instance_id.clone()),
            required_cleanup: GuardCleanupRequirement::Quarantine,
            settlement: GuardSettlementDisposition::InterruptTask,
            finalization: GuardFinalizationDisposition::Blocked,
            recovery: GuardRecovery::Unverified,
        });
    }

    for execution in state
        .request_records
        .iter()
        .filter_map(|request| request.synchronous_execution.as_ref())
        .filter(|execution| {
            execution.state == runtime::SynchronousExecutionState::Running
                && execution.executor.fence.runtime_instance_id != *current_instance_id
                && !executions_with_proofs.contains(execution.execution_id.as_str())
        })
    {
        result.push(CleanGuardRecord {
            task_id: None,
            execution_id: execution.execution_id.clone(),
            containing_boot_id: None,
            runtime_instance_id: Some(execution.executor.fence.runtime_instance_id.clone()),
            required_cleanup: GuardCleanupRequirement::Quarantine,
            settlement: GuardSettlementDisposition::InterruptSynchronous,
            finalization: GuardFinalizationDisposition::Blocked,
            recovery: GuardRecovery::Unverified,
        });
    }

    result.sort_by(|left, right| {
        left.execution_id
            .as_str()
            .cmp(right.execution_id.as_str())
            .then_with(|| {
                left.containing_boot_id
                    .as_ref()
                    .map(UuidV4::as_str)
                    .cmp(&right.containing_boot_id.as_ref().map(UuidV4::as_str))
            })
    });
    Ok(GuardRecoveryPlan {
        prior_runtime_instance_ids,
        records: result,
    })
}

fn ensure_unique_persisted_execution_ids(state: &crate::CanonicalState) -> Result<(), DomainError> {
    let mut execution_ids = BTreeSet::new();
    for execution_id in state.tasks.iter().map(|task| &task.execution_id).chain(
        state
            .request_records
            .iter()
            .filter_map(|request| request.synchronous_execution.as_ref())
            .map(|execution| &execution.execution_id),
    ) {
        if !execution_ids.insert(execution_id.as_str()) {
            return Err(duplicate_execution_identity());
        }
    }
    Ok(())
}

fn duplicate_execution_identity() -> DomainError {
    DomainError::new(ErrorCode::IoError, "duplicate persisted execution identity")
}

fn is_non_terminal_task(state: contract::TaskState) -> bool {
    matches!(
        state,
        contract::TaskState::Created | contract::TaskState::Queued | contract::TaskState::Running
    )
}

fn cleanup_requirement(recovery: &GuardRecovery) -> GuardCleanupRequirement {
    match recovery {
        GuardRecovery::Clean { .. } => GuardCleanupRequirement::NoGuardAction,
        GuardRecovery::Live { .. } => GuardCleanupRequirement::AwaitGuardExit,
        GuardRecovery::Unverified => GuardCleanupRequirement::Quarantine,
    }
}

fn task_fence(task: &crate::StoredTask) -> &contract::Fence {
    task.fence()
        .expect("Task origins are validated before recovery planning")
}

fn settlement_disposition(
    task: Option<&crate::StoredTask>,
    synchronous: Option<&crate::StoredSynchronousExecution>,
) -> GuardSettlementDisposition {
    if task.is_some_and(|task| is_non_terminal_task(task.state)) {
        GuardSettlementDisposition::InterruptTask
    } else if synchronous
        .is_some_and(|execution| execution.state == runtime::SynchronousExecutionState::Running)
    {
        GuardSettlementDisposition::InterruptSynchronous
    } else {
        GuardSettlementDisposition::None
    }
}

fn finalization_disposition(
    task: Option<&crate::StoredTask>,
    recovery: &GuardRecovery,
    proof_exists: bool,
) -> GuardFinalizationDisposition {
    if !proof_exists || !matches!(recovery, GuardRecovery::Clean { .. }) {
        GuardFinalizationDisposition::Blocked
    } else if task.is_some() {
        GuardFinalizationDisposition::CleanupTaskTemporaryAndRemoveProof
    } else {
        GuardFinalizationDisposition::RemoveProof
    }
}

pub fn encode_guard_frame<T: Serialize>(value: &T) -> Result<Vec<u8>, DomainError> {
    let payload = serde_json::to_vec(value)
        .map_err(|_| DomainError::new(ErrorCode::InternalError, "guard frame encoding failed"))?;
    if payload.is_empty() || payload.len() > GUARD_FRAME_LIMIT_BYTES {
        return Err(DomainError::new(
            ErrorCode::ResourceLimit,
            "guard frame is outside its byte limit",
        ));
    }
    let length = u32::try_from(payload.len())
        .map_err(|_| DomainError::new(ErrorCode::ResourceLimit, "guard frame is too large"))?;
    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

pub fn classify_guard_proof(
    containing_boot_id: &str,
    current_boot_id: &UuidV4,
    expected: &GuardIdentity,
    bytes: &[u8],
    process_facts: &dyn ProcessFacts,
) -> Result<GuardRecovery, DomainError> {
    let containing_boot_id = UuidV4::parse(containing_boot_id.to_owned())
        .map_err(|_| DomainError::new(ErrorCode::IoError, "guard boot directory is corrupt"))?;
    if &containing_boot_id != current_boot_id {
        return Ok(GuardRecovery::Clean { clean: None });
    }
    if bytes.len() > GUARD_PROOF_LIMIT_BYTES {
        return Ok(GuardRecovery::Unverified);
    }
    let frames = match decode_frames(bytes) {
        Some(frames) => frames,
        None => return Ok(GuardRecovery::Unverified),
    };
    if frames.len() < 2 || frames.len() > 3 {
        return Ok(GuardRecovery::Unverified);
    }
    let identity: GuardIdentity = match serde_json::from_slice(frames[0]) {
        Ok(value) => value,
        Err(_) => return Ok(GuardRecovery::Unverified),
    };
    if &identity != expected || identity.boot_id != containing_boot_id {
        return Ok(GuardRecovery::Unverified);
    }
    let started: GuardStarted = match serde_json::from_slice(frames[1]) {
        Ok(value) => value,
        Err(_) => return Ok(GuardRecovery::Unverified),
    };
    if frames.len() == 3 {
        let clean: GuardClean = match serde_json::from_slice(frames[2]) {
            Ok(value) => value,
            Err(_) => return Ok(GuardRecovery::Unverified),
        };
        return Ok(GuardRecovery::Clean { clean: Some(clean) });
    }
    if process_facts.is_same_process(started.pid, started.start_ticks)? {
        Ok(GuardRecovery::Live {
            pid: started.pid,
            start_ticks: started.start_ticks,
        })
    } else {
        Ok(GuardRecovery::Unverified)
    }
}

/// Whether every S-EXEC-001 proof under `canonical_base` is clean for its execution or belongs to a
/// boot other than the current one, independent of business JSON (S-UI-017).
pub fn guard_cleanup_verified(
    canonical_base: &Path,
    current_boot_id: &UuidV4,
    process_facts: &dyn ProcessFacts,
) -> Result<bool, DomainError> {
    transition_guard_cleanup_verified(
        current_boot_id,
        &GuardProofDirectory::new(canonical_base),
        process_facts,
    )
}

pub(crate) fn transition_guard_cleanup_verified(
    current_boot_id: &UuidV4,
    proofs: &dyn GuardProofReader,
    process_facts: &dyn ProcessFacts,
) -> Result<bool, DomainError> {
    let records = proofs.list_proofs()?;
    let mut execution_ids = records
        .iter()
        .map(|record| record.execution_id.as_str())
        .collect::<Vec<_>>();
    execution_ids.sort_unstable();
    if execution_ids.windows(2).any(|pair| pair[0] == pair[1]) {
        return Ok(false);
    }
    for record in records {
        if record.containing_boot_id != *current_boot_id {
            continue;
        }
        let Some(identity) = proof_identity(&record.bytes) else {
            return Ok(false);
        };
        if identity.execution_id != record.execution_id
            || identity.boot_id != record.containing_boot_id
        {
            return Ok(false);
        }
        if !matches!(
            classify_guard_proof(
                record.containing_boot_id.as_str(),
                current_boot_id,
                &identity,
                &record.bytes,
                process_facts,
            )?,
            GuardRecovery::Clean { .. }
        ) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn decode_frames(bytes: &[u8]) -> Option<Vec<&[u8]>> {
    let mut cursor = 0_usize;
    let mut frames = Vec::new();
    while cursor < bytes.len() {
        let header = bytes.get(cursor..cursor.checked_add(4)?)?;
        let length = u32::from_be_bytes(header.try_into().ok()?) as usize;
        if length == 0 || length > GUARD_FRAME_LIMIT_BYTES {
            return None;
        }
        cursor = cursor.checked_add(4)?;
        let end = cursor.checked_add(length)?;
        let frame = bytes.get(cursor..end)?;
        std::str::from_utf8(frame).ok()?;
        frames.push(frame);
        cursor = end;
    }
    Some(frames)
}

fn proof_identity(bytes: &[u8]) -> Option<GuardIdentity> {
    if bytes.len() > GUARD_PROOF_LIMIT_BYTES {
        return None;
    }
    let frames = decode_frames(bytes)?;
    let identity = frames.first()?;
    serde_json::from_slice(identity).ok()
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeTransitionIntent {
    pub schema_version: u32,
    pub transition_id: UuidV4,
    pub runtime_epoch: UuidV4,
    pub from_host: contract::RuntimeHost,
    pub from_generation: u64,
    pub from_instance_id: UuidV4,
    pub target_host: contract::RuntimeHost,
    pub target_generation: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransitionRecovery {
    RemoveUncommittedIntent,
    ActivateCommittedTarget,
}

pub fn classify_transition(
    intent: &RuntimeTransitionIntent,
    owner: &crate::RuntimeOwner,
) -> Result<TransitionRecovery, DomainError> {
    if intent.schema_version != 1
        || intent.target_generation
            != intent.from_generation.checked_add(1).ok_or_else(|| {
                DomainError::new(ErrorCode::ResourceLimit, "host generation exhausted")
            })?
        || intent.from_host == intent.target_host
        || owner.runtime_epoch != intent.runtime_epoch
    {
        return Err(DomainError::new(
            ErrorCode::IoError,
            "transition intent is corrupt",
        ));
    }
    if owner.host == intent.target_host && owner.host_generation == intent.target_generation {
        return Ok(TransitionRecovery::ActivateCommittedTarget);
    }
    if owner.host == intent.from_host && owner.host_generation == intent.from_generation {
        return Ok(TransitionRecovery::RemoveUncommittedIntent);
    }
    Err(DomainError::new(
        ErrorCode::IoError,
        "owner and transition intent disagree",
    ))
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeResetIntent {
    pub schema_version: u32,
    pub reset_id: UuidV4,
    pub runtime_epoch: UuidV4,
    pub source_host_generation: u64,
    pub target_host: contract::RuntimeHost,
    pub target_host_generation: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResetOwnerState {
    Source,
    Target,
}

pub fn validate_reset_owner(
    intent: &RuntimeResetIntent,
    owner: &crate::RuntimeOwner,
) -> Result<ResetOwnerState, DomainError> {
    if intent.schema_version != 1
        || intent.target_host != contract::RuntimeHost::ApkRuntime
        || intent.target_host_generation
            != intent
                .source_host_generation
                .checked_add(1)
                .ok_or_else(|| {
                    DomainError::new(ErrorCode::ResourceLimit, "host generation exhausted")
                })?
        || owner.runtime_epoch != intent.runtime_epoch
    {
        return Err(DomainError::new(
            ErrorCode::IoError,
            "reset intent is corrupt",
        ));
    }
    if owner.host_generation == intent.source_host_generation {
        return Ok(ResetOwnerState::Source);
    }
    if owner.host == intent.target_host && owner.host_generation == intent.target_host_generation {
        return Ok(ResetOwnerState::Target);
    }
    Err(DomainError::new(
        ErrorCode::IoError,
        "owner and reset intent disagree",
    ))
}
