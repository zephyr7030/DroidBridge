//! The one shared Command semantic handler (S-AUTH-CMD-001).
//!
//! Both host surfaces admit Command through this module, so the public result and
//! error semantics, the process lifecycle and the output ownership are singular.
//! Whichever host owns the requested identity supplies the process primitive; the
//! handler itself never selects an identity.

use crate::{
    AdmittedExecution, ArtifactPort, CapabilityPort, ExecutionCancelOutcome, ExecutionCompletion,
    ExecutionFailure, ExecutionOutcome, ExecutionPayload, ExecutionPort, HostControlPort,
    LocalExecutionClaim, LocalExecutionClaims, PersistencePort, PortFuture, ProviderToken,
    RESERVE_FLOOR_BYTES, RuntimeCore, SynchronousAdmission, TaskAdmission, TaskAdmissionResult,
    UI_ENVELOPE_LIMIT_BYTES,
};
use contract::{
    CommandCall, CommandFailureCode, CommandResult, CommandRunInput, CommandTerminalState,
    ErrorCode, MotherTool, RequestId, RunAs, TaskAccepted, TaskTerminalResult, UuidV4,
};
use domain::{DomainError, ExecutorRequest, resolve_executor};
use std::time::Instant;

const MAX_INLINE_ENVELOPE_OVERHEAD: usize = 1_024;

/// R-CMD-002 command bound, in UTF-8 bytes.
pub const COMMAND_MAX_COMMAND_BYTES: usize = 32_768;
/// R-CMD-002 `cwd` bound, in UTF-8 bytes.
pub const COMMAND_MAX_CWD_BYTES: usize = 4_096;
/// R-CMD-002 stdin bound, in UTF-8 bytes.
pub const COMMAND_MAX_STDIN_BYTES: usize = 65_536;
/// R-CMD-002 lower `max_output_bytes` bound.
pub const COMMAND_MIN_OUTPUT_BYTES: u64 = 1_024;
/// R-CMD-002 upper `max_output_bytes` bound, equal to the S-ART-002 stdout/stderr cap.
pub const COMMAND_MAX_OUTPUT_BYTES: u64 = 1_048_576;
/// S-TASK-002 normal inline bound for one retained stream.
pub const COMMAND_INLINE_LIMIT_BYTES: usize = 65_536;
/// R-CMD-002 lower `timeout_ms` bound.
pub const COMMAND_MIN_TIMEOUT_MS: u64 = 1_000;
/// S-TASK-003 App/Shizuku `timeout_ms` ceiling imposed by App-hosted foreground protection.
pub const COMMAND_APP_TIMEOUT_MAX_MS: u64 = 150_000;
/// S-TASK-003 root `timeout_ms` ceiling.
pub const COMMAND_ROOT_TIMEOUT_MAX_MS: u64 = 3_600_000;

/// S-TASK-002 fixes the invoked program for every identity; the caller command is one
/// argv element and is never reparsed or rewritten.
pub const COMMAND_PROGRAM: &str = "/system/bin/sh";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandProcessCause {
    Exited,
    Timeout,
    Cancelled,
    OwnerLost,
}

/// The bounded retained result of one guarded command process: the stream bytes are
/// the retained prefix, and the truncation facts report that more output was drained
/// and discarded.
#[derive(Clone, Debug, PartialEq)]
pub struct CommandProcessOutcome {
    pub cause: CommandProcessCause,
    pub exit_code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stdout_truncated: bool,
    pub stderr: Vec<u8>,
    pub stderr_truncated: bool,
}

/// One already-admitted command handed to the host surface that owns its identity.
/// The argv is fixed by the shared handler, so neither host can reinterpret the shell
/// text, and `run_as` is the already-admitted identity rather than a preference.
#[derive(Clone, Debug, PartialEq)]
pub struct CommandProcessRequest {
    pub run_as: RunAs,
    pub program: String,
    pub arguments: Vec<String>,
    pub cwd: Option<String>,
    pub stdin: Option<Vec<u8>>,
    pub timeout_ms: u64,
    pub max_output_bytes: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CommandProcessSettlement {
    pub outcome: CommandProcessOutcome,
    pub cleanup_verified: bool,
}

/// The single process primitive both host surfaces implement for Command. The APK
/// surface backs `app` with the App runner and `shell` with the Shizuku process
/// primitive; the Magisk surface backs `root` with the daemon runner and delegates
/// `app`/`shell` to the APK surface. Shizuku never becomes a Command implementation.
pub trait CommandProcessPort: Send + Sync {
    fn run(
        &self,
        execution: &AdmittedExecution,
        request: CommandProcessRequest,
        claim: &LocalExecutionClaim,
    ) -> Result<CommandProcessSettlement, ExecutionFailure>;
}

/// The Android process primitive's settlement for one guarded command. One wire shape
/// carries it from whichever identity ran the command, so the App surface that owns the
/// Shizuku primitive and the Magisk surface that forwards `app`/`shell` to it read the
/// same fields through the same decoder and cannot diverge on terminal meaning.
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct AndroidCommandSettlement {
    cause: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    #[serde(default)]
    stdout_base64: String,
    #[serde(default)]
    stdout_truncated: bool,
    #[serde(default)]
    stderr_base64: String,
    #[serde(default)]
    stderr_truncated: bool,
    /// A Shizuku settlement reports cleanup separately, and reports an unverified
    /// cleanup as a typed failure instead of a settlement, so its payload omits this.
    #[serde(
        default = "verified_cleanup",
        skip_serializing_if = "is_verified_cleanup"
    )]
    cleanup_verified: bool,
}

const fn verified_cleanup() -> bool {
    true
}

const fn is_verified_cleanup(verified: &bool) -> bool {
    *verified
}

impl AndroidCommandSettlement {
    pub fn decode(payload: &[u8]) -> Result<CommandProcessSettlement, DomainError> {
        let settlement: Self = serde_json::from_slice(payload)
            .map_err(|_| invalid_settlement("command settlement is invalid"))?;
        let cause = match settlement.cause.as_str() {
            "exited" => CommandProcessCause::Exited,
            "cancelled" => CommandProcessCause::Cancelled,
            "timeout" => CommandProcessCause::Timeout,
            "owner_lost" => CommandProcessCause::OwnerLost,
            _ => return Err(invalid_settlement("command settlement cause is invalid")),
        };
        let stdout = decode_stream(&settlement.stdout_base64, "command stdout is invalid")?;
        let stderr = decode_stream(&settlement.stderr_base64, "command stderr is invalid")?;
        Ok(CommandProcessSettlement {
            outcome: CommandProcessOutcome {
                cause,
                exit_code: (cause == CommandProcessCause::Exited)
                    .then_some(settlement.exit_code)
                    .flatten(),
                stdout,
                stdout_truncated: settlement.stdout_truncated,
                stderr,
                stderr_truncated: settlement.stderr_truncated,
            },
            cleanup_verified: settlement.cleanup_verified,
        })
    }

    pub fn encode(settlement: &CommandProcessSettlement) -> Result<Vec<u8>, DomainError> {
        let cause = match settlement.outcome.cause {
            CommandProcessCause::Exited => "exited",
            CommandProcessCause::Timeout => "timeout",
            CommandProcessCause::Cancelled => "cancelled",
            CommandProcessCause::OwnerLost => "owner_lost",
        };
        use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
        serde_json::to_vec(&Self {
            cause: cause.to_owned(),
            exit_code: settlement.outcome.exit_code,
            stdout_base64: BASE64.encode(&settlement.outcome.stdout),
            stdout_truncated: settlement.outcome.stdout_truncated,
            stderr_base64: BASE64.encode(&settlement.outcome.stderr),
            stderr_truncated: settlement.outcome.stderr_truncated,
            cleanup_verified: settlement.cleanup_verified,
        })
        .map_err(|_| {
            DomainError::new(
                ErrorCode::InternalError,
                "command settlement encoding failed",
            )
        })
    }
}

fn decode_stream(encoded: &str, reason: &'static str) -> Result<Vec<u8>, DomainError> {
    use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
    BASE64
        .decode(encoded)
        .map_err(|_| invalid_settlement(reason))
}

const fn invalid_settlement(reason: &'static str) -> DomainError {
    DomainError::new(ErrorCode::IoError, reason)
}

pub async fn handle_command_public<P, A, E, C, H>(
    core: &RuntimeCore<P, A, E, C, H>,
    request_id: RequestId,
    payload_sha256: String,
    call: CommandCall,
    timestamp: String,
    now_ms: u64,
) -> Result<serde_json::Value, DomainError>
where
    P: PersistencePort + 'static,
    A: ArtifactPort + Clone + 'static,
    E: ExecutionPort + 'static,
    C: CapabilityPort + 'static,
    H: HostControlPort + 'static,
{
    let capability = core.capability_snapshot()?;
    let route = command_executor_request(&capability, &call)?;
    let execution_id = new_uuid()?;
    let payload = ExecutionPayload::CommandCall(call.clone());
    if is_command_task(&call) {
        let task_id = new_uuid()?;
        let admission = core
            .admit_task(TaskAdmission {
                request_id,
                payload_sha256,
                task_id: task_id.clone(),
                execution_id,
                tool: MotherTool::Command,
                action: command_action(&call).to_owned(),
                route,
                payload,
                created_at: timestamp.clone(),
                settlement_bound_bytes: command_settlement_bound_bytes(),
                now_ms,
            })
            .await?;
        let admitted_task_id = match admission {
            TaskAdmissionResult::Admitted(snapshot) => {
                let core = core.clone();
                let running_id = snapshot.task_id.clone();
                let started_at = timestamp.clone();
                tokio::spawn(async move {
                    let _ = core.run_task(&running_id, started_at, now_ms).await;
                });
                snapshot.task_id
            }
            TaskAdmissionResult::Replay(snapshot) => snapshot.task_id,
        };
        serde_json::to_value(TaskAccepted {
            task_id: admitted_task_id,
        })
        .map_err(|_| DomainError::new(ErrorCode::InternalError, "Task result encoding failed"))
    } else {
        core.run_synchronous(
            SynchronousAdmission {
                request_id,
                payload_sha256,
                execution_id,
                operation: format!("command.{}", command_action(&call)),
                route,
                payload,
                settlement_bound_bytes: command_settlement_bound_bytes(),
                now_ms,
            },
            timestamp,
            now_ms,
        )
        .await
        .map_err(|error| DomainError::new(error.code, "command execution failed"))
    }
}

/// S-TASK-002 keeps one Contract response inside the S-CONTRACT-002 frame limit, so
/// one admitted command can never settle more than that frame.
pub const fn command_settlement_bound_bytes() -> u64 {
    UI_ENVELOPE_LIMIT_BYTES as u64
}

fn command_action(call: &CommandCall) -> &'static str {
    match call {
        CommandCall::Run(_) => "run",
    }
}

fn is_command_task(call: &CommandCall) -> bool {
    match call {
        CommandCall::Run(input) => input.as_task,
    }
}

pub(crate) fn new_uuid() -> Result<UuidV4, DomainError> {
    UuidV4::parse(uuid::Uuid::new_v4().to_string())
        .map_err(|_| DomainError::new(ErrorCode::InternalError, "UUID generation failed"))
}

/// R-CMD-002 bounds plus the S-TASK-003 per-identity timeout ceiling. Every bound is
/// enforced before executor resolution, so an out-of-range request never reaches a
/// process primitive.
pub fn validate_command_input(call: &CommandCall) -> Result<(), DomainError> {
    let CommandCall::Run(input) = call;
    if input.command.is_empty()
        || input.command.len() > COMMAND_MAX_COMMAND_BYTES
        || input.command.contains('\0')
    {
        return Err(DomainError::invalid("command.run command is out of bounds"));
    }
    if let Some(cwd) = &input.cwd
        && (cwd.is_empty()
            || !cwd.starts_with('/')
            || cwd.len() > COMMAND_MAX_CWD_BYTES
            || cwd.contains('\0'))
    {
        return Err(DomainError::invalid("command.run cwd is out of bounds"));
    }
    if let Some(stdin) = &input.stdin
        && stdin.len() > COMMAND_MAX_STDIN_BYTES
    {
        return Err(DomainError::invalid("command.run stdin is out of bounds"));
    }
    if !(COMMAND_MIN_OUTPUT_BYTES..=COMMAND_MAX_OUTPUT_BYTES).contains(&input.max_output_bytes) {
        return Err(DomainError::invalid(
            "command.run max_output_bytes is out of bounds",
        ));
    }
    let timeout_max = match input.run_as {
        RunAs::Root => COMMAND_ROOT_TIMEOUT_MAX_MS,
        _ => COMMAND_APP_TIMEOUT_MAX_MS,
    };
    if !(COMMAND_MIN_TIMEOUT_MS..=timeout_max).contains(&input.timeout_ms) {
        return Err(DomainError::invalid(
            "command.run timeout_ms is out of bounds",
        ));
    }
    Ok(())
}

/// Revalidates the fixed host-process boundary. Public admission already performs the
/// same bounds check, but Android/companion transports are not allowed to widen the
/// program, argv, identity, UTF-8 input, timeout, cwd, or retained-output contract.
pub fn validate_command_process_request(
    request: &CommandProcessRequest,
    expected_run_as: RunAs,
) -> Result<(), DomainError> {
    if request.run_as != expected_run_as
        || request.program != COMMAND_PROGRAM
        || request.arguments.len() != 2
        || request.arguments[0] != "-c"
    {
        return Err(DomainError::invalid(
            "command process request does not match the admitted identity or shell",
        ));
    }
    let stdin = request
        .stdin
        .as_deref()
        .map(std::str::from_utf8)
        .transpose()
        .map_err(|_| DomainError::invalid("command process stdin is not UTF-8"))?
        .map(str::to_owned);
    validate_command_input(&CommandCall::Run(CommandRunInput {
        command: request.arguments[1].clone(),
        run_as: request.run_as,
        cwd: request.cwd.clone(),
        stdin,
        timeout_ms: request.timeout_ms,
        max_output_bytes: request.max_output_bytes,
        as_task: false,
    }))
}

/// One typed rejection from an Android Command process primitive.
///
/// `CLEANUP_UNVERIFIED` is not a member of the closed error-code set, so an in-process
/// adapter and a companion reply both surface it as `INTERNAL_ERROR`; that is the one
/// code that means the primitive could not prove descendant cleanup. Every other code is
/// a rejection the primitive reports before it could start a process, so cleanup is
/// verified. Both host Command surfaces classify a primitive failure through this one
/// function and cannot diverge on terminal meaning.
pub fn android_command_failure(error: DomainError) -> ExecutionFailure {
    ExecutionFailure {
        cleanup_verified: error.code != ErrorCode::InternalError,
        error,
    }
}

/// Decodes one Command settlement a process primitive reported. A payload that does not
/// decode is not evidence about the process at all, so it is an unverified cleanup and
/// never a pre-start rejection both host surfaces could mistake for a clean run.
pub fn decode_android_command_settlement(
    payload: &[u8],
) -> Result<CommandProcessSettlement, ExecutionFailure> {
    AndroidCommandSettlement::decode(payload).map_err(|error| ExecutionFailure {
        error,
        cleanup_verified: false,
    })
}

pub fn command_executor_request(
    capability: &crate::CapabilitySnapshot,
    call: &CommandCall,
) -> Result<ExecutorRequest, DomainError> {
    validate_command_input(call)?;
    if capability.context.readiness != contract::RuntimeReadiness::Ready {
        return Err(DomainError::new(
            ErrorCode::CapabilityUnavailable,
            "Runtime is not ready for command execution",
        ));
    }
    let CommandCall::Run(input) = call;
    let mut facts = capability.resolver_facts;
    facts.app_native = capability.context.app_execution_surface;
    facts.generations.app_native = capability.fence.host_generation;
    let request = ExecutorRequest::Command(input.run_as);
    resolve_executor(
        capability.context.host,
        capability.fence.clone(),
        facts,
        request,
    )?;
    Ok(request)
}

fn command_executor_is_current(
    current: &crate::CapabilitySnapshot,
    execution: &AdmittedExecution,
    provider: ProviderToken,
) -> bool {
    let fence = &execution.executor.fence;
    if current.context.readiness != contract::RuntimeReadiness::Ready
        || current.context.host != execution.executor.host
        || current.fence.runtime_epoch != fence.runtime_epoch
        || current.fence.host_generation != fence.host_generation
        || current.fence.runtime_instance_id != fence.runtime_instance_id
    {
        return false;
    }
    match provider {
        ProviderToken::AppNative => {
            current.context.app_execution_surface == contract::CapabilityState::Available
                && current.resolver_facts.app_native == contract::CapabilityState::Available
                && execution.executor.capability_generation == current.fence.host_generation
        }
        ProviderToken::Shizuku => {
            current.resolver_facts.shizuku == contract::CapabilityState::Available
                && execution.executor.capability_generation
                    == current.resolver_facts.generations.shizuku
        }
        ProviderToken::MagiskNative => {
            current.context.host == contract::RuntimeHost::MagiskBackend
                && current.resolver_facts.magisk_native == contract::CapabilityState::Available
                && execution.executor.capability_generation
                    == current.resolver_facts.generations.magisk_native
        }
        _ => false,
    }
}

/// The identity one provider realizes. Admission binds provider to `run_as`, so this
/// projection is the only place an actual identity is derived, and a mismatch is a
/// stale authority rather than a fallback.
const fn run_as_of(provider: ProviderToken) -> Option<RunAs> {
    match provider {
        ProviderToken::AppNative => Some(RunAs::App),
        ProviderToken::Shizuku => Some(RunAs::Shell),
        ProviderToken::MagiskNative => Some(RunAs::Root),
        _ => None,
    }
}

#[derive(Clone)]
pub struct NativeCommandExecutionSurface<A, C, P> {
    artifacts: A,
    capabilities: C,
    identities: &'static [RunAs],
    process: P,
    claims: LocalExecutionClaims,
}

impl<A, C, P> NativeCommandExecutionSurface<A, C, P> {
    /// `identities` are the identities this host surface owns: the APK surface owns
    /// `app` and `shell`, the Magisk surface owns `root` and forwards the other two.
    pub fn new(artifacts: A, capabilities: C, identities: &'static [RunAs], process: P) -> Self {
        Self {
            artifacts,
            capabilities,
            identities,
            process,
            claims: LocalExecutionClaims::default(),
        }
    }
}

impl<A, C, P> ExecutionPort for NativeCommandExecutionSurface<A, C, P>
where
    A: ArtifactPort + Clone + 'static,
    C: CapabilityPort + Clone + 'static,
    P: CommandProcessPort + Clone + 'static,
{
    fn claim_and_start<'a>(
        &'a self,
        execution: AdmittedExecution,
    ) -> PortFuture<'a, Result<ExecutionCompletion, ExecutionFailure>> {
        let claim = match self.claims.claim(execution.execution_id.clone()) {
            Ok(claim) => claim,
            Err(error) => {
                return Box::pin(async move {
                    Err(ExecutionFailure {
                        error,
                        cleanup_verified: true,
                    })
                });
            }
        };
        let claims = self.claims.clone();
        let artifacts = self.artifacts.clone();
        let capabilities = self.capabilities.clone();
        let identities = self.identities;
        let process = self.process.clone();
        Box::pin(async move {
            let result = execute_command(
                artifacts,
                capabilities,
                identities,
                process,
                &execution,
                &claim,
            )
            .await;
            let cleanup_verified = match &result {
                Ok(completion) => completion.cleanup_verified,
                Err(failure) => failure.cleanup_verified,
            };
            claims.finish(&claim, cleanup_verified);
            result
        })
    }

    fn cancel<'a>(
        &'a self,
        execution_id: &'a UuidV4,
    ) -> PortFuture<'a, Result<ExecutionCancelOutcome, DomainError>> {
        Box::pin(self.claims.cancel(execution_id))
    }
}

async fn execute_command<A, C, P>(
    artifacts: A,
    capabilities: C,
    identities: &'static [RunAs],
    process: P,
    execution: &AdmittedExecution,
    claim: &LocalExecutionClaim,
) -> Result<ExecutionCompletion, ExecutionFailure>
where
    A: ArtifactPort,
    C: CapabilityPort,
    P: CommandProcessPort + Clone + 'static,
{
    let call = match &execution.payload {
        ExecutionPayload::CommandCall(call) => call.clone(),
        _ => {
            return Err(execution_failure(
                ErrorCode::Unsupported,
                "native command surface received a non-command request",
                true,
            ));
        }
    };
    let CommandCall::Run(input) = &call;
    if !identities.contains(&input.run_as) {
        return Err(execution_failure(
            ErrorCode::StaleAuthority,
            "command identity is not owned by this host surface",
            true,
        ));
    }
    if run_as_of(execution.executor.provider) != Some(input.run_as) {
        return Err(execution_failure(
            ErrorCode::StaleAuthority,
            "command execution identity does not match the requested identity",
            true,
        ));
    }
    if let Err(error) = validate_command_input(&call) {
        return Err(ExecutionFailure {
            error,
            cleanup_verified: true,
        });
    }
    let current = capabilities.current().map_err(|error| ExecutionFailure {
        error,
        cleanup_verified: true,
    })?;
    if !command_executor_is_current(&current, execution, execution.executor.provider) {
        return Err(execution_failure(
            ErrorCode::StaleAuthority,
            "command executor fence or generation is stale",
            true,
        ));
    }
    claim.checkpoint().map_err(|error| ExecutionFailure {
        error,
        cleanup_verified: true,
    })?;
    let request = CommandProcessRequest {
        run_as: input.run_as,
        program: COMMAND_PROGRAM.to_owned(),
        arguments: vec!["-c".to_owned(), input.command.clone()],
        cwd: input.cwd.clone(),
        stdin: input.stdin.as_ref().map(|value| value.as_bytes().to_vec()),
        timeout_ms: input.timeout_ms,
        max_output_bytes: input.max_output_bytes,
    };
    let started = Instant::now();
    let settlement = run_blocking(process, execution.clone(), request, claim.clone()).await;
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let settlement = match settlement {
        Ok(settlement) => settlement,
        Err(failure) => {
            if failure.error.code == ErrorCode::Cancelled {
                return Ok(command_cancellation(
                    execution,
                    claim,
                    failure.cleanup_verified,
                ));
            }
            return Err(failure);
        }
    };
    match settlement.outcome.cause {
        CommandProcessCause::Cancelled => {
            return Ok(command_cancellation(
                execution,
                claim,
                settlement.cleanup_verified,
            ));
        }
        CommandProcessCause::OwnerLost => {
            return Err(ExecutionFailure {
                error: DomainError::new(
                    ErrorCode::IoError,
                    "command process lost its owner before verified cleanup",
                ),
                cleanup_verified: settlement.cleanup_verified,
            });
        }
        CommandProcessCause::Exited | CommandProcessCause::Timeout => {}
    }
    let cleanup_verified = settlement.cleanup_verified && claim.cleanup_is_verified();
    if !cleanup_verified {
        return Err(ExecutionFailure {
            error: DomainError::new(ErrorCode::IoError, "command process cleanup is unverified"),
            cleanup_verified: false,
        });
    }
    let result = match command_result(
        &settlement.outcome,
        input,
        execution.executor.execution_class,
        duration_ms,
        &artifacts,
        &execution.execution_id,
        claim,
    ) {
        Ok(result) => result,
        Err(error) if error.code == ErrorCode::Cancelled => {
            return Ok(command_cancellation(execution, claim, cleanup_verified));
        }
        Err(error) => {
            return Err(ExecutionFailure {
                error,
                cleanup_verified,
            });
        }
    };
    let outcome = if execution.task_id.is_none() {
        let value = serde_json::to_value(&result).map_err(|_| ExecutionFailure {
            error: DomainError::new(ErrorCode::InternalError, "command result encoding failed"),
            cleanup_verified,
        })?;
        let encoded_bytes = encoded_len(&value).map_err(|error| ExecutionFailure {
            error,
            cleanup_verified,
        })?;
        ExecutionOutcome::SynchronousCompleted {
            result: value,
            encoded_bytes,
        }
    } else {
        let terminal = TaskTerminalResult::Command(result);
        let encoded_bytes = encoded_len(&terminal).map_err(|error| ExecutionFailure {
            error,
            cleanup_verified,
        })?;
        ExecutionOutcome::Completed {
            result: terminal,
            encoded_bytes,
        }
    };
    Ok(ExecutionCompletion {
        fence: execution_fence(execution),
        capability_generation: execution.executor.capability_generation,
        outcome,
        cleanup_verified,
    })
}

/// One command process occupies its own blocking thread for as long as the process
/// runs, so a long root command never stops the Runtime from driving its other work.
async fn run_blocking<P: CommandProcessPort + Clone + 'static>(
    process: P,
    execution: AdmittedExecution,
    request: CommandProcessRequest,
    claim: LocalExecutionClaim,
) -> Result<CommandProcessSettlement, ExecutionFailure> {
    tokio::task::spawn_blocking(move || process.run(&execution, request, &claim))
        .await
        .map_err(|_| {
            execution_failure(
                ErrorCode::InternalError,
                "command process runner did not report a settlement",
                false,
            )
        })?
}

fn command_cancellation(
    execution: &AdmittedExecution,
    claim: &LocalExecutionClaim,
    cleanup_verified: bool,
) -> ExecutionCompletion {
    ExecutionCompletion {
        fence: execution_fence(execution),
        capability_generation: execution.executor.capability_generation,
        outcome: ExecutionOutcome::Cancelled {
            error: contract::PublicError {
                code: ErrorCode::Cancelled,
                operation: "command.run".to_owned(),
                retryable: false,
                message: None,
                capability: None,
                details: None,
            },
            encoded_bytes: RESERVE_FLOOR_BYTES,
        },
        cleanup_verified: cleanup_verified && claim.cleanup_is_verified(),
    }
}

fn command_result<A: ArtifactPort>(
    outcome: &CommandProcessOutcome,
    input: &CommandRunInput,
    execution_class: contract::ExecutionClass,
    duration_ms: u64,
    artifacts: &A,
    execution_id: &UuidV4,
    claim: &LocalExecutionClaim,
) -> Result<CommandResult, DomainError> {
    let (state, failure_code) = match outcome.cause {
        CommandProcessCause::Exited if outcome.exit_code == Some(0) => {
            (CommandTerminalState::Completed, None)
        }
        CommandProcessCause::Exited => (
            CommandTerminalState::Failed,
            Some(CommandFailureCode::ExecutionFailed),
        ),
        CommandProcessCause::Timeout => (
            CommandTerminalState::Failed,
            Some(CommandFailureCode::Timeout),
        ),
        CommandProcessCause::Cancelled | CommandProcessCause::OwnerLost => {
            return Err(DomainError::new(
                ErrorCode::Cancelled,
                "command process reported a non-terminal cause",
            ));
        }
    };
    let mut result = CommandResult {
        state,
        failure_code,
        exit_code: matches!(outcome.cause, CommandProcessCause::Exited)
            .then_some(outcome.exit_code)
            .flatten(),
        requested_run_as: input.run_as,
        actual_run_as: input.run_as,
        execution_class,
        duration_ms,
        stdout: None,
        stdout_ref: None,
        stdout_truncated: outcome.stdout_truncated,
        stderr: None,
        stderr_ref: None,
        stderr_truncated: outcome.stderr_truncated,
    };
    let stdout_inline = inline_text(&outcome.stdout);
    let stderr_inline = inline_text(&outcome.stderr);
    if let (Some(stdout), Some(stderr)) = (&stdout_inline, &stderr_inline) {
        result.stdout = Some(stdout.clone());
        result.stderr = Some(stderr.clone());
        if encoded_fits(&result) {
            return Ok(result);
        }
        result.stdout = None;
        result.stderr = None;
    }
    result.stdout_ref = publish_stream(artifacts, execution_id, "stdout", claim, &outcome.stdout)?;
    if result.stdout_ref.is_none() {
        result.stdout = stdout_inline;
    }
    result.stderr_ref = publish_stream(artifacts, execution_id, "stderr", claim, &outcome.stderr)?;
    if result.stderr_ref.is_none() {
        result.stderr = stderr_inline;
    }
    if !encoded_fits(&result) {
        return Err(DomainError::new(
            ErrorCode::ResourceLimit,
            "command result exceeds the protocol frame limit",
        ));
    }
    Ok(result)
}

/// The retained bytes become one artifact ref; an empty stream has no artifact and
/// keeps its empty inline value instead.
fn publish_stream<A: ArtifactPort>(
    artifacts: &A,
    execution_id: &UuidV4,
    kind: &str,
    claim: &LocalExecutionClaim,
    bytes: &[u8],
) -> Result<Option<String>, DomainError> {
    if bytes.is_empty() {
        return Ok(None);
    }
    let metadata = claim.publish(|| artifacts.publish_for_execution(execution_id, kind, bytes))?;
    Ok(Some(metadata.artifact_ref))
}

fn inline_text(bytes: &[u8]) -> Option<String> {
    let value = std::str::from_utf8(bytes).ok()?;
    (value.len() <= COMMAND_INLINE_LIMIT_BYTES).then(|| value.to_owned())
}

fn encoded_fits(result: &CommandResult) -> bool {
    serde_json::to_vec(result).is_ok_and(|encoded| {
        encoded.len() + MAX_INLINE_ENVELOPE_OVERHEAD <= UI_ENVELOPE_LIMIT_BYTES
    })
}

fn encoded_len(value: &impl serde::Serialize) -> Result<u64, DomainError> {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len() as u64)
        .map_err(|_| DomainError::new(ErrorCode::InternalError, "command result encoding failed"))
}

pub(crate) fn execution_fence(execution: &AdmittedExecution) -> domain::AdmissionFence {
    domain::AdmissionFence {
        runtime_epoch: execution.executor.fence.runtime_epoch.clone(),
        host_generation: execution.executor.fence.host_generation,
        runtime_instance_id: execution.executor.fence.runtime_instance_id.clone(),
    }
}

pub(crate) fn execution_failure(
    code: ErrorCode,
    message: &'static str,
    cleanup_verified: bool,
) -> ExecutionFailure {
    ExecutionFailure {
        error: DomainError::new(code, message),
        cleanup_verified,
    }
}
