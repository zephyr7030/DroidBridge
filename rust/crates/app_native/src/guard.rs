//! The App-UID execution guard scope.
//!
//! One guarded process owns exactly one proof whose identity is the Runtime instance
//! that admitted it. The APK process writes those proofs while it is the Runtime host
//! and while it is the authenticated companion of the Magisk host, so one scope serves
//! both roles and a guarded command has a single proof-ownership path (S-AUTH-CMD-001,
//! S-EXEC-001).

use contract::{ErrorCode, UuidV4};
use domain::DomainError;
use serde::Serialize;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock, atomic::AtomicBool},
};

/// The addressed owner of every guard proof this process writes.
///
/// `base`, `boot_id`, `runtime_epoch`, and `runtime_instance_id` are only read from the
/// `unix` proof-writing path, which does not compile on a non-Unix host.
#[derive(Clone)]
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) struct GuardScope {
    base: PathBuf,
    boot_id: UuidV4,
    runtime_epoch: UuidV4,
    runtime_instance_id: UuidV4,
    guard_path: Arc<Mutex<Option<PathBuf>>>,
    quarantined: Arc<AtomicBool>,
}

/// The settlement of one guard proof, as the App surface reports it to its caller.
#[derive(Serialize)]
pub(crate) struct GuardSettlement {
    pub(crate) cleanup_verified: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) shell_exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cause: Option<&'static str>,
}

static GUARD_SCOPE: OnceLock<Mutex<Option<GuardScope>>> = OnceLock::new();

fn scope_slot() -> &'static Mutex<Option<GuardScope>> {
    GUARD_SCOPE.get_or_init(|| Mutex::new(None))
}

/// Publishes the scope this process is the guard owner under. The APK Runtime host
/// installs its own scope, and the companion adopts the Magisk host's instance identity
/// in its place, so a proof always names the Runtime instance that admitted it.
pub(crate) fn publish_scope(scope: GuardScope) -> Result<(), DomainError> {
    *scope_slot()
        .lock()
        .map_err(|_| DomainError::new(ErrorCode::InternalError, "guard scope lock failed"))? =
        Some(scope);
    Ok(())
}

pub(crate) fn scope() -> Result<GuardScope, DomainError> {
    scope_slot()
        .lock()
        .map_err(|_| DomainError::new(ErrorCode::InternalError, "guard scope lock failed"))?
        .clone()
        .ok_or_else(|| {
            DomainError::new(
                ErrorCode::CapabilityUnavailable,
                "execution guard scope is unavailable",
            )
        })
}

pub(crate) fn is_quarantined() -> bool {
    scope_slot()
        .lock()
        .ok()
        .and_then(|slot| slot.as_ref().map(GuardScope::quarantined))
        .unwrap_or(false)
}

impl GuardScope {
    pub(crate) fn new(
        base: PathBuf,
        runtime_epoch: UuidV4,
        runtime_instance_id: UuidV4,
    ) -> Result<Self, DomainError> {
        ignore_sigpipe();
        Ok(Self {
            base,
            boot_id: crate::read_boot_id()?,
            runtime_epoch,
            runtime_instance_id,
            guard_path: Arc::new(Mutex::new(None)),
            quarantined: Arc::new(AtomicBool::new(false)),
        })
    }

    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) fn base(&self) -> &std::path::Path {
        &self.base
    }

    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) fn boot_id(&self) -> &UuidV4 {
        &self.boot_id
    }

    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) fn runtime_epoch(&self) -> &UuidV4 {
        &self.runtime_epoch
    }

    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) fn runtime_instance_id(&self) -> &UuidV4 {
        &self.runtime_instance_id
    }

    pub(crate) fn quarantined(&self) -> bool {
        self.quarantined.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Binds the guard binary this process launches. The Runtime host binds it from the
    /// verified APK native library directory; the companion binds the same packaged
    /// binary it advertises to the Magisk host.
    pub(crate) fn bind_guard_path(&self, guard_path: PathBuf) {
        if let Ok(mut slot) = self.guard_path.lock() {
            *slot = Some(guard_path);
        }
    }

    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) fn guard_path(&self) -> Result<PathBuf, DomainError> {
        self.guard_path
            .lock()
            .map_err(|_| DomainError::new(ErrorCode::InternalError, "guard path lock failed"))?
            .clone()
            .ok_or_else(|| {
                DomainError::new(
                    ErrorCode::CapabilityUnavailable,
                    "execution guard binary is not bound",
                )
            })
    }

    pub(crate) fn quarantine(&self) {
        self.quarantined
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// A guarded command's input is a pipe this process writes, so a command that closes its
/// input early must not deliver a fatal signal to the Runtime process.
#[cfg(unix)]
fn ignore_sigpipe() {
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
}

#[cfg(not(unix))]
fn ignore_sigpipe() {}

#[cfg(unix)]
mod unix {
    use super::{GuardScope, GuardSettlement};
    use crate::{
        ProcFacts, block_guard_termination_signals, clear_cloexec, guard_proof_capacity_available,
        io_error, sync_directory,
    };
    use contract::{ErrorCode, UuidV4};
    use domain::DomainError;
    use persistence::{
        GuardIdentity, GuardProofDirectory, GuardProofReader, GuardRecovery, classify_guard_proof,
        encode_guard_frame,
    };
    use runtime::{
        CommandProcessCause, CommandProcessOutcome, CommandProcessRequest,
        CommandProcessSettlement, ExecutionFailure, LocalExecutionClaim,
    };
    use std::{
        fs,
        io::Write,
        os::{
            fd::{AsRawFd, FromRawFd, OwnedFd},
            unix::{fs::OpenOptionsExt, process::ExitStatusExt},
        },
        path::PathBuf,
        process::{Child, Command, Stdio},
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
            mpsc::{Receiver, Sender, channel},
        },
        thread,
        time::{Duration, Instant},
    };

    const CANCEL_SIGNAL: i32 = 15;
    const TIMEOUT_SIGNAL: i32 = 14;
    const KILL_SIGNAL: i32 = 9;
    /// The guard's own cleanup budget after a signal is five seconds; this is that
    /// budget plus one poll cycle, after which the guard itself is no longer trusted to
    /// report a verified cleanup.
    const GUARD_SETTLE_MS: u64 = 6_000;
    /// One stream reaches end-of-file when the guard and its group release the write
    /// ends they own, so this window normally closes on its own.
    const STREAM_EOF_WAIT_MS: u64 = 2_000;
    const STREAM_STOP_WAIT_MS: u64 = 500;
    const DRAIN_POLL_MS: i32 = 25;
    const REAP_POLL_MS: u64 = 20;
    const READ_CHUNK_BYTES: usize = 16_384;

    struct Reaped {
        exit_code: Option<i32>,
        timed_out: bool,
    }

    struct BoundedStream {
        bytes: Vec<u8>,
        truncated: bool,
    }

    impl GuardScope {
        fn directory(&self) -> PathBuf {
            self.base
                .join("execution-guards")
                .join(self.boot_id.as_str())
        }

        fn identity(&self, execution_id: &UuidV4) -> GuardIdentity {
            GuardIdentity {
                runtime_epoch: self.runtime_epoch.clone(),
                runtime_instance_id: self.runtime_instance_id.clone(),
                execution_id: execution_id.clone(),
                boot_id: self.boot_id.clone(),
            }
        }

        /// Writes the identity frame and returns the proof the guard must inherit. The
        /// header is the whole proof until the guard records a start, which is what
        /// makes an aborted proof distinguishable from an unsettled one.
        pub(crate) fn prepare(&self, execution_id: &UuidV4) -> Result<fs::File, DomainError> {
            if self.quarantined() || !guard_proof_capacity_available(&self.base) {
                return Err(DomainError::new(
                    ErrorCode::ResourceLimit,
                    "execution guard proof admission rejected",
                ));
            }
            let directory = self.directory();
            fs::create_dir_all(&directory).map_err(io_error)?;
            sync_directory(&directory)?;
            let identity = self.identity(execution_id);
            let header = encode_guard_frame(&identity)?;
            let proof_path = directory.join(format!("{}.proof", identity.execution_id.as_str()));
            let mut proof = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&proof_path)
                .map_err(io_error)?;
            if proof
                .write_all(&header)
                .and_then(|_| proof.sync_all())
                .is_err()
            {
                drop(proof);
                let _ = fs::remove_file(proof_path);
                let _ = sync_directory(&directory);
                return Err(DomainError::new(
                    ErrorCode::IoError,
                    "guard proof header write failed",
                ));
            }
            Ok(proof)
        }

        pub(crate) fn settle(&self, execution_id: &UuidV4) -> Result<GuardSettlement, DomainError> {
            let identity = self.identity(execution_id);
            let deadline = Instant::now() + Duration::from_millis(5_000);
            let recovery = loop {
                let bytes = GuardProofDirectory::new(&self.base)
                    .read_proof(&self.boot_id, execution_id)?
                    .unwrap_or_default();
                let recovery = classify_guard_proof(
                    self.boot_id.as_str(),
                    &self.boot_id,
                    &identity,
                    &bytes,
                    &ProcFacts,
                )?;
                if matches!(recovery, GuardRecovery::Clean { .. }) || Instant::now() >= deadline {
                    break recovery;
                }
                thread::sleep(Duration::from_millis(REAP_POLL_MS));
            };
            let result = match recovery {
                GuardRecovery::Clean { clean: Some(clean) } => GuardSettlement {
                    cleanup_verified: true,
                    shell_exit_code: clean.shell_exit_code,
                    cause: Some(guard_cause_token(clean.cause)),
                },
                GuardRecovery::Clean { clean: None }
                | GuardRecovery::Live { .. }
                | GuardRecovery::Unverified => GuardSettlement {
                    cleanup_verified: false,
                    shell_exit_code: None,
                    cause: None,
                },
            };
            if result.cleanup_verified {
                let directory = self.directory();
                fs::remove_file(directory.join(format!("{}.proof", execution_id.as_str())))
                    .map_err(io_error)?;
                sync_directory(&directory)?;
            } else {
                self.quarantine();
            }
            Ok(result)
        }

        /// Retires a proof whose guard never recorded a start. Only exact header bytes
        /// prove that, so a guard that already started is never retired here.
        pub(crate) fn abort(&self, execution_id: &UuidV4) -> Result<bool, DomainError> {
            let identity = self.identity(execution_id);
            let directory = self.directory();
            let path = directory.join(format!("{}.proof", execution_id.as_str()));
            let bytes = fs::read(&path).map_err(io_error)?;
            if bytes != encode_guard_frame(&identity)? {
                return Ok(false);
            }
            fs::remove_file(path).map_err(io_error)?;
            sync_directory(&directory)?;
            Ok(true)
        }

        /// Runs one already-admitted command under the guard as this process's own
        /// identity. The guard owns process-group confinement, the deadline signal and
        /// the reaping, so the retained streams and the cleanup fact have one owner.
        pub(crate) fn run_command(
            &self,
            execution_id: &UuidV4,
            request: CommandProcessRequest,
            claim: &LocalExecutionClaim,
        ) -> Result<CommandProcessSettlement, ExecutionFailure> {
            let guard_path = self.guard_path().map_err(pre_start)?;
            let (stdout_read, stdout_write) = pipe()?;
            let (stderr_read, stderr_write) = pipe()?;
            let (stdin_child, stdin_source) = match request.stdin.as_ref() {
                Some(bytes) => {
                    let (read, write) = pipe()?;
                    (read, Some((write, bytes.clone())))
                }
                None => (
                    fs::File::open("/dev/null")
                        .map_err(io_error)
                        .map_err(pre_start)?,
                    None,
                ),
            };
            let (lifetime_read, lifetime_write) = pipe()?;
            let proof = self.prepare(execution_id).map_err(pre_start)?;
            let proof_raw = proof.as_raw_fd();
            let lifetime_raw = lifetime_read.as_raw_fd();

            let mut command = Command::new(&guard_path);
            command
                .arg("--proof-fd")
                .arg(proof_raw.to_string())
                .arg("--lifetime-fd")
                .arg(lifetime_raw.to_string())
                .arg("--")
                .arg(&request.program)
                .args(&request.arguments)
                .stdin(Stdio::from(stdin_child))
                .stdout(Stdio::from(stdout_write))
                .stderr(Stdio::from(stderr_write))
                .current_dir(request.cwd.as_deref().unwrap_or("/"));
            unsafe {
                use std::os::unix::process::CommandExt;
                command.pre_exec(move || {
                    block_guard_termination_signals()?;
                    clear_cloexec(proof_raw)?;
                    clear_cloexec(lifetime_raw)?;
                    Ok(())
                });
            }
            let mut child = match command.spawn() {
                Ok(child) => child,
                Err(error) => {
                    drop(proof);
                    drop(lifetime_read);
                    drop(lifetime_write);
                    return Err(self.unstarted(execution_id, error));
                }
            };
            // `Command` retains the configured `Stdio` handles so it can be spawned
            // again. Keeping it alive here keeps this process's stdout/stderr write
            // ends open after the guard exits, forcing every collector to wait for its
            // EOF deadline. This run has exactly one spawn, so release those duplicate
            // handles as soon as the child owns its copies.
            drop(command);
            let pid = match i32::try_from(child.id()) {
                Ok(pid) => pid,
                Err(_) => {
                    drop(proof);
                    drop(lifetime_read);
                    drop(lifetime_write);
                    return Err(self.unstarted(
                        execution_id,
                        std::io::Error::other("command guard PID is invalid"),
                    ));
                }
            };
            drop(proof);
            drop(lifetime_read);
            let mut lifetime_write = Some(lifetime_write);

            let limit = usize::try_from(request.max_output_bytes).unwrap_or(usize::MAX);
            let stop = Arc::new(AtomicBool::new(false));
            let (sender, receiver) = channel();
            let stdout_reader =
                spawn_drainer(stdout_read, limit, Arc::clone(&stop), false, sender.clone());
            let stderr_reader = spawn_drainer(stderr_read, limit, Arc::clone(&stop), true, sender);
            let stdin_writer = stdin_source.map(|(mut writer, bytes)| {
                thread::spawn(move || {
                    // A command that closes its input early is not a failure.
                    let _ = writer.write_all(&bytes);
                })
            });

            let reaped = self.reap(&mut child, pid, &request, claim);
            drop(lifetime_write.take());
            let streams = collect_streams(receiver, &stop);
            if let Some(writer) = stdin_writer {
                let _ = writer.join();
            }
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();

            let settlement = self
                .settle(execution_id)
                .map_err(|error| ExecutionFailure {
                    error,
                    cleanup_verified: false,
                })?;
            let reaped = reaped.map_err(|failure| ExecutionFailure {
                error: failure.error,
                cleanup_verified: settlement.cleanup_verified,
            })?;
            let (stdout, stderr) = streams.map_err(|error| ExecutionFailure {
                error,
                cleanup_verified: settlement.cleanup_verified,
            })?;
            let cause = match settlement.cause {
                Some("cancelled") => CommandProcessCause::Cancelled,
                Some("timeout") => CommandProcessCause::Timeout,
                Some("owner_lost") => CommandProcessCause::OwnerLost,
                _ if reaped.timed_out => CommandProcessCause::Timeout,
                _ => CommandProcessCause::Exited,
            };
            let exit_code = (cause == CommandProcessCause::Exited)
                .then(|| settlement.shell_exit_code.or(reaped.exit_code))
                .flatten();
            Ok(CommandProcessSettlement {
                outcome: CommandProcessOutcome {
                    cause,
                    exit_code,
                    stdout: stdout.bytes,
                    stdout_truncated: stdout.truncated,
                    stderr: stderr.bytes,
                    stderr_truncated: stderr.truncated,
                },
                cleanup_verified: settlement.cleanup_verified,
            })
        }

        /// Waits for the guard while it stays reachable for cancellation and for the
        /// deadline. The guard turns both into verified cleanup, so this only escalates
        /// when the guard itself stops answering.
        fn reap(
            &self,
            child: &mut Child,
            pid: i32,
            request: &CommandProcessRequest,
            claim: &LocalExecutionClaim,
        ) -> Result<Reaped, ExecutionFailure> {
            let deadline = Instant::now() + Duration::from_millis(request.timeout_ms);
            let mut signalled: Option<Instant> = None;
            let mut timed_out = false;
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        return Ok(Reaped {
                            exit_code: status.code().or_else(|| status.signal().map(|s| 128 + s)),
                            timed_out,
                        });
                    }
                    Ok(None) => {}
                    Err(error) => return Err(pre_start(io_error(error))),
                }
                let now = Instant::now();
                if signalled.is_none() {
                    let cancelled = claim.checkpoint().is_err();
                    if cancelled || now >= deadline {
                        timed_out = !cancelled;
                        signal(
                            pid,
                            if cancelled {
                                CANCEL_SIGNAL
                            } else {
                                TIMEOUT_SIGNAL
                            },
                        );
                        signalled = Some(now);
                    }
                }
                if signalled.is_some_and(|at| {
                    now.duration_since(at) >= Duration::from_millis(GUARD_SETTLE_MS)
                }) {
                    // The guard owns the group and the proof; a guard that ignores its
                    // own budget can no longer report verified cleanup for this run.
                    signal(pid, KILL_SIGNAL);
                    let _ = child.wait();
                    return Ok(Reaped {
                        exit_code: None,
                        timed_out,
                    });
                }
                thread::sleep(Duration::from_millis(REAP_POLL_MS));
            }
        }

        /// Reports a pre-start failure. A proof that is still exactly its identity frame
        /// proves no process was started, so the failure is verified; anything else has
        /// to settle before it is trusted.
        fn unstarted(&self, execution_id: &UuidV4, error: std::io::Error) -> ExecutionFailure {
            let aborted = self.abort(execution_id).unwrap_or(false);
            let cleanup_verified = aborted
                || self
                    .settle(execution_id)
                    .map(|settlement| settlement.cleanup_verified)
                    .unwrap_or(false);
            ExecutionFailure {
                error: io_error(error),
                cleanup_verified,
            }
        }
    }

    const fn guard_cause_token(cause: persistence::GuardCleanCause) -> &'static str {
        match cause {
            persistence::GuardCleanCause::Exited => "exited",
            persistence::GuardCleanCause::Cancelled => "cancelled",
            persistence::GuardCleanCause::Timeout => "timeout",
            persistence::GuardCleanCause::OwnerLost => "owner_lost",
        }
    }

    fn pre_start(error: DomainError) -> ExecutionFailure {
        ExecutionFailure {
            error,
            cleanup_verified: true,
        }
    }

    fn pipe() -> Result<(fs::File, fs::File), ExecutionFailure> {
        let mut ends = [0_i32; 2];
        if unsafe { libc::pipe2(ends.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            return Err(pre_start(io_error(std::io::Error::last_os_error())));
        }
        Ok(unsafe {
            (
                fs::File::from_raw_fd(ends[0]),
                fs::File::from_raw_fd(ends[1]),
            )
        })
    }

    fn signal(pid: i32, signal: i32) {
        let _ = unsafe { libc::kill(pid, signal) };
    }

    fn spawn_drainer(
        file: fs::File,
        limit: usize,
        stop: Arc<AtomicBool>,
        is_stderr: bool,
        sender: Sender<(bool, Result<BoundedStream, DomainError>)>,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            let _ = sender.send((is_stderr, drain(file, limit, &stop)));
        })
    }

    /// Retains the bounded prefix of one stream and keeps reading to end-of-file so the
    /// writer is never blocked by this process.
    fn drain(
        file: fs::File,
        limit: usize,
        stop: &AtomicBool,
    ) -> Result<BoundedStream, DomainError> {
        let fd = file.as_raw_fd();
        let mut bytes = Vec::with_capacity(limit.min(READ_CHUNK_BYTES));
        let mut truncated = false;
        let mut buffer = vec![0_u8; READ_CHUNK_BYTES];
        loop {
            let stopped = stop.load(Ordering::Acquire);
            let mut descriptor = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            let ready =
                unsafe { libc::poll(&mut descriptor, 1, if stopped { 0 } else { DRAIN_POLL_MS }) };
            if ready < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(io_error(error));
            }
            if ready == 0 {
                if stopped {
                    return Ok(BoundedStream { bytes, truncated });
                }
                continue;
            }
            let count =
                unsafe { libc::read(fd, buffer.as_mut_ptr().cast::<libc::c_void>(), buffer.len()) };
            if count < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(io_error(error));
            }
            if count == 0 {
                return Ok(BoundedStream { bytes, truncated });
            }
            let count = usize::try_from(count).unwrap_or(buffer.len());
            let remaining = limit.saturating_sub(bytes.len());
            if remaining > 0 {
                let take = remaining.min(count);
                bytes.extend_from_slice(&buffer[..take]);
            }
            if count > remaining {
                truncated = true;
            }
        }
    }

    /// Collects both retained streams. A verified cleanup means the guard's group is
    /// gone, so each write end is closed and this window closes on its own; the stop
    /// flag only bounds the case where a stream outlives its owner, where the settle
    /// step already reports unverified cleanup and these bytes are never used.
    fn collect_streams(
        receiver: Receiver<(bool, Result<BoundedStream, DomainError>)>,
        stop: &AtomicBool,
    ) -> Result<(BoundedStream, BoundedStream), DomainError> {
        let mut stdout = None;
        let mut stderr = None;
        let mut deadline = Instant::now() + Duration::from_millis(STREAM_EOF_WAIT_MS);
        let mut forced = false;
        while stdout.is_none() || stderr.is_none() {
            let now = Instant::now();
            if now >= deadline {
                if forced {
                    return Err(DomainError::new(
                        ErrorCode::IoError,
                        "command output did not reach end-of-file",
                    ));
                }
                stop.store(true, Ordering::Release);
                forced = true;
                deadline = now + Duration::from_millis(STREAM_STOP_WAIT_MS);
            }
            let wait = deadline
                .saturating_duration_since(Instant::now())
                .max(Duration::from_millis(1));
            match receiver.recv_timeout(wait) {
                Ok((true, Ok(stream))) => stderr = Some(stream),
                Ok((false, Ok(stream))) => stdout = Some(stream),
                Ok((_, Err(error))) => {
                    stop.store(true, Ordering::Release);
                    return Err(error);
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    stop.store(true, Ordering::Release);
                    return Err(DomainError::new(
                        ErrorCode::IoError,
                        "command output reader stopped without a result",
                    ));
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
        match (stdout, stderr) {
            (Some(stdout), Some(stderr)) => Ok((stdout, stderr)),
            _ => Err(DomainError::new(
                ErrorCode::InternalError,
                "command output collection ended without both streams",
            )),
        }
    }

    /// Runs one already-admitted command as this process's own identity.
    pub(crate) fn run_guarded_command(
        execution_id: &UuidV4,
        request: CommandProcessRequest,
        claim: &LocalExecutionClaim,
    ) -> Result<CommandProcessSettlement, ExecutionFailure> {
        super::scope()
            .map_err(pre_start)?
            .run_command(execution_id, request, claim)
    }

    pub(crate) fn settle_proof(execution_id: &UuidV4) -> Result<GuardSettlement, DomainError> {
        super::scope()?.settle(execution_id)
    }

    pub(crate) fn abort_proof(execution_id: &UuidV4) -> Result<bool, DomainError> {
        super::scope()?.abort(execution_id)
    }

    pub(crate) fn prepare_proof(execution_id: &UuidV4) -> Result<fs::File, DomainError> {
        super::scope()?.prepare(execution_id)
    }

    /// The bounded command input as the descriptor a guarded process reads, plus the
    /// writer that feeds it. The reader end is handed to the process owner, which closes
    /// it, so this side only ever owns the writer.
    pub(crate) struct StdinFeed {
        descriptor: Option<OwnedFd>,
        writer: Option<thread::JoinHandle<()>>,
    }

    impl StdinFeed {
        pub(crate) fn open(bytes: Option<Vec<u8>>) -> Result<Self, ExecutionFailure> {
            let Some(bytes) = bytes else {
                return Ok(Self {
                    descriptor: None,
                    writer: None,
                });
            };
            let (read, mut writer) = pipe()?;
            let descriptor = OwnedFd::from(read);
            let feed = thread::spawn(move || {
                // A command that closes its input early is not a failure.
                let _ = writer.write_all(&bytes);
            });
            Ok(Self {
                descriptor: Some(descriptor),
                writer: Some(feed),
            })
        }

        /// Transfers the reader end to the process owner.
        pub(crate) fn descriptor(&self) -> Option<i32> {
            self.descriptor
                .as_ref()
                .map(std::os::fd::AsRawFd::as_raw_fd)
        }

        /// Waits for the input to be written. It is bounded because the process owner
        /// closes the reader end when the command settles.
        pub(crate) fn finish(mut self) {
            drop(self.descriptor.take());
            if let Some(writer) = self.writer.take() {
                let _ = writer.join();
            }
        }
    }
}

#[cfg(unix)]
pub(crate) use unix::{StdinFeed, abort_proof, prepare_proof, run_guarded_command, settle_proof};

// The guard is a Unix process primitive; a process without it cannot admit a command,
// so every entry point reports the capability instead of degrading to another runner.
#[cfg(not(unix))]
mod fallback {
    use super::GuardSettlement;
    use contract::{ErrorCode, UuidV4};
    use domain::DomainError;
    use runtime::{CommandProcessRequest, CommandProcessSettlement, ExecutionFailure};

    fn unavailable() -> DomainError {
        DomainError::new(
            ErrorCode::CapabilityUnavailable,
            "execution guard requires Unix",
        )
    }

    pub(crate) fn run_guarded_command(
        _execution_id: &UuidV4,
        _request: CommandProcessRequest,
        _claim: &runtime::LocalExecutionClaim,
    ) -> Result<CommandProcessSettlement, ExecutionFailure> {
        Err(ExecutionFailure {
            error: unavailable(),
            cleanup_verified: true,
        })
    }

    #[allow(dead_code)] // only reachable via the #[cfg(unix)] JNI entry point.
    pub(crate) fn prepare_proof(_execution_id: &UuidV4) -> Result<std::fs::File, DomainError> {
        Err(unavailable())
    }

    pub(crate) fn settle_proof(_execution_id: &UuidV4) -> Result<GuardSettlement, DomainError> {
        Ok(GuardSettlement {
            cleanup_verified: false,
            shell_exit_code: None,
            cause: None,
        })
    }

    pub(crate) fn abort_proof(_execution_id: &UuidV4) -> Result<bool, DomainError> {
        Ok(false)
    }
}

#[cfg(not(unix))]
#[allow(unused_imports)] // prepare_proof is only called from the #[cfg(unix)] JNI entry point.
pub(crate) use fallback::{abort_proof, prepare_proof, run_guarded_command, settle_proof};
