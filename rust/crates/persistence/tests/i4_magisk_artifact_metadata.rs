//! S-PERSIST-004 on the Magisk writer path: artifacts a root daemon publishes keep the
//! canonical App owner, mode and label, so the APK Runtime can keep writing the same store
//! after a host transition. The path only exists for a root writer on Android, so this test
//! runs on a device as root.
#![cfg(target_os = "android")]

use contract::{RuntimeHost, UuidV4};
use persistence::{CanonicalState, RuntimeArtifactPort, RuntimeLive, RuntimeOwner, StateStore};
use runtime::ArtifactPort;
use std::{
    ffi::CString,
    fs,
    os::unix::{ffi::OsStrExt, fs::MetadataExt, fs::PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};

const APP_UID: u32 = 10_999;

fn id(index: u64) -> UuidV4 {
    UuidV4::parse(format!("00000000-0000-4000-8000-{index:012x}")).unwrap()
}

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "droidbridge-i4-magisk-artifact-{}",
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
        host: RuntimeHost::MagiskBackend,
        host_generation: 1,
    }
}

fn live() -> RuntimeLive {
    RuntimeLive {
        runtime_epoch: id(1),
        host: RuntimeHost::MagiskBackend,
        host_generation: 1,
        runtime_instance_id: id(2),
        boot_id: id(3),
        pid: 42,
        start_ticks: 99,
    }
}

fn selinux_label(path: &Path) -> Option<Vec<u8>> {
    let path = CString::new(path.as_os_str().as_bytes()).unwrap();
    let name = c"security.selinux";
    let length = unsafe { libc::getxattr(path.as_ptr(), name.as_ptr(), std::ptr::null_mut(), 0) };
    if length <= 0 {
        return None;
    }
    let mut value = vec![0_u8; length as usize];
    let read = unsafe {
        libc::getxattr(
            path.as_ptr(),
            name.as_ptr(),
            value.as_mut_ptr().cast(),
            value.len(),
        )
    };
    assert_eq!(read, length);
    Some(value)
}

fn give_to_app(path: &Path, mode: u32) {
    let raw = CString::new(path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::chown(raw.as_ptr(), APP_UID, APP_UID) }, 0);
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

fn assert_canonical(path: &Path, mode: u32, label: Option<&[u8]>) {
    let metadata = fs::metadata(path).unwrap();
    assert_eq!(
        (metadata.uid(), metadata.gid(), metadata.mode() & 0o777),
        (APP_UID, APP_UID, mode),
        "{} keeps the canonical App owner and mode",
        path.display()
    );
    if let Some(label) = label {
        assert_eq!(
            selinux_label(path).as_deref(),
            Some(label),
            "{} keeps the canonical label",
            path.display()
        );
    }
}

#[test]
fn i4_g05_magisk_writer_artifacts_keep_the_canonical_app_metadata() {
    assert_eq!(
        unsafe { libc::geteuid() },
        0,
        "the Magisk writer path is only taken by a root writer"
    );
    let directory = TestDirectory::new();
    let base = directory.path();
    let store = Arc::new(StateStore::new(base.to_path_buf()));
    store
        .initialize(&owner(), &CanonicalState::default())
        .unwrap();
    // The APK Runtime created the live record before the daemon replaces it (S-PERSIST-004).
    fs::write(
        base.join("runtime-live.json"),
        serde_json::to_vec(&live()).unwrap(),
    )
    .unwrap();
    give_to_app(base, 0o700);
    for entry in fs::read_dir(base).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            give_to_app(&path, 0o600);
        }
    }
    let label = selinux_label(base);
    // A directory an earlier root write left with root metadata is repaired, not trusted.
    let legacy = base.join("artifacts").join("packet");
    fs::create_dir_all(&legacy).unwrap();
    fs::set_permissions(base.join("artifacts"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(&legacy, fs::Permissions::from_mode(0o755)).unwrap();

    let lease = Arc::new(store.acquire_lifetime(live()).unwrap());
    let artifacts = RuntimeArtifactPort::new(Arc::clone(&store), Arc::clone(&lease));
    let data = artifacts.publish(b"durable data").unwrap();
    let packet = artifacts.publish_as("packet", b"packet bytes").unwrap();

    assert_canonical(&base.join("artifacts"), 0o700, label.as_deref());
    for (kind, published) in [("data", &data), ("packet", &packet)] {
        let kind_directory = base.join("artifacts").join(kind);
        assert_canonical(&kind_directory, 0o700, label.as_deref());
        let identity = published.artifact_ref.rsplit(':').next().unwrap();
        assert_canonical(&kind_directory.join(identity), 0o600, None);
    }
    assert_eq!(artifacts.open(&data.artifact_ref).unwrap(), b"durable data");
    assert_eq!(
        artifacts.open(&packet.artifact_ref).unwrap(),
        b"packet bytes"
    );
}
