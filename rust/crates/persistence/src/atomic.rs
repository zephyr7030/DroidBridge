use crate::{
    CanonicalState, FileLock, RuntimeLive, RuntimeOwner, RuntimeResetIntent,
    RuntimeTransitionIntent, WriterFence, io_error, sync_directory, validate_artifact_record,
    validate_reset_owner,
};
use contract::{ErrorCode, RuntimeHost};
use domain::DomainError;
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

pub const STORE_LIMIT_BYTES: usize = 8 * 1024 * 1024;

/// The S-UI-017 maintenance blocker observed from the canonical files alone.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MaintenanceBlocker {
    None,
    OwnerCorrupt,
    StoreCorrupt,
}

impl MaintenanceBlocker {
    pub const fn token(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::OwnerCorrupt => "owner_corrupt",
            Self::StoreCorrupt => "store_corrupt",
        }
    }
}

pub fn verify_magisk_metadata_surface(base: &Path) -> Result<(), DomainError> {
    let source = base.join("runtime-state.json");
    if !source.is_file() {
        return Err(DomainError::new(
            ErrorCode::NotFound,
            "canonical state is not initialized",
        ));
    }
    let temporary = base.join(".magisk-metadata-self-test");
    if temporary.exists() {
        fs::remove_file(&temporary).map_err(io_error)?;
        sync_directory(base)?;
    }
    let file = open_new_private(&temporary)?;
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::MetadataExt;

        let metadata = fs::metadata(&source).map_err(io_error)?;
        if unsafe { libc::fchown(file.as_raw_fd(), metadata.uid(), metadata.gid()) } != 0
            || unsafe { libc::fchmod(file.as_raw_fd(), 0o600) } != 0
        {
            return Err(io_error(std::io::Error::last_os_error()));
        }
        preserve_selinux_label(&source, &file)?;
        let verified = file.metadata().map_err(io_error)?;
        if verified.uid() != metadata.uid()
            || verified.gid() != metadata.gid()
            || verified.mode() & 0o777 != 0o600
        {
            return Err(DomainError::new(
                ErrorCode::IoError,
                "Magisk metadata self-test verification failed",
            ));
        }
    }
    file.sync_all().map_err(io_error)?;
    drop(file);
    sync_directory(base)?;
    fs::remove_file(&temporary).map_err(io_error)?;
    sync_directory(base)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommitPhase {
    LockWait,
    Parse,
    DomainMutation,
    Serialize,
    TempWriteFsync,
    RenameDirectoryFsync,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CommitInstrumentation {
    pub lock_wait_ns: u128,
    pub parse_ns: u128,
    pub domain_mutation_ns: u128,
    pub serialize_ns: u128,
    pub temp_write_fsync_ns: u128,
    pub rename_directory_fsync_ns: u128,
    pub total_ns: u128,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CrashPoint {
    BeforeTempCreate,
    AfterTempFsync,
    AfterRename,
}

#[derive(Clone, Debug)]
pub struct StateStore {
    base: PathBuf,
}

pub struct LifetimeLease {
    base: PathBuf,
    live: RuntimeLive,
    _lock: FileLock,
}

pub struct PendingDeadOwnerTakeover {
    lease: Arc<LifetimeLease>,
    intent: RuntimeTransitionIntent,
    recovery_plan: crate::GuardRecoveryPlan,
}

impl PendingDeadOwnerTakeover {
    pub fn lease(&self) -> &Arc<LifetimeLease> {
        &self.lease
    }

    pub fn recovery_plan(&self) -> &crate::GuardRecoveryPlan {
        &self.recovery_plan
    }
}

impl LifetimeLease {
    pub fn live(&self) -> &RuntimeLive {
        &self.live
    }
}

impl StateStore {
    pub fn new(base: PathBuf) -> Self {
        Self { base }
    }

    pub(crate) fn base_path(&self) -> &Path {
        &self.base
    }

    pub fn initialize(
        &self,
        owner: &RuntimeOwner,
        state: &CanonicalState,
    ) -> Result<(), DomainError> {
        if owner.schema_version != 1 || owner.host_generation == 0 {
            return Err(DomainError::invalid("initial runtime owner is invalid"));
        }
        let state_bytes = serde_json::to_vec(state).map_err(|_| {
            DomainError::new(ErrorCode::InternalError, "store serialization failed")
        })?;
        validate_state(state, state_bytes.len())?;
        fs::create_dir_all(&self.base).map_err(io_error)?;
        write_new_json(&self.base.join("runtime-owner.json"), owner)?;
        write_new_json(&self.base.join("runtime-state.json"), state)?;
        open_lock_file(&self.base.join("runtime-live.lock"))?;
        open_lock_file(&self.base.join("runtime-state.lock"))?;
        sync_directory(&self.base)
    }

    pub fn read_owner(&self) -> Result<RuntimeOwner, DomainError> {
        let owner: RuntimeOwner = read_json(&self.base.join("runtime-owner.json"))?;
        if owner.schema_version != 1 || owner.host_generation == 0 {
            return Err(DomainError::new(
                ErrorCode::IoError,
                "runtime owner is corrupt",
            ));
        }
        Ok(owner)
    }

    pub fn observe_transition(
        &self,
    ) -> Result<Option<(crate::TransitionRecovery, RuntimeTransitionIntent)>, DomainError> {
        let _state_lock = FileLock::acquire(&self.base.join("runtime-state.lock"))?;
        let transition_path = self.base.join("runtime-transition.json");
        if !transition_path.exists() {
            return Ok(None);
        }
        let intent: RuntimeTransitionIntent = read_json(&transition_path)?;
        let owner = self.read_owner()?;
        let recovery = crate::classify_transition(&intent, &owner)?;
        Ok(Some((recovery, intent)))
    }

    pub fn record_transition_intent(
        &self,
        lease: &LifetimeLease,
        intent: &RuntimeTransitionIntent,
    ) -> Result<(), DomainError> {
        if self.base.join("runtime-reset-intent.json").exists() {
            return Err(DomainError::new(
                ErrorCode::HostTransitionPending,
                "Runtime reset is pending",
            ));
        }
        self.validate_lease(lease)?;
        let owner = self.read_owner()?;
        if intent.schema_version != 1
            || intent.runtime_epoch != owner.runtime_epoch
            || intent.from_host != owner.host
            || intent.from_generation != owner.host_generation
            || intent.from_instance_id != lease.live.runtime_instance_id
            || intent.target_host == intent.from_host
            || intent.target_generation
                != intent.from_generation.checked_add(1).ok_or_else(|| {
                    DomainError::new(ErrorCode::ResourceLimit, "host generation exhausted")
                })?
        {
            return Err(DomainError::new(
                ErrorCode::StaleAuthority,
                "transition intent does not match the live owner",
            ));
        }
        write_new_json(&self.base.join("runtime-transition.json"), intent)?;
        sync_directory(&self.base)
    }

    pub fn record_reset_intent(
        &self,
        lease: &LifetimeLease,
        intent: &RuntimeResetIntent,
        synchronous_work_absent: bool,
        cleanup_verified: bool,
    ) -> Result<(), DomainError> {
        if !synchronous_work_absent || !cleanup_verified {
            return Err(DomainError::new(
                ErrorCode::IoError,
                "reset requires zero live work and verified cleanup",
            ));
        }
        if self.base.join("runtime-transition.json").exists() {
            return Err(DomainError::new(
                ErrorCode::HostTransitionPending,
                "Runtime transition is pending",
            ));
        }
        self.validate_lease(lease)?;
        let owner = self.read_owner()?;
        let state = self.load(lease)?;
        let automation_work = state.automation_executions.iter().any(|execution| {
            matches!(
                execution.summary.state,
                contract::AutomationExecutionState::Queued
                    | contract::AutomationExecutionState::Running
            )
        });
        if state.tasks.iter().any(|task| {
            matches!(
                task.state,
                contract::TaskState::Created
                    | contract::TaskState::Queued
                    | contract::TaskState::Running
            )
        }) || automation_work
            || !state.reservations.is_empty()
            || intent.schema_version != 1
            || intent.runtime_epoch != owner.runtime_epoch
            || intent.source_host_generation != owner.host_generation
            || intent.target_host != RuntimeHost::ApkRuntime
            || intent.target_host_generation
                != intent
                    .source_host_generation
                    .checked_add(1)
                    .ok_or_else(|| {
                        DomainError::new(ErrorCode::ResourceLimit, "host generation exhausted")
                    })?
        {
            return Err(DomainError::new(
                ErrorCode::StaleAuthority,
                "reset intent does not match zero-work owner state",
            ));
        }
        write_new_json(&self.base.join("runtime-reset-intent.json"), intent)?;
        sync_directory(&self.base)
    }

    pub fn acquire_lifetime(&self, live: RuntimeLive) -> Result<LifetimeLease, DomainError> {
        let lock = FileLock::acquire(&self.base.join("runtime-live.lock"))?;
        let owner: RuntimeOwner = read_json(&self.base.join("runtime-owner.json"))?;
        if self.base.join("runtime-reset-intent.json").exists() {
            return Err(DomainError::new(
                ErrorCode::HostTransitionPending,
                "Runtime reset is pending",
            ));
        }
        let transition_path = self.base.join("runtime-transition.json");
        if transition_path.exists() {
            let intent: RuntimeTransitionIntent = read_json(&transition_path)?;
            match crate::classify_transition(&intent, &owner)? {
                crate::TransitionRecovery::ActivateCommittedTarget => {}
                crate::TransitionRecovery::RemoveUncommittedIntent => {
                    return Err(DomainError::new(
                        ErrorCode::HostTransitionPending,
                        "Runtime transition recovery is pending",
                    ));
                }
            }
        }
        let fence = WriterFence {
            runtime_epoch: live.runtime_epoch.clone(),
            host: live.host,
            host_generation: live.host_generation,
            runtime_instance_id: live.runtime_instance_id.clone(),
        };
        if !fence.matches(&owner, &live) {
            return Err(DomainError::new(
                ErrorCode::StaleAuthority,
                "live identity does not match runtime owner",
            ));
        }
        atomic_replace_json(&self.base, "runtime-live.json", &live)?;
        Ok(LifetimeLease {
            base: self.base.clone(),
            live,
            _lock: lock,
        })
    }

    pub fn load(&self, lease: &LifetimeLease) -> Result<CanonicalState, DomainError> {
        self.load_measured(lease).map(|(state, _)| state)
    }

    /// The canonical state and its encoded size. The store is written as exactly this encoding,
    /// so the bytes read are the measurement and the state is never encoded again to learn it.
    pub fn load_measured(
        &self,
        lease: &LifetimeLease,
    ) -> Result<(CanonicalState, u64), DomainError> {
        let _state_lock = FileLock::acquire(&self.base.join("runtime-state.lock"))?;
        self.validate_lease(lease)?;
        let path = self.base.join("runtime-state.json");
        let bytes = fs::read(path).map_err(io_error)?;
        let state = decode_canonical_state(&bytes)?;
        Ok((state, bytes.len() as u64))
    }

    pub fn commit_owner_transition(
        &self,
        intent: &RuntimeTransitionIntent,
        current_boot_id: &contract::UuidV4,
        process_facts: &dyn crate::ProcessFacts,
    ) -> Result<RuntimeOwner, DomainError> {
        let _lifetime_lock = FileLock::try_acquire(&self.base.join("runtime-live.lock"))?
            .ok_or_else(|| {
                DomainError::new(
                    ErrorCode::HostTransitionPending,
                    "source Runtime still owns the lifetime lock",
                )
            })?;
        let _state_lock = FileLock::acquire(&self.base.join("runtime-state.lock"))?;
        if self.base.join("runtime-reset-intent.json").exists() {
            return Err(DomainError::new(
                ErrorCode::HostTransitionPending,
                "Runtime reset is pending",
            ));
        }
        let owner: RuntimeOwner = read_json(&self.base.join("runtime-owner.json"))?;
        let live: RuntimeLive = read_json(&self.base.join("runtime-live.json"))?;
        let recorded: RuntimeTransitionIntent =
            read_json(&self.base.join("runtime-transition.json"))?;
        if &recorded != intent
            || intent.schema_version != 1
            || intent.runtime_epoch != owner.runtime_epoch
            || intent.from_host != owner.host
            || intent.from_generation != owner.host_generation
            || intent.runtime_epoch != live.runtime_epoch
            || intent.from_host != live.host
            || intent.from_generation != live.host_generation
            || intent.from_instance_id != live.runtime_instance_id
            || intent.target_host == intent.from_host
            || intent.target_generation
                != intent.from_generation.checked_add(1).ok_or_else(|| {
                    DomainError::new(ErrorCode::ResourceLimit, "host generation exhausted")
                })?
        {
            return Err(DomainError::new(
                ErrorCode::StaleAuthority,
                "transition intent does not match the live owner",
            ));
        }
        let state = decode_canonical_state(
            &fs::read(self.base.join("runtime-state.json")).map_err(io_error)?,
        )?;
        let task_work = state.tasks.iter().any(|task| {
            matches!(
                task.state,
                contract::TaskState::Created
                    | contract::TaskState::Queued
                    | contract::TaskState::Running
            )
        });
        let automation_work = state.automation_executions.iter().any(|execution| {
            matches!(
                execution.summary.state,
                contract::AutomationExecutionState::Queued
                    | contract::AutomationExecutionState::Running
            )
        });
        if task_work || automation_work || !state.reservations.is_empty() {
            return Err(DomainError::new(
                ErrorCode::HostTransitionPending,
                "source Runtime still has non-terminal work",
            ));
        }
        if !crate::recovery::transition_guard_cleanup_verified(
            current_boot_id,
            &crate::GuardProofDirectory::new(&self.base),
            process_facts,
        )? {
            return Err(DomainError::new(
                ErrorCode::HostTransitionPending,
                "source Runtime cleanup is unverified",
            ));
        }
        let next = RuntimeOwner {
            schema_version: 1,
            runtime_epoch: owner.runtime_epoch,
            host: intent.target_host,
            host_generation: intent.target_generation,
        };
        atomic_replace_json(&self.base, "runtime-owner.json", &next)?;
        Ok(next)
    }

    pub fn abort_owner_transition(
        &self,
        lease: &LifetimeLease,
        intent: &RuntimeTransitionIntent,
    ) -> Result<(), DomainError> {
        let _state_lock = FileLock::acquire(&self.base.join("runtime-state.lock"))?;
        self.validate_lease(lease)?;
        let recorded: RuntimeTransitionIntent =
            read_json(&self.base.join("runtime-transition.json"))?;
        let owner = self.read_owner()?;
        if &recorded != intent
            || owner.runtime_epoch != intent.runtime_epoch
            || owner.host != intent.from_host
            || owner.host_generation != intent.from_generation
            || lease.live.runtime_instance_id != intent.from_instance_id
        {
            return Err(DomainError::new(
                ErrorCode::StaleAuthority,
                "transition abort does not match the live source",
            ));
        }
        remove_file_synced(&self.base, "runtime-transition.json")
    }

    pub fn finish_owner_transition(
        &self,
        lease: &LifetimeLease,
        intent: &RuntimeTransitionIntent,
    ) -> Result<(), DomainError> {
        let _state_lock = FileLock::acquire(&self.base.join("runtime-state.lock"))?;
        self.validate_lease(lease)?;
        let recorded: RuntimeTransitionIntent =
            read_json(&self.base.join("runtime-transition.json"))?;
        let owner = self.read_owner()?;
        if &recorded != intent
            || owner.runtime_epoch != intent.runtime_epoch
            || owner.host != intent.target_host
            || owner.host_generation != intent.target_generation
            || lease.live.runtime_epoch != intent.runtime_epoch
            || lease.live.host != intent.target_host
            || lease.live.host_generation != intent.target_generation
        {
            return Err(DomainError::new(
                ErrorCode::StaleAuthority,
                "transition completion does not match the live target",
            ));
        }
        remove_file_synced(&self.base, "runtime-transition.json")
    }

    pub fn finish_remote_owner_transition(
        &self,
        intent: &RuntimeTransitionIntent,
        target_instance_id: &contract::UuidV4,
    ) -> Result<(), DomainError> {
        let _state_lock = FileLock::acquire(&self.base.join("runtime-state.lock"))?;
        let recorded: RuntimeTransitionIntent =
            read_json(&self.base.join("runtime-transition.json"))?;
        let owner = self.read_owner()?;
        let live: RuntimeLive = read_json(&self.base.join("runtime-live.json"))?;
        if &recorded != intent
            || owner.runtime_epoch != intent.runtime_epoch
            || owner.host != intent.target_host
            || owner.host_generation != intent.target_generation
            || live.runtime_epoch != intent.runtime_epoch
            || live.host != intent.target_host
            || live.host_generation != intent.target_generation
            || &live.runtime_instance_id != target_instance_id
        {
            return Err(DomainError::new(
                ErrorCode::StaleAuthority,
                "transition completion does not match the remote target",
            ));
        }
        remove_file_synced(&self.base, "runtime-transition.json")
    }

    pub fn finish_committed_transition(
        &self,
        target_instance_id: &contract::UuidV4,
    ) -> Result<bool, DomainError> {
        let _state_lock = FileLock::acquire(&self.base.join("runtime-state.lock"))?;
        let transition_path = self.base.join("runtime-transition.json");
        if !transition_path.exists() {
            return Ok(false);
        }
        let intent: RuntimeTransitionIntent = read_json(&transition_path)?;
        let owner = self.read_owner()?;
        let live: RuntimeLive = read_json(&self.base.join("runtime-live.json"))?;
        let valid = intent.schema_version == 1
            && intent.target_host != intent.from_host
            && intent.target_generation
                == intent.from_generation.checked_add(1).ok_or_else(|| {
                    DomainError::new(ErrorCode::ResourceLimit, "host generation exhausted")
                })?
            && owner.runtime_epoch == intent.runtime_epoch
            && owner.host == intent.target_host
            && owner.host_generation == intent.target_generation
            && live.runtime_epoch == intent.runtime_epoch
            && live.host == intent.target_host
            && live.host_generation == intent.target_generation
            && &live.runtime_instance_id == target_instance_id;
        if !valid {
            return Err(DomainError::new(
                ErrorCode::StaleAuthority,
                "committed transition does not match the live target",
            ));
        }
        remove_file_synced(&self.base, "runtime-transition.json")?;
        Ok(true)
    }

    pub fn validate_live_instance(
        &self,
        expected_host: RuntimeHost,
        expected_instance_id: &contract::UuidV4,
    ) -> Result<RuntimeOwner, DomainError> {
        let _state_lock = FileLock::acquire(&self.base.join("runtime-state.lock"))?;
        if self.base.join("runtime-reset-intent.json").exists()
            || self.base.join("runtime-transition.json").exists()
        {
            return Err(DomainError::new(
                ErrorCode::HostTransitionPending,
                "Runtime transition recovery is pending",
            ));
        }
        let owner = self.read_owner()?;
        let live: RuntimeLive = read_json(&self.base.join("runtime-live.json"))?;
        let fence = WriterFence {
            runtime_epoch: owner.runtime_epoch.clone(),
            host: expected_host,
            host_generation: owner.host_generation,
            runtime_instance_id: expected_instance_id.clone(),
        };
        if owner.host != expected_host || !fence.matches(&owner, &live) {
            return Err(DomainError::new(
                ErrorCode::StaleAuthority,
                "live instance does not match the current Runtime owner",
            ));
        }
        Ok(owner)
    }

    pub fn record_remote_transition_intent(
        &self,
        intent: &RuntimeTransitionIntent,
    ) -> Result<(), DomainError> {
        let _state_lock = FileLock::acquire(&self.base.join("runtime-state.lock"))?;
        if self.base.join("runtime-reset-intent.json").exists() {
            return Err(DomainError::new(
                ErrorCode::HostTransitionPending,
                "Runtime reset is pending",
            ));
        }
        let owner = self.read_owner()?;
        let live: RuntimeLive = read_json(&self.base.join("runtime-live.json"))?;
        if intent.schema_version != 1
            || intent.runtime_epoch != owner.runtime_epoch
            || intent.from_host != owner.host
            || intent.from_generation != owner.host_generation
            || intent.from_instance_id != live.runtime_instance_id
            || live.runtime_epoch != owner.runtime_epoch
            || live.host != owner.host
            || live.host_generation != owner.host_generation
            || intent.target_host == intent.from_host
            || intent.target_generation
                != intent.from_generation.checked_add(1).ok_or_else(|| {
                    DomainError::new(ErrorCode::ResourceLimit, "host generation exhausted")
                })?
        {
            return Err(DomainError::new(
                ErrorCode::StaleAuthority,
                "remote transition intent does not match the live owner",
            ));
        }
        write_new_json(&self.base.join("runtime-transition.json"), intent)?;
        sync_directory(&self.base)
    }

    pub fn abort_remote_transition_intent(
        &self,
        intent: &RuntimeTransitionIntent,
    ) -> Result<(), DomainError> {
        let _state_lock = FileLock::acquire(&self.base.join("runtime-state.lock"))?;
        let recorded: RuntimeTransitionIntent =
            read_json(&self.base.join("runtime-transition.json"))?;
        let owner = self.read_owner()?;
        let live: RuntimeLive = read_json(&self.base.join("runtime-live.json"))?;
        if &recorded != intent
            || owner.runtime_epoch != intent.runtime_epoch
            || owner.host != intent.from_host
            || owner.host_generation != intent.from_generation
            || live.runtime_epoch != intent.runtime_epoch
            || live.host != intent.from_host
            || live.host_generation != intent.from_generation
            || live.runtime_instance_id != intent.from_instance_id
        {
            return Err(DomainError::new(
                ErrorCode::StaleAuthority,
                "remote transition abort does not match the live source",
            ));
        }
        remove_file_synced(&self.base, "runtime-transition.json")
    }

    pub fn begin_dead_owner_takeover(
        &self,
        intent: &RuntimeTransitionIntent,
        target_live: RuntimeLive,
        current_boot_id: &contract::UuidV4,
        process_facts: &dyn crate::ProcessFacts,
    ) -> Result<PendingDeadOwnerTakeover, DomainError> {
        let lock =
            FileLock::try_acquire(&self.base.join("runtime-live.lock"))?.ok_or_else(|| {
                DomainError::new(
                    ErrorCode::HostTransitionPending,
                    "source Runtime still owns the lifetime lock",
                )
            })?;
        let _state_lock = FileLock::acquire(&self.base.join("runtime-state.lock"))?;
        if self.base.join("runtime-reset-intent.json").exists() {
            return Err(DomainError::new(
                ErrorCode::HostTransitionPending,
                "Runtime reset is pending",
            ));
        }
        let owner = self.read_owner()?;
        let previous_live: RuntimeLive = read_json(&self.base.join("runtime-live.json"))?;
        let valid = intent.schema_version == 1
            && intent.from_host != intent.target_host
            && intent.runtime_epoch == owner.runtime_epoch
            && intent.from_host == owner.host
            && intent.from_generation == owner.host_generation
            && intent.from_instance_id == previous_live.runtime_instance_id
            && previous_live.runtime_epoch == owner.runtime_epoch
            && previous_live.host == owner.host
            && previous_live.host_generation == owner.host_generation
            && intent.target_host == RuntimeHost::ApkRuntime
            && intent.target_generation
                == owner.host_generation.checked_add(1).ok_or_else(|| {
                    DomainError::new(ErrorCode::ResourceLimit, "host generation exhausted")
                })?
            && target_live.runtime_epoch == intent.runtime_epoch
            && target_live.host == intent.target_host
            && target_live.host_generation == intent.target_generation
            && target_live.runtime_instance_id != intent.from_instance_id
            && target_live.boot_id == *current_boot_id;
        if !valid {
            return Err(DomainError::new(
                ErrorCode::StaleAuthority,
                "dead owner takeover identity is stale",
            ));
        }
        let state = decode_canonical_state(
            &fs::read(self.base.join("runtime-state.json")).map_err(io_error)?,
        )?;
        let recovery_plan = crate::await_guard_recovery_plan(
            &state,
            &target_live.runtime_instance_id,
            current_boot_id,
            &crate::GuardProofDirectory::new(&self.base),
            process_facts,
        )?;
        if !recovery_plan.guards_are_clean() {
            return Err(DomainError::new(
                ErrorCode::IoError,
                "dead owner takeover requires verified source cleanup",
            ));
        }
        let transition_path = self.base.join("runtime-transition.json");
        if transition_path.exists() {
            let recorded: RuntimeTransitionIntent = read_json(&transition_path)?;
            if recorded != *intent {
                return Err(DomainError::new(
                    ErrorCode::StaleAuthority,
                    "another Runtime transition is pending",
                ));
            }
        } else {
            write_new_json(&transition_path, intent)?;
            sync_directory(&self.base)?;
        }
        let next = RuntimeOwner {
            schema_version: 1,
            runtime_epoch: owner.runtime_epoch,
            host: intent.target_host,
            host_generation: intent.target_generation,
        };
        atomic_replace_json(&self.base, "runtime-owner.json", &next)?;
        atomic_replace_json(&self.base, "runtime-live.json", &target_live)?;
        Ok(PendingDeadOwnerTakeover {
            lease: Arc::new(LifetimeLease {
                base: self.base.clone(),
                live: target_live,
                _lock: lock,
            }),
            intent: intent.clone(),
            recovery_plan,
        })
    }

    pub fn resume_dead_owner_takeover(
        &self,
        intent: &RuntimeTransitionIntent,
        target_live: RuntimeLive,
        current_boot_id: &contract::UuidV4,
        process_facts: &dyn crate::ProcessFacts,
    ) -> Result<PendingDeadOwnerTakeover, DomainError> {
        let lock =
            FileLock::try_acquire(&self.base.join("runtime-live.lock"))?.ok_or_else(|| {
                DomainError::new(
                    ErrorCode::HostTransitionPending,
                    "takeover target still owns the lifetime lock",
                )
            })?;
        let _state_lock = FileLock::acquire(&self.base.join("runtime-state.lock"))?;
        if self.base.join("runtime-reset-intent.json").exists() {
            return Err(DomainError::new(
                ErrorCode::HostTransitionPending,
                "Runtime reset is pending",
            ));
        }
        let recorded: RuntimeTransitionIntent =
            read_json(&self.base.join("runtime-transition.json"))?;
        let owner = self.read_owner()?;
        let previous_live: RuntimeLive = read_json(&self.base.join("runtime-live.json"))?;
        let previous_live_is_source = previous_live.runtime_epoch == intent.runtime_epoch
            && previous_live.host == intent.from_host
            && previous_live.host_generation == intent.from_generation
            && previous_live.runtime_instance_id == intent.from_instance_id;
        let previous_live_is_target = previous_live.runtime_epoch == intent.runtime_epoch
            && previous_live.host == intent.target_host
            && previous_live.host_generation == intent.target_generation;
        let valid = recorded == *intent
            && intent.schema_version == 1
            && intent.from_host != intent.target_host
            && owner.runtime_epoch == intent.runtime_epoch
            && owner.host == intent.target_host
            && owner.host_generation == intent.target_generation
            && intent.target_generation
                == intent.from_generation.checked_add(1).ok_or_else(|| {
                    DomainError::new(ErrorCode::ResourceLimit, "host generation exhausted")
                })?
            && target_live.runtime_epoch == intent.runtime_epoch
            && target_live.host == intent.target_host
            && target_live.host_generation == intent.target_generation
            && target_live.runtime_instance_id != intent.from_instance_id
            && target_live.boot_id == *current_boot_id
            && (!previous_live_is_target
                || target_live.runtime_instance_id != previous_live.runtime_instance_id)
            && (previous_live_is_source || previous_live_is_target);
        if !valid {
            return Err(DomainError::new(
                ErrorCode::StaleAuthority,
                "dead owner takeover resume identity is stale",
            ));
        }
        let state = decode_canonical_state(
            &fs::read(self.base.join("runtime-state.json")).map_err(io_error)?,
        )?;
        let recovery_plan = crate::await_guard_recovery_plan(
            &state,
            &target_live.runtime_instance_id,
            current_boot_id,
            &crate::GuardProofDirectory::new(&self.base),
            process_facts,
        )?;
        if !recovery_plan.guards_are_clean() {
            return Err(DomainError::new(
                ErrorCode::IoError,
                "dead owner takeover resume requires verified cleanup",
            ));
        }
        atomic_replace_json(&self.base, "runtime-live.json", &target_live)?;
        Ok(PendingDeadOwnerTakeover {
            lease: Arc::new(LifetimeLease {
                base: self.base.clone(),
                live: target_live,
                _lock: lock,
            }),
            intent: intent.clone(),
            recovery_plan,
        })
    }

    pub fn complete_dead_owner_takeover(
        &self,
        pending: PendingDeadOwnerTakeover,
        process_facts: &dyn crate::ProcessFacts,
    ) -> Result<Arc<LifetimeLease>, DomainError> {
        let _state_lock = FileLock::acquire(&self.base.join("runtime-state.lock"))?;
        if self.base.join("runtime-reset-intent.json").exists() {
            return Err(DomainError::new(
                ErrorCode::HostTransitionPending,
                "Runtime reset is pending",
            ));
        }
        self.validate_lease(&pending.lease)?;
        let recorded: RuntimeTransitionIntent =
            read_json(&self.base.join("runtime-transition.json"))?;
        if recorded != pending.intent {
            return Err(DomainError::new(
                ErrorCode::StaleAuthority,
                "dead owner takeover completion intent is stale",
            ));
        }
        let state = decode_canonical_state(
            &fs::read(self.base.join("runtime-state.json")).map_err(io_error)?,
        )?;
        let recovery_plan = crate::await_guard_recovery_plan(
            &state,
            &pending.lease.live.runtime_instance_id,
            &pending.lease.live.boot_id,
            &crate::GuardProofDirectory::new(&self.base),
            process_facts,
        )?;
        if !recovery_plan.guards_are_clean() {
            return Err(DomainError::new(
                ErrorCode::IoError,
                "dead owner takeover cleanup remains unverified",
            ));
        }
        let task_work = state.tasks.iter().any(|task| {
            matches!(
                task.state,
                contract::TaskState::Created
                    | contract::TaskState::Queued
                    | contract::TaskState::Running
            )
        });
        let automation_work = state.automation_executions.iter().any(|execution| {
            matches!(
                execution.summary.state,
                contract::AutomationExecutionState::Queued
                    | contract::AutomationExecutionState::Running
            )
        });
        let synchronous_work = state
            .request_records
            .iter()
            .filter_map(|request| request.synchronous_execution.as_ref())
            .any(|execution| execution.state == runtime::SynchronousExecutionState::Running);
        if !recovery_plan.records().is_empty()
            || !recovery_plan.prior_instances().is_empty()
            || task_work
            || automation_work
            || synchronous_work
            || !state.reservations.is_empty()
        {
            return Err(DomainError::new(
                ErrorCode::HostTransitionPending,
                "dead owner takeover recovery is not complete",
            ));
        }
        remove_file_synced(&self.base, "runtime-transition.json")?;
        Ok(pending.lease)
    }

    pub fn recover_confirmed_reset(
        &self,
        cleanup_verified: bool,
    ) -> Result<RuntimeOwner, DomainError> {
        if !cleanup_verified {
            return Err(DomainError::new(
                ErrorCode::IoError,
                "reset cannot bypass unverified cleanup",
            ));
        }
        let _lifetime_lock = FileLock::acquire(&self.base.join("runtime-live.lock"))?;
        if self.base.join("runtime-transition.json").exists() {
            return Err(DomainError::new(
                ErrorCode::IoError,
                "reset and transition intents conflict",
            ));
        }
        let intent_path = self.base.join("runtime-reset-intent.json");
        let intent: RuntimeResetIntent = read_json(&intent_path)?;
        let owner: RuntimeOwner = read_json(&self.base.join("runtime-owner.json"))?;
        validate_reset_owner(&intent, &owner)?;
        let reset_trash = self.base.join("reset-trash").join(intent.reset_id.as_str());
        let artifacts = self.base.join("artifacts");
        let trashed_artifacts = reset_trash.join("artifacts");
        if artifacts.exists() {
            if trashed_artifacts.exists() {
                return Err(DomainError::new(
                    ErrorCode::IoError,
                    "reset artifact source and trash both exist",
                ));
            }
            fs::create_dir_all(&reset_trash).map_err(io_error)?;
            fs::rename(&artifacts, &trashed_artifacts).map_err(io_error)?;
            sync_directory(&self.base)?;
            sync_directory(&reset_trash)?;
        }
        let owner = {
            let _state_lock = FileLock::acquire(&self.base.join("runtime-state.lock"))?;
            let recorded_intent: RuntimeResetIntent = read_json(&intent_path)?;
            if recorded_intent != intent {
                return Err(DomainError::new(
                    ErrorCode::StaleAuthority,
                    "reset intent changed during recovery",
                ));
            }
            let mut owner: RuntimeOwner = read_json(&self.base.join("runtime-owner.json"))?;
            let owner_state = validate_reset_owner(&intent, &owner)?;
            let empty_state = CanonicalState::default();
            let expected = serde_json::to_vec(&empty_state).map_err(|_| {
                DomainError::new(ErrorCode::InternalError, "reset store encoding failed")
            })?;
            let state_path = self.base.join("runtime-state.json");
            let current_state = match fs::read(&state_path) {
                Ok(bytes) => Some(bytes),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(io_error(error)),
            };
            if current_state.as_deref() != Some(expected.as_slice()) {
                atomic_replace_json(&self.base, "runtime-state.json", &empty_state)?;
            }
            if owner_state == crate::ResetOwnerState::Source {
                owner = RuntimeOwner {
                    schema_version: 1,
                    runtime_epoch: intent.runtime_epoch.clone(),
                    host: RuntimeHost::ApkRuntime,
                    host_generation: intent.target_host_generation,
                };
                atomic_replace_json(&self.base, "runtime-owner.json", &owner)?;
            }
            owner
        };
        if reset_trash.exists() {
            fs::remove_dir_all(&reset_trash).map_err(io_error)?;
            if let Some(parent) = reset_trash.parent() {
                sync_directory(parent)?;
            }
        }
        fs::remove_file(&intent_path).map_err(io_error)?;
        sync_directory(&self.base)?;
        Ok(owner)
    }

    /// Classifies an initialized canonical base for S-UI-017 without a lease or a Core. A base with
    /// neither owner nor state is uninitialized rather than corrupt; an unreadable file that exists
    /// is an I/O failure, never a guessed corruption.
    pub fn maintenance_blocker(&self) -> Result<MaintenanceBlocker, DomainError> {
        let owner_path = self.base.join("runtime-owner.json");
        let state_path = self.base.join("runtime-state.json");
        if !owner_path.exists() && !state_path.exists() {
            return Ok(MaintenanceBlocker::None);
        }
        let owner_valid = match fs::read(&owner_path) {
            Ok(bytes) => serde_json::from_slice::<RuntimeOwner>(&bytes)
                .is_ok_and(|owner| owner.schema_version == 1 && owner.host_generation != 0),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(io_error(error)),
        };
        if !owner_valid {
            return Ok(MaintenanceBlocker::OwnerCorrupt);
        }
        let _state_lock = FileLock::acquire(&self.base.join("runtime-state.lock"))?;
        match fs::read(&state_path) {
            Ok(bytes) if decode_canonical_state(&bytes).is_ok() => Ok(MaintenanceBlocker::None),
            Ok(_) => Ok(MaintenanceBlocker::StoreCorrupt),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(MaintenanceBlocker::StoreCorrupt)
            }
            Err(error) => Err(io_error(error)),
        }
    }

    /// Records the S-UPD-006 reset intent for a store whose business JSON cannot be loaded. Owner,
    /// live lock and guard evidence alone establish the reset, and `recover_confirmed_reset` then
    /// completes it; a live Runtime instance or any pending intent refuses the reset.
    pub fn record_corrupt_store_reset_intent(
        &self,
        intent: &RuntimeResetIntent,
        cleanup_verified: bool,
    ) -> Result<(), DomainError> {
        if !cleanup_verified {
            return Err(DomainError::new(
                ErrorCode::IoError,
                "reset cannot bypass unverified cleanup",
            ));
        }
        let _lifetime_lock = FileLock::try_acquire(&self.base.join("runtime-live.lock"))?
            .ok_or_else(|| {
                DomainError::new(
                    ErrorCode::HostTransitionPending,
                    "a Runtime instance holds the live lock",
                )
            })?;
        self.refuse_pending_intents()?;
        if self.maintenance_blocker()? != MaintenanceBlocker::StoreCorrupt {
            return Err(DomainError::new(
                ErrorCode::StaleAuthority,
                "the canonical store is not corrupt",
            ));
        }
        let owner = self.read_owner()?;
        if intent.runtime_epoch != owner.runtime_epoch
            || intent.source_host_generation != owner.host_generation
            || validate_reset_owner(intent, &owner)? != crate::ResetOwnerState::Source
        {
            return Err(DomainError::new(
                ErrorCode::StaleAuthority,
                "reset intent does not start from the current owner",
            ));
        }
        write_new_json(&self.base.join("runtime-reset-intent.json"), intent)?;
        sync_directory(&self.base)
    }

    /// The S-AUTH-001 malformed-owner reset (S-UI-017): under the live lock it replaces only the
    /// owner with a fresh epoch at APK generation 1 and leaves the canonical store for ordinary
    /// interruption reconciliation.
    pub fn reset_malformed_owner(
        &self,
        runtime_epoch: contract::UuidV4,
        cleanup_verified: bool,
    ) -> Result<RuntimeOwner, DomainError> {
        if !cleanup_verified {
            return Err(DomainError::new(
                ErrorCode::IoError,
                "reset cannot bypass unverified cleanup",
            ));
        }
        let _lifetime_lock = FileLock::try_acquire(&self.base.join("runtime-live.lock"))?
            .ok_or_else(|| {
                DomainError::new(
                    ErrorCode::HostTransitionPending,
                    "a Runtime instance holds the live lock",
                )
            })?;
        self.refuse_pending_intents()?;
        if self.maintenance_blocker()? != MaintenanceBlocker::OwnerCorrupt {
            return Err(DomainError::new(
                ErrorCode::StaleAuthority,
                "the runtime owner is not corrupt",
            ));
        }
        let owner = RuntimeOwner {
            schema_version: 1,
            runtime_epoch,
            host: RuntimeHost::ApkRuntime,
            host_generation: 1,
        };
        atomic_replace_json(&self.base, "runtime-owner.json", &owner)?;
        Ok(owner)
    }

    fn refuse_pending_intents(&self) -> Result<(), DomainError> {
        if self.base.join("runtime-transition.json").exists()
            || self.base.join("runtime-reset-intent.json").exists()
        {
            return Err(DomainError::new(
                ErrorCode::StaleAuthority,
                "a pending Runtime intent must be recovered first",
            ));
        }
        Ok(())
    }

    pub fn compare_and_commit<F>(
        &self,
        lease: &LifetimeLease,
        expected_revision: u64,
        mutate: F,
    ) -> Result<CommitInstrumentation, DomainError>
    where
        F: FnOnce(&mut CanonicalState) -> Result<(), DomainError>,
    {
        self.compare_and_commit_with_crash(lease, expected_revision, mutate, |_| false)
    }

    pub fn compare_and_commit_with_crash<F, C>(
        &self,
        lease: &LifetimeLease,
        expected_revision: u64,
        mutate: F,
        crash: C,
    ) -> Result<CommitInstrumentation, DomainError>
    where
        F: FnOnce(&mut CanonicalState) -> Result<(), DomainError>,
        C: Fn(CrashPoint) -> bool,
    {
        let total_started = Instant::now();
        let lock_started = Instant::now();
        let state_lock = FileLock::acquire(&self.base.join("runtime-state.lock"))?;
        let lock_wait_ns = lock_started.elapsed().as_nanos();
        self.validate_lease(lease)?;

        let parse_started = Instant::now();
        let state_path = self.base.join("runtime-state.json");
        let state_bytes = fs::read(&state_path).map_err(io_error)?;
        let mut state = decode_canonical_state(&state_bytes)?;
        let parse_ns = parse_started.elapsed().as_nanos();
        if state.store_revision != expected_revision {
            return Err(DomainError::new(
                ErrorCode::RevisionConflict,
                "store revision does not match",
            ));
        }

        let mutation_started = Instant::now();
        mutate(&mut state)?;
        if state.store_revision != expected_revision {
            return Err(DomainError::new(
                ErrorCode::RevisionConflict,
                "domain mutation changed the store revision",
            ));
        }
        state.store_revision = expected_revision.checked_add(1).ok_or_else(|| {
            DomainError::new(ErrorCode::ResourceLimit, "store revision exhausted")
        })?;
        let domain_mutation_ns = mutation_started.elapsed().as_nanos();

        let serialize_started = Instant::now();
        let bytes = serde_json::to_vec(&state).map_err(|_| {
            DomainError::new(ErrorCode::InternalError, "store serialization failed")
        })?;
        validate_state(&state, bytes.len())?;
        let unused_reservations = state.reservations.iter().try_fold(0_usize, |sum, item| {
            usize::try_from(item.reserved_bytes)
                .ok()
                .and_then(|value| sum.checked_add(value))
        });
        if bytes.len() > STORE_LIMIT_BYTES
            || unused_reservations
                .and_then(|reserved| bytes.len().checked_add(reserved))
                .is_none_or(|total| total > STORE_LIMIT_BYTES)
        {
            return Err(DomainError::new(
                ErrorCode::ResourceLimit,
                "store plus reservations exceeds hard limit",
            ));
        }
        let serialize_ns = serialize_started.elapsed().as_nanos();
        if crash(CrashPoint::BeforeTempCreate) {
            return Err(injected_crash());
        }

        let write_started = Instant::now();
        let temporary = self
            .base
            .join(format!(".runtime-state.{}.tmp", state.store_revision));
        let mut file = open_new_private(&temporary)?;
        file.write_all(&bytes).map_err(io_error)?;
        preserve_replacement_metadata(&self.base.join("runtime-state.json"), &file)?;
        file.sync_all().map_err(io_error)?;
        let temp_write_fsync_ns = write_started.elapsed().as_nanos();
        if crash(CrashPoint::AfterTempFsync) {
            return Err(injected_crash());
        }

        let rename_started = Instant::now();
        replace_file(&temporary, &self.base.join("runtime-state.json"))?;
        if crash(CrashPoint::AfterRename) {
            return Err(injected_crash());
        }
        sync_directory(&self.base)?;
        state_lock.sync()?;
        let rename_directory_fsync_ns = rename_started.elapsed().as_nanos();
        Ok(CommitInstrumentation {
            lock_wait_ns,
            parse_ns,
            domain_mutation_ns,
            serialize_ns,
            temp_write_fsync_ns,
            rename_directory_fsync_ns,
            total_ns: total_started.elapsed().as_nanos(),
        })
    }

    pub fn recover_temps(&self, lease: &LifetimeLease) -> Result<usize, DomainError> {
        let _lock = FileLock::acquire(&self.base.join("runtime-state.lock"))?;
        self.validate_lease(lease)?;
        let mut removed = 0;
        for entry in fs::read_dir(&self.base).map_err(io_error)? {
            let entry = entry.map_err(io_error)?;
            let name = entry.file_name();
            if name.to_string_lossy().starts_with(".runtime-state.")
                && name.to_string_lossy().ends_with(".tmp")
            {
                fs::remove_file(entry.path()).map_err(io_error)?;
                removed += 1;
            }
        }
        sync_directory(&self.base)?;
        Ok(removed)
    }

    pub fn cleanup_task_temporary(
        &self,
        lease: &LifetimeLease,
        task_id: &contract::TaskId,
        recovery: &crate::GuardRecovery,
    ) -> Result<bool, DomainError> {
        self.validate_lease(lease)?;
        if !matches!(recovery, crate::GuardRecovery::Clean { .. }) {
            return Err(DomainError::new(
                ErrorCode::IoError,
                "Task temporary data requires clean guard proof",
            ));
        }
        let temporary = self.base.join("tmp").join(task_id.as_str());
        if !temporary.exists() {
            return Ok(false);
        }
        fs::remove_dir_all(&temporary).map_err(io_error)?;
        sync_directory(&self.base.join("tmp"))?;
        Ok(true)
    }

    pub fn validate_lease(&self, lease: &LifetimeLease) -> Result<(), DomainError> {
        if lease.base != self.base {
            return Err(DomainError::new(
                ErrorCode::StaleAuthority,
                "lifetime lease belongs to another store",
            ));
        }
        let owner: RuntimeOwner = read_json(&self.base.join("runtime-owner.json"))?;
        let live: RuntimeLive = read_json(&self.base.join("runtime-live.json"))?;
        let fence = WriterFence {
            runtime_epoch: lease.live.runtime_epoch.clone(),
            host: lease.live.host,
            host_generation: lease.live.host_generation,
            runtime_instance_id: lease.live.runtime_instance_id.clone(),
        };
        if live != lease.live || !fence.matches(&owner, &live) {
            return Err(DomainError::new(
                ErrorCode::StaleAuthority,
                "lifetime lease is stale",
            ));
        }
        Ok(())
    }
}

pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, DomainError> {
    let bytes = fs::read(path).map_err(io_error)?;
    serde_json::from_slice(&bytes)
        .map_err(|_| DomainError::new(ErrorCode::IoError, "canonical JSON is corrupt"))
}

pub fn decode_canonical_state(bytes: &[u8]) -> Result<CanonicalState, DomainError> {
    if bytes.len() > STORE_LIMIT_BYTES {
        return Err(DomainError::new(
            ErrorCode::ResourceLimit,
            "store is too large",
        ));
    }
    let state: CanonicalState = serde_json::from_slice(bytes)
        .map_err(|_| DomainError::new(ErrorCode::IoError, "canonical JSON is corrupt"))?;
    validate_state(&state, bytes.len())?;
    Ok(state)
}

pub(crate) fn atomic_replace_json<T: Serialize>(
    base: &Path,
    name: &str,
    value: &T,
) -> Result<(), DomainError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|_| DomainError::new(ErrorCode::InternalError, "JSON serialization failed"))?;
    let temporary = base.join(format!(".{name}.tmp"));
    if temporary.exists() {
        fs::remove_file(&temporary).map_err(io_error)?;
        sync_directory(base)?;
    }
    let mut file = open_new_private(&temporary)?;
    file.write_all(&bytes).map_err(io_error)?;
    preserve_replacement_metadata(&base.join(name), &file)?;
    file.sync_all().map_err(io_error)?;
    drop(file);
    replace_file(&temporary, &base.join(name))?;
    sync_directory(base)
}

fn write_new_json<T: Serialize>(path: &Path, value: &T) -> Result<(), DomainError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|_| DomainError::new(ErrorCode::InternalError, "JSON serialization failed"))?;
    let mut file = open_new_private(path)?;
    file.write_all(&bytes).map_err(io_error)?;
    file.sync_all().map_err(io_error)
}

fn remove_file_synced(base: &Path, name: &str) -> Result<(), DomainError> {
    fs::remove_file(base.join(name)).map_err(io_error)?;
    sync_directory(base)
}

fn open_new_private(path: &Path) -> Result<fs::File, DomainError> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).map_err(io_error)
}

fn open_lock_file(path: &Path) -> Result<(), DomainError> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .and_then(|file| file.sync_all())
        .map_err(io_error)
}

fn preserve_replacement_metadata(source: &Path, replacement: &fs::File) -> Result<(), DomainError> {
    if !source.exists() {
        #[cfg(target_os = "android")]
        match android_metadata_strategy(unsafe { libc::geteuid() }) {
            AndroidMetadataStrategy::PlatformAssigned => {
                verify_android_selinux_label(None, replacement)?;
            }
            AndroidMetadataStrategy::CopyCanonical => {
                return Err(DomainError::new(
                    ErrorCode::IoError,
                    "Magisk writer cannot initialize canonical metadata",
                ));
            }
        }
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::MetadataExt;

        let metadata = fs::metadata(source).map_err(io_error)?;
        if metadata.mode() & 0o777 != 0o600 {
            return Err(DomainError::new(
                ErrorCode::IoError,
                "canonical file metadata is invalid",
            ));
        }
        let replacement_metadata = replacement.metadata().map_err(io_error)?;
        if replacement_metadata.uid() != metadata.uid()
            || replacement_metadata.gid() != metadata.gid()
        {
            let result =
                unsafe { libc::fchown(replacement.as_raw_fd(), metadata.uid(), metadata.gid()) };
            if result != 0 {
                return Err(io_error(std::io::Error::last_os_error()));
            }
        }
        let result = unsafe { libc::fchmod(replacement.as_raw_fd(), 0o600) };
        if result != 0 {
            return Err(io_error(std::io::Error::last_os_error()));
        }
        preserve_selinux_label(source, replacement)?;
        let replacement_metadata = replacement.metadata().map_err(io_error)?;
        if replacement_metadata.uid() != metadata.uid()
            || replacement_metadata.gid() != metadata.gid()
            || replacement_metadata.mode() & 0o777 != 0o600
        {
            return Err(DomainError::new(
                ErrorCode::IoError,
                "replacement metadata verification failed",
            ));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = replacement;
    }
    Ok(())
}

/// Creates each missing directory from the canonical `base` down to `directory`. A Magisk
/// writer gives every directory on that path the App owner, mode 0700 and the canonical
/// directory label (S-PERSIST-004), including one an earlier root write left with other
/// metadata, so the APK Runtime can keep writing the same store after a host transition.
pub(crate) fn create_canonical_directory(base: &Path, directory: &Path) -> Result<(), DomainError> {
    let relative = directory.strip_prefix(base).map_err(|_| {
        DomainError::new(
            ErrorCode::InternalError,
            "store directory is outside the canonical base",
        )
    })?;
    let mut current = base.to_path_buf();
    for component in relative.components() {
        current.push(component);
        match fs::create_dir(&current) {
            Ok(()) => sync_directory(current.parent().expect("created directory has a parent"))?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(io_error(error)),
        }
        preserve_magisk_directory_metadata(base, &current)?;
    }
    Ok(())
}

/// Gives a file a Magisk writer created inside the store the canonical file metadata before it
/// is renamed into place (S-PERSIST-004). An App writer's file already has the App owner and
/// the platform-assigned label.
pub(crate) fn preserve_magisk_file_metadata(
    canonical: &Path,
    file: &fs::File,
) -> Result<(), DomainError> {
    #[cfg(unix)]
    if unsafe { libc::geteuid() } == 0 {
        return preserve_replacement_metadata(canonical, file);
    }
    let _ = (canonical, file);
    Ok(())
}

#[cfg(unix)]
fn preserve_magisk_directory_metadata(base: &Path, directory: &Path) -> Result<(), DomainError> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;

    if unsafe { libc::geteuid() } != 0 {
        return Ok(());
    }
    let canonical = fs::metadata(base).map_err(io_error)?;
    let handle = fs::File::open(directory).map_err(io_error)?;
    let current = handle.metadata().map_err(io_error)?;
    if !current.is_dir() {
        return Err(DomainError::new(
            ErrorCode::IoError,
            "store path is not a directory",
        ));
    }
    if (current.uid() != canonical.uid() || current.gid() != canonical.gid())
        && unsafe { libc::fchown(handle.as_raw_fd(), canonical.uid(), canonical.gid()) } != 0
    {
        return Err(io_error(std::io::Error::last_os_error()));
    }
    if current.mode() & 0o777 != 0o700 && unsafe { libc::fchmod(handle.as_raw_fd(), 0o700) } != 0 {
        return Err(io_error(std::io::Error::last_os_error()));
    }
    preserve_selinux_label(base, &handle)?;
    let verified = handle.metadata().map_err(io_error)?;
    if verified.uid() != canonical.uid()
        || verified.gid() != canonical.gid()
        || verified.mode() & 0o777 != 0o700
    {
        return Err(DomainError::new(
            ErrorCode::IoError,
            "store directory metadata verification failed",
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn preserve_magisk_directory_metadata(_: &Path, _: &Path) -> Result<(), DomainError> {
    Ok(())
}

#[cfg(target_os = "android")]
fn preserve_selinux_label(source: &Path, replacement: &fs::File) -> Result<(), DomainError> {
    match android_metadata_strategy(unsafe { libc::geteuid() }) {
        AndroidMetadataStrategy::PlatformAssigned => {
            verify_android_selinux_label(Some(source), replacement)
        }
        AndroidMetadataStrategy::CopyCanonical => copy_android_selinux_label(source, replacement),
    }
}

#[cfg(any(target_os = "android", test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AndroidMetadataStrategy {
    PlatformAssigned,
    CopyCanonical,
}

#[cfg(any(target_os = "android", test))]
const fn android_metadata_strategy(effective_uid: u32) -> AndroidMetadataStrategy {
    if effective_uid == 0 {
        AndroidMetadataStrategy::CopyCanonical
    } else {
        AndroidMetadataStrategy::PlatformAssigned
    }
}

#[cfg(target_os = "android")]
fn copy_android_selinux_label(source: &Path, replacement: &fs::File) -> Result<(), DomainError> {
    use std::ffi::CString;
    use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};

    let source_path = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| DomainError::new(ErrorCode::IoError, "canonical path is invalid"))?;
    let name = c"security.selinux";
    let source_label = read_required_xattr(|value, length| unsafe {
        libc::getxattr(source_path.as_ptr(), name.as_ptr(), value, length)
    })?;
    let result = unsafe {
        libc::fsetxattr(
            replacement.as_raw_fd(),
            name.as_ptr(),
            source_label.as_ptr().cast(),
            source_label.len(),
            0,
        )
    };
    if result != 0 {
        return Err(io_error(std::io::Error::last_os_error()));
    }
    verify_android_selinux_label(Some(source), replacement)
}

#[cfg(target_os = "android")]
fn verify_android_selinux_label(
    source: Option<&Path>,
    replacement: &fs::File,
) -> Result<(), DomainError> {
    use std::ffi::CString;
    use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};

    let name = c"security.selinux";
    let replacement_label = read_required_xattr(|value, length| unsafe {
        libc::fgetxattr(replacement.as_raw_fd(), name.as_ptr(), value, length)
    })?;
    let source_label = source
        .map(|path| {
            let path = CString::new(path.as_os_str().as_bytes())
                .map_err(|_| DomainError::new(ErrorCode::IoError, "canonical path is invalid"))?;
            read_required_xattr(|value, length| unsafe {
                libc::getxattr(path.as_ptr(), name.as_ptr(), value, length)
            })
        })
        .transpose()?;
    validate_platform_selinux_labels(source_label.as_deref(), &replacement_label)
}

#[cfg(target_os = "android")]
fn read_required_xattr(
    read: impl Fn(*mut libc::c_void, usize) -> isize,
) -> Result<Vec<u8>, DomainError> {
    let length = read(std::ptr::null_mut(), 0);
    if length <= 0 {
        return Err(if length < 0 {
            io_error(std::io::Error::last_os_error())
        } else {
            DomainError::new(ErrorCode::IoError, "SELinux metadata is empty")
        });
    }
    let mut value = vec![0_u8; length as usize];
    let actual = read(value.as_mut_ptr().cast(), value.len());
    if actual != length {
        return Err(DomainError::new(
            ErrorCode::IoError,
            "SELinux metadata changed while being verified",
        ));
    }
    Ok(value)
}

#[cfg(any(target_os = "android", test))]
fn validate_platform_selinux_labels(
    source: Option<&[u8]>,
    replacement: &[u8],
) -> Result<(), DomainError> {
    if replacement.is_empty() || source.is_some_and(|value| value != replacement) {
        return Err(DomainError::new(
            ErrorCode::IoError,
            "replacement SELinux metadata verification failed",
        ));
    }
    Ok(())
}

#[cfg(all(target_os = "linux", not(target_os = "android")))]
fn preserve_selinux_label(source: &Path, replacement: &fs::File) -> Result<(), DomainError> {
    use std::ffi::CString;
    use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};

    let source = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| DomainError::new(ErrorCode::IoError, "canonical path is invalid"))?;
    let name = c"security.selinux";
    let length = unsafe { libc::getxattr(source.as_ptr(), name.as_ptr(), std::ptr::null_mut(), 0) };
    if length < 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENODATA)
            || error.raw_os_error() == Some(libc::ENOTSUP)
        {
            return Ok(());
        }
        return Err(io_error(error));
    }
    let mut value = vec![0_u8; length as usize];
    let read = unsafe {
        libc::getxattr(
            source.as_ptr(),
            name.as_ptr(),
            value.as_mut_ptr().cast(),
            value.len(),
        )
    };
    if read != length {
        return Err(io_error(std::io::Error::last_os_error()));
    }
    let result = unsafe {
        libc::fsetxattr(
            replacement.as_raw_fd(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        )
    };
    if result != 0 {
        return Err(io_error(std::io::Error::last_os_error()));
    }
    let mut verified = vec![0_u8; value.len()];
    let verified_length = unsafe {
        libc::fgetxattr(
            replacement.as_raw_fd(),
            name.as_ptr(),
            verified.as_mut_ptr().cast(),
            verified.len(),
        )
    };
    if verified_length != length || verified != value {
        return Err(DomainError::new(
            ErrorCode::IoError,
            "replacement SELinux metadata verification failed",
        ));
    }
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "android", target_os = "linux"))))]
fn preserve_selinux_label(_: &Path, _: &fs::File) -> Result<(), DomainError> {
    Ok(())
}

#[cfg(windows)]
fn replace_file(source: &Path, target: &Path) -> Result<(), DomainError> {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }
    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let target = target
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            target.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        return Err(io_error(std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace_file(source: &Path, target: &Path) -> Result<(), DomainError> {
    fs::rename(source, target).map_err(io_error)
}

fn exclusive_error(error: std::io::Error) -> DomainError {
    if error.kind() == std::io::ErrorKind::AlreadyExists {
        DomainError::new(
            ErrorCode::AlreadyExists,
            "exclusive commit destination already exists",
        )
    } else {
        io_error(error)
    }
}

#[cfg(any(target_os = "android", target_os = "linux"))]
pub(crate) fn replace_file_exclusive(source: &Path, target: &Path) -> Result<(), DomainError> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let invalid = || DomainError::new(ErrorCode::IoError, "commit path is invalid");
    let source = CString::new(source.as_os_str().as_bytes()).map_err(|_| invalid())?;
    let target = CString::new(target.as_os_str().as_bytes()).map_err(|_| invalid())?;
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            target.as_ptr(),
            libc::RENAME_NOREPLACE as libc::c_uint,
        )
    };
    if result != 0 {
        return Err(exclusive_error(std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(not(any(target_os = "android", target_os = "linux")))]
pub(crate) fn replace_file_exclusive(source: &Path, target: &Path) -> Result<(), DomainError> {
    fs::hard_link(source, target).map_err(exclusive_error)?;
    fs::remove_file(source).map_err(io_error)
}

fn validate_state(state: &CanonicalState, encoded_bytes: usize) -> Result<(), DomainError> {
    if state.schema_version != 1 {
        return Err(DomainError::new(
            ErrorCode::ProtocolIncompatible,
            "unsupported store schema",
        ));
    }
    if state.request_records.len() > 4096
        || state.automations.len() > 256
        || state.artifact_manifest.len() > crate::MAX_ARTIFACT_RECORDS
    {
        return Err(DomainError::new(
            ErrorCode::ResourceLimit,
            "store collection exceeds its record limit",
        ));
    }
    let mut identities = HashSet::new();
    if state
        .request_records
        .iter()
        .any(|record| !identities.insert(record.request_id.as_str().to_owned()))
    {
        return Err(DomainError::new(
            ErrorCode::IoError,
            "duplicate request identity",
        ));
    }
    identities.clear();
    for record in &state.automations {
        domain::validate_automation(&record.automation)
            .map_err(|_| DomainError::new(ErrorCode::IoError, "canonical Automation is invalid"))?;
    }
    if state
        .tasks
        .iter()
        .any(|task| !identities.insert(task.task_id.as_str().to_owned()))
    {
        return Err(DomainError::new(
            ErrorCode::IoError,
            "duplicate Task identity",
        ));
    }
    identities.clear();
    if state
        .tasks
        .iter()
        .any(|task| !identities.insert(task.execution_id.as_str().to_owned()))
    {
        return Err(DomainError::new(
            ErrorCode::IoError,
            "duplicate Task execution identity",
        ));
    }
    let running = state
        .tasks
        .iter()
        .filter(|task| task.state == contract::TaskState::Running)
        .count();
    let queued = state
        .tasks
        .iter()
        .filter(|task| {
            matches!(
                task.state,
                contract::TaskState::Created | contract::TaskState::Queued
            )
        })
        .count();
    let terminal = state.tasks.len().saturating_sub(running + queued);
    if running > 64 || queued > 256 || terminal > 500 {
        return Err(DomainError::new(
            ErrorCode::ResourceLimit,
            "Task registry exceeds its retention limit",
        ));
    }
    identities.clear();
    if state
        .automations
        .iter()
        .any(|record| !identities.insert(record.automation.automation_id.as_str().to_owned()))
    {
        return Err(DomainError::new(
            ErrorCode::IoError,
            "duplicate Automation identity",
        ));
    }
    identities.clear();
    let mut per_automation = HashMap::<String, usize>::new();
    let mut terminal_automation_executions = 0_usize;
    let mut non_terminal_automation_executions = 0_usize;
    for execution in &state.automation_executions {
        if !identities.insert(execution.summary.execution_id.as_str().to_owned()) {
            return Err(DomainError::new(
                ErrorCode::IoError,
                "duplicate AutomationExecution identity",
            ));
        }
        if matches!(
            execution.summary.state,
            contract::AutomationExecutionState::Queued
                | contract::AutomationExecutionState::Running
        ) {
            non_terminal_automation_executions += 1;
        } else {
            terminal_automation_executions += 1;
            *per_automation
                .entry(execution.automation_id.as_str().to_owned())
                .or_default() += 1;
        }
    }
    if terminal_automation_executions > 2000
        || non_terminal_automation_executions > 320
        || per_automation.values().any(|count| *count > 100)
    {
        return Err(DomainError::new(
            ErrorCode::ResourceLimit,
            "AutomationExecution history exceeds its retention limit",
        ));
    }
    identities.clear();
    let mut artifact_bytes = 0_u64;
    for artifact in &state.artifact_manifest {
        validate_artifact_record(artifact)?;
        if !identities.insert(artifact.artifact_ref.clone()) {
            return Err(DomainError::new(
                ErrorCode::IoError,
                "duplicate artifact reference",
            ));
        }
        artifact_bytes = artifact_bytes.checked_add(artifact.size).ok_or_else(|| {
            DomainError::new(
                ErrorCode::ResourceLimit,
                "artifact byte accounting overflow",
            )
        })?;
    }
    if artifact_bytes > crate::MAX_ARTIFACT_TOTAL_BYTES {
        return Err(DomainError::new(
            ErrorCode::ResourceLimit,
            "artifact manifest exceeds its byte limit",
        ));
    }
    identities.clear();
    let reserved_bytes = state
        .reservations
        .iter()
        .try_fold(0_usize, |sum, reservation| {
            if reservation.reserved_bytes < runtime::RESERVE_FLOOR_BYTES
                || !identities.insert(reservation.execution_id.as_str().to_owned())
            {
                return None;
            }
            usize::try_from(reservation.reserved_bytes)
                .ok()
                .and_then(|value| sum.checked_add(value))
        });
    if reserved_bytes
        .and_then(|reserved| encoded_bytes.checked_add(reserved))
        .is_none_or(|total| total > STORE_LIMIT_BYTES)
    {
        return Err(DomainError::new(
            ErrorCode::ResourceLimit,
            "store plus reservations exceeds hard limit",
        ));
    }
    runtime::RuntimeState::try_from(state.clone()).map_err(|_| {
        DomainError::new(
            ErrorCode::IoError,
            "canonical Task or request state is invalid",
        )
    })?;
    Ok(())
}

fn injected_crash() -> DomainError {
    DomainError::new(ErrorCode::IoError, "injected commit crash")
}

/// Executions left `running` by a Runtime instance that is gone: a synchronous execution still
/// running, or a Task that is neither terminal nor an AutomationExecution container. A daemon that
/// finds one without a guard proof refuses to recover ("prior execution cleanup is unverified")
/// and exits, which no reboot changes; clearing them is the user's recovery until the settlement
/// path stops leaving them behind.
fn stranded(state: &CanonicalState) -> (Vec<usize>, Vec<usize>) {
    let requests = state
        .request_records
        .iter()
        .enumerate()
        .filter(|(_, request)| {
            request
                .synchronous_execution
                .as_ref()
                .is_some_and(|execution| !execution.state.is_terminal())
        })
        .map(|(index, _)| index)
        .collect();
    let tasks = state
        .tasks
        .iter()
        .enumerate()
        .filter(|(_, task)| {
            task.automation_owner.is_none()
                && matches!(
                    task.state,
                    contract::TaskState::Created
                        | contract::TaskState::Queued
                        | contract::TaskState::Running
                )
        })
        .map(|(index, _)| index)
        .collect();
    (requests, tasks)
}

/// How many stranded executions the store holds. While no Runtime holds the live lock every
/// unsettled execution is stranded; while one does (a daemon that keeps restarting and refusing to
/// recover holds it for most of each attempt), only those of an instance other than the live one.
pub fn stranded_execution_count(base: &Path) -> Result<usize, DomainError> {
    let path = base.join("runtime-state.json");
    if !path.exists() {
        return Ok(0);
    }
    let live_instance = match FileLock::try_acquire(&base.join("runtime-live.lock"))? {
        Some(_free) => None,
        None => {
            Some(read_json::<RuntimeLive>(&base.join("runtime-live.json"))?.runtime_instance_id)
        }
    };
    let _state_lock = FileLock::acquire(&base.join("runtime-state.lock"))?;
    let state = decode_canonical_state(&fs::read(path).map_err(io_error)?)?;
    let (requests, tasks) = stranded(&state);
    let foreign = |instance: Option<&contract::UuidV4>| match (&live_instance, instance) {
        (None, _) => true,
        (Some(live), Some(instance)) => live != instance,
        (Some(_), None) => false,
    };
    let requests = requests
        .into_iter()
        .filter(|index| {
            foreign(
                state.request_records[*index]
                    .synchronous_execution
                    .as_ref()
                    .map(|execution| &execution.executor.fence.runtime_instance_id),
            )
        })
        .count();
    let tasks = tasks
        .into_iter()
        .filter(|index| {
            foreign(
                state.tasks[*index]
                    .executor
                    .as_ref()
                    .map(|executor| &executor.fence.runtime_instance_id),
            )
        })
        .count();
    Ok(requests + tasks)
}

/// Marks every stranded execution interrupted the way `recover_old_instance` settles a lost
/// instance, and releases its reservation. Refused while a live Runtime owns the store; the live
/// lock is held for the whole rewrite so no host starts on the half-written state.
/// A daemon that keeps restarting holds the live lock for most of each attempt, so the lock is
/// retried until `wait` has passed before the clear is refused.
pub fn clear_stranded_executions(
    base: &Path,
    ended_at: &str,
    now_ms: u64,
    wait: std::time::Duration,
) -> Result<usize, DomainError> {
    let deadline = Instant::now() + wait;
    let _live = loop {
        if let Some(lock) = FileLock::try_acquire(&base.join("runtime-live.lock"))? {
            break lock;
        }
        if Instant::now() >= deadline {
            return Err(DomainError::new(
                ErrorCode::HostTransitionPending,
                "a live Runtime owns the store",
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    let _state_lock = FileLock::acquire(&base.join("runtime-state.lock"))?;
    let path = base.join("runtime-state.json");
    let mut state = decode_canonical_state(&fs::read(&path).map_err(io_error)?)?;
    let (requests, tasks) = stranded(&state);
    if requests.is_empty() && tasks.is_empty() {
        return Ok(0);
    }
    let expires_at_ms = now_ms.saturating_add(86_400_000);
    let mut released = HashSet::new();
    let interrupted_error = |operation: String| contract::PublicError {
        code: ErrorCode::IoError,
        operation,
        retryable: false,
        message: None,
        capability: None,
        details: None,
    };
    for index in &requests {
        let request = &mut state.request_records[*index];
        if let Some(execution) = request.synchronous_execution.as_mut() {
            execution.state = runtime::SynchronousExecutionState::Interrupted;
            execution.ended_at = Some(ended_at.to_owned());
            execution.error = Some(interrupted_error(execution.operation.clone()));
            execution.terminal_bytes = runtime::RESERVE_FLOOR_BYTES.min(execution.reserved_bytes);
            released.insert(execution.execution_id.as_str().to_owned());
        }
        request.expires_at_ms = Some(expires_at_ms);
    }
    for index in &tasks {
        let task = &mut state.tasks[*index];
        let tool = serde_json::to_value(task.tool)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_default();
        task.state = contract::TaskState::Interrupted;
        task.ended_at = Some(ended_at.to_owned());
        task.error = Some(interrupted_error(format!("{tool}.{}", task.action)));
        released.insert(task.execution_id.as_str().to_owned());
        let request_id = task.request_id.clone();
        if let Some(request_id) = request_id
            && let Some(request) = state
                .request_records
                .iter_mut()
                .find(|request| request.request_id == request_id)
        {
            request.expires_at_ms = Some(expires_at_ms);
        }
    }
    state
        .reservations
        .retain(|reservation| !released.contains(reservation.execution_id.as_str()));
    state.store_revision = state
        .store_revision
        .checked_add(1)
        .ok_or_else(|| DomainError::new(ErrorCode::ResourceLimit, "store revision exhausted"))?;
    // The rewritten state must still be a state the Runtime accepts.
    let encoded = serde_json::to_vec(&state)
        .map_err(|_| DomainError::new(ErrorCode::InternalError, "JSON serialization failed"))?;
    decode_canonical_state(&encoded)?;
    atomic_replace_json(base, "runtime-state.json", &state)?;
    Ok(requests.len() + tasks.len())
}

#[cfg(test)]
mod tests {
    use super::{
        AndroidMetadataStrategy, android_metadata_strategy, validate_platform_selinux_labels,
    };

    #[test]
    fn platform_assigned_selinux_label_requires_nonempty_exact_match() {
        let label = b"u:object_r:app_data_file:s0:c1,c2\0";
        assert!(validate_platform_selinux_labels(None, label).is_ok());
        assert!(validate_platform_selinux_labels(Some(label), label).is_ok());
        assert!(validate_platform_selinux_labels(None, b"").is_err());
        assert!(
            validate_platform_selinux_labels(
                Some(b"u:object_r:app_data_file:s0:c1,c2\0"),
                b"u:object_r:app_data_file:s0:c3,c4\0",
            )
            .is_err()
        );
    }

    #[test]
    fn magisk_root_writer_copies_canonical_android_metadata() {
        assert_eq!(
            android_metadata_strategy(0),
            AndroidMetadataStrategy::CopyCanonical,
        );
        assert_eq!(
            android_metadata_strategy(10_123),
            AndroidMetadataStrategy::PlatformAssigned,
        );
    }
}
