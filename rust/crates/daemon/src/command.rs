//! The Magisk execution surface's Command process port.
//!
//! It realizes exactly the identities S-AUTH-CMD-001 gives this surface: `root` through
//! this daemon's own guard runner, and `app`/`shell` forwarded to the authenticated APK
//! execution surface that owns them. The daemon never impersonates an App or shell
//! identity, and Shizuku stays a process primitive inside the APK surface rather than
//! becoming a third Command implementation.

use crate::companion::{CompanionPort, CompanionPrimitiveRequest};
use crate::magisk_guard_recovery::{
    ProcFacts, create_guard_directory, guard_proof_capacity_available, io_error, sync_directory,
};
use contract::{ErrorCode, RunAs, UuidV4};
use domain::DomainError;
use persistence::{
    GuardCleanCause, GuardIdentity, GuardProofDirectory, GuardProofReader, GuardRecovery,
    classify_guard_proof, encode_guard_frame,
};
use runtime::{
    AdmittedExecution, AndroidCommandSettlement, CommandProcessCause, CommandProcessOutcome,
    CommandProcessPort, CommandProcessRequest, CommandProcessSettlement, ExecutionFailure,
    LocalExecutionClaim, android_command_failure, validate_command_process_request,
};
use serde_json::{Value, json};
use std::{
    fs, io,
    io::Write,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    os::unix::{
        fs::OpenOptionsExt,
        process::{CommandExt, ExitStatusExt},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, Sender, channel},
    },
    thread,
    time::{Duration, Instant},
};

/// The guard's own cleanup budget after a signal is five seconds; this is that budget
/// plus one poll cycle, after which the guard is no longer trusted to report a verified
/// cleanup.
const GUARD_SETTLE_MS: u64 = 6_000;
/// S-IPC-DAEMON-005 install bound; guard cleanup follows it.
const MAINTENANCE_INSTALL_TIMEOUT_MS: u64 = 300_000;
const CANCEL_SIGNAL: i32 = 15;
const TIMEOUT_SIGNAL: i32 = 14;
const KILL_SIGNAL: i32 = 9;
/// One stream reaches end-of-file when the guard and its group release the write ends
/// they own, so this window normally closes on its own.
const STREAM_EOF_WAIT_MS: u64 = 2_000;
const STREAM_STOP_WAIT_MS: u64 = 500;
const DRAIN_POLL_MS: i32 = 25;
const REAP_POLL_MS: u64 = 20;
const READ_CHUNK_BYTES: usize = 16_384;
/// A forwarded identity settles inside the APK surface, whose own cleanup window is
/// bounded; this is that window plus transport slack, after which the daemon has no
/// settlement to report and says so instead of inventing one.
const FORWARD_SETTLE_MARGIN_MS: u64 = 10_000;
const FORWARD_POLL_MS: Duration = Duration::from_millis(25);

/// The daemon's own record that an execution could not prove descendant cleanup. The
/// Runtime host observes it and withdraws the root execution capabilities, so no later
/// admission starts new side effects while the uncertainty stands (S-AUTH-001).
#[derive(Default)]
pub(crate) struct CommandQuarantine {
    flagged: AtomicBool,
}

impl CommandQuarantine {
    pub(crate) fn observe(&self) {
        self.flagged.store(true, Ordering::Release);
    }

    pub(crate) fn is_flagged(&self) -> bool {
        self.flagged.load(Ordering::Acquire)
    }
}

/// The daemon's root Command runners. One scope owns the guard proof directory, the
/// packaged guard and the runtime identity its proofs are framed with, so a root command
/// has exactly one owner of its process group, its deadline and its cleanup proof.
pub(crate) struct RootCommandGuard {
    base: PathBuf,
    guard_path: PathBuf,
    runtime_epoch: UuidV4,
    runtime_instance_id: UuidV4,
    boot_id: UuidV4,
    quarantine: Arc<CommandQuarantine>,
}

/// The fixed S-MAGISK-005 package-shell commands.
pub(crate) enum PackageRootPrimitive {
    ListThirdParty,
    ListSystem,
    ForceStop(String),
}

/// The closed visual command vocabulary executed by the daemon's root guard. Public
/// Visual input is converted into these fixed programs and argument positions before
/// this boundary; no caller-supplied shell command reaches it.
pub(crate) enum VisualRootPrimitive {
    ScreenshotPng,
    HierarchyDump(PathBuf),
    Tap {
        x: u32,
        y: u32,
    },
    LongPress {
        x: u32,
        y: u32,
    },
    Swipe {
        from_x: u32,
        from_y: u32,
        to_x: u32,
        to_y: u32,
        duration_ms: u64,
    },
    Text(String),
    /// `input keycombination`: the modifier keycodes held while the final key is pressed.
    KeyCombination(Vec<i32>),
    Key(i32),
}

impl RootCommandGuard {
    pub(crate) fn new(
        base: PathBuf,
        guard_path: PathBuf,
        runtime_epoch: UuidV4,
        runtime_instance_id: UuidV4,
        boot_id: UuidV4,
        quarantine: Arc<CommandQuarantine>,
    ) -> Self {
        Self {
            base,
            guard_path,
            runtime_epoch,
            runtime_instance_id,
            boot_id,
            quarantine,
        }
    }

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
    /// header is the whole proof until the guard records a start, which is what makes an
    /// aborted proof distinguishable from an unsettled one.
    fn prepare(&self, execution_id: &UuidV4) -> Result<fs::File, ExecutionFailure> {
        if self.quarantine.is_flagged() || !guard_proof_capacity_available(&self.base) {
            return Err(pre_start(DomainError::new(
                ErrorCode::ResourceLimit,
                "root guard proof admission rejected",
            )));
        }
        let directory = self.directory();
        create_guard_directory(&self.base, &directory).map_err(pre_start)?;
        let identity = self.identity(execution_id);
        let proof_path = directory.join(format!("{}.proof", identity.execution_id.as_str()));
        let mut proof = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&proof_path)
            .map_err(|_| pre_start(io_error("cannot create root guard proof")))?;
        crate::magisk_guard_recovery::copy_canonical_metadata(
            &self.base.join("runtime-state.json"),
            &proof,
            0o600,
        )
        .map_err(pre_start)?;
        if proof
            .write_all(&encode_guard_frame(&identity).map_err(pre_start)?)
            .and_then(|_| proof.sync_all())
            .is_err()
        {
            drop(proof);
            let _ = fs::remove_file(&proof_path);
            let _ = sync_directory(&directory);
            return Err(pre_start(DomainError::new(
                ErrorCode::IoError,
                "root guard proof header write failed",
            )));
        }
        Ok(proof)
    }

    /// Reads the guard's own durable verdict for one execution. The proof is the only
    /// authority: a missing or partial frame is unverified, never a clean run.
    fn settle(&self, execution_id: &UuidV4) -> Result<GuardSettlement, DomainError> {
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
                cause: Some(clean.cause),
            },
            GuardRecovery::Clean { clean: None }
            | GuardRecovery::Live { .. }
            | GuardRecovery::Unverified => GuardSettlement {
                cleanup_verified: false,
                shell_exit_code: None,
                cause: None,
            },
        };
        let directory = self.directory();
        if result.cleanup_verified {
            fs::remove_file(directory.join(format!("{}.proof", execution_id.as_str())))
                .map_err(|_| io_error("cannot remove clean root guard proof"))?;
            sync_directory(&directory)?;
        } else {
            self.quarantine.observe();
        }
        Ok(result)
    }

    /// Retires a proof whose guard never recorded a start. Only exact header bytes prove
    /// that, so a guard that already started is never retired here.
    fn abort(&self, execution_id: &UuidV4) -> Result<bool, DomainError> {
        let identity = self.identity(execution_id);
        let directory = self.directory();
        let path = directory.join(format!("{}.proof", execution_id.as_str()));
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(_) => return Err(io_error("cannot read root guard proof")),
        };
        if bytes != encode_guard_frame(&identity)? {
            return Ok(false);
        }
        fs::remove_file(path).map_err(|_| io_error("cannot retire root guard proof"))?;
        sync_directory(&directory)?;
        Ok(true)
    }

    /// Reports a pre-start failure. A proof that is still exactly its identity frame
    /// proves no process was started, so the failure is verified; anything else has to
    /// settle before it is trusted.
    fn unstarted(&self, execution_id: &UuidV4, reason: DomainError) -> ExecutionFailure {
        let aborted = self.abort(execution_id).unwrap_or(false);
        let cleanup_verified = aborted
            || self
                .settle(execution_id)
                .map(|settlement| settlement.cleanup_verified)
                .unwrap_or(false);
        ExecutionFailure {
            error: reason,
            cleanup_verified,
        }
    }

    /// Runs one already-admitted command as this daemon's root identity. The guard owns
    /// process-group confinement, the deadline signal and the reaping, so the retained
    /// streams and the cleanup fact have one owner.
    fn run(
        &self,
        execution_id: &UuidV4,
        request: CommandProcessRequest,
        claim: &LocalExecutionClaim,
    ) -> Result<CommandProcessSettlement, ExecutionFailure> {
        self.run_with_stdout(execution_id, request, claim, None)
    }

    /// Runs one fixed S-UPD-002/003 self-maintenance installer argument array. The installer
    /// exit code is reported but never taken as installed-state truth.
    pub(crate) fn run_maintenance_install(
        &self,
        execution_id: &UuidV4,
        program: &str,
        arguments: Vec<String>,
        claim: &LocalExecutionClaim,
    ) -> Result<CommandProcessSettlement, ExecutionFailure> {
        self.run_with_stdout(
            execution_id,
            CommandProcessRequest {
                run_as: RunAs::Root,
                program: program.to_owned(),
                arguments,
                cwd: Some("/".to_owned()),
                stdin: None,
                timeout_ms: MAINTENANCE_INSTALL_TIMEOUT_MS,
                max_output_bytes: 65_536,
            },
            claim,
            None,
        )
    }

    /// Runs one fixed S-MAGISK-005 package-shell argument array and returns its bounded
    /// standard output. Package names are argument positions, never shell text.
    pub(crate) fn run_package(
        &self,
        execution_id: &UuidV4,
        primitive: PackageRootPrimitive,
        claim: &LocalExecutionClaim,
    ) -> Result<Vec<u8>, ExecutionFailure> {
        let list = |selector: &str| {
            [
                "package",
                "list",
                "packages",
                selector,
                "--show-versioncode",
                "--user",
                "0",
            ]
            .map(str::to_owned)
            .to_vec()
        };
        let (program, arguments, timeout_ms, max_output_bytes) = match primitive {
            PackageRootPrimitive::ListThirdParty => (
                "/system/bin/cmd",
                list("-3"),
                runtime::ANDROID_PACKAGE_LIST_TIMEOUT_MS,
                runtime::ANDROID_PACKAGE_LIST_MAX_STDOUT_BYTES as u64,
            ),
            PackageRootPrimitive::ListSystem => (
                "/system/bin/cmd",
                list("-s"),
                runtime::ANDROID_PACKAGE_LIST_TIMEOUT_MS,
                runtime::ANDROID_PACKAGE_LIST_MAX_STDOUT_BYTES as u64,
            ),
            PackageRootPrimitive::ForceStop(package_name) => (
                "/system/bin/am",
                vec![
                    "force-stop".to_owned(),
                    "--user".to_owned(),
                    "0".to_owned(),
                    package_name,
                ],
                runtime::ANDROID_FORCE_STOP_TIMEOUT_MS,
                1_048_576,
            ),
        };
        let settlement = self.run_with_stdout(
            execution_id,
            CommandProcessRequest {
                run_as: RunAs::Root,
                program: program.to_owned(),
                arguments,
                cwd: Some("/".to_owned()),
                stdin: None,
                timeout_ms,
                max_output_bytes,
            },
            claim,
            None,
        )?;
        match settlement.outcome.cause {
            CommandProcessCause::Timeout => Err(ExecutionFailure {
                error: DomainError::new(ErrorCode::Timeout, "package root primitive timed out"),
                cleanup_verified: settlement.cleanup_verified,
            }),
            CommandProcessCause::Cancelled | CommandProcessCause::OwnerLost => {
                Err(ExecutionFailure {
                    error: DomainError::new(ErrorCode::Cancelled, "package root primitive stopped"),
                    cleanup_verified: settlement.cleanup_verified,
                })
            }
            CommandProcessCause::Exited
                if settlement.outcome.exit_code == Some(0)
                    && !settlement.outcome.stdout_truncated =>
            {
                Ok(settlement.outcome.stdout)
            }
            CommandProcessCause::Exited => Err(ExecutionFailure {
                error: DomainError::new(ErrorCode::IoError, "package root primitive failed"),
                cleanup_verified: settlement.cleanup_verified,
            }),
        }
    }

    pub(crate) fn run_visual(
        &self,
        execution_id: &UuidV4,
        primitive: VisualRootPrimitive,
        output: Option<&fs::File>,
        claim: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure> {
        let (program, arguments, timeout_ms, max_output_bytes, requires_output) = match primitive {
            VisualRootPrimitive::ScreenshotPng => (
                "/system/bin/screencap",
                vec!["-p".to_owned()],
                5_000,
                8 * 1_024 * 1_024,
                true,
            ),
            VisualRootPrimitive::HierarchyDump(path) => (
                "/system/bin/uiautomator",
                vec!["dump".to_owned(), path.to_string_lossy().into_owned()],
                15_000,
                1_048_576,
                false,
            ),
            VisualRootPrimitive::Tap { x, y } => (
                "/system/bin/input",
                vec!["tap".to_owned(), x.to_string(), y.to_string()],
                5_000,
                1_048_576,
                false,
            ),
            VisualRootPrimitive::LongPress { x, y } => (
                "/system/bin/input",
                vec![
                    "swipe".to_owned(),
                    x.to_string(),
                    y.to_string(),
                    x.to_string(),
                    y.to_string(),
                    "500".to_owned(),
                ],
                5_000,
                1_048_576,
                false,
            ),
            VisualRootPrimitive::Swipe {
                from_x,
                from_y,
                to_x,
                to_y,
                duration_ms,
            } => (
                "/system/bin/input",
                vec![
                    "swipe".to_owned(),
                    from_x.to_string(),
                    from_y.to_string(),
                    to_x.to_string(),
                    to_y.to_string(),
                    duration_ms.to_string(),
                ],
                duration_ms.saturating_add(5_000).min(15_000),
                1_048_576,
                false,
            ),
            VisualRootPrimitive::Text(text) => (
                "/system/bin/input",
                vec!["text".to_owned(), text],
                5_000,
                1_048_576,
                false,
            ),
            VisualRootPrimitive::Key(key_code) => (
                "/system/bin/input",
                vec!["keyevent".to_owned(), key_code.to_string()],
                5_000,
                1_048_576,
                false,
            ),
            VisualRootPrimitive::KeyCombination(ref key_codes) => (
                "/system/bin/input",
                std::iter::once("keycombination".to_owned())
                    .chain(key_codes.iter().map(i32::to_string))
                    .collect(),
                5_000,
                1_048_576,
                false,
            ),
        };
        let input_program = program == "/system/bin/input";
        if requires_output != output.is_some() {
            return Err(pre_start(DomainError::new(
                ErrorCode::InternalError,
                "visual root output ownership is invalid",
            )));
        }
        let output = output
            .map(fs::File::try_clone)
            .transpose()
            .map_err(|_| pre_start(io_error("cannot duplicate visual output file")))?;
        let settlement = self.run_with_stdout(
            execution_id,
            CommandProcessRequest {
                run_as: RunAs::Root,
                program: program.to_owned(),
                arguments,
                cwd: Some("/".to_owned()),
                stdin: None,
                timeout_ms,
                max_output_bytes,
            },
            claim,
            output,
        )?;
        match settlement.outcome.cause {
            CommandProcessCause::Timeout => Err(ExecutionFailure {
                error: DomainError::new(ErrorCode::Timeout, "visual root primitive timed out"),
                cleanup_verified: settlement.cleanup_verified,
            }),
            CommandProcessCause::Cancelled | CommandProcessCause::OwnerLost => {
                Err(ExecutionFailure {
                    error: DomainError::new(ErrorCode::Cancelled, "visual root primitive stopped"),
                    cleanup_verified: settlement.cleanup_verified,
                })
            }
            CommandProcessCause::Exited
                if settlement.outcome.exit_code == Some(0)
                    && !settlement.outcome.stdout_truncated
                    && !settlement.outcome.stderr_truncated =>
            {
                Ok(())
            }
            CommandProcessCause::Exited => Err(ExecutionFailure {
                // `input` exiting non-zero is the input program refusing the event (a framework
                // exception, an unmappable character), not a transport or file failure.
                error: DomainError::new(
                    if input_program {
                        ErrorCode::ExecutionFailed
                    } else {
                        ErrorCode::IoError
                    },
                    "visual root primitive failed",
                ),
                cleanup_verified: settlement.cleanup_verified,
            }),
        }
    }

    fn run_with_stdout(
        &self,
        execution_id: &UuidV4,
        request: CommandProcessRequest,
        claim: &LocalExecutionClaim,
        stdout_file: Option<fs::File>,
    ) -> Result<CommandProcessSettlement, ExecutionFailure> {
        if !self.guard_path.is_absolute() || !self.guard_path.is_file() {
            return Err(pre_start(DomainError::new(
                ErrorCode::CapabilityUnavailable,
                "root execution guard is unavailable",
            )));
        }
        let (stdout_read, stdout_write) = pipe()?;
        let (stderr_read, stderr_write) = pipe()?;
        let (stdin_child, stdin_source) = match request.stdin.as_ref() {
            Some(bytes) => {
                let (read, write) = pipe()?;
                (read, Some((write, bytes.clone())))
            }
            None => (
                fs::File::open("/dev/null")
                    .map_err(|_| pre_start(io_error("cannot open root command input")))?,
                None,
            ),
        };
        let (lifetime_read, lifetime_write) = pipe()?;
        let proof = self.prepare(execution_id)?;
        let proof_raw = proof.as_raw_fd();
        let lifetime_raw = lifetime_read.as_raw_fd();

        let mut command = Command::new(&self.guard_path);
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
            command.pre_exec(move || {
                block_guard_termination_signals()?;
                clear_cloexec(proof_raw)?;
                clear_cloexec(lifetime_raw)?;
                Ok(())
            });
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(_) => {
                drop(proof);
                drop(lifetime_read);
                drop(lifetime_write);
                return Err(self.unstarted(
                    execution_id,
                    DomainError::new(ErrorCode::IoError, "cannot launch the root execution guard"),
                ));
            }
        };
        // `Command` retains its configured `Stdio` handles for a possible second
        // spawn. This execution has one owner and one spawn; release the parent's
        // duplicate pipe ends now so stream EOF follows the guard's actual lifetime.
        drop(command);
        let pid = match i32::try_from(child.id()) {
            Ok(pid) => pid,
            Err(_) => {
                drop(proof);
                drop(lifetime_read);
                drop(lifetime_write);
                return Err(self.unstarted(
                    execution_id,
                    DomainError::new(ErrorCode::InternalError, "root guard PID is invalid"),
                ));
            }
        };
        drop(proof);
        drop(lifetime_read);
        let mut lifetime_write = Some(lifetime_write);

        let limit = usize::try_from(request.max_output_bytes).unwrap_or(usize::MAX);
        let stop = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = channel();
        let stdout_reader = match stdout_file {
            Some(output) => spawn_file_drainer(
                stdout_read,
                output,
                limit,
                Arc::clone(&stop),
                sender.clone(),
            ),
            None => spawn_drainer(stdout_read, limit, Arc::clone(&stop), false, sender.clone()),
        };
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
            Some(GuardCleanCause::Cancelled) => CommandProcessCause::Cancelled,
            Some(GuardCleanCause::Timeout) => CommandProcessCause::Timeout,
            Some(GuardCleanCause::OwnerLost) => CommandProcessCause::OwnerLost,
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
    /// deadline. The guard turns both into verified cleanup, so this only escalates when
    /// the guard itself stops answering.
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
                Err(_) => {
                    return Err(pre_start(DomainError::new(
                        ErrorCode::IoError,
                        "cannot observe the root execution guard",
                    )));
                }
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
            if signalled
                .is_some_and(|at| now.duration_since(at) >= Duration::from_millis(GUARD_SETTLE_MS))
            {
                // The guard owns the group and the proof; a guard that ignores its own
                // budget can no longer report verified cleanup for this run.
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
}

/// The Magisk Command process port: the daemon's root runner plus the forwarded APK
/// identities, behind the one shared Command handler.
#[derive(Clone)]
pub(crate) struct MagiskCommandProcessPort {
    root: Arc<RootCommandGuard>,
    companion: CompanionPort,
}

impl MagiskCommandProcessPort {
    pub(crate) fn new(root: Arc<RootCommandGuard>, companion: CompanionPort) -> Self {
        Self { root, companion }
    }

    /// Forwards one `app` or `shell` command to the APK execution surface that owns that
    /// identity. The APK surface keeps its own process primitive, so the daemon supplies
    /// the bounded request and the cancellation and owns neither the process group nor
    /// the cleanup verdict.
    fn forward(
        &self,
        execution_id: &UuidV4,
        request: CommandProcessRequest,
        claim: &LocalExecutionClaim,
        primitive: &str,
        cancel: &str,
    ) -> Result<CommandProcessSettlement, ExecutionFailure> {
        // Each forwarded identity carries the command input the way its own runner
        // consumes it: the App guard runner feeds its guarded child from the bytes it
        // holds, while the Shizuku guard passes the descriptor straight to its child.
        let mut payload = json!({
            "operation": "process_start",
            "program": request.program,
            "arguments": request.arguments,
            "timeout_ms": request.timeout_ms,
            "cwd": request.cwd.clone().unwrap_or_else(|| "/".to_owned()),
            "max_output_bytes": request.max_output_bytes,
        });
        let mut stdin = StdinFeed::closed();
        if primitive == APP_PROCESS_START {
            if let Some(bytes) = request.stdin.as_deref() {
                payload["stdin"] = Value::String(
                    std::str::from_utf8(bytes)
                        .map_err(|_| pre_start(DomainError::invalid("command stdin is not UTF-8")))?
                        .to_owned(),
                );
            }
        } else {
            stdin = StdinFeed::open(request.stdin.clone())?;
        }
        let descriptors = match stdin.take_descriptor() {
            Some(descriptor) => vec![("stdin".to_owned(), descriptor)],
            None => Vec::new(),
        };
        let (channel, mut transaction) = self
            .companion
            .submit(CompanionPrimitiveRequest {
                primitive: primitive.to_owned(),
                payload,
                execution_id: execution_id.clone(),
                descriptors,
            })
            .map_err(pre_start)?;
        let deadline =
            Instant::now() + Duration::from_millis(request.timeout_ms + FORWARD_SETTLE_MARGIN_MS);
        let mut cancelled = false;
        let settlement = loop {
            if !cancelled && claim.checkpoint().is_err() {
                cancelled = true;
                // The owner of the forwarded identity performs the cancellation; this
                // only has to make the request visible on the connection that carries it.
                let _ = channel.cancel_execution(cancel, execution_id);
            }
            match transaction.poll(FORWARD_POLL_MS) {
                Some(result) => break Some(result),
                None if Instant::now() < deadline => {}
                None => break None,
            }
        };
        stdin.finish();
        let result = match settlement {
            Some(Ok(result)) => result,
            Some(Err(error)) => return Err(android_command_failure(error)),
            None => {
                return Err(ExecutionFailure {
                    error: DomainError::new(
                        ErrorCode::Timeout,
                        "forwarded command did not settle within its bound",
                    ),
                    cleanup_verified: false,
                });
            }
        };
        if !result.descriptors.is_empty() {
            return Err(pre_start(DomainError::new(
                ErrorCode::InternalError,
                "forwarded command returned unexpected descriptors",
            )));
        }
        let encoded = serde_json::to_vec(&result.payload).map_err(|_| {
            pre_start(DomainError::new(
                ErrorCode::InternalError,
                "forwarded command settlement encoding failed",
            ))
        })?;
        AndroidCommandSettlement::decode(&encoded).map_err(android_command_failure)
    }
}

impl CommandProcessPort for MagiskCommandProcessPort {
    fn run(
        &self,
        execution: &AdmittedExecution,
        request: CommandProcessRequest,
        claim: &LocalExecutionClaim,
    ) -> Result<CommandProcessSettlement, ExecutionFailure> {
        let execution_id = &execution.execution_id;
        match request.run_as {
            RunAs::Root => {
                validate_command_process_request(&request, RunAs::Root).map_err(pre_start)?;
                self.root.run(execution_id, request, claim)
            }
            RunAs::App => {
                validate_command_process_request(&request, RunAs::App).map_err(pre_start)?;
                self.forward(
                    execution_id,
                    request,
                    claim,
                    APP_PROCESS_START,
                    APP_PROCESS_CANCEL,
                )
            }
            RunAs::Shell => {
                validate_command_process_request(&request, RunAs::Shell).map_err(pre_start)?;
                self.forward(
                    execution_id,
                    request,
                    claim,
                    SHIZUKU_PROCESS_START,
                    SHIZUKU_PROCESS_CANCEL,
                )
            }
        }
    }
}

/// The daemon's reading of one guard proof.
struct GuardSettlement {
    cleanup_verified: bool,
    shell_exit_code: Option<i32>,
    cause: Option<GuardCleanCause>,
}

struct Reaped {
    exit_code: Option<i32>,
    timed_out: bool,
}

struct BoundedStream {
    bytes: Vec<u8>,
    truncated: bool,
}

/// The bounded command input as the descriptor a forwarded process reads, plus the
/// writer that feeds it. The reader end is handed to the process owner, which closes it,
/// so this side only ever owns the writer.
struct StdinFeed {
    descriptor: Option<OwnedFd>,
    writer: Option<thread::JoinHandle<()>>,
}

/// The forwarded process primitives. The App surface owns `app` and the Shizuku
/// provider owns `shell`, so each name belongs to exactly one identity.
const APP_PROCESS_START: &str = "AppProcessStart";
const APP_PROCESS_CANCEL: &str = "AppProcessCancel";
const SHIZUKU_PROCESS_START: &str = "ShizukuProcessStart";
const SHIZUKU_PROCESS_CANCEL: &str = "ShizukuProcessCancel";

impl StdinFeed {
    /// No command input to feed. The caller carries it itself.
    fn closed() -> Self {
        Self {
            descriptor: None,
            writer: None,
        }
    }

    fn open(bytes: Option<Vec<u8>>) -> Result<Self, ExecutionFailure> {
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
    fn take_descriptor(&mut self) -> Option<OwnedFd> {
        self.descriptor.take()
    }

    /// Waits for the input to be written. It is bounded because the process owner closes
    /// the reader end when the forwarded command settles.
    fn finish(mut self) {
        drop(self.descriptor.take());
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
    }
}

fn pre_start(error: DomainError) -> ExecutionFailure {
    ExecutionFailure {
        error,
        cleanup_verified: true,
    }
}

fn clear_cloexec(fd: i32) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn block_guard_termination_signals() -> io::Result<()> {
    let mut signals = unsafe { std::mem::zeroed::<libc::sigset_t>() };
    if unsafe { libc::sigemptyset(&mut signals) } != 0 {
        return Err(io::Error::last_os_error());
    }
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGALRM] {
        if unsafe { libc::sigaddset(&mut signals, signal) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    if unsafe { libc::sigprocmask(libc::SIG_BLOCK, &signals, std::ptr::null_mut()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn pipe() -> Result<(fs::File, fs::File), ExecutionFailure> {
    let mut ends = [0_i32; 2];
    if unsafe { libc::pipe2(ends.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(pre_start(io_error("cannot create root command pipe")));
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

fn spawn_file_drainer(
    input: fs::File,
    output: fs::File,
    limit: usize,
    stop: Arc<AtomicBool>,
    sender: Sender<(bool, Result<BoundedStream, DomainError>)>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let _ = sender.send((false, drain_to_file(input, output, limit, &stop)));
    })
}

/// Retains the bounded prefix of one stream and keeps reading to end-of-file so the
/// writer is never blocked by this process.
fn drain(file: fs::File, limit: usize, stop: &AtomicBool) -> Result<BoundedStream, DomainError> {
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
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(io_error("cannot poll a root command stream"));
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
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(io_error("cannot read a root command stream"));
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

/// Streams one bounded visual capture directly into its Runtime-owned private file and
/// keeps draining excess bytes so the child cannot block. The returned stream carries
/// no duplicate in-memory copy; only the truncation fact is retained.
fn drain_to_file(
    input: fs::File,
    mut output: fs::File,
    limit: usize,
    stop: &AtomicBool,
) -> Result<BoundedStream, DomainError> {
    let fd = input.as_raw_fd();
    let mut written = 0usize;
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
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(io_error("cannot poll a visual root stream"));
        }
        if ready == 0 {
            if stopped {
                output
                    .flush()
                    .map_err(|_| io_error("cannot flush visual output"))?;
                output
                    .sync_all()
                    .map_err(|_| io_error("cannot sync visual output"))?;
                return Ok(BoundedStream {
                    bytes: Vec::new(),
                    truncated,
                });
            }
            continue;
        }
        let count =
            unsafe { libc::read(fd, buffer.as_mut_ptr().cast::<libc::c_void>(), buffer.len()) };
        if count < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(io_error("cannot read a visual root stream"));
        }
        if count == 0 {
            output
                .flush()
                .map_err(|_| io_error("cannot flush visual output"))?;
            output
                .sync_all()
                .map_err(|_| io_error("cannot sync visual output"))?;
            return Ok(BoundedStream {
                bytes: Vec::new(),
                truncated,
            });
        }
        let count = usize::try_from(count).unwrap_or(buffer.len());
        let remaining = limit.saturating_sub(written);
        if remaining > 0 {
            let take = remaining.min(count);
            output
                .write_all(&buffer[..take])
                .map_err(|_| io_error("cannot write visual output"))?;
            written += take;
        }
        if count > remaining {
            truncated = true;
        }
    }
}

/// Collects both retained streams. A verified cleanup means the guard's group is gone,
/// so each write end is closed and this window closes on its own; the stop flag only
/// bounds the case where a stream outlives its owner, where the settle step already
/// reports unverified cleanup and these bytes are never used.
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
                    "root command output did not reach end-of-file",
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
                    "root command output reader stopped without a result",
                ));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
    match (stdout, stderr) {
        (Some(stdout), Some(stderr)) => Ok((stdout, stderr)),
        _ => Err(DomainError::new(
            ErrorCode::InternalError,
            "root command output collection ended without both streams",
        )),
    }
}

/// The root guard path the Magisk package installs, relative to the module root.
pub(crate) fn guard_path(module_root: &Path) -> PathBuf {
    module_root.join("bin/droidbridge-exec-guard")
}
