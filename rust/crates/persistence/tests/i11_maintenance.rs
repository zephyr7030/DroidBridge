//! I11 S-UI-017 maintenance primitives: blocker classification, both resets and guard evidence.

use contract::{ErrorCode, RuntimeHost, UuidV4};
use domain::DomainError;
use persistence::*;
use std::{
    fs,
    path::{Path, PathBuf},
};

fn id(index: u64) -> UuidV4 {
    UuidV4::parse(format!("00000000-0000-4000-8000-{index:012x}")).unwrap()
}

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let number = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "droidbridge-i11-{label}-{}-{number}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn owner() -> RuntimeOwner {
    RuntimeOwner {
        schema_version: 1,
        runtime_epoch: id(1),
        host: RuntimeHost::ApkRuntime,
        host_generation: 1,
    }
}

fn live() -> RuntimeLive {
    RuntimeLive {
        runtime_epoch: id(1),
        host: RuntimeHost::ApkRuntime,
        host_generation: 1,
        runtime_instance_id: id(2),
        boot_id: id(3),
        pid: 42,
        start_ticks: 99,
    }
}

fn initialized(label: &str) -> (TestDirectory, StateStore) {
    let directory = TestDirectory::new(label);
    let store = StateStore::new(directory.path().to_path_buf());
    store
        .initialize(&owner(), &CanonicalState::default())
        .unwrap();
    (directory, store)
}

fn reset_intent() -> RuntimeResetIntent {
    RuntimeResetIntent {
        schema_version: 1,
        reset_id: id(40),
        runtime_epoch: id(1),
        source_host_generation: 1,
        target_host: RuntimeHost::ApkRuntime,
        target_host_generation: 2,
    }
}

fn code(result: Result<impl std::fmt::Debug, DomainError>) -> ErrorCode {
    result.unwrap_err().code
}

#[test]
fn i11_maintenance_blocker_distinguishes_owner_from_store_corruption() {
    let fresh = TestDirectory::new("fresh");
    assert_eq!(
        StateStore::new(fresh.path().to_path_buf())
            .maintenance_blocker()
            .unwrap(),
        MaintenanceBlocker::None
    );

    let (directory, store) = initialized("valid");
    assert_eq!(
        store.maintenance_blocker().unwrap(),
        MaintenanceBlocker::None
    );

    fs::write(directory.path().join("runtime-state.json"), b"{not json").unwrap();
    assert_eq!(
        store.maintenance_blocker().unwrap(),
        MaintenanceBlocker::StoreCorrupt
    );
    assert_eq!(MaintenanceBlocker::StoreCorrupt.token(), "store_corrupt");

    // Owner corruption is reported first, whatever the store holds.
    fs::write(
        directory.path().join("runtime-owner.json"),
        br#"{"schema_version":1,"runtime_epoch":"00000000-0000-4000-8000-000000000001","host":"apk_runtime","host_generation":0}"#,
    )
    .unwrap();
    assert_eq!(
        store.maintenance_blocker().unwrap(),
        MaintenanceBlocker::OwnerCorrupt
    );
    fs::remove_file(directory.path().join("runtime-owner.json")).unwrap();
    assert_eq!(
        store.maintenance_blocker().unwrap(),
        MaintenanceBlocker::OwnerCorrupt
    );
    assert_eq!(MaintenanceBlocker::OwnerCorrupt.token(), "owner_corrupt");
}

#[test]
fn i11_malformed_owner_reset_replaces_only_the_owner_at_generation_one() {
    let (directory, store) = initialized("owner-reset");
    assert_eq!(
        code(store.reset_malformed_owner(id(9), true)),
        ErrorCode::StaleAuthority
    );

    fs::write(directory.path().join("runtime-owner.json"), b"garbage").unwrap();
    let state_before = fs::read(directory.path().join("runtime-state.json")).unwrap();
    assert_eq!(
        code(store.reset_malformed_owner(id(9), false)),
        ErrorCode::IoError
    );

    fs::write(directory.path().join("runtime-transition.json"), b"{}").unwrap();
    assert_eq!(
        code(store.reset_malformed_owner(id(9), true)),
        ErrorCode::StaleAuthority
    );
    fs::remove_file(directory.path().join("runtime-transition.json")).unwrap();

    let reset = store.reset_malformed_owner(id(9), true).unwrap();
    assert_eq!(
        reset,
        RuntimeOwner {
            schema_version: 1,
            runtime_epoch: id(9),
            host: RuntimeHost::ApkRuntime,
            host_generation: 1,
        }
    );
    assert_eq!(store.read_owner().unwrap(), reset);
    assert_eq!(
        fs::read(directory.path().join("runtime-state.json")).unwrap(),
        state_before
    );
    assert_eq!(
        store.maintenance_blocker().unwrap(),
        MaintenanceBlocker::None
    );
}

#[test]
fn i11_corrupt_store_reset_completes_through_the_confirmed_reset_recovery() {
    let (directory, store) = initialized("store-reset");
    assert_eq!(
        code(store.record_corrupt_store_reset_intent(&reset_intent(), true)),
        ErrorCode::StaleAuthority
    );

    fs::write(directory.path().join("runtime-state.json"), b"{not json").unwrap();
    assert_eq!(
        code(store.record_corrupt_store_reset_intent(&reset_intent(), false)),
        ErrorCode::IoError
    );

    // A live Runtime instance keeps the reset out.
    let lease = store.acquire_lifetime(live());
    if let Ok(lease) = lease {
        assert_eq!(
            code(store.record_corrupt_store_reset_intent(&reset_intent(), true)),
            ErrorCode::HostTransitionPending
        );
        drop(lease);
    }

    let mut stale = reset_intent();
    stale.source_host_generation = 7;
    stale.target_host_generation = 8;
    assert_eq!(
        code(store.record_corrupt_store_reset_intent(&stale, true)),
        ErrorCode::StaleAuthority
    );

    store
        .record_corrupt_store_reset_intent(&reset_intent(), true)
        .unwrap();
    assert!(directory.path().join("runtime-reset-intent.json").exists());
    let owner = store.recover_confirmed_reset(true).unwrap();
    assert_eq!(owner.host_generation, 2);
    assert_eq!(
        read_json::<CanonicalState>(&directory.path().join("runtime-state.json")).unwrap(),
        CanonicalState::default()
    );
    assert!(!directory.path().join("runtime-reset-intent.json").exists());
    assert_eq!(
        store.maintenance_blocker().unwrap(),
        MaintenanceBlocker::None
    );
}

struct NoLiveProcess;

impl ProcessFacts for NoLiveProcess {
    fn is_same_process(&self, _pid: u32, _start_ticks: u64) -> Result<bool, DomainError> {
        Ok(false)
    }
}

fn write_proof(base: &Path, boot: &UuidV4, execution: &UuidV4, clean: bool) {
    let directory = base.join("execution-guards").join(boot.as_str());
    fs::create_dir_all(&directory).unwrap();
    let mut bytes = encode_guard_frame(&GuardIdentity {
        runtime_epoch: id(1),
        runtime_instance_id: id(2),
        execution_id: execution.clone(),
        boot_id: boot.clone(),
    })
    .unwrap();
    bytes.extend(
        encode_guard_frame(&GuardStarted {
            pid: 4242,
            start_ticks: 7,
        })
        .unwrap(),
    );
    if clean {
        bytes.extend(
            encode_guard_frame(&GuardClean {
                shell_exit_code: Some(0),
                cause: GuardCleanCause::Exited,
            })
            .unwrap(),
        );
    }
    fs::write(
        directory.join(format!("{}.proof", execution.as_str())),
        bytes,
    )
    .unwrap();
}

#[test]
fn i11_guard_cleanup_evidence_ignores_business_json() {
    let directory = TestDirectory::new("guards");
    let current_boot = id(3);
    assert!(guard_cleanup_verified(directory.path(), &current_boot, &NoLiveProcess).unwrap());

    // A prior boot's unfinished proof cannot hold a surviving descendant.
    write_proof(directory.path(), &id(50), &id(60), false);
    write_proof(directory.path(), &current_boot, &id(61), true);
    fs::write(directory.path().join("runtime-state.json"), b"{not json").unwrap();
    assert!(guard_cleanup_verified(directory.path(), &current_boot, &NoLiveProcess).unwrap());

    // The current boot's proof without a clean frame and without its guard stays unverified.
    write_proof(directory.path(), &current_boot, &id(62), false);
    assert!(!guard_cleanup_verified(directory.path(), &current_boot, &NoLiveProcess).unwrap());
}
