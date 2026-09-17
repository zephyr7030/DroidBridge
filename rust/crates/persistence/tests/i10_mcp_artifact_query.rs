//! I10 S-MCP-006 internal artifact query answered from the authoritative ArtifactStore manifest.

use contract::{RuntimeHost, UuidV4};
use persistence::{
    ArtifactKind, ArtifactRecord, CanonicalState, RuntimeArtifactPort, RuntimeLive, RuntimeOwner,
    StateStore,
};
use runtime::{MCP_RESOURCE_PAGE_SIZE, mcp_resource_cursor};
use serde_json::{Value, json};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

const CREATED_MS: u64 = 1_789_459_200_000;
const DAY_MS: u64 = 86_400_000;

fn id(index: u64) -> UuidV4 {
    UuidV4::parse(format!("00000000-0000-4000-8000-{index:012x}")).unwrap()
}

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let number = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "droidbridge-i10-mcp-artifact-{}-{number}",
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

fn timestamp(ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64)
        .unwrap()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn record(kind: ArtifactKind, index: u64, created_ms: u64, bytes: &[u8]) -> ArtifactRecord {
    ArtifactRecord {
        artifact_ref: format!("dbref:{}:{}", kind.token(), id(index)),
        kind,
        size: bytes.len() as u64,
        created_at: timestamp(created_ms),
        expires_at: timestamp(created_ms + DAY_MS),
        sha256: None,
        mime: (kind == ArtifactKind::Image).then(|| "image/png".to_owned()),
        task_id: None,
        request_id: None,
    }
}

fn port(directory: &TestDirectory, records: Vec<(ArtifactRecord, Vec<u8>)>) -> RuntimeArtifactPort {
    for (record, bytes) in &records {
        let mut parts = record.artifact_ref.split(':').skip(1);
        let kind = parts.next().unwrap();
        let file = directory.path().join("artifacts").join(kind);
        fs::create_dir_all(&file).unwrap();
        fs::write(file.join(parts.next().unwrap()), bytes).unwrap();
    }
    let store = Arc::new(StateStore::new(directory.path().to_path_buf()));
    let state = CanonicalState {
        artifact_manifest: records.into_iter().map(|(record, _)| record).collect(),
        ..CanonicalState::default()
    };
    store.initialize(&owner(), &state).unwrap();
    let lease = Arc::new(store.acquire_lifetime(live()).unwrap());
    RuntimeArtifactPort::new(store, lease)
}

fn query(operation: Value) -> Value {
    json!({"protocol_version": 1, "artifact_query": operation})
}

#[test]
fn i10_g07_resource_list_projects_only_live_mcp_kinds_in_canonical_order() {
    let directory = TestDirectory::new();
    let now = CREATED_MS + 10_000;
    let stdout = record(ArtifactKind::Stdout, 10, CREATED_MS + 1_000, b"out");
    let image = record(ArtifactKind::Image, 11, CREATED_MS + 2_000, b"\x89PNG");
    // Same creation instant: the whole ref string breaks the tie in descending order, so
    // `dbref:image:` precedes `dbref:data:`.
    let data_a = record(ArtifactKind::Data, 12, CREATED_MS + 2_000, b"a");
    let capture = record(ArtifactKind::Capture, 13, CREATED_MS + 3_000, b"pcap");
    let expired = record(ArtifactKind::Stderr, 14, CREATED_MS - DAY_MS, b"old");
    let port = port(
        &directory,
        [&stdout, &image, &data_a, &capture, &expired]
            .into_iter()
            .map(|record| (record.clone(), vec![1; record.size as usize]))
            .collect(),
    );

    let listed = port
        .answer_mcp_query(&query(json!({"operation": "list"})), now)
        .unwrap();
    assert!(listed.descriptor.is_none());
    assert_eq!(
        listed.payload,
        json!({"resources": [
            {"uri": image.artifact_ref, "size": 4, "mime": "image/png"},
            {"uri": data_a.artifact_ref, "size": 1},
            {"uri": stdout.artifact_ref, "size": 3},
        ]})
    );

    // Resuming strictly after an issued ref; a cursor for a ref outside the live order is invalid.
    let resumed = port
        .answer_mcp_query(
            &query(
                json!({"operation": "list", "cursor": mcp_resource_cursor(&data_a.artifact_ref)}),
            ),
            now,
        )
        .unwrap();
    assert_eq!(
        resumed.payload,
        json!({"resources": [{"uri": stdout.artifact_ref, "size": 3}]})
    );
    for cursor in [
        mcp_resource_cursor(&expired.artifact_ref),
        mcp_resource_cursor(&capture.artifact_ref),
        "not-issued".to_owned(),
    ] {
        let rejected = port
            .answer_mcp_query(&query(json!({"operation": "list", "cursor": cursor})), now)
            .unwrap();
        assert_eq!(rejected.payload, json!({"error": "invalid_cursor"}));
    }

    let metadata = port
        .answer_mcp_query(
            &query(json!({"operation": "metadata", "refs": [
                stdout.artifact_ref, expired.artifact_ref, capture.artifact_ref,
            ]})),
            now,
        )
        .unwrap();
    assert_eq!(
        metadata.payload,
        json!({"artifacts": [{"ref": stdout.artifact_ref, "expires_at": stdout.expires_at}]})
    );

    for invalid in [
        json!({"protocol_version": 2, "artifact_query": {"operation": "list"}}),
        query(json!({"operation": "list", "extra": true})),
        query(json!({"operation": "metadata", "refs": []})),
        query(json!({"operation": "metadata", "refs": [stdout.artifact_ref, stdout.artifact_ref]})),
        query(json!({"operation": "read"})),
        query(json!({"operation": "delete", "uri": stdout.artifact_ref})),
    ] {
        let error = port.answer_mcp_query(&invalid, now).unwrap_err();
        assert_eq!(
            error.code,
            contract::ErrorCode::InvalidArgument,
            "{invalid}"
        );
    }
}

#[test]
fn i10_g07_resource_list_pages_at_two_hundred_with_a_resumable_cursor() {
    let directory = TestDirectory::new();
    let records = (0..(MCP_RESOURCE_PAGE_SIZE as u64 + 1))
        .map(|index| {
            let record = record(ArtifactKind::Data, 100 + index, CREATED_MS + index, b"x");
            (record, b"x".to_vec())
        })
        .collect::<Vec<_>>();
    let oldest = records[0].0.artifact_ref.clone();
    let port = port(&directory, records);
    let now = CREATED_MS + 60_000;

    let first = port
        .answer_mcp_query(&query(json!({"operation": "list"})), now)
        .unwrap();
    let resources = first.payload["resources"].as_array().unwrap();
    assert_eq!(resources.len(), MCP_RESOURCE_PAGE_SIZE);
    let last = resources.last().unwrap()["uri"].as_str().unwrap();
    assert_eq!(first.payload["next_cursor"], mcp_resource_cursor(last));

    let second = port
        .answer_mcp_query(
            &query(json!({"operation": "list", "cursor": first.payload["next_cursor"]})),
            now,
        )
        .unwrap();
    assert_eq!(
        second.payload,
        json!({"resources": [{"uri": oldest, "size": 1}]})
    );
}

#[test]
fn i10_g07_resource_read_hands_out_one_descriptor_over_authoritative_bytes() {
    let directory = TestDirectory::new();
    let now = CREATED_MS + 10_000;
    let image = record(ArtifactKind::Image, 20, CREATED_MS, b"\x89PNG");
    let capture = record(ArtifactKind::Capture, 21, CREATED_MS, b"pcap");
    let short = record(ArtifactKind::Stdout, 22, CREATED_MS, b"twelve bytes");
    let port = port(
        &directory,
        vec![
            (image.clone(), b"\x89PNG".to_vec()),
            (capture.clone(), b"pcap".to_vec()),
            (short.clone(), b"short".to_vec()),
        ],
    );

    let read = port
        .answer_mcp_query(
            &query(json!({"operation": "read", "uri": image.artifact_ref})),
            now,
        )
        .unwrap();
    assert_eq!(
        read.payload,
        json!({"uri": image.artifact_ref, "kind": "image", "size": 4, "mime": "image/png"})
    );
    let mut bytes = Vec::new();
    read.descriptor.unwrap().read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"\x89PNG");

    for uri in [
        capture.artifact_ref.clone(),
        format!("dbref:stdout:{}", id(99)),
        "file:///data/local/tmp/x".to_owned(),
    ] {
        let missing = port
            .answer_mcp_query(&query(json!({"operation": "read", "uri": uri})), now)
            .unwrap();
        assert_eq!(missing.payload, json!({"error": "not_found"}));
        assert!(missing.descriptor.is_none());
    }
    let expired = port
        .answer_mcp_query(
            &query(json!({"operation": "read", "uri": image.artifact_ref})),
            CREATED_MS + DAY_MS,
        )
        .unwrap();
    assert_eq!(expired.payload, json!({"error": "not_found"}));

    // Bytes that disagree with the manifest are a host failure, never a partial read.
    let error = port
        .answer_mcp_query(
            &query(json!({"operation": "read", "uri": short.artifact_ref})),
            now,
        )
        .unwrap_err();
    assert_eq!(error.code, contract::ErrorCode::IoError);
}
