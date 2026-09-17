use chrono::{DateTime, SecondsFormat, Utc};
use contract::{ErrorCode, RequestId, TaskId, UuidV4};
use domain::DomainError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex},
};

pub const MAX_ARTIFACT_RECORDS: usize = 1024;
pub const MAX_ARTIFACT_TOTAL_BYTES: u64 = 512 * 1024 * 1024;
pub const ARTIFACT_TTL_MS: u64 = 86_400_000;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Stdout,
    Stderr,
    Data,
    Image,
    Capture,
    Packet,
}

impl ArtifactKind {
    pub const fn token(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
            Self::Data => "data",
            Self::Image => "image",
            Self::Capture => "capture",
            Self::Packet => "packet",
        }
    }

    pub const fn maximum_bytes(self) -> u64 {
        match self {
            Self::Stdout | Self::Stderr | Self::Data => 1024 * 1024,
            Self::Image => 8 * 1024 * 1024,
            Self::Packet => 128 * 1024 * 1024,
            Self::Capture => 256 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRecord {
    #[serde(rename = "ref")]
    pub artifact_ref: String,
    pub kind: ArtifactKind,
    pub size: u64,
    pub created_at: String,
    pub expires_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<RequestId>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactMetadata {
    #[serde(rename = "ref")]
    pub artifact_ref: String,
    pub kind: ArtifactKind,
    pub size: u64,
    pub created_at: String,
    pub expires_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
}

impl From<&ArtifactRecord> for ArtifactMetadata {
    fn from(record: &ArtifactRecord) -> Self {
        Self {
            artifact_ref: record.artifact_ref.clone(),
            kind: record.kind,
            size: record.size,
            created_at: record.created_at.clone(),
            expires_at: record.expires_at.clone(),
            sha256: record.sha256.clone(),
            mime: record.mime.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactAdmission {
    pub evict: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactReconciliation {
    pub removed_temporaries: usize,
    pub removed_orphans: usize,
    pub missing_refs: Vec<String>,
}

pub fn plan_artifact_admission(
    manifest: &[ArtifactRecord],
    kind: ArtifactKind,
    size: u64,
    now_ms: u64,
    eligible_terminal_refs: &BTreeSet<String>,
) -> Result<ArtifactAdmission, DomainError> {
    if size == 0 || size > kind.maximum_bytes() {
        return Err(DomainError::new(
            ErrorCode::ResourceLimit,
            "artifact size is outside its kind limit",
        ));
    }
    let mut evict = manifest
        .iter()
        .filter_map(|record| {
            timestamp_ms(&record.expires_at)
                .filter(|expires_at| *expires_at <= now_ms)
                .map(|_| record.artifact_ref.clone())
        })
        .collect::<BTreeSet<_>>();
    if manifest.iter().any(|record| {
        timestamp_ms(&record.created_at).is_none() || timestamp_ms(&record.expires_at).is_none()
    }) {
        return Err(DomainError::invalid(
            "artifact manifest timestamp is invalid",
        ));
    }
    let mut remaining = manifest
        .iter()
        .filter(|record| !evict.contains(&record.artifact_ref))
        .collect::<Vec<_>>();
    let mut bytes = remaining
        .iter()
        .try_fold(0_u64, |sum, record| sum.checked_add(record.size));
    if bytes.is_none() {
        return Err(DomainError::new(
            ErrorCode::ResourceLimit,
            "artifact byte accounting overflow",
        ));
    }
    remaining.sort_by(|left, right| {
        (left.created_at.as_str(), left.artifact_ref.as_str())
            .cmp(&(right.created_at.as_str(), right.artifact_ref.as_str()))
    });
    for candidate in remaining {
        let count_after = manifest.len() + 1 - evict.len();
        let bytes_after = bytes.and_then(|value| value.checked_add(size));
        if count_after <= MAX_ARTIFACT_RECORDS
            && bytes_after.is_some_and(|value| value <= MAX_ARTIFACT_TOTAL_BYTES)
        {
            break;
        }
        if eligible_terminal_refs.contains(&candidate.artifact_ref) {
            evict.insert(candidate.artifact_ref.clone());
            bytes = bytes.and_then(|value| value.checked_sub(candidate.size));
        }
    }
    let count_after = manifest.len() + 1 - evict.len();
    let bytes_after = bytes.and_then(|value| value.checked_add(size));
    if count_after > MAX_ARTIFACT_RECORDS
        || bytes_after.is_none_or(|value| value > MAX_ARTIFACT_TOTAL_BYTES)
    {
        return Err(DomainError::new(
            ErrorCode::ResourceLimit,
            "artifact capacity cannot be freed deterministically",
        ));
    }
    Ok(ArtifactAdmission {
        evict: evict.into_iter().collect(),
    })
}

#[derive(Clone)]
pub struct ArtifactStore {
    base: PathBuf,
    state_store: Arc<crate::StateStore>,
    lease: Arc<crate::LifetimeLease>,
}

#[derive(Clone)]
pub struct RuntimeArtifactPort {
    store: ArtifactStore,
    state_store: Arc<crate::StateStore>,
    lease: Arc<crate::LifetimeLease>,
    mutation: Arc<Mutex<()>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ResolvedArtifactOwner {
    task_id: Option<TaskId>,
    request_id: RequestId,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ArtifactPublish {
    pub kind: ArtifactKind,
    pub id: UuidV4,
    pub bytes: Vec<u8>,
    pub created_at: String,
    pub created_at_ms: u64,
    pub expires_at: String,
    pub expires_at_ms: u64,
    pub mime: Option<String>,
    pub task_id: Option<TaskId>,
    pub request_id: Option<RequestId>,
}

impl ArtifactStore {
    pub fn new(state_store: Arc<crate::StateStore>, lease: Arc<crate::LifetimeLease>) -> Self {
        Self {
            base: state_store.base_path().to_path_buf(),
            state_store,
            lease,
        }
    }

    pub fn publish(&self, publish: ArtifactPublish) -> Result<ArtifactRecord, DomainError> {
        self.state_store.validate_lease(&self.lease)?;
        if publish.bytes.is_empty() || publish.bytes.len() as u64 > publish.kind.maximum_bytes() {
            return Err(DomainError::new(
                ErrorCode::ResourceLimit,
                "artifact size is outside its kind limit",
            ));
        }
        if publish.expires_at_ms
            != publish
                .created_at_ms
                .checked_add(ARTIFACT_TTL_MS)
                .ok_or_else(|| {
                    DomainError::new(ErrorCode::ResourceLimit, "artifact expiry overflow")
                })?
        {
            return Err(DomainError::invalid("artifact expiry is not the fixed TTL"));
        }
        if timestamp_ms(&publish.created_at) != Some(publish.created_at_ms)
            || timestamp_ms(&publish.expires_at) != Some(publish.expires_at_ms)
        {
            return Err(DomainError::invalid(
                "artifact timestamps are not canonical",
            ));
        }
        if publish.kind == ArtifactKind::Image
            && !matches!(publish.mime.as_deref(), Some("image/heic" | "image/png"))
        {
            return Err(DomainError::invalid("image artifact MIME is invalid"));
        }
        let artifact_ref = format!("dbref:{}:{}", publish.kind.token(), publish.id);
        let directory = self.base.join("artifacts").join(publish.kind.token());
        crate::atomic::create_canonical_directory(&self.base, &directory)?;
        let target = directory.join(publish.id.as_str());
        let temporary = directory.join(format!(".{}.tmp", publish.id.as_str()));
        let publication = (|| {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .map_err(io_error)?;
            file.write_all(&publish.bytes).map_err(io_error)?;
            crate::atomic::preserve_magisk_file_metadata(
                &self.base.join("runtime-state.json"),
                &file,
            )?;
            file.sync_all().map_err(io_error)?;
            drop(file);
            crate::atomic::replace_file_exclusive(&temporary, &target)?;
            sync_directory(&directory)
        })();
        if publication.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        publication?;
        let digest = Sha256::digest(&publish.bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Ok(ArtifactRecord {
            artifact_ref,
            kind: publish.kind,
            size: publish.bytes.len() as u64,
            created_at: publish.created_at,
            expires_at: publish.expires_at,
            sha256: Some(digest),
            mime: publish.mime,
            task_id: publish.task_id,
            request_id: publish.request_id,
        })
    }

    pub fn open(&self, artifact_ref: &str) -> Result<Vec<u8>, DomainError> {
        let metadata = self.metadata(artifact_ref)?;
        let bytes = fs::read(self.path_for_ref(artifact_ref)?).map_err(io_error)?;
        if bytes.len() as u64 != metadata.size
            || metadata.sha256.as_ref().is_some_and(|expected| {
                Sha256::digest(&bytes)
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
                    != *expected
            })
        {
            return Err(DomainError::new(
                ErrorCode::IoError,
                "artifact bytes do not match canonical metadata",
            ));
        }
        Ok(bytes)
    }

    pub fn metadata(&self, artifact_ref: &str) -> Result<ArtifactMetadata, DomainError> {
        self.path_for_ref(artifact_ref)?;
        let state = self.state_store.load(&self.lease)?;
        state
            .artifact_manifest
            .iter()
            .find(|record| record.artifact_ref == artifact_ref)
            .map(ArtifactMetadata::from)
            .ok_or_else(|| DomainError::new(ErrorCode::NotFound, "artifact reference not found"))
    }

    pub fn delete(&self, artifact_ref: &str) -> Result<(), DomainError> {
        self.state_store.validate_lease(&self.lease)?;
        let path = self.path_for_ref(artifact_ref)?;
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error(error)),
        }
        sync_directory(path.parent().expect("artifact path has parent"))
    }

    pub fn reconcile(&self) -> Result<ArtifactReconciliation, DomainError> {
        let state = self.state_store.load(&self.lease)?;
        let manifest = &state.artifact_manifest;
        let expected = manifest
            .iter()
            .map(|record| {
                self.path_for_ref(&record.artifact_ref)
                    .map(|path| (path, record.artifact_ref.clone()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let expected_paths = expected
            .iter()
            .map(|(path, _)| path.clone())
            .collect::<BTreeSet<_>>();
        let mut removed_temporaries = 0;
        let mut removed_orphans = 0;
        let root = self.base.join("artifacts");
        for kind in [
            ArtifactKind::Stdout,
            ArtifactKind::Stderr,
            ArtifactKind::Data,
            ArtifactKind::Image,
            ArtifactKind::Capture,
            ArtifactKind::Packet,
        ] {
            let directory = root.join(kind.token());
            if !directory.exists() {
                continue;
            }
            for entry in fs::read_dir(&directory).map_err(io_error)? {
                let entry = entry.map_err(io_error)?;
                let file_type = entry.file_type().map_err(io_error)?;
                if !file_type.is_file() {
                    return Err(DomainError::new(
                        ErrorCode::IoError,
                        "artifact directory contains a non-file entry",
                    ));
                }
                let name = entry.file_name();
                let path = entry.path();
                if name.to_string_lossy().starts_with('.')
                    && name.to_string_lossy().ends_with(".tmp")
                {
                    fs::remove_file(&path).map_err(io_error)?;
                    removed_temporaries += 1;
                } else if !expected_paths.contains(&path) {
                    fs::remove_file(&path).map_err(io_error)?;
                    removed_orphans += 1;
                }
            }
            sync_directory(&directory)?;
        }
        let missing_refs = expected
            .into_iter()
            .filter_map(|(path, artifact_ref)| (!path.is_file()).then_some(artifact_ref))
            .collect();
        Ok(ArtifactReconciliation {
            removed_temporaries,
            removed_orphans,
            missing_refs,
        })
    }

    fn path_for_ref(&self, artifact_ref: &str) -> Result<PathBuf, DomainError> {
        let mut parts = artifact_ref.split(':');
        if parts.next() != Some("dbref") {
            return Err(DomainError::invalid("invalid artifact reference"));
        }
        let kind = parts
            .next()
            .and_then(parse_kind)
            .ok_or_else(|| DomainError::invalid("invalid artifact kind"))?;
        let id = parts
            .next()
            .ok_or_else(|| DomainError::invalid("missing artifact identity"))?;
        if parts.next().is_some() {
            return Err(DomainError::invalid("invalid artifact reference"));
        }
        let id = UuidV4::parse(id.to_owned())
            .map_err(|_| DomainError::invalid("invalid artifact identity"))?;
        Ok(self
            .base
            .join("artifacts")
            .join(kind.token())
            .join(id.as_str()))
    }
}

/// How many Runtime commits one manifest change retries across before it reports the conflict.
const MANIFEST_COMMIT_ATTEMPTS: usize = 8;

impl RuntimeArtifactPort {
    /// Commits a change that touches only the artifact manifest. The Runtime core commits under
    /// its own lock, so the revision read before artifact bytes were written can be stale when the
    /// record is committed (a capture stopped by a request that settled meanwhile). The change
    /// revalidates what it depends on inside the store lock, so it is retried on the revision the
    /// store now holds instead of failing the execution that produced the bytes.
    fn commit_manifest<F>(&self, revision: u64, mutate: F) -> Result<(), DomainError>
    where
        F: Fn(&mut crate::CanonicalState) -> Result<(), DomainError>,
    {
        let mut revision = revision;
        for _ in 0..MANIFEST_COMMIT_ATTEMPTS {
            match self
                .state_store
                .compare_and_commit(&self.lease, revision, |candidate| mutate(candidate))
            {
                Ok(_) => return Ok(()),
                Err(error) if error.code == ErrorCode::RevisionConflict => {
                    let current = self.state_store.load(&self.lease)?.store_revision;
                    if current == revision {
                        return Err(error);
                    }
                    revision = current;
                }
                Err(error) => return Err(error),
            }
        }
        Err(DomainError::new(
            ErrorCode::RevisionConflict,
            "Runtime commits kept advancing the store revision",
        ))
    }

    pub fn new(state_store: Arc<crate::StateStore>, lease: Arc<crate::LifetimeLease>) -> Self {
        Self {
            store: ArtifactStore::new(Arc::clone(&state_store), Arc::clone(&lease)),
            state_store,
            lease,
            mutation: Arc::new(Mutex::new(())),
        }
    }

    /// Publishes bytes as one artifact of `kind` (S-ART-002). The kind owns the byte limit
    /// and the manifest record, so a producer with larger bytes still publishes under a
    /// limit that describes it.
    fn publish_kind(
        &self,
        kind: ArtifactKind,
        execution_id: Option<&UuidV4>,
        mime: Option<&str>,
        bytes: &[u8],
    ) -> Result<runtime::ArtifactMetadata, DomainError> {
        let _guard = self
            .mutation
            .lock()
            .map_err(|_| DomainError::new(ErrorCode::InternalError, "artifact lock failed"))?;
        let now = Utc::now();
        let created_at_ms = u64::try_from(now.timestamp_millis())
            .map_err(|_| DomainError::new(ErrorCode::InternalError, "system time is invalid"))?;
        let expires_at_ms = created_at_ms.checked_add(ARTIFACT_TTL_MS).ok_or_else(|| {
            DomainError::new(ErrorCode::ResourceLimit, "artifact expiry overflow")
        })?;
        let id = UuidV4::parse(uuid::Uuid::new_v4().to_string())
            .map_err(|_| DomainError::new(ErrorCode::InternalError, "UUID generation failed"))?;
        let mut state = self.state_store.load(&self.lease)?;
        let mut owner = execution_id
            .map(|execution_id| Self::resolve_execution_owner(&state, execution_id))
            .transpose()?;
        let eligible = Self::eligible_terminal_refs(&state);
        let admission = plan_artifact_admission(
            &state.artifact_manifest,
            kind,
            bytes.len() as u64,
            created_at_ms,
            &eligible,
        )?;
        if !admission.evict.is_empty() {
            let evict = admission.evict.clone();
            self.commit_manifest(state.store_revision, |candidate| {
                candidate
                    .artifact_manifest
                    .retain(|item| !evict.contains(&item.artifact_ref));
                Ok(())
            })?;
            for artifact_ref in &admission.evict {
                self.store.delete(artifact_ref)?;
            }
            state = self.state_store.load(&self.lease)?;
            owner = execution_id
                .map(|execution_id| Self::resolve_execution_owner(&state, execution_id))
                .transpose()?;
        }
        let publish = ArtifactPublish {
            kind,
            id,
            bytes: bytes.to_vec(),
            created_at: now.to_rfc3339_opts(SecondsFormat::Millis, true),
            created_at_ms,
            expires_at: DateTime::from_timestamp_millis(i64::try_from(expires_at_ms).map_err(
                |_| DomainError::new(ErrorCode::ResourceLimit, "artifact expiry is invalid"),
            )?)
            .ok_or_else(|| {
                DomainError::new(ErrorCode::ResourceLimit, "artifact expiry is invalid")
            })?
            .to_rfc3339_opts(SecondsFormat::Millis, true),
            expires_at_ms,
            mime: mime.map(str::to_owned),
            task_id: owner.as_ref().and_then(|owner| owner.task_id.clone()),
            request_id: owner.as_ref().map(|owner| owner.request_id.clone()),
        };
        let record = self.store.publish(publish)?;
        let expected_revision = state.store_revision;
        let record_for_commit = record.clone();
        let execution_id_for_commit = execution_id.cloned();
        let owner_for_commit = owner.clone();
        let committed = self.commit_manifest(expected_revision, |candidate| {
            if let Some(execution_id) = execution_id_for_commit.as_ref()
                && Some(Self::resolve_execution_owner(candidate, execution_id)?) != owner_for_commit
            {
                return Err(DomainError::new(
                    ErrorCode::StaleAuthority,
                    "artifact execution owner changed before publication",
                ));
            }
            candidate.artifact_manifest.push(record_for_commit.clone());
            Ok(())
        });
        if let Err(error) = committed {
            let _ = self.store.delete(&record.artifact_ref);
            return Err(error);
        }
        Ok(Self::runtime_metadata(&record))
    }

    fn resolve_execution_owner(
        state: &crate::CanonicalState,
        execution_id: &UuidV4,
    ) -> Result<ResolvedArtifactOwner, DomainError> {
        let mut owners = state
            .tasks
            .iter()
            .filter(|task| &task.execution_id == execution_id)
            .filter_map(|task| {
                // An AutomationExecution container Task owns no executor output.
                task.request_id
                    .clone()
                    .map(|request_id| ResolvedArtifactOwner {
                        task_id: Some(task.task_id.clone()),
                        request_id,
                    })
            })
            .chain(state.request_records.iter().filter_map(|request| {
                request
                    .synchronous_execution
                    .as_ref()
                    .filter(|execution| &execution.execution_id == execution_id)
                    .map(|_| ResolvedArtifactOwner {
                        task_id: None,
                        request_id: request.request_id.clone(),
                    })
            }));
        let owner = owners.next().ok_or_else(|| {
            DomainError::new(
                ErrorCode::StaleAuthority,
                "artifact execution has no canonical owner",
            )
        })?;
        if owners.next().is_some() {
            return Err(DomainError::new(
                ErrorCode::IoError,
                "artifact execution has duplicate canonical owners",
            ));
        }
        Ok(owner)
    }

    /// Answers one S-MCP-006 internal artifact query from this host's own manifest. Only live
    /// `stdout|stderr|data|image` artifacts are projected, ordered `(created_at desc, ref desc)`;
    /// a read hands out one read-only descriptor over the authoritative bytes, so nothing is
    /// copied into a second store and no expiry is extended.
    pub fn answer_mcp_query(
        &self,
        query: &serde_json::Value,
        now_ms: u64,
    ) -> Result<runtime::McpArtifactReply, DomainError> {
        let query = runtime::McpArtifactQuery::decode(query)?;
        let state = self.state_store.load(&self.lease)?;
        let mut live = state
            .artifact_manifest
            .iter()
            .filter(|record| {
                matches!(
                    record.kind,
                    ArtifactKind::Stdout
                        | ArtifactKind::Stderr
                        | ArtifactKind::Data
                        | ArtifactKind::Image
                ) && timestamp_ms(&record.expires_at).is_some_and(|expires_at| expires_at > now_ms)
            })
            .collect::<Vec<_>>();
        // Canonical millisecond RFC3339 timestamps order lexicographically.
        live.sort_by(|left, right| {
            (right.created_at.as_str(), right.artifact_ref.as_str())
                .cmp(&(left.created_at.as_str(), left.artifact_ref.as_str()))
        });
        let reply = |payload| runtime::McpArtifactReply {
            payload,
            descriptor: None,
        };
        match query {
            runtime::McpArtifactQuery::List { cursor } => {
                let start = match cursor {
                    None => 0,
                    Some(cursor) => {
                        let resumed = runtime::mcp_resource_cursor_ref(&cursor).and_then(|after| {
                            live.iter().position(|record| record.artifact_ref == after)
                        });
                        match resumed {
                            Some(index) => index + 1,
                            None => {
                                return Ok(reply(serde_json::json!({"error": "invalid_cursor"})));
                            }
                        }
                    }
                };
                let page = live
                    .iter()
                    .skip(start)
                    .take(runtime::MCP_RESOURCE_PAGE_SIZE)
                    .collect::<Vec<_>>();
                let resources = page
                    .iter()
                    .map(|record| {
                        let mut resource = serde_json::json!({
                            "uri": record.artifact_ref,
                            "size": record.size,
                        });
                        if let Some(mime) = &record.mime {
                            resource["mime"] = serde_json::Value::String(mime.clone());
                        }
                        resource
                    })
                    .collect::<Vec<_>>();
                let mut payload = serde_json::json!({"resources": resources});
                if start + page.len() < live.len()
                    && let Some(last) = page.last()
                {
                    payload["next_cursor"] =
                        serde_json::Value::String(runtime::mcp_resource_cursor(&last.artifact_ref));
                }
                Ok(reply(payload))
            }
            runtime::McpArtifactQuery::Metadata { refs } => Ok(reply(serde_json::json!({
                "artifacts": refs
                    .iter()
                    .filter_map(|requested| {
                        live.iter()
                            .find(|record| &record.artifact_ref == requested)
                            .map(|record| serde_json::json!({
                                "ref": record.artifact_ref,
                                "expires_at": record.expires_at,
                            }))
                    })
                    .collect::<Vec<_>>(),
            }))),
            runtime::McpArtifactQuery::Read { uri } => {
                let Some(record) = live.iter().find(|record| record.artifact_ref == uri) else {
                    return Ok(reply(serde_json::json!({"error": "not_found"})));
                };
                let file = fs::File::open(self.store.path_for_ref(&uri)?).map_err(io_error)?;
                if file.metadata().map_err(io_error)?.len() != record.size {
                    return Err(DomainError::new(
                        ErrorCode::IoError,
                        "artifact bytes do not match canonical metadata",
                    ));
                }
                let mut payload = serde_json::json!({
                    "uri": record.artifact_ref,
                    "kind": record.kind.token(),
                    "size": record.size,
                });
                if let Some(mime) = &record.mime {
                    payload["mime"] = serde_json::Value::String(mime.clone());
                }
                Ok(runtime::McpArtifactReply {
                    payload,
                    descriptor: Some(file),
                })
            }
        }
    }

    fn runtime_metadata(record: &ArtifactRecord) -> runtime::ArtifactMetadata {
        runtime::ArtifactMetadata {
            artifact_ref: record.artifact_ref.clone(),
            byte_count: record.size,
            sha256: record.sha256.clone().unwrap_or_default(),
            mime: record.mime.clone(),
        }
    }

    fn eligible_terminal_refs(state: &crate::CanonicalState) -> BTreeSet<String> {
        state
            .artifact_manifest
            .iter()
            .filter(|record| {
                record.task_id.as_ref().is_some_and(|task_id| {
                    state.tasks.iter().any(|task| {
                        &task.task_id == task_id
                            && matches!(
                                task.state,
                                contract::TaskState::Completed
                                    | contract::TaskState::Failed
                                    | contract::TaskState::Cancelled
                                    | contract::TaskState::Interrupted
                            )
                    })
                })
            })
            .map(|record| record.artifact_ref.clone())
            .collect()
    }
}

impl runtime::ArtifactPort for RuntimeArtifactPort {
    fn publish(&self, bytes: &[u8]) -> Result<runtime::ArtifactMetadata, DomainError> {
        self.publish_kind(ArtifactKind::Data, None, None, bytes)
    }

    fn publish_as(
        &self,
        kind: &str,
        bytes: &[u8],
    ) -> Result<runtime::ArtifactMetadata, DomainError> {
        let kind = parse_kind(kind)
            .ok_or_else(|| DomainError::invalid("artifact kind is not an S-ART-001 kind"))?;
        self.publish_kind(kind, None, None, bytes)
    }

    fn publish_for_execution(
        &self,
        execution_id: &UuidV4,
        kind: &str,
        bytes: &[u8],
    ) -> Result<runtime::ArtifactMetadata, DomainError> {
        let kind = parse_kind(kind)
            .ok_or_else(|| DomainError::invalid("artifact kind is not an S-ART-001 kind"))?;
        self.publish_kind(kind, Some(execution_id), None, bytes)
    }

    fn publish_image_for_execution(
        &self,
        execution_id: &UuidV4,
        mime: &str,
        bytes: &[u8],
    ) -> Result<runtime::ArtifactMetadata, DomainError> {
        if !matches!(mime, "image/heic" | "image/png") {
            return Err(DomainError::invalid("image artifact MIME is invalid"));
        }
        self.publish_kind(ArtifactKind::Image, Some(execution_id), Some(mime), bytes)
    }

    fn open(&self, artifact_ref: &str) -> Result<Vec<u8>, DomainError> {
        self.store.open(artifact_ref)
    }

    fn metadata(&self, artifact_ref: &str) -> Result<runtime::ArtifactMetadata, DomainError> {
        let metadata = self.store.metadata(artifact_ref)?;
        Ok(runtime::ArtifactMetadata {
            artifact_ref: metadata.artifact_ref,
            byte_count: metadata.size,
            sha256: metadata.sha256.unwrap_or_default(),
            mime: metadata.mime,
        })
    }

    fn delete(&self, artifact_ref: &str) -> Result<(), DomainError> {
        let _guard = self
            .mutation
            .lock()
            .map_err(|_| DomainError::new(ErrorCode::InternalError, "artifact lock failed"))?;
        let state = self.state_store.load(&self.lease)?;
        if !state
            .artifact_manifest
            .iter()
            .any(|record| record.artifact_ref == artifact_ref)
        {
            return Err(DomainError::new(
                ErrorCode::NotFound,
                "artifact reference not found",
            ));
        }
        let artifact_ref_for_commit = artifact_ref.to_owned();
        self.commit_manifest(state.store_revision, |candidate| {
            candidate
                .artifact_manifest
                .retain(|record| record.artifact_ref != artifact_ref_for_commit);
            Ok(())
        })?;
        self.store.delete(artifact_ref)
    }
}

fn timestamp_ms(value: &str) -> Option<u64> {
    let parsed = DateTime::parse_from_rfc3339(value).ok()?;
    if parsed
        .with_timezone(&Utc)
        .to_rfc3339_opts(SecondsFormat::Millis, true)
        != value
    {
        return None;
    }
    parsed.timestamp_millis().try_into().ok()
}

pub(crate) fn validate_artifact_record(record: &ArtifactRecord) -> Result<(), DomainError> {
    let created_at = timestamp_ms(&record.created_at)
        .ok_or_else(|| DomainError::invalid("artifact creation timestamp is invalid"))?;
    let expires_at = timestamp_ms(&record.expires_at)
        .ok_or_else(|| DomainError::invalid("artifact expiry timestamp is invalid"))?;
    if record.size == 0
        || record.size > record.kind.maximum_bytes()
        || Some(expires_at) != created_at.checked_add(ARTIFACT_TTL_MS)
    {
        return Err(DomainError::invalid("artifact manifest record is invalid"));
    }
    if record.kind == ArtifactKind::Image
        && !matches!(record.mime.as_deref(), Some("image/heic" | "image/png"))
    {
        return Err(DomainError::invalid("image artifact MIME is invalid"));
    }
    let mut parts = record.artifact_ref.split(':');
    if parts.next() != Some("dbref")
        || parts.next().and_then(parse_kind) != Some(record.kind)
        || parts
            .next()
            .and_then(|value| UuidV4::parse(value.to_owned()).ok())
            .is_none()
        || parts.next().is_some()
    {
        return Err(DomainError::invalid("artifact reference is invalid"));
    }
    Ok(())
}

fn parse_kind(value: &str) -> Option<ArtifactKind> {
    match value {
        "stdout" => Some(ArtifactKind::Stdout),
        "stderr" => Some(ArtifactKind::Stderr),
        "data" => Some(ArtifactKind::Data),
        "image" => Some(ArtifactKind::Image),
        "capture" => Some(ArtifactKind::Capture),
        "packet" => Some(ArtifactKind::Packet),
        _ => None,
    }
}

pub(crate) fn sync_directory(path: &std::path::Path) -> Result<(), DomainError> {
    #[cfg(unix)]
    {
        fs::File::open(path)
            .and_then(|file| file.sync_all())
            .map_err(io_error)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

pub(crate) fn io_error(_: std::io::Error) -> DomainError {
    DomainError::new(ErrorCode::IoError, "persistence I/O failed")
}

#[cfg(test)]
mod tests {
    use super::{ArtifactKind, ArtifactPublish, ArtifactStore, RuntimeArtifactPort};
    use crate::{CanonicalState, RuntimeLive, RuntimeOwner, StateStore};
    use contract::{RuntimeHost, UuidV4};
    use std::sync::Arc;

    fn id(index: u64) -> UuidV4 {
        UuidV4::parse(format!("00000000-0000-4000-8000-{index:012x}")).unwrap()
    }

    /// A manifest change prepared against a revision the Runtime core has since advanced is
    /// committed on the current revision instead of failing the execution that produced it.
    #[test]
    fn manifest_commits_retry_across_runtime_commits() {
        let base =
            std::env::temp_dir().join(format!("droidbridge-manifest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let store = Arc::new(StateStore::new(base.clone()));
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
                    runtime_epoch: id(1),
                    host: RuntimeHost::ApkRuntime,
                    host_generation: 1,
                    runtime_instance_id: id(2),
                    boot_id: id(3),
                    pid: 42,
                    start_ticks: 99,
                })
                .unwrap(),
        );
        let port = RuntimeArtifactPort::new(Arc::clone(&store), Arc::clone(&lease));
        let record = ArtifactStore::new(Arc::clone(&store), Arc::clone(&lease))
            .publish(ArtifactPublish {
                kind: ArtifactKind::Image,
                id: id(30),
                bytes: b"capture".to_vec(),
                created_at: "2026-01-01T00:00:00.000Z".to_owned(),
                created_at_ms: 1_767_225_600_000,
                expires_at: "2026-01-02T00:00:00.000Z".to_owned(),
                expires_at_ms: 1_767_312_000_000,
                mime: Some("image/png".to_owned()),
                task_id: None,
                request_id: None,
            })
            .unwrap();

        let stale = store.load(&lease).unwrap().store_revision;
        // The Runtime core commits twice while the artifact bytes are being written.
        for revision in [stale, stale + 1] {
            store
                .compare_and_commit(&lease, revision, |_| Ok(()))
                .unwrap();
        }
        port.commit_manifest(stale, |candidate| {
            candidate.artifact_manifest.push(record.clone());
            Ok(())
        })
        .unwrap();
        let state = store.load(&lease).unwrap();
        assert_eq!(state.store_revision, stale + 3);
        assert_eq!(state.artifact_manifest, vec![record]);

        // A refusal from the change itself is not retried.
        let refused = port
            .commit_manifest(state.store_revision, |_| {
                Err(domain::DomainError::new(
                    contract::ErrorCode::StaleAuthority,
                    "owner changed",
                ))
            })
            .unwrap_err();
        assert_eq!(refused.code, contract::ErrorCode::StaleAuthority);
        drop(port);
        drop(lease);
        let _ = std::fs::remove_dir_all(&base);
    }
}
