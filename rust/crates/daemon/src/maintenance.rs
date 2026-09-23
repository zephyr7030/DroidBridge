//! Backend-only S-IPC-DAEMON-005 self-maintenance installers.
//!
//! Each install accepts only the durable HostController record's exact signed facts plus one
//! verified read-only artifact descriptor, stages that artifact privately and runs one fixed
//! argument array under the root execution guard. No caller path, package or flag reaches a
//! command, and a lost or unverified attempt is never launched again by this process.

use crate::ModuleIdentity;
use crate::WireEnvelope;
use crate::command::{CommandQuarantine, RootCommandGuard, guard_path};
use crate::magisk_guard_recovery::{io_error, read_boot_id};
use contract::{
    ErrorCode, MaintenanceInstallApk, MaintenanceInstallModule, MaintenanceStatus, UuidV4,
};
use domain::DomainError;
use runtime::LocalExecutionClaims;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs,
    io::{Read, Write},
    os::fd::OwnedFd,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::Arc,
};

const RECORD: &str = "update-maintenance.json";
const STAGING_DIRECTORY: &str = "/data/local/tmp";
const PM: &str = "/system/bin/pm";
const MAGISK: &str = "/system/bin/magisk";
const KERNELSU: &str = "/data/adb/ksud";
const APATCH: &str = "/data/adb/ap/bin/apd";
const ROOT_PROVIDER_RECORD: &str = "root-provider";
const MAX_ARTIFACT_BYTES: u64 = 536_870_912;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RootProvider {
    Magisk,
    KernelSu,
    APatch,
}

/// The HostController `update-maintenance.json` record, read only to revalidate a request.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct MaintenanceRecord {
    schema_version: u32,
    update_id: String,
    kind: String,
    target_version: String,
    target_version_code: u64,
    target_apk_sha256: Option<String>,
    target_apk_size: Option<u64>,
    target_apk_signer_sha256: String,
    target_module_sha256: Option<String>,
    target_module_size: Option<u64>,
    maintenance_execution_id: Option<String>,
    requires_module: bool,
    apk_install_provider: Option<String>,
    phase: String,
    apk_session_id: Option<i64>,
}

struct Attempt {
    execution_id: String,
    payload: Value,
    reply: Value,
    cleanup_verified: bool,
}

#[derive(Clone, Copy)]
pub(crate) enum InstallKind {
    Apk,
    Module,
}

/// Attempt facts retained for this daemon process, keyed by update identity.
#[derive(Default)]
pub(crate) struct MaintenanceAttempts {
    attempts: HashMap<String, Attempt>,
    claims: LocalExecutionClaims,
}

impl MaintenanceAttempts {
    /// `MaintenanceStatus`: describes only this daemon's attempt for the active record.
    pub(crate) fn status(&self, base: &Path, payload: &Value) -> Value {
        let answered = (|| {
            let request: MaintenanceStatus = decode(payload)?;
            let record = read_record(base)?;
            if record.update_id != request.update_id.as_str() {
                return Err(stale("maintenance record identity changed"));
            }
            Ok(match self.attempts.get(request.update_id.as_str()) {
                Some(attempt) => json!({
                    "update_id": request.update_id,
                    "install_active": false,
                    "execution_id": attempt.execution_id,
                    "cleanup": if attempt.cleanup_verified { "clean" } else { "unverified" },
                }),
                None => json!({
                    "update_id": request.update_id,
                    "install_active": false,
                    "cleanup": "none",
                }),
            })
        })();
        answered.unwrap_or_else(|error| error_json(error.code))
    }

    pub(crate) fn install(
        &mut self,
        kind: InstallKind,
        base: &Path,
        module_root: &Path,
        identity: &ModuleIdentity,
        request: &WireEnvelope,
        descriptors: Vec<OwnedFd>,
    ) -> Value {
        self.try_install(kind, base, module_root, identity, request, descriptors)
            .unwrap_or_else(|error| error_json(error.code))
    }

    fn try_install(
        &mut self,
        kind: InstallKind,
        base: &Path,
        module_root: &Path,
        identity: &ModuleIdentity,
        request: &WireEnvelope,
        descriptors: Vec<OwnedFd>,
    ) -> Result<Value, DomainError> {
        let record = read_record(base)?;
        let target = match kind {
            InstallKind::Apk => {
                let payload: MaintenanceInstallApk = decode(&request.payload)?;
                let matches = record.kind == "product_update"
                    && record.phase == "apk_installing"
                    && record.apk_install_provider.as_deref() == Some("magisk_privileged")
                    && record.apk_session_id.is_none()
                    && payload.package == identity.package
                    && payload.version_code == record.target_version_code
                    && Some(payload.sha256.as_str()) == record.target_apk_sha256.as_deref()
                    && Some(payload.size) == record.target_apk_size
                    && payload.signer_sha256 == record.target_apk_signer_sha256;
                Target {
                    update_id: payload.update_id.as_str().to_owned(),
                    execution_id: payload.execution_id,
                    sha256: payload.sha256,
                    size: payload.size,
                    matches,
                    role: "verified_apk",
                    suffix: "apk",
                }
            }
            InstallKind::Module => {
                let payload: MaintenanceInstallModule = decode(&request.payload)?;
                let matches = record.phase == "module_installing"
                    && record.requires_module
                    && payload.module_id == identity.module_id
                    && payload.version_code == record.target_version_code
                    && Some(payload.sha256.as_str()) == record.target_module_sha256.as_deref()
                    && Some(payload.size) == record.target_module_size;
                Target {
                    update_id: payload.update_id.as_str().to_owned(),
                    execution_id: payload.execution_id,
                    sha256: payload.sha256,
                    size: payload.size,
                    matches,
                    role: "verified_module_zip",
                    suffix: "zip",
                }
            }
        };
        if record.update_id != target.update_id
            || record.maintenance_execution_id.as_deref() != Some(target.execution_id.as_str())
        {
            return Err(stale("maintenance record does not name this attempt"));
        }
        if !target.matches || target.size == 0 || target.size > MAX_ARTIFACT_BYTES {
            return Err(DomainError::invalid(
                "maintenance request does not match the signed record",
            ));
        }
        if let Some(prior) = self.attempts.get(&target.update_id) {
            if prior.execution_id == target.execution_id.as_str() {
                return if prior.payload == request.payload {
                    Ok(prior.reply.clone())
                } else {
                    Err(DomainError::invalid("maintenance attempt payload changed"))
                };
            }
            if !prior.cleanup_verified {
                return Err(io_error("prior maintenance attempt cleanup is unverified"));
            }
        }
        let artifact = if descriptors.len() == 1 && request.fd_roles == [target.role] {
            descriptors.into_iter().next()
        } else {
            None
        }
        .ok_or_else(|| DomainError::invalid("maintenance artifact descriptor role is invalid"))?;
        let instance = request
            .runtime_instance_id
            .clone()
            .ok_or_else(|| stale("maintenance request carries no APK Runtime instance"))?;

        let staged = stage(artifact, &target)?;
        let guard = RootCommandGuard::new(
            base.to_path_buf(),
            guard_path(module_root),
            request.runtime_epoch.clone(),
            instance,
            read_boot_id()?,
            Arc::new(CommandQuarantine::default()),
        );
        let staged_path = staged.to_string_lossy().into_owned();
        let (program, arguments) = match kind {
            InstallKind::Apk => (
                PM,
                vec![
                    "install".into(),
                    "-r".into(),
                    "--user".into(),
                    "0".into(),
                    staged_path,
                ],
            ),
            InstallKind::Module => module_install_command(module_root, staged_path)?,
        };
        let settled = self
            .claims
            .claim(target.execution_id.clone())
            .map_err(|error| runtime::ExecutionFailure {
                error,
                cleanup_verified: true,
            })
            .and_then(|claim| {
                guard.run_maintenance_install(&target.execution_id, program, arguments, &claim)
            });
        let removed = fs::remove_file(&staged).is_ok();
        let (reply, cleanup_verified) = match settled {
            Ok(settlement) if settlement.cleanup_verified && removed => (
                json!({
                    "update_id": target.update_id,
                    "execution_id": target.execution_id,
                    "process_exit_code": settlement.outcome.exit_code.unwrap_or(-1),
                    "cleanup": "clean",
                }),
                true,
            ),
            Ok(settlement) => (error_json(ErrorCode::IoError), settlement.cleanup_verified),
            Err(failure) => (error_json(failure.error.code), failure.cleanup_verified),
        };
        self.attempts.insert(
            target.update_id,
            Attempt {
                execution_id: target.execution_id.as_str().to_owned(),
                payload: request.payload.clone(),
                reply: reply.clone(),
                cleanup_verified,
            },
        );
        Ok(reply)
    }
}

fn module_install_command(
    module_root: &Path,
    staged_path: String,
) -> Result<(&'static str, Vec<String>), DomainError> {
    Ok(module_install_command_for(
        read_root_provider(module_root)?,
        staged_path,
    ))
}

fn module_install_command_for(
    provider: RootProvider,
    staged_path: String,
) -> (&'static str, Vec<String>) {
    match provider {
        RootProvider::Magisk => (MAGISK, vec!["--install-module".into(), staged_path]),
        RootProvider::KernelSu => (
            KERNELSU,
            vec!["module".into(), "install".into(), staged_path],
        ),
        RootProvider::APatch => (APATCH, vec!["module".into(), "install".into(), staged_path]),
    }
}

/// The installed module records the manager that installed it. A module installed before that
/// record existed carries none, so the managers' own binaries answer for it; a device with
/// neither, or with both, cannot be updated without guessing and is refused.
fn read_root_provider(module_root: &Path) -> Result<RootProvider, DomainError> {
    let Ok(value) = fs::read(module_root.join(ROOT_PROVIDER_RECORD)) else {
        return installed_provider(
            Path::new(APATCH).exists(),
            Path::new(KERNELSU).exists(),
            Path::new(MAGISK).exists(),
        );
    };
    match trimmed(&value) {
        b"magisk" => Ok(RootProvider::Magisk),
        b"kernelsu" => Ok(RootProvider::KernelSu),
        b"apatch" => Ok(RootProvider::APatch),
        _ => Err(unsupported_provider()),
    }
}

fn installed_provider(
    apatch: bool,
    kernelsu: bool,
    magisk: bool,
) -> Result<RootProvider, DomainError> {
    match (apatch, kernelsu, magisk) {
        (true, false, false) => Ok(RootProvider::APatch),
        (false, true, false) => Ok(RootProvider::KernelSu),
        (false, false, true) => Ok(RootProvider::Magisk),
        _ => Err(unsupported_provider()),
    }
}

fn unsupported_provider() -> DomainError {
    DomainError::new(
        ErrorCode::CapabilityUnavailable,
        "root module provider is unsupported",
    )
}

/// The record is one line; a writer that omitted the newline still names the same provider.
fn trimmed(value: &[u8]) -> &[u8] {
    let start = value
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(value.len());
    let end = value
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |index| index + 1);
    &value[start..end]
}

struct Target {
    update_id: String,
    execution_id: UuidV4,
    sha256: String,
    size: u64,
    matches: bool,
    role: &'static str,
    suffix: &'static str,
}

/// Copies the verified read-only descriptor into one private temp file, proving size and digest.
fn stage(artifact: OwnedFd, target: &Target) -> Result<PathBuf, DomainError> {
    let mut source = fs::File::from(artifact);
    let metadata = source
        .metadata()
        .map_err(|_| io_error("cannot inspect maintenance artifact"))?;
    if !metadata.is_file() || metadata.len() != target.size {
        return Err(DomainError::invalid(
            "maintenance artifact is not the signed regular file",
        ));
    }
    let path = PathBuf::from(STAGING_DIRECTORY).join(format!(
        "droidbridge-maintenance-{}.{}",
        target.execution_id.as_str(),
        target.suffix
    ));
    let mut staged = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|_| io_error("cannot create maintenance staging file"))?;
    let copied = (|| {
        let mut digest = Sha256::new();
        let mut buffer = vec![0_u8; 64 * 1024];
        let mut total = 0_u64;
        loop {
            let read = source
                .read(&mut buffer)
                .map_err(|_| io_error("cannot read maintenance artifact"))?;
            if read == 0 {
                break;
            }
            total += read as u64;
            if total > target.size {
                return Err(DomainError::invalid(
                    "maintenance artifact exceeds its signed size",
                ));
            }
            digest.update(&buffer[..read]);
            staged
                .write_all(&buffer[..read])
                .map_err(|_| io_error("cannot stage maintenance artifact"))?;
        }
        staged
            .sync_all()
            .map_err(|_| io_error("cannot sync maintenance artifact"))?;
        let actual: String = digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        if total != target.size || actual != target.sha256 {
            return Err(DomainError::invalid(
                "maintenance artifact does not match its signed digest",
            ));
        }
        Ok(())
    })();
    if let Err(error) = copied {
        drop(staged);
        let _ = fs::remove_file(&path);
        return Err(error);
    }
    Ok(path)
}

fn read_record(base: &Path) -> Result<MaintenanceRecord, DomainError> {
    let bytes = fs::read(base.join(RECORD)).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => stale("no update maintenance is recorded"),
        _ => io_error("cannot read update maintenance record"),
    })?;
    let record: MaintenanceRecord = serde_json::from_slice(&bytes)
        .map_err(|_| io_error("update maintenance record is malformed"))?;
    if record.schema_version != 1 {
        return Err(io_error("update maintenance record schema is unsupported"));
    }
    Ok(record)
}

fn decode<T: serde::de::DeserializeOwned>(payload: &Value) -> Result<T, DomainError> {
    serde_json::from_value(payload.clone())
        .map_err(|_| DomainError::invalid("invalid maintenance payload"))
}

fn stale(reason: &'static str) -> DomainError {
    DomainError::new(ErrorCode::StaleAuthority, reason)
}

fn error_json(code: ErrorCode) -> Value {
    json!({"error": {"code": code, "retryable": false}})
}

#[cfg(test)]
mod tests {
    use super::{RootProvider, installed_provider, module_install_command_for, read_root_provider};
    use contract::ErrorCode;
    use std::fs;

    #[test]
    fn a_module_without_a_provider_record_follows_the_installed_manager() {
        assert!(matches!(
            installed_provider(true, false, false),
            Ok(RootProvider::APatch)
        ));
        assert!(matches!(
            installed_provider(false, true, false),
            Ok(RootProvider::KernelSu)
        ));
        assert!(matches!(
            installed_provider(false, false, true),
            Ok(RootProvider::Magisk)
        ));
        // A device showing more than one manager, or none, would be guessed at, so it is refused.
        for ambiguous in [
            (true, true, true),
            (false, true, true),
            (false, false, false),
        ] {
            assert_eq!(
                installed_provider(ambiguous.0, ambiguous.1, ambiguous.2)
                    .err()
                    .map(|error| error.code),
                Some(ErrorCode::CapabilityUnavailable),
            );
        }
    }

    #[test]
    fn a_recorded_provider_is_read_with_or_without_its_newline() {
        let module_root =
            std::env::temp_dir().join(format!("root-provider-{}", std::process::id()));
        fs::create_dir_all(&module_root).unwrap();
        let record = module_root.join("root-provider");
        for (contents, expected) in [
            ("kernelsu\n", RootProvider::KernelSu),
            ("kernelsu", RootProvider::KernelSu),
            ("magisk\n", RootProvider::Magisk),
            ("apatch\n", RootProvider::APatch),
        ] {
            fs::write(&record, contents).unwrap();
            assert_eq!(read_root_provider(&module_root).unwrap(), expected);
        }
        fs::write(&record, "supersu\n").unwrap();
        assert_eq!(
            read_root_provider(&module_root)
                .err()
                .map(|error| error.code),
            Some(ErrorCode::CapabilityUnavailable),
        );
        fs::remove_dir_all(&module_root).unwrap();
    }

    #[test]
    fn root_providers_use_their_native_module_installers() {
        assert_eq!(
            module_install_command_for(
                RootProvider::Magisk,
                "/data/local/tmp/update.zip".to_owned(),
            ),
            (
                "/system/bin/magisk",
                vec![
                    "--install-module".to_owned(),
                    "/data/local/tmp/update.zip".to_owned(),
                ],
            ),
        );
        assert_eq!(
            module_install_command_for(
                RootProvider::KernelSu,
                "/data/local/tmp/update.zip".to_owned(),
            ),
            (
                "/data/adb/ksud",
                vec![
                    "module".to_owned(),
                    "install".to_owned(),
                    "/data/local/tmp/update.zip".to_owned(),
                ],
            ),
        );
        assert_eq!(
            module_install_command_for(
                RootProvider::APatch,
                "/data/local/tmp/update.zip".to_owned(),
            ),
            (
                "/data/adb/ap/bin/apd",
                vec![
                    "module".to_owned(),
                    "install".to_owned(),
                    "/data/local/tmp/update.zip".to_owned(),
                ],
            ),
        );
    }
}
