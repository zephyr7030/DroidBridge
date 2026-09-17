use crate::{atomic_replace_json, read_json};
use chrono::{DateTime, SecondsFormat, Utc};
use contract::{ErrorCode, ExecutionId, UuidV4};
use domain::DomainError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const FAULT_RECORD_LIMIT: usize = 64;
pub const FAULT_FILE_LIMIT_BYTES: usize = 65_536;
pub const FAULT_RETENTION_MS: u64 = 7 * 86_400_000;
pub const FAULT_COALESCE_MS: u64 = 60_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FaultRole {
    Runtime,
    Host,
    Supervisor,
    Maintenance,
}

impl FaultRole {
    const fn file_name(self) -> &'static str {
        match self {
            Self::Runtime => "runtime.json",
            Self::Host => "host.json",
            Self::Supervisor => "supervisor.json",
            Self::Maintenance => "maintenance.json",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FaultRecord {
    pub record_id: UuidV4,
    pub at: String,
    pub component: String,
    pub code: String,
    pub phase: String,
    pub product_version: String,
    pub boot_id: UuidV4,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_instance_id: Option<UuidV4>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<ExecutionId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
    pub repeat_count: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FaultFile {
    pub schema_version: u32,
    pub records: Vec<FaultRecord>,
}

impl Default for FaultFile {
    fn default() -> Self {
        Self {
            schema_version: 1,
            records: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct FaultFileStore {
    diagnostics: PathBuf,
    role: FaultRole,
}

impl FaultFileStore {
    pub fn new(canonical_base: &Path, role: FaultRole) -> Self {
        Self {
            diagnostics: canonical_base.join("diagnostics"),
            role,
        }
    }

    pub fn initialize_all_by_apk(canonical_base: &Path) -> Result<(), DomainError> {
        if !canonical_base.is_dir() {
            return Err(DomainError::new(
                ErrorCode::NotFound,
                "canonical base does not exist",
            ));
        }
        let diagnostics = canonical_base.join("diagnostics");
        std::fs::create_dir_all(&diagnostics).map_err(crate::io_error)?;
        for role in [
            FaultRole::Runtime,
            FaultRole::Host,
            FaultRole::Supervisor,
            FaultRole::Maintenance,
        ] {
            let path = diagnostics.join(role.file_name());
            if path.exists() {
                let existing: FaultFile = read_json(&path)?;
                validate_file(&existing)?;
            } else {
                atomic_replace_json(&diagnostics, role.file_name(), &FaultFile::default())?;
            }
        }
        Ok(())
    }

    pub fn append(&self, mut record: FaultRecord, now_ms: u64) -> Result<(), DomainError> {
        validate_record(&record)?;
        if timestamp_ms(&record.at) != Some(now_ms) {
            return Err(DomainError::invalid(
                "fault record time does not match the writer clock",
            ));
        }
        let path = self.path();
        let mut file: FaultFile = read_json(&path)?;
        validate_file(&file)?;
        file.records.retain(|item| {
            timestamp_ms(&item.at).is_some_and(|at| now_ms.saturating_sub(at) <= FAULT_RETENTION_MS)
        });
        if let Some(previous) = file.records.iter_mut().rev().find(|item| {
            item.component == record.component
                && item.code == record.code
                && item.phase == record.phase
                && item.runtime_instance_id == record.runtime_instance_id
                && timestamp_ms(&item.at)
                    .is_some_and(|at| now_ms.saturating_sub(at) <= FAULT_COALESCE_MS)
        }) {
            previous.repeat_count = previous.repeat_count.saturating_add(1);
            previous.at = record.at;
        } else {
            record.repeat_count = record.repeat_count.max(1);
            file.records.push(record);
        }
        while file.records.len() > FAULT_RECORD_LIMIT {
            file.records.remove(0);
        }
        while serde_json::to_vec(&file)
            .map_err(|_| DomainError::new(ErrorCode::InternalError, "fault encoding failed"))?
            .len()
            > FAULT_FILE_LIMIT_BYTES
        {
            if file.records.len() <= 1 {
                return Err(DomainError::new(
                    ErrorCode::ResourceLimit,
                    "fault record exceeds bounded file",
                ));
            }
            file.records.remove(0);
        }
        atomic_replace_json(&self.diagnostics, self.role.file_name(), &file)
    }

    pub fn read(&self) -> Result<FaultFile, DomainError> {
        let file = read_json(&self.path())?;
        validate_file(&file)?;
        Ok(file)
    }

    fn path(&self) -> PathBuf {
        self.diagnostics.join(self.role.file_name())
    }
}

fn validate_file(file: &FaultFile) -> Result<(), DomainError> {
    if file.schema_version != 1 || file.records.len() > FAULT_RECORD_LIMIT {
        return Err(DomainError::new(
            ErrorCode::IoError,
            "fault file is corrupt",
        ));
    }
    for record in &file.records {
        validate_record(record)?;
    }
    Ok(())
}

fn validate_record(record: &FaultRecord) -> Result<(), DomainError> {
    if timestamp_ms(&record.at).is_none()
        || !bounded_ascii(&record.component, 64)
        || !bounded_ascii(&record.code, 64)
        || !bounded_ascii(&record.phase, 64)
        || !bounded_ascii(&record.product_version, 64)
        || record.repeat_count == 0
    {
        return Err(DomainError::invalid("fault record is invalid"));
    }
    Ok(())
}

fn bounded_ascii(value: &str, maximum: usize) -> bool {
    !value.is_empty() && value.len() <= maximum && value.is_ascii()
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
