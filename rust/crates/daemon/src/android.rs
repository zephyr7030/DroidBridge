//! Magisk-hosted Android mother-tool primitives (S-MAGISK-005, S-AUTH-ANDROID-001).

use crate::{
    HelperFamily,
    command::{PackageRootPrimitive, RootCommandGuard},
    companion::CompanionPort,
    unix_transport::{receive_json, send_json},
};
use contract::{AndroidIntentInput, AndroidLaunchInput, ErrorCode};
use domain::DomainError;
use runtime::{
    AdmittedExecution, AndroidNotificationIdentity, AndroidNotificationRecord,
    AndroidPrimitivePort, ExecutionFailure, FrameworkPackageInspection, LocalExecutionClaim,
    PrivilegedPackageRecord, ProviderToken,
};
use serde_json::{Value, json};
use std::{
    fs::File,
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{net::UnixStream, process::CommandExt},
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const HELPER_OPERATION_TIMEOUT: Duration = Duration::from_millis(10_000);
const CLIPBOARD_CHILD_TIMEOUT: Duration = Duration::from_millis(10_000);
const CLIPBOARD_CHILD_MAX_OUTPUT: u64 = 1_048_576;
const CLIPBOARD_CHILD_JAR_FD: i32 = 3;
const SHELL_UID: libc::uid_t = 2_000;

/// One authenticated helper socket. A transport failure poisons it so the host replaces
/// the helper instead of reading a late response for a newer request.
pub(crate) struct HelperConnection {
    stream: Mutex<UnixStream>,
    broken: AtomicBool,
}

impl HelperConnection {
    pub(crate) fn new(stream: UnixStream) -> Self {
        Self {
            stream: Mutex::new(stream),
            broken: AtomicBool::new(false),
        }
    }

    pub(crate) fn is_broken(&self) -> bool {
        self.broken.load(Ordering::Acquire)
    }

    pub(crate) fn request(&self, request: &Value) -> Result<Value, DomainError> {
        if self.is_broken() {
            return Err(helper_unavailable());
        }
        let mut stream = self
            .stream
            .lock()
            .map_err(|_| DomainError::new(ErrorCode::InternalError, "helper stream lock failed"))?;
        let exchange = (|| {
            stream
                .set_read_timeout(Some(HELPER_OPERATION_TIMEOUT))
                .and_then(|()| stream.set_write_timeout(Some(HELPER_OPERATION_TIMEOUT)))
                .map_err(|_| DomainError::new(ErrorCode::IoError, "helper timeout setup failed"))?;
            send_json(&mut stream, request)?;
            receive_json::<Value>(&mut stream)
        })();
        match exchange.and_then(|response| decode_operation_response(&response)) {
            Err(error)
                if error.code == ErrorCode::IoError
                    || error.code == ErrorCode::ProtocolIncompatible =>
            {
                self.broken.store(true, Ordering::Release);
                Err(DomainError::new(
                    ErrorCode::CapabilityUnavailable,
                    "framework helper transport failed",
                ))
            }
            other => other,
        }
    }
}

fn decode_operation_response(response: &Value) -> Result<Value, DomainError> {
    let object = response.as_object().ok_or_else(invalid_response)?;
    match object.get("ok").and_then(Value::as_bool) {
        Some(true) if object.len() == 2 => {
            object.get("result").cloned().ok_or_else(invalid_response)
        }
        Some(false) if object.len() == 2 => {
            let code = object
                .get("code")
                .and_then(Value::as_str)
                .and_then(|token| {
                    serde_json::from_value::<ErrorCode>(Value::String(token.to_owned())).ok()
                })
                .ok_or_else(invalid_response)?;
            Err(DomainError::new(code, "framework helper operation failed"))
        }
        _ => Err(invalid_response()),
    }
}

fn invalid_response() -> DomainError {
    DomainError::new(ErrorCode::IoError, "framework helper response is invalid")
}

fn helper_unavailable() -> DomainError {
    DomainError::new(
        ErrorCode::CapabilityUnavailable,
        "framework helper is unavailable",
    )
}

#[derive(Clone)]
struct PublishedHelper {
    connection: Arc<HelperConnection>,
    jar: PathBuf,
}

/// The shared handle through which admitted Magisk Android work reaches the live helper
/// generation. The host publishes a helper after Hello and withdraws it on loss.
#[derive(Clone, Default)]
pub(crate) struct HelperPort {
    helper: Arc<Mutex<Option<PublishedHelper>>>,
    denied: Arc<[AtomicBool; 3]>,
}

impl HelperPort {
    pub(crate) fn publish(&self, connection: Arc<HelperConnection>, jar: PathBuf) {
        for denied in self.denied.iter() {
            denied.store(false, Ordering::Release);
        }
        if let Ok(mut helper) = self.helper.lock() {
            *helper = Some(PublishedHelper { connection, jar });
        }
    }

    pub(crate) fn withdraw(&self) {
        if let Ok(mut helper) = self.helper.lock() {
            *helper = None;
        }
    }

    pub(crate) fn operation_denied(&self, family: HelperFamily) -> bool {
        self.denied[family.index()].load(Ordering::Acquire)
    }

    fn published(&self) -> Result<PublishedHelper, DomainError> {
        self.helper
            .lock()
            .map_err(|_| DomainError::new(ErrorCode::InternalError, "helper port lock failed"))?
            .clone()
            .ok_or_else(helper_unavailable)
    }

    fn request(&self, family: HelperFamily, request: Value) -> Result<Value, DomainError> {
        let result = self.published()?.connection.request(&request);
        self.observe_denial(family, &result);
        result
    }

    pub(crate) fn clipboard(
        &self,
        operation: &str,
        input: &Value,
        claim: &LocalExecutionClaim,
    ) -> Result<Value, DomainError> {
        let jar = self.published()?.jar;
        let result = run_clipboard_child(&jar, operation, input, Some(claim));
        self.observe_denial(HelperFamily::Clipboard, &result);
        result
    }

    fn observe_denial(&self, family: HelperFamily, result: &Result<Value, DomainError>) {
        if matches!(result, Err(error) if error.code == ErrorCode::PermissionDenied) {
            self.denied[family.index()].store(true, Ordering::Release);
        }
    }
}

/// Runs one fixed clipboard child from the API-specific jar. This native launcher clears
/// supplementary groups and sets only GID/UID 2000 before `app_process`; the jar travels
/// as an inherited descriptor because the module root is not traversable by UID 2000.
pub(crate) fn run_clipboard_child(
    jar: &Path,
    operation: &str,
    input: &Value,
    claim: Option<&LocalExecutionClaim>,
) -> Result<Value, DomainError> {
    let jar_file = File::open(jar).map_err(|_| child_failure())?;
    let jar_fd = jar_file.as_raw_fd();
    let payload = serde_json::to_vec(input).map_err(|_| {
        DomainError::new(
            ErrorCode::InternalError,
            "clipboard request encoding failed",
        )
    })?;
    let mut command = Command::new("/system/bin/app_process");
    command
        .env(
            "CLASSPATH",
            format!("/proc/self/fd/{CLIPBOARD_CHILD_JAR_FD}"),
        )
        .arg("/system/bin")
        .arg("com.droidbridge.helper.DroidBridgeClipboardChild")
        .arg(operation)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(jar_fd, CLIPBOARD_CHILD_JAR_FD) < 0
                || libc::fcntl(CLIPBOARD_CHILD_JAR_FD, libc::F_SETFD, 0) < 0
                || libc::setgroups(0, std::ptr::null()) != 0
                || libc::setgid(SHELL_UID) != 0
                || libc::setuid(SHELL_UID) != 0
                || libc::getuid() != SHELL_UID
                || libc::getgid() != SHELL_UID
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().map_err(|_| child_failure())?;
    drop(jar_file);
    let mut stdin = child.stdin.take().ok_or_else(child_failure)?;
    let writer = thread::spawn(move || {
        let _ = stdin.write_all(&payload);
    });
    let mut stdout = child.stdout.take().ok_or_else(child_failure)?;
    let reader = thread::spawn(move || {
        let mut output = Vec::new();
        let read = (&mut stdout)
            .take(CLIPBOARD_CHILD_MAX_OUTPUT + 1)
            .read_to_end(&mut output);
        read.map(|_| output)
    });
    let deadline = Instant::now() + CLIPBOARD_CHILD_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {}
            Err(_) => break Err(child_failure()),
        }
        let cancelled = claim.is_some_and(|claim| claim.checkpoint().is_err());
        if cancelled || Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break Err(DomainError::new(
                if cancelled {
                    ErrorCode::Cancelled
                } else {
                    ErrorCode::Timeout
                },
                "clipboard child did not complete",
            ));
        }
        thread::sleep(Duration::from_millis(20));
    };
    let _ = writer.join();
    let output = reader
        .join()
        .map_err(|_| child_failure())?
        .map_err(|_| child_failure())?;
    let status = status?;
    if !status.success() || output.len() as u64 > CLIPBOARD_CHILD_MAX_OUTPUT {
        return Err(child_failure());
    }
    let response: Value = serde_json::from_slice(&output).map_err(|_| child_failure())?;
    decode_operation_response(&response)
}

fn child_failure() -> DomainError {
    DomainError::new(ErrorCode::IoError, "clipboard child failed")
}

#[derive(Clone)]
pub(crate) struct MagiskAndroidPort {
    root: Arc<RootCommandGuard>,
    companion: CompanionPort,
    helper: HelperPort,
}

impl MagiskAndroidPort {
    pub(crate) fn new(
        root: Arc<RootCommandGuard>,
        companion: CompanionPort,
        helper: HelperPort,
    ) -> Self {
        Self {
            root,
            companion,
            helper,
        }
    }
}

fn clean(error: DomainError) -> ExecutionFailure {
    ExecutionFailure {
        error,
        cleanup_verified: true,
    }
}

fn stale() -> ExecutionFailure {
    clean(DomainError::new(
        ErrorCode::StaleAuthority,
        "Android primitive provider is not bound to the Magisk surface",
    ))
}

fn require_completed(result: Value) -> Result<(), DomainError> {
    if result != json!({"completed": true}) {
        return Err(invalid_response());
    }
    Ok(())
}

impl AndroidPrimitivePort for MagiskAndroidPort {
    fn framework_package_inspect(
        &self,
        _execution: &AdmittedExecution,
        _package_name: &str,
        _claim: &LocalExecutionClaim,
    ) -> Result<FrameworkPackageInspection, ExecutionFailure> {
        Err(stale())
    }

    fn package_inventory(
        &self,
        execution: &AdmittedExecution,
        include_system: bool,
        claim: &LocalExecutionClaim,
    ) -> Result<Vec<PrivilegedPackageRecord>, ExecutionFailure> {
        if execution.executor.provider != ProviderToken::MagiskNative {
            return Err(stale());
        }
        let mut records = runtime::parse_package_inventory(
            &self.root.run_package(
                &execution.execution_id,
                PackageRootPrimitive::ListThirdParty,
                claim,
            )?,
            false,
        )
        .map_err(clean)?;
        if include_system {
            claim.checkpoint().map_err(clean)?;
            records.extend(
                runtime::parse_package_inventory(
                    &self.root.run_package(
                        &execution.execution_id,
                        PackageRootPrimitive::ListSystem,
                        claim,
                    )?,
                    true,
                )
                .map_err(clean)?,
            );
        }
        runtime::canonical_inventory(records).map_err(clean)
    }

    fn force_stop(
        &self,
        execution: &AdmittedExecution,
        package_name: &str,
        claim: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure> {
        if execution.executor.provider != ProviderToken::MagiskNative {
            return Err(stale());
        }
        self.root
            .run_package(
                &execution.execution_id,
                PackageRootPrimitive::ForceStop(package_name.to_owned()),
                claim,
            )
            .map(|_| ())
    }

    fn launch(
        &self,
        execution: &AdmittedExecution,
        input: &AndroidLaunchInput,
        claim: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure> {
        claim.checkpoint().map_err(clean)?;
        match execution.executor.provider {
            ProviderToken::MagiskFramework => {
                let request = match input {
                    AndroidLaunchInput::Package { package_name } => {
                        json!({"operation": "launch_package", "package_name": package_name})
                    }
                    AndroidLaunchInput::Component {
                        package_name,
                        class_name,
                    } => json!({
                        "operation": "launch_component",
                        "package_name": package_name,
                        "class_name": class_name,
                    }),
                };
                self.helper
                    .request(HelperFamily::Launch, request)
                    .and_then(require_completed)
                    .map_err(clean)
            }
            ProviderToken::AppFramework => {
                runtime::bridge_launch(&self.companion, execution, input).map_err(clean)
            }
            _ => Err(stale()),
        }
    }

    fn start_intent(
        &self,
        execution: &AdmittedExecution,
        input: &AndroidIntentInput,
        claim: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure> {
        claim.checkpoint().map_err(clean)?;
        match execution.executor.provider {
            ProviderToken::MagiskFramework => {
                let mut request = serde_json::to_value(input).map_err(|_| {
                    clean(DomainError::new(
                        ErrorCode::InternalError,
                        "intent request encoding failed",
                    ))
                })?;
                let operation = match input {
                    AndroidIntentInput::View { .. } => "start_view_intent",
                    AndroidIntentInput::ExplicitActivity { .. } => "start_explicit_activity",
                };
                request["operation"] = Value::String(operation.to_owned());
                self.helper
                    .request(HelperFamily::Launch, request)
                    .and_then(require_completed)
                    .map_err(clean)
            }
            ProviderToken::AppFramework => {
                runtime::bridge_start_intent(&self.companion, execution, input).map_err(clean)
            }
            _ => Err(stale()),
        }
    }

    fn clipboard_read(
        &self,
        execution: &AdmittedExecution,
        claim: &LocalExecutionClaim,
    ) -> Result<Option<String>, ExecutionFailure> {
        claim.checkpoint().map_err(clean)?;
        match execution.executor.provider {
            ProviderToken::MagiskFramework => self
                .helper
                .clipboard("read", &json!({}), claim)
                .and_then(|result| {
                    runtime::decode_clipboard_read(
                        &serde_json::to_vec(&result).map_err(|_| invalid_response())?,
                    )
                })
                .map_err(clean),
            ProviderToken::AppFramework => {
                runtime::bridge_clipboard_read(&self.companion, execution).map_err(clean)
            }
            _ => Err(stale()),
        }
    }

    fn clipboard_write(
        &self,
        execution: &AdmittedExecution,
        text: &str,
        claim: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure> {
        claim.checkpoint().map_err(clean)?;
        match execution.executor.provider {
            ProviderToken::MagiskFramework => self
                .helper
                .clipboard("write", &json!({"text": text}), claim)
                .and_then(require_completed)
                .map_err(clean),
            ProviderToken::AppFramework => {
                runtime::bridge_clipboard_write(&self.companion, execution, text).map_err(clean)
            }
            _ => Err(stale()),
        }
    }

    fn clipboard_clear(
        &self,
        execution: &AdmittedExecution,
        claim: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure> {
        claim.checkpoint().map_err(clean)?;
        match execution.executor.provider {
            ProviderToken::MagiskFramework => self
                .helper
                .clipboard("clear", &json!({}), claim)
                .and_then(require_completed)
                .map_err(clean),
            ProviderToken::AppFramework => {
                runtime::bridge_clipboard_clear(&self.companion, execution).map_err(clean)
            }
            _ => Err(stale()),
        }
    }

    fn notification_snapshot(
        &self,
        execution: &AdmittedExecution,
        claim: &LocalExecutionClaim,
    ) -> Result<Vec<AndroidNotificationRecord>, ExecutionFailure> {
        claim.checkpoint().map_err(clean)?;
        match execution.executor.provider {
            ProviderToken::MagiskFramework => self
                .helper
                .request(
                    HelperFamily::Notifications,
                    json!({"operation": "notification_snapshot"}),
                )
                .and_then(|result| {
                    runtime::decode_notification_snapshot(
                        &serde_json::to_vec(&result).map_err(|_| invalid_response())?,
                    )
                })
                .map_err(clean),
            ProviderToken::NotificationListener => {
                runtime::bridge_notification_snapshot(&self.companion, execution).map_err(clean)
            }
            _ => Err(stale()),
        }
    }

    fn notification_dismiss(
        &self,
        execution: &AdmittedExecution,
        identity: &AndroidNotificationIdentity,
        claim: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure> {
        claim.checkpoint().map_err(clean)?;
        match execution.executor.provider {
            ProviderToken::MagiskFramework => self
                .helper
                .request(
                    HelperFamily::Notifications,
                    json!({
                        "operation": "notification_dismiss",
                        "key": identity.key,
                        "generation": identity.generation,
                    }),
                )
                .and_then(require_completed)
                .map_err(clean),
            ProviderToken::NotificationListener => {
                runtime::bridge_notification_dismiss(&self.companion, execution, identity)
                    .map_err(clean)
            }
            _ => Err(stale()),
        }
    }

    fn notification_invoke(
        &self,
        execution: &AdmittedExecution,
        identity: &AndroidNotificationIdentity,
        action_index: u8,
        claim: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure> {
        claim.checkpoint().map_err(clean)?;
        match execution.executor.provider {
            ProviderToken::MagiskFramework => self
                .helper
                .request(
                    HelperFamily::Notifications,
                    json!({
                        "operation": "notification_invoke",
                        "key": identity.key,
                        "generation": identity.generation,
                        "action_index": action_index,
                    }),
                )
                .and_then(require_completed)
                .map_err(clean),
            ProviderToken::NotificationListener => runtime::bridge_notification_invoke(
                &self.companion,
                execution,
                identity,
                action_index,
            )
            .map_err(clean),
            _ => Err(stale()),
        }
    }
}
