use contract::{ErrorCode, UuidV4};
use domain::DomainError;
use persistence::{
    CleanGuardRecord, GuardCleanCause, GuardFinalizationDisposition, GuardIdentity,
    GuardProofDirectory, GuardProofReader, GuardRecovery, GuardRecoveryPlan, LifetimeLease,
    ProcessFacts, RuntimeLive, StateStore, classify_guard_proof, encode_guard_frame,
};
use std::{
    fs, io,
    io::Write,
    os::fd::{AsRawFd, FromRawFd},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const MAX_GUARD_PROOF_FILES: usize = 384;
const MAX_GUARD_PROOF_BYTES: u64 = 1_572_864;

pub(crate) struct MagiskCleanupVerification {
    _private: (),
}

pub(crate) trait MagiskGuardRecoveryMechanics {
    fn finalize(&mut self, record: &CleanGuardRecord) -> Result<(), DomainError>;
    fn finish(&mut self, had_records: bool) -> Result<(), DomainError>;
}

pub(crate) fn execute_guard_recovery(
    plan: &GuardRecoveryPlan,
    mechanics: &mut impl MagiskGuardRecoveryMechanics,
) -> Result<MagiskCleanupVerification, DomainError> {
    if !plan.guards_are_clean() {
        return Err(io_error("root guard recovery is not proven clean"));
    }
    let mut failed = false;
    for record in plan.records() {
        if record.finalization == GuardFinalizationDisposition::Blocked
            || mechanics.finalize(record).is_err()
        {
            failed = true;
        }
    }
    if mechanics.finish(!plan.records().is_empty()).is_err() {
        failed = true;
    }
    if failed {
        Err(io_error("root guard finalization is unverified"))
    } else {
        Ok(MagiskCleanupVerification { _private: () })
    }
}

pub(crate) struct FilesystemMagiskGuardRecovery<'a> {
    base: &'a Path,
    store: &'a StateStore,
    lease: &'a LifetimeLease,
    touched_directories: Vec<PathBuf>,
}

impl<'a> FilesystemMagiskGuardRecovery<'a> {
    pub(crate) fn new(base: &'a Path, store: &'a StateStore, lease: &'a LifetimeLease) -> Self {
        Self {
            base,
            store,
            lease,
            touched_directories: Vec::new(),
        }
    }
}

impl MagiskGuardRecoveryMechanics for FilesystemMagiskGuardRecovery<'_> {
    fn finalize(&mut self, record: &CleanGuardRecord) -> Result<(), DomainError> {
        match record.finalization {
            GuardFinalizationDisposition::Blocked => {
                return Err(io_error("blocked root guard record cannot be finalized"));
            }
            GuardFinalizationDisposition::CleanupTaskTemporaryAndRemoveProof => {
                let task_id = record
                    .task_id
                    .as_ref()
                    .ok_or_else(|| io_error("root guard cleanup task identity is missing"))?;
                self.store
                    .cleanup_task_temporary(self.lease, task_id, &record.recovery)?;
            }
            GuardFinalizationDisposition::RemoveProof => {}
        }
        let boot_id = record
            .containing_boot_id
            .as_ref()
            .ok_or_else(|| io_error("root guard boot identity is missing"))?;
        let root = self.base.join("execution-guards");
        let directory = root.join(boot_id.as_str());
        let proof = directory.join(format!("{}.proof", record.execution_id.as_str()));
        fs::remove_file(proof).map_err(|_| io_error("cannot remove clean root guard proof"))?;
        sync_directory(&directory)?;
        if !self.touched_directories.contains(&directory) {
            self.touched_directories.push(directory);
        }
        Ok(())
    }

    fn finish(&mut self, had_records: bool) -> Result<(), DomainError> {
        let root = self.base.join("execution-guards");
        let mut failed = false;
        for directory in self.touched_directories.drain(..) {
            match fs::read_dir(&directory) {
                Ok(mut entries) => {
                    if entries.next().is_none() && fs::remove_dir(&directory).is_err() {
                        failed = true;
                    }
                }
                Err(_) => failed = true,
            }
        }
        if had_records && sync_directory(&root).is_err() {
            failed = true;
        }
        if failed {
            Err(io_error("cannot durably finalize root guard records"))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy)]
enum GuardProbe {
    ChildExit,
    OwnerLost,
}

pub(crate) fn probe_root_guard(
    canonical_base: &Path,
    guard_path: &Path,
    live: &RuntimeLive,
) -> Result<bool, DomainError> {
    if !guard_path.is_absolute() || !guard_path.is_file() {
        return Ok(false);
    }
    let exited = run_guard_probe(canonical_base, guard_path, live, GuardProbe::ChildExit)?;
    let owner_lost = run_guard_probe(canonical_base, guard_path, live, GuardProbe::OwnerLost)?;
    Ok(exited && owner_lost)
}

fn run_guard_probe(
    canonical_base: &Path,
    guard_path: &Path,
    live: &RuntimeLive,
    mode: GuardProbe,
) -> Result<bool, DomainError> {
    if !guard_proof_capacity_available(canonical_base) {
        return Ok(false);
    }
    let execution_id = new_uuid()?;
    let directory = canonical_base
        .join("execution-guards")
        .join(live.boot_id.as_str());
    create_guard_directory(canonical_base, &directory)?;
    let proof_path = directory.join(format!("{}.proof", execution_id.as_str()));
    let marker_path = directory.join(format!("{}.ready", execution_id.as_str()));
    let mut proof_options = fs::OpenOptions::new();
    proof_options
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600);
    let mut proof = proof_options
        .open(&proof_path)
        .map_err(|_| io_error("cannot create root guard proof"))?;
    copy_canonical_metadata(&canonical_base.join("runtime-state.json"), &proof, 0o600)?;
    let identity = GuardIdentity {
        runtime_epoch: live.runtime_epoch.clone(),
        runtime_instance_id: live.runtime_instance_id.clone(),
        execution_id: execution_id.clone(),
        boot_id: live.boot_id.clone(),
    };
    proof
        .write_all(&encode_guard_frame(&identity)?)
        .map_err(|_| io_error("cannot write root guard proof"))?;
    proof
        .sync_all()
        .map_err(|_| io_error("cannot sync root guard proof"))?;

    let mut pipe = [0_i32; 2];
    if unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io_error("cannot create root guard lifetime pipe"));
    }
    let lifetime_read = unsafe { fs::File::from_raw_fd(pipe[0]) };
    let lifetime_write = unsafe { fs::File::from_raw_fd(pipe[1]) };
    make_inheritable(proof.as_raw_fd())?;
    make_inheritable(lifetime_read.as_raw_fd())?;
    let mut command = Command::new(guard_path);
    command
        .arg("--proof-fd")
        .arg(proof.as_raw_fd().to_string())
        .arg("--lifetime-fd")
        .arg(lifetime_read.as_raw_fd().to_string())
        .arg("--")
        .arg(guard_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    match mode {
        GuardProbe::ChildExit => {
            command.arg("--probe-child");
        }
        GuardProbe::OwnerLost => {
            command.arg("--probe-owner-death-child").arg(&marker_path);
        }
    }
    let mut child = command
        .spawn()
        .map_err(|_| io_error("cannot launch root guard probe"))?;
    drop(proof);
    drop(lifetime_read);
    let mut lifetime_write = Some(lifetime_write);
    if matches!(mode, GuardProbe::OwnerLost) {
        let marker_deadline = Instant::now() + Duration::from_secs(2);
        while !marker_path.exists() && Instant::now() < marker_deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if !marker_path.exists() {
            drop(lifetime_write.take());
            return Ok(false);
        }
        drop(lifetime_write.take());
    }
    let deadline = Instant::now() + Duration::from_secs(8);
    let exited = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|_| io_error("cannot observe root guard probe"))?
        {
            break status.success();
        }
        if Instant::now() >= deadline {
            drop(lifetime_write.take());
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    drop(lifetime_write.take());
    let bytes = GuardProofDirectory::new(canonical_base)
        .read_proof(&live.boot_id, &execution_id)?
        .unwrap_or_default();
    let recovery = classify_guard_proof(
        live.boot_id.as_str(),
        &live.boot_id,
        &identity,
        &bytes,
        &ProcFacts,
    )?;
    let expected = match mode {
        GuardProbe::ChildExit => GuardCleanCause::Exited,
        GuardProbe::OwnerLost => GuardCleanCause::OwnerLost,
    };
    let clean = matches!(
        recovery,
        GuardRecovery::Clean { clean: Some(ref value) } if value.cause == expected
    ) && exited;
    if clean {
        fs::remove_file(&proof_path)
            .map_err(|_| io_error("cannot remove clean root guard proof"))?;
        if marker_path.exists() {
            fs::remove_file(&marker_path)
                .map_err(|_| io_error("cannot remove root guard marker"))?;
        }
        sync_directory(&directory)?;
    }
    Ok(clean)
}

pub(crate) fn guard_proof_capacity_available(base: &Path) -> bool {
    let root = base.join("execution-guards");
    let boot_entries = match fs::read_dir(root) {
        Ok(entries) => match entries.collect::<Result<Vec<_>, _>>() {
            Ok(entries) => entries,
            Err(_) => return false,
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => return true,
        Err(_) => return false,
    };
    let mut count = 0_usize;
    let mut bytes = 0_u64;
    for boot_entry in boot_entries {
        let metadata = match fs::symlink_metadata(boot_entry.path()) {
            Ok(metadata) if metadata.is_dir() => metadata,
            _ => return false,
        };
        let _ = metadata;
        let entries = match fs::read_dir(boot_entry.path())
            .and_then(|entries| entries.collect::<Result<Vec<_>, _>>())
        {
            Ok(entries) => entries,
            Err(_) => return false,
        };
        for entry in entries {
            let name = match entry.file_name().into_string() {
                Ok(value) => value,
                Err(_) => return false,
            };
            if !name.ends_with(".proof") {
                continue;
            }
            let metadata = match fs::symlink_metadata(entry.path()) {
                Ok(metadata) if metadata.is_file() => metadata,
                _ => return false,
            };
            count = match count.checked_add(1) {
                Some(value) => value,
                None => return false,
            };
            bytes = match bytes.checked_add(metadata.len()) {
                Some(value) => value,
                None => return false,
            };
        }
    }
    count < MAX_GUARD_PROOF_FILES
        && bytes
            .checked_add(persistence::GUARD_PROOF_LIMIT_BYTES as u64)
            .is_some_and(|value| value <= MAX_GUARD_PROOF_BYTES)
}

pub(crate) fn create_guard_directory(base: &Path, directory: &Path) -> Result<(), DomainError> {
    let root = base.join("execution-guards");
    if !root.exists() {
        fs::create_dir(&root).map_err(|_| io_error("cannot create guard proof root"))?;
        let file = fs::File::open(&root).map_err(|_| io_error("cannot open guard proof root"))?;
        copy_canonical_metadata(base, &file, 0o700)?;
        sync_directory(base)?;
    }
    if !directory.exists() {
        fs::create_dir(directory).map_err(|_| io_error("cannot create guard boot directory"))?;
        let file =
            fs::File::open(directory).map_err(|_| io_error("cannot open guard boot directory"))?;
        copy_canonical_metadata(&root, &file, 0o700)?;
        sync_directory(&root)?;
    }
    sync_directory(directory)
}

pub(crate) fn copy_canonical_metadata(
    source: &Path,
    target: &fs::File,
    mode: u32,
) -> Result<(), DomainError> {
    let metadata =
        fs::metadata(source).map_err(|_| io_error("cannot inspect canonical metadata"))?;
    if unsafe { libc::fchown(target.as_raw_fd(), metadata.uid(), metadata.gid()) } != 0
        || unsafe { libc::fchmod(target.as_raw_fd(), mode) } != 0
    {
        return Err(io_error("cannot apply canonical owner or mode"));
    }
    #[cfg(target_os = "android")]
    copy_selinux_label(source, target)?;
    let actual = target
        .metadata()
        .map_err(|_| io_error("cannot verify canonical metadata"))?;
    if actual.uid() != metadata.uid()
        || actual.gid() != metadata.gid()
        || actual.mode() & 0o777 != mode
    {
        return Err(io_error("canonical metadata verification failed"));
    }
    Ok(())
}

#[cfg(target_os = "android")]
fn copy_selinux_label(source: &Path, target: &fs::File) -> Result<(), DomainError> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let source = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| io_error("canonical path is invalid"))?;
    let name = c"security.selinux";
    let length = unsafe { libc::getxattr(source.as_ptr(), name.as_ptr(), std::ptr::null_mut(), 0) };
    if length <= 0 {
        return Err(io_error("cannot read canonical SELinux label"));
    }
    let mut label = vec![0_u8; length as usize];
    let actual = unsafe {
        libc::getxattr(
            source.as_ptr(),
            name.as_ptr(),
            label.as_mut_ptr().cast(),
            label.len(),
        )
    };
    if actual != length
        || unsafe {
            libc::fsetxattr(
                target.as_raw_fd(),
                name.as_ptr(),
                label.as_ptr().cast(),
                label.len(),
                0,
            )
        } != 0
    {
        return Err(io_error("cannot copy canonical SELinux label"));
    }
    Ok(())
}

pub(crate) fn make_inheritable(fd: i32) -> Result<(), DomainError> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0 {
        Err(io_error("cannot make root guard descriptor inheritable"))
    } else {
        Ok(())
    }
}

pub(crate) fn sync_directory(path: &Path) -> Result<(), DomainError> {
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| io_error("cannot sync guard proof directory"))
}

pub(crate) struct ProcFacts;

impl ProcessFacts for ProcFacts {
    fn is_same_process(&self, pid: u32, start_ticks: u64) -> Result<bool, DomainError> {
        let path = PathBuf::from(format!("/proc/{pid}/stat"));
        match read_start_ticks(&path) {
            Ok(actual) => Ok(actual == start_ticks),
            Err(_) if !path.exists() => Ok(false),
            Err(error) => Err(error),
        }
    }
}

pub(crate) fn read_boot_id() -> Result<UuidV4, DomainError> {
    let value = fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map_err(|_| io_error("cannot read boot id"))?;
    UuidV4::parse(value.trim().to_owned()).map_err(|_| io_error("boot id is invalid"))
}

pub(crate) fn read_start_ticks(path: &Path) -> Result<u64, DomainError> {
    let stat = fs::read_to_string(path).map_err(|_| io_error("cannot read process identity"))?;
    let close = stat
        .rfind(')')
        .ok_or_else(|| io_error("process identity is invalid"))?;
    stat.get(close + 2..)
        .and_then(|tail| tail.split_whitespace().nth(19))
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| io_error("process start ticks are invalid"))
}

pub(crate) fn new_uuid() -> Result<UuidV4, DomainError> {
    UuidV4::parse(uuid::Uuid::new_v4().hyphenated().to_string())
        .map_err(|_| DomainError::new(ErrorCode::InternalError, "UUID generation failed"))
}

pub(crate) const fn io_error(reason: &'static str) -> DomainError {
    DomainError::new(ErrorCode::IoError, reason)
}

#[cfg(test)]
mod tests {
    use super::{MagiskGuardRecoveryMechanics, execute_guard_recovery};
    use contract::UuidV4;
    use domain::DomainError;
    use persistence::{
        CanonicalState, CleanGuardRecord, GuardProofReader, GuardProofRecord, ProcessFacts,
        await_guard_recovery_plan,
    };

    struct FailingMechanics;

    impl MagiskGuardRecoveryMechanics for FailingMechanics {
        fn finalize(&mut self, _record: &CleanGuardRecord) -> Result<(), DomainError> {
            Err(DomainError::invalid("metadata failure"))
        }

        fn finish(&mut self, _had_records: bool) -> Result<(), DomainError> {
            Err(DomainError::invalid("metadata failure"))
        }
    }

    struct EmptyProofs;

    impl GuardProofReader for EmptyProofs {
        fn read_proof(
            &self,
            _boot_id: &UuidV4,
            _execution_id: &UuidV4,
        ) -> Result<Option<Vec<u8>>, DomainError> {
            Ok(None)
        }

        fn list_proofs(&self) -> Result<Vec<GuardProofRecord>, DomainError> {
            Ok(Vec::new())
        }
    }

    struct NeverLive;

    impl ProcessFacts for NeverLive {
        fn is_same_process(&self, _pid: u32, _start_ticks: u64) -> Result<bool, DomainError> {
            Ok(false)
        }
    }

    fn id(value: u8) -> UuidV4 {
        UuidV4::parse(format!("00000000-0000-4000-8000-{value:012x}")).unwrap()
    }

    #[test]
    fn i7_g11_mechanics_failure_cannot_construct_cleanup_verification() {
        let plan = await_guard_recovery_plan(
            &CanonicalState::default(),
            &id(1),
            &id(2),
            &EmptyProofs,
            &NeverLive,
        )
        .unwrap();
        assert!(execute_guard_recovery(&plan, &mut FailingMechanics).is_err());
    }
}
