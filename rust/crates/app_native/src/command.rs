//! The APK execution surface's Command process port.
//!
//! It realizes exactly the two identities S-AUTH-CMD-001 gives this surface: `app` with
//! the App-UID guard runner this process owns, and `shell` with the typed Shizuku
//! process primitive. Shizuku therefore stays a primitive provider rather than a third
//! Command implementation, and no identity request falls through to another identity.

use crate::guard;
use contract::{ErrorCode, RunAs};
use domain::DomainError;
use runtime::{
    AdmittedExecution, CommandProcessPort, CommandProcessRequest, CommandProcessSettlement,
    ExecutionFailure, LocalExecutionClaim, LocalExecutionClaims, validate_command_process_request,
};

#[derive(Clone, Copy, Debug)]
pub(crate) struct ApkCommandProcessPort;

impl CommandProcessPort for ApkCommandProcessPort {
    fn run(
        &self,
        execution: &AdmittedExecution,
        request: CommandProcessRequest,
        claim: &LocalExecutionClaim,
    ) -> Result<CommandProcessSettlement, ExecutionFailure> {
        match request.run_as {
            RunAs::App => {
                validate_command_process_request(&request, RunAs::App).map_err(pre_start)?;
                guard::run_guarded_command(&execution.execution_id, request, claim)
            }
            RunAs::Shell => {
                validate_command_process_request(&request, RunAs::Shell).map_err(pre_start)?;
                shell_command(execution, &request, claim)
            }
            RunAs::Root => Err(ExecutionFailure {
                error: DomainError::new(
                    ErrorCode::RunAsUnavailable,
                    "root identity is unavailable on the APK surface",
                ),
                cleanup_verified: true,
            }),
        }
    }
}

fn pre_start(error: DomainError) -> ExecutionFailure {
    ExecutionFailure {
        error,
        cleanup_verified: true,
    }
}

#[cfg(target_os = "android")]
fn shell_command(
    execution: &AdmittedExecution,
    request: &CommandProcessRequest,
    claim: &LocalExecutionClaim,
) -> Result<CommandProcessSettlement, ExecutionFailure> {
    use runtime::{android_command_failure, decode_android_command_settlement};

    let payload = serde_json::to_vec(&serde_json::json!({
        "operation": "process_start",
        "program": request.program,
        "arguments": request.arguments,
        "timeout_ms": request.timeout_ms,
        "cwd": request.cwd.clone().unwrap_or_else(|| "/".to_owned()),
        "max_output_bytes": request.max_output_bytes,
    }))
    .map_err(|_| {
        pre_start(DomainError::new(
            ErrorCode::InternalError,
            "shell command payload encoding failed",
        ))
    })?;
    let stdin = guard::StdinFeed::open(request.stdin.clone())?;
    let result = run_shizuku_guarded(
        execution,
        claim,
        &payload,
        stdin.descriptor().map(|fd| ("stdin", fd)),
    );
    stdin.finish();
    let result = result.map_err(android_command_failure)?;
    if !result.descriptors.is_empty() {
        return Err(pre_start(DomainError::new(
            ErrorCode::IoError,
            "shell command returned unexpected descriptors",
        )));
    }
    decode_android_command_settlement(&result.payload)
}

/// Runs the Shizuku process primitive for this execution while this thread stays able
/// to deliver the typed cancel the APK surface owns for that identity. The primitive
/// carries the command's own deadline, so a cancelled run settles through the same
/// proof and this port never has to guess at cleanup.
#[cfg(target_os = "android")]
pub(crate) fn run_shizuku_guarded(
    execution: &AdmittedExecution,
    claim: &LocalExecutionClaim,
    payload: &[u8],
    descriptor: Option<(&str, std::os::fd::RawFd)>,
) -> Result<runtime::AndroidPrimitiveResult, DomainError> {
    use std::time::Duration;

    let owned_execution = execution.clone();
    let owned_payload = payload.to_vec();
    let owned_descriptor = descriptor.map(|(role, fd)| (role.to_owned(), fd));
    let worker = std::thread::spawn(move || match owned_descriptor {
        Some((role, fd)) => crate::dispatch_android_execution_for_with_descriptor(
            "shizuku.shell",
            "ShizukuProcessStart",
            &owned_payload,
            &owned_execution,
            Some((&role, fd)),
        ),
        None => crate::dispatch_android_execution_for(
            "shizuku.shell",
            "ShizukuProcessStart",
            &owned_payload,
            &owned_execution,
        ),
    });
    let mut cancelled = false;
    while !worker.is_finished() {
        if !cancelled && claim.checkpoint().is_err() {
            cancelled = true;
            let _ = cancel_shell(execution);
        }
        std::thread::sleep(Duration::from_millis(SHELL_CANCEL_POLL_MS));
    }
    worker.join().unwrap_or_else(|_| {
        Err(DomainError::new(
            ErrorCode::InternalError,
            "the Shizuku process primitive did not report a settlement",
        ))
    })
}

#[cfg(not(target_os = "android"))]
pub(crate) fn run_shizuku_guarded(
    _execution: &AdmittedExecution,
    _claim: &LocalExecutionClaim,
    _payload: &[u8],
    _descriptor: Option<(&str, i32)>,
) -> Result<runtime::AndroidPrimitiveResult, DomainError> {
    Err(DomainError::new(
        ErrorCode::CapabilityUnavailable,
        "Shizuku guarded primitives require Android",
    ))
}

/// Requests cancellation of the one Shizuku process this execution started. It is the
/// same typed primitive the Magisk surface forwards, so the identity that owns the
/// process is always the one that cancels it.
#[cfg(target_os = "android")]
fn cancel_shell(
    execution: &AdmittedExecution,
) -> Result<runtime::AndroidPrimitiveResult, DomainError> {
    let payload = serde_json::to_vec(&serde_json::json!({
        "execution_id": execution.execution_id.as_str(),
    }))
    .map_err(|_| {
        DomainError::new(
            ErrorCode::InternalError,
            "shell cancel payload encoding failed",
        )
    })?;
    crate::dispatch_android_execution_for(
        "shizuku.shell",
        "ShizukuProcessCancel",
        &payload,
        execution,
    )
}

#[cfg(target_os = "android")]
const SHELL_CANCEL_POLL_MS: u64 = 25;

/// The App-identity commands this process runs as the authenticated companion of the
/// Magisk host. One claim per execution, so a forwarded cancel reaches the very run its
/// guard polls and no second runner can appear for one execution.
static APP_COMMAND_CLAIMS: std::sync::OnceLock<LocalExecutionClaims> = std::sync::OnceLock::new();

fn app_command_claims() -> &'static LocalExecutionClaims {
    APP_COMMAND_CLAIMS.get_or_init(LocalExecutionClaims::default)
}

/// One `AppProcessStart` request as the Magisk Command surface forwards it.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AppCommandRequest {
    operation: String,
    program: String,
    arguments: Vec<String>,
    timeout_ms: u64,
    cwd: String,
    max_output_bytes: u64,
    #[serde(default)]
    stdin: Option<String>,
}

/// Runs one App-identity command the Magisk host delegated to this authenticated
/// companion and reports it through the one settlement both host Command surfaces
/// decode. The App surface owns this identity, so the command runs under the guard scope
/// this process published or adopted and never under a different identity.
pub(crate) fn run_app_command(execution_id: &str, request_json: &str) -> String {
    match run_app_command_inner(execution_id, request_json) {
        Ok(encoded) => encoded,
        Err(failure) => encode_command_failure(&failure),
    }
}

/// Requests cancellation of the one App-command run this process holds for the
/// execution. `false` means nothing was left to cancel, which is the same outcome a
/// cancellation of an already-settled execution reports.
pub(crate) fn cancel_app_command(execution_id: &str) -> bool {
    match contract::UuidV4::parse(execution_id.to_owned()) {
        Ok(execution_id) => app_command_claims().request_cancel(&execution_id),
        Err(_) => false,
    }
}

fn run_app_command_inner(
    execution_id: &str,
    request_json: &str,
) -> Result<String, ExecutionFailure> {
    use runtime::{
        AndroidCommandSettlement, COMMAND_MAX_STDIN_BYTES, validate_command_process_request,
    };

    let execution_id = contract::UuidV4::parse(execution_id.to_owned())
        .map_err(|_| pre_start(DomainError::invalid("invalid command execution identity")))?;
    let request: AppCommandRequest = serde_json::from_str(request_json)
        .map_err(|_| pre_start(DomainError::invalid("invalid App command request")))?;
    if request.operation != "process_start" {
        return Err(pre_start(DomainError::new(
            ErrorCode::Unsupported,
            "unsupported App command operation",
        )));
    }
    if request
        .stdin
        .as_deref()
        .is_some_and(|stdin| stdin.len() > COMMAND_MAX_STDIN_BYTES)
    {
        return Err(pre_start(DomainError::new(
            ErrorCode::InvalidArgument,
            "command stdin exceeds its bound",
        )));
    }
    let request = CommandProcessRequest {
        run_as: RunAs::App,
        program: request.program,
        arguments: request.arguments,
        cwd: Some(request.cwd),
        stdin: request.stdin.map(String::into_bytes),
        timeout_ms: request.timeout_ms,
        max_output_bytes: request.max_output_bytes,
    };
    validate_command_process_request(&request, RunAs::App).map_err(pre_start)?;
    let claims = app_command_claims();
    let claim = claims.claim(execution_id.clone()).map_err(pre_start)?;
    let outcome = guard::run_guarded_command(&execution_id, request, &claim);
    let cleanup_verified = match &outcome {
        Ok(settlement) => settlement.cleanup_verified,
        Err(failure) => failure.cleanup_verified,
    };
    claims.finish(&claim, cleanup_verified);
    let settlement = outcome?;
    let encoded = AndroidCommandSettlement::encode(&settlement).map_err(pre_start)?;
    String::from_utf8(encoded).map_err(|_| {
        pre_start(DomainError::new(
            ErrorCode::InternalError,
            "command settlement is not UTF-8",
        ))
    })
}

/// The typed rejection this process reports for a command it could not settle. An
/// unverified cleanup is reported as the one code the command family reads that way on
/// both surfaces, so a caller can never mistake it for a clean run.
fn encode_command_failure(failure: &ExecutionFailure) -> String {
    let code = if failure.cleanup_verified {
        serde_json::to_value(failure.error.code)
            .unwrap_or_else(|_| serde_json::Value::String("INTERNAL_ERROR".to_owned()))
    } else {
        serde_json::Value::String("CLEANUP_UNVERIFIED".to_owned())
    };
    serde_json::json!({
        "error": {
            "code": code,
            "retryable": false,
        },
    })
    .to_string()
}

#[cfg(not(target_os = "android"))]
fn shell_command(
    _execution: &AdmittedExecution,
    _request: &CommandProcessRequest,
    _claim: &LocalExecutionClaim,
) -> Result<CommandProcessSettlement, ExecutionFailure> {
    Err(pre_start(DomainError::new(
        ErrorCode::CapabilityUnavailable,
        "Shizuku shell commands require Android",
    )))
}
