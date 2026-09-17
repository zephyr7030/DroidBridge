use crate::{ApkCore, quarantine_runtime, sync_directory};
use chrono::{SecondsFormat, Utc};
use contract::ErrorCode;
use domain::DomainError;
use persistence::{
    CleanGuardRecord, GuardFinalizationDisposition, GuardRecoveryPlan, LifetimeLease,
    PendingDeadOwnerTakeover, ProcessFacts, StateStore,
};
use runtime::ApkRuntimeVertical;
use std::{fs, path::Path, sync::Arc};

pub(super) struct AppCleanupVerification {
    _private: (),
}

impl AppCleanupVerification {
    pub(super) fn complete_takeover(
        &self,
        store: &StateStore,
        pending: PendingDeadOwnerTakeover,
        process_facts: &dyn ProcessFacts,
    ) -> Result<Arc<LifetimeLease>, DomainError> {
        store.complete_dead_owner_takeover(pending, process_facts)
    }
}

pub(super) fn reconcile_app_recovery(
    base: &Path,
    store: &StateStore,
    lease: &LifetimeLease,
    runtime: &ApkRuntimeVertical,
    core: &ApkCore,
    async_runtime: &tokio::runtime::Runtime,
    recovery: &GuardRecoveryPlan,
) -> Result<AppCleanupVerification, DomainError> {
    if !recovery.guards_are_clean() {
        quarantine_runtime(runtime)?;
        return Err(DomainError::new(
            ErrorCode::IoError,
            "App Runtime recovery cleanup is unverified",
        ));
    }
    if !recovery.prior_instances().is_empty() {
        let now = Utc::now();
        let terminal_at_ms = u64::try_from(now.timestamp_millis())
            .map_err(|_| DomainError::new(ErrorCode::InternalError, "system time is invalid"))?;
        let ended_at = now.to_rfc3339_opts(SecondsFormat::Millis, true);
        for old_instance_id in recovery.prior_instances() {
            async_runtime.block_on(core.recover_old_instance(
                old_instance_id,
                ended_at.clone(),
                terminal_at_ms,
            ))?;
        }
    }
    if !finalize_clean_guard_records(base, store, lease, recovery.records()) {
        quarantine_runtime(runtime)?;
        return Err(DomainError::new(
            ErrorCode::IoError,
            "App Runtime recovery finalization failed",
        ));
    }
    Ok(AppCleanupVerification { _private: () })
}

fn finalize_clean_guard_records(
    base: &Path,
    store: &StateStore,
    lease: &LifetimeLease,
    records: &[CleanGuardRecord],
) -> bool {
    let root = base.join("execution-guards");
    let mut clean = true;
    let mut touched_directories = Vec::new();
    for record in records {
        match record.finalization {
            GuardFinalizationDisposition::Blocked => {
                clean = false;
                continue;
            }
            GuardFinalizationDisposition::CleanupTaskTemporaryAndRemoveProof => {
                let Some(task_id) = &record.task_id else {
                    clean = false;
                    continue;
                };
                if store
                    .cleanup_task_temporary(lease, task_id, &record.recovery)
                    .is_err()
                {
                    clean = false;
                    continue;
                }
            }
            GuardFinalizationDisposition::RemoveProof => {}
        }
        let Some(boot_id) = &record.containing_boot_id else {
            clean = false;
            continue;
        };
        let directory = root.join(boot_id.as_str());
        let proof = directory.join(format!("{}.proof", record.execution_id.as_str()));
        if fs::remove_file(proof).is_err() || sync_directory(&directory).is_err() {
            clean = false;
            continue;
        }
        if !touched_directories.contains(&directory) {
            touched_directories.push(directory);
        }
    }
    for directory in touched_directories {
        match fs::read_dir(&directory) {
            Ok(mut entries) => {
                if entries.next().is_none() && fs::remove_dir(&directory).is_err() {
                    clean = false;
                }
            }
            Err(_) => clean = false,
        }
    }
    if !records.is_empty() && sync_directory(&root).is_err() {
        clean = false;
    }
    clean
}

#[cfg(test)]
mod tests {
    use super::*;
    use contract::{RuntimeHost, UuidV4};
    use persistence::{
        CanonicalState, GuardCleanupRequirement, GuardRecovery, GuardSettlementDisposition,
        RuntimeLive, RuntimeOwner,
    };

    fn id(index: u64) -> UuidV4 {
        UuidV4::parse(format!("00000000-0000-4000-8000-{index:012x}")).unwrap()
    }

    fn clean_record(execution_id: UuidV4, boot_id: UuidV4) -> CleanGuardRecord {
        CleanGuardRecord {
            task_id: None,
            execution_id,
            containing_boot_id: Some(boot_id),
            runtime_instance_id: Some(id(3)),
            recovery: GuardRecovery::Clean { clean: None },
            required_cleanup: GuardCleanupRequirement::NoGuardAction,
            settlement: GuardSettlementDisposition::None,
            finalization: GuardFinalizationDisposition::RemoveProof,
        }
    }

    fn store_and_lease(base: &Path) -> (StateStore, Arc<LifetimeLease>) {
        let store = StateStore::new(base.to_path_buf());
        let owner = RuntimeOwner {
            schema_version: 1,
            runtime_epoch: id(1),
            host: RuntimeHost::ApkRuntime,
            host_generation: 1,
        };
        store
            .initialize(&owner, &CanonicalState::default())
            .unwrap();
        let lease = Arc::new(
            store
                .acquire_lifetime(RuntimeLive {
                    runtime_epoch: owner.runtime_epoch,
                    host: owner.host,
                    host_generation: owner.host_generation,
                    runtime_instance_id: id(2),
                    boot_id: id(4),
                    pid: std::process::id(),
                    start_ticks: 1,
                })
                .unwrap(),
        );
        (store, lease)
    }

    #[test]
    fn i5_g06_cleanup_verification_rejects_missing_and_duplicate_finalization() {
        let base = std::env::temp_dir().join(format!(
            "droidbridge-i5-app-cleanup-{}",
            uuid::Uuid::new_v4()
        ));
        let (store, lease) = store_and_lease(&base);
        let boot_id = id(5);
        let execution_id = id(6);
        let record = clean_record(execution_id.clone(), boot_id.clone());

        assert!(!finalize_clean_guard_records(
            &base,
            &store,
            &lease,
            std::slice::from_ref(&record),
        ));

        let directory = base.join("execution-guards").join(boot_id.as_str());
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join(format!("{}.proof", execution_id.as_str())),
            [],
        )
        .unwrap();
        assert!(!finalize_clean_guard_records(
            &base,
            &store,
            &lease,
            &[record.clone(), record],
        ));

        drop(lease);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn i5_g06_cleanup_verification_rejects_blocked_records() {
        let base = std::env::temp_dir().join(format!(
            "droidbridge-i5-app-blocked-{}",
            uuid::Uuid::new_v4()
        ));
        let (store, lease) = store_and_lease(&base);
        let mut record = clean_record(id(7), id(8));
        record.recovery = GuardRecovery::Unverified;
        record.required_cleanup = GuardCleanupRequirement::Quarantine;
        record.finalization = GuardFinalizationDisposition::Blocked;

        assert!(!finalize_clean_guard_records(
            &base,
            &store,
            &lease,
            &[record],
        ));

        drop(lease);
        fs::remove_dir_all(base).unwrap();
    }
}
