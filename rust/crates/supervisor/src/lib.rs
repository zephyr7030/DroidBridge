use std::time::Duration;

pub mod stderr_log;

const RESTART_DELAYS_SECONDS: [u64; 6] = [1, 2, 4, 8, 16, 30];
const CLEAN_RUN: Duration = Duration::from_secs(300);

#[derive(Default)]
pub struct RestartBackoff {
    failure_index: usize,
}

impl RestartBackoff {
    pub fn after_run(&mut self, run_time: Duration) -> Duration {
        if run_time >= CLEAN_RUN {
            self.failure_index = 0;
        }
        let delay = Duration::from_secs(
            RESTART_DELAYS_SECONDS[self.failure_index.min(RESTART_DELAYS_SECONDS.len() - 1)],
        );
        self.failure_index = (self.failure_index + 1).min(RESTART_DELAYS_SECONDS.len() - 1);
        delay
    }
}

#[cfg(unix)]
pub mod process {
    use super::{RestartBackoff, stderr_log};
    use chrono::{SecondsFormat, Utc};
    use contract::{ErrorCode, UuidV4};
    use domain::DomainError;
    use persistence::{FaultFileStore, FaultRecord, FaultRole};
    use std::{
        fs,
        os::unix::process::{CommandExt, ExitStatusExt},
        path::{Path, PathBuf},
        process::{Command, ExitCode, Stdio},
        thread,
        time::Instant,
    };

    pub fn main() -> ExitCode {
        match run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::from(1),
        }
    }

    fn run() -> Result<(), DomainError> {
        let executable = fs::canonicalize(
            std::env::current_exe().map_err(|_| error("cannot resolve supervisor executable"))?,
        )
        .map_err(|_| error("cannot canonicalize supervisor executable"))?;
        let module_root = executable
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| error("supervisor is outside a module root"))?
            .to_path_buf();
        if executable != module_root.join("bin/droidbridge-supervisor") {
            return Err(DomainError::new(
                ErrorCode::PermissionDenied,
                "supervisor executable is outside its module root",
            ));
        }
        let package = match module_root.file_name().and_then(|value| value.to_str()) {
            Some("droidbridge") => "com.droidbridge.android",
            Some("droidbridge_debug") => "com.droidbridge.android.debug",
            _ => {
                return Err(DomainError::new(
                    ErrorCode::PermissionDenied,
                    "supervisor module identity is invalid",
                ));
            }
        };
        let daemon = fs::canonicalize(module_root.join("bin/droidbridged"))
            .map_err(|_| error("cannot resolve daemon executable"))?;
        if daemon != module_root.join("bin/droidbridged") {
            return Err(DomainError::new(
                ErrorCode::PermissionDenied,
                "daemon executable escapes its module root",
            ));
        }
        let canonical_base = PathBuf::from("/data/user_de/0")
            .join(package)
            .join("files/droidbridge");
        // The daemon's stderr outlives the process that wrote it: it is the only record of a panic,
        // which the fault store reports as an exit code alone. The log lives in the App's canonical
        // base, which the App alone creates, so stderr is kept only once that base exists.
        let log_directory = canonical_base.is_dir().then(|| canonical_base.join("logs"));
        let mut backoff = RestartBackoff::default();
        loop {
            let started = Instant::now();
            let mut command = Command::new(&daemon);
            command
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(if log_directory.is_some() {
                    Stdio::piped()
                } else {
                    Stdio::null()
                });
            unsafe {
                command.pre_exec(|| {
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let mut child = match command.spawn() {
                Ok(child) => child,
                Err(spawn_error) => {
                    // A daemon that never started writes no stderr and leaves no fault record, so
                    // this line is the whole record of the failure.
                    if let Some(directory) = &log_directory {
                        let _ = stderr_log::append(
                            directory,
                            format!(
                                "droidbridge-supervisor: cannot spawn {}: {spawn_error}\n",
                                daemon.display()
                            )
                            .as_bytes(),
                        );
                    }
                    return Err(error("cannot supervise daemon"));
                }
            };
            let draining =
                child
                    .stderr
                    .take()
                    .zip(log_directory.clone())
                    .map(|(stderr, directory)| {
                        thread::spawn(move || stderr_log::drain(stderr, &directory))
                    });
            let status = child.wait().map_err(|_| error("cannot supervise daemon"))?;
            if let Some(draining) = draining {
                let _ = draining.join();
            }
            if canonical_base.is_dir() {
                let now = Utc::now();
                let record = FaultRecord {
                    record_id: new_uuid()?,
                    at: now.to_rfc3339_opts(SecondsFormat::Millis, true),
                    component: "droidbridge-supervisor".to_owned(),
                    code: "DAEMON_EXIT".to_owned(),
                    phase: "supervise".to_owned(),
                    product_version: env!("CARGO_PKG_VERSION").to_owned(),
                    boot_id: read_boot_id()?,
                    runtime_instance_id: None,
                    execution_id: None,
                    exit_code: status.code(),
                    signal: status.signal(),
                    repeat_count: 1,
                };
                let now_ms = u64::try_from(now.timestamp_millis())
                    .map_err(|_| error("supervisor clock is invalid"))?;
                let _ = FaultFileStore::new(&canonical_base, FaultRole::Supervisor)
                    .append(record, now_ms);
            }
            thread::sleep(backoff.after_run(started.elapsed()));
        }
    }

    fn read_boot_id() -> Result<UuidV4, DomainError> {
        let value = fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .map_err(|_| error("cannot read boot id"))?;
        UuidV4::parse(value.trim().to_owned()).map_err(|_| error("boot id is invalid"))
    }

    fn new_uuid() -> Result<UuidV4, DomainError> {
        UuidV4::parse(uuid::Uuid::new_v4().hyphenated().to_string())
            .map_err(|_| error("UUID generation failed"))
    }

    const fn error(reason: &'static str) -> DomainError {
        DomainError::new(ErrorCode::IoError, reason)
    }
}
