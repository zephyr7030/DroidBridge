//! Shared Android mother-tool semantics and notification-reference ownership
//! (R-ANDROID-001..012, S-AUTH-ANDROID-001).

use crate::{
    AdmittedExecution, AndroidExecutionDispatch, ArtifactPort, CapabilityPort, CapabilitySnapshot,
    ExecutionCancelOutcome, ExecutionCompletion, ExecutionFailure, ExecutionOutcome,
    ExecutionPayload, ExecutionPort, ExecutorRecord, FilesystemPreflightPort, HostControlPort,
    LocalExecutionClaim, LocalExecutionClaims, PersistencePort, PortFuture, ProviderToken,
    RuntimeCore, SynchronousAdmission, UI_ENVELOPE_LIMIT_BYTES,
    command::{execution_failure, execution_fence, new_uuid},
};
use contract::{
    AndroidCall, AndroidClipboardInput, AndroidClipboardResult, AndroidIntentInput,
    AndroidIntentResult, AndroidLaunchInput, AndroidNotificationInput, AndroidNotificationResult,
    AndroidPackageInput, AndroidPackageResult, ComponentName, ErrorCode, IntentExtraValue,
    IntentOperation, LaunchResult, NotificationAction, NotificationSummary, PackageFact, RequestId,
    RuntimeHost, True, UuidV4,
};
use domain::{AndroidRoute, DomainError, ExecutorRequest, PackageInspectFact};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::{Arc, Mutex},
};

pub const ANDROID_NOTIFICATION_REF_LIMIT: usize = 256;
pub const ANDROID_NOTIFICATION_REF_TTL_MS: u64 = 300_000;
pub const ANDROID_PACKAGE_LIST_TIMEOUT_MS: u64 = 15_000;
pub const ANDROID_PACKAGE_LIST_MAX_STDOUT_BYTES: usize = 8 * 1_024 * 1_024;
pub const ANDROID_FORCE_STOP_TIMEOUT_MS: u64 = 15_000;
pub const ANDROID_CLIPBOARD_TEXT_MAX_BYTES: usize = 65_536;
const NOTIFICATION_STALE_LIMIT: usize = 1_024;
const PACKAGE_NAME_MAX_BYTES: usize = 255;
const CLASS_NAME_MAX_BYTES: usize = 512;
const URI_MAX_BYTES: usize = 4_096;
const EXTRAS_MAX_KEYS: usize = 32;
const EXTRA_KEY_MAX_BYTES: usize = 128;
const EXTRA_STRING_MAX_BYTES: usize = 4_096;
const VERSION_NAME_MAX_BYTES: usize = 256;
const NOTIFICATION_TITLE_MAX_BYTES: usize = 256;
const NOTIFICATION_TEXT_MAX_BYTES: usize = 512;
const NOTIFICATION_ACTION_TITLE_MAX_BYTES: usize = 512;
const NOTIFICATION_ACTIONS_MAX: usize = 32;

#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AndroidExecutionEnvelope {
    pub call: AndroidCall,
    pub admitted_at: String,
    pub admitted_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub privileged_inspect: Option<AndroidSourceAdmission>,
}

/// Clipboard text, notification content and Intent extras never reach ordinary logs
/// (S-SEC-003), so the envelope's diagnostic form names only the action.
impl fmt::Debug for AndroidExecutionEnvelope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AndroidExecutionEnvelope")
            .field("action", &android_action(&self.call))
            .field("admitted_at_ms", &self.admitted_at_ms)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AndroidSourceAdmission {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor: Option<ExecutorRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
}

impl AndroidSourceAdmission {
    fn resolve(capability: &CapabilitySnapshot, route: AndroidRoute) -> Self {
        match crate::resolve_execution(capability, ExecutorRequest::Android(route)) {
            Ok(executor) => Self {
                executor: Some(ExecutorRecord::from(&executor)),
                unavailable_reason: None,
            },
            Err(error) => Self {
                executor: None,
                unavailable_reason: Some(error_code_token(error.code)),
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum FrameworkPackageInspection {
    Visible(PackageFact),
    VisibilityOrAbsent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrivilegedPackageRecord {
    pub package_name: String,
    pub version_code: u64,
    pub system: bool,
}

#[derive(Clone, Eq, PartialEq)]
pub struct AndroidNotificationRecord {
    pub key: String,
    pub generation: u64,
    pub package_name: String,
    pub posted_at_ms: Option<u64>,
    pub title: Option<String>,
    pub text: Option<String>,
    pub actions: Vec<AndroidNotificationActionRecord>,
}

impl fmt::Debug for AndroidNotificationRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AndroidNotificationRecord")
            .field("generation", &self.generation)
            .field("action_count", &self.actions.len())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct AndroidNotificationActionRecord {
    pub title: Option<String>,
    pub requires_remote_input: bool,
}

impl fmt::Debug for AndroidNotificationActionRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AndroidNotificationActionRecord")
            .field("requires_remote_input", &self.requires_remote_input)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct AndroidNotificationIdentity {
    pub key: String,
    pub generation: u64,
}

impl fmt::Debug for AndroidNotificationIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AndroidNotificationIdentity")
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

/// The typed Android primitives one host supplies for the shared semantics below. The
/// admitted executor selects the provider; an adapter never substitutes another one.
pub trait AndroidPrimitivePort: Send + Sync {
    fn framework_package_inspect(
        &self,
        execution: &AdmittedExecution,
        package_name: &str,
        claim: &LocalExecutionClaim,
    ) -> Result<FrameworkPackageInspection, ExecutionFailure>;

    /// Returns the exact user-0 inventory: third-party packages, plus system packages
    /// when `include_system` is set.
    fn package_inventory(
        &self,
        execution: &AdmittedExecution,
        include_system: bool,
        claim: &LocalExecutionClaim,
    ) -> Result<Vec<PrivilegedPackageRecord>, ExecutionFailure>;

    fn force_stop(
        &self,
        execution: &AdmittedExecution,
        package_name: &str,
        claim: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure>;

    fn launch(
        &self,
        execution: &AdmittedExecution,
        input: &AndroidLaunchInput,
        claim: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure>;

    fn start_intent(
        &self,
        execution: &AdmittedExecution,
        input: &AndroidIntentInput,
        claim: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure>;

    /// `None` is the collapsed empty/non-text/not-readable observation; a thrown
    /// platform failure is an `Err`.
    fn clipboard_read(
        &self,
        execution: &AdmittedExecution,
        claim: &LocalExecutionClaim,
    ) -> Result<Option<String>, ExecutionFailure>;

    fn clipboard_write(
        &self,
        execution: &AdmittedExecution,
        text: &str,
        claim: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure>;

    fn clipboard_clear(
        &self,
        execution: &AdmittedExecution,
        claim: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure>;

    fn notification_snapshot(
        &self,
        execution: &AdmittedExecution,
        claim: &LocalExecutionClaim,
    ) -> Result<Vec<AndroidNotificationRecord>, ExecutionFailure>;

    fn notification_dismiss(
        &self,
        execution: &AdmittedExecution,
        identity: &AndroidNotificationIdentity,
        claim: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure>;

    fn notification_invoke(
        &self,
        execution: &AdmittedExecution,
        identity: &AndroidNotificationIdentity,
        action_index: u8,
        claim: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct UnavailableAndroidPrimitivePort;

fn unavailable_primitive<T>() -> Result<T, ExecutionFailure> {
    Err(execution_failure(
        ErrorCode::CapabilityUnavailable,
        "Android primitive provider is unavailable",
        true,
    ))
}

impl AndroidPrimitivePort for UnavailableAndroidPrimitivePort {
    fn framework_package_inspect(
        &self,
        _: &AdmittedExecution,
        _: &str,
        _: &LocalExecutionClaim,
    ) -> Result<FrameworkPackageInspection, ExecutionFailure> {
        unavailable_primitive()
    }

    fn package_inventory(
        &self,
        _: &AdmittedExecution,
        _: bool,
        _: &LocalExecutionClaim,
    ) -> Result<Vec<PrivilegedPackageRecord>, ExecutionFailure> {
        unavailable_primitive()
    }

    fn force_stop(
        &self,
        _: &AdmittedExecution,
        _: &str,
        _: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure> {
        unavailable_primitive()
    }

    fn launch(
        &self,
        _: &AdmittedExecution,
        _: &AndroidLaunchInput,
        _: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure> {
        unavailable_primitive()
    }

    fn start_intent(
        &self,
        _: &AdmittedExecution,
        _: &AndroidIntentInput,
        _: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure> {
        unavailable_primitive()
    }

    fn clipboard_read(
        &self,
        _: &AdmittedExecution,
        _: &LocalExecutionClaim,
    ) -> Result<Option<String>, ExecutionFailure> {
        unavailable_primitive()
    }

    fn clipboard_write(
        &self,
        _: &AdmittedExecution,
        _: &str,
        _: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure> {
        unavailable_primitive()
    }

    fn clipboard_clear(
        &self,
        _: &AdmittedExecution,
        _: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure> {
        unavailable_primitive()
    }

    fn notification_snapshot(
        &self,
        _: &AdmittedExecution,
        _: &LocalExecutionClaim,
    ) -> Result<Vec<AndroidNotificationRecord>, ExecutionFailure> {
        unavailable_primitive()
    }

    fn notification_dismiss(
        &self,
        _: &AdmittedExecution,
        _: &AndroidNotificationIdentity,
        _: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure> {
        unavailable_primitive()
    }

    fn notification_invoke(
        &self,
        _: &AdmittedExecution,
        _: &AndroidNotificationIdentity,
        _: u8,
        _: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure> {
        unavailable_primitive()
    }
}

#[derive(Clone)]
pub struct NativeAndroidExecutionSurface<C, P = UnavailableAndroidPrimitivePort> {
    capabilities: C,
    own_package: Arc<str>,
    primitives: P,
    notifications: Arc<Mutex<NotificationRefCache>>,
    claims: LocalExecutionClaims,
}

impl<C> NativeAndroidExecutionSurface<C, UnavailableAndroidPrimitivePort> {
    pub fn new(capabilities: C, own_package: impl Into<String>) -> Self {
        Self {
            capabilities,
            own_package: Arc::from(own_package.into()),
            primitives: UnavailableAndroidPrimitivePort,
            notifications: Arc::new(Mutex::new(NotificationRefCache::default())),
            claims: LocalExecutionClaims::default(),
        }
    }
}

impl<C, P> NativeAndroidExecutionSurface<C, P> {
    pub fn with_primitives<N>(self, primitives: N) -> NativeAndroidExecutionSurface<C, N> {
        NativeAndroidExecutionSurface {
            capabilities: self.capabilities,
            own_package: self.own_package,
            primitives,
            notifications: self.notifications,
            claims: self.claims,
        }
    }
}

impl<C, P> ExecutionPort for NativeAndroidExecutionSurface<C, P>
where
    C: CapabilityPort + Clone + 'static,
    P: AndroidPrimitivePort + Clone + 'static,
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
        let capabilities = self.capabilities.clone();
        let primitives = self.primitives.clone();
        let own_package = Arc::clone(&self.own_package);
        let notifications = Arc::clone(&self.notifications);
        let claims = self.claims.clone();
        Box::pin(async move {
            let result = execute_android(
                &capabilities,
                &own_package,
                &primitives,
                &notifications,
                &execution,
                &claim,
            );
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

pub async fn handle_android_public<P, A, E, C, H>(
    core: &RuntimeCore<P, A, E, C, H>,
    request_id: RequestId,
    payload_sha256: String,
    call: AndroidCall,
    timestamp: String,
    now_ms: u64,
) -> Result<serde_json::Value, DomainError>
where
    P: PersistencePort + 'static,
    A: ArtifactPort + Clone + 'static,
    E: ExecutionPort + FilesystemPreflightPort + 'static,
    C: CapabilityPort + 'static,
    H: HostControlPort + 'static,
{
    validate_android_input(&call)?;
    let capability = core.capability_snapshot()?;
    let host = capability.context.host;
    let privileged_inspect = match (&call, host) {
        (AndroidCall::Package(AndroidPackageInput::Inspect { .. }), RuntimeHost::ApkRuntime) => {
            Some(AndroidSourceAdmission::resolve(
                &capability,
                AndroidRoute::PackageInspect(PackageInspectFact::VisibilityOrAbsent),
            ))
        }
        _ => None,
    };
    let execution_id = new_uuid()?;
    let route = android_route(&call, host);
    core.run_synchronous(
        SynchronousAdmission {
            request_id,
            payload_sha256,
            execution_id,
            operation: format!("android.{}", android_action(&call)),
            route: ExecutorRequest::Android(route),
            payload: ExecutionPayload::AndroidCall(AndroidExecutionEnvelope {
                call,
                admitted_at: timestamp.clone(),
                admitted_at_ms: now_ms,
                privileged_inspect,
            }),
            settlement_bound_bytes: UI_ENVELOPE_LIMIT_BYTES as u64,
            now_ms,
        },
        timestamp,
        now_ms,
    )
    .await
    .map_err(|error| DomainError::new(error.code, "Android execution failed"))
}

/// Revalidates R-ANDROID-002/006/007/008/009 bounds before any executor is resolved.
pub fn validate_android_input(call: &AndroidCall) -> Result<(), DomainError> {
    match call {
        AndroidCall::Package(
            AndroidPackageInput::Inspect { package_name }
            | AndroidPackageInput::ForceStop { package_name },
        ) => validate_package_name(package_name),
        AndroidCall::Package(AndroidPackageInput::List {
            after_package,
            limit,
            ..
        }) => {
            if !(1..=200).contains(limit) {
                return Err(DomainError::invalid("package list limit is out of range"));
            }
            after_package
                .as_deref()
                .map_or(Ok(()), validate_package_name)
        }
        AndroidCall::Launch(AndroidLaunchInput::Package { package_name }) => {
            validate_package_name(package_name)
        }
        AndroidCall::Launch(AndroidLaunchInput::Component {
            package_name,
            class_name,
        }) => {
            validate_package_name(package_name)?;
            validate_class_name(class_name)
        }
        AndroidCall::Intent(AndroidIntentInput::View {
            data_uri,
            package_name,
        }) => {
            validate_uri(data_uri)?;
            package_name
                .as_deref()
                .map_or(Ok(()), validate_package_name)
        }
        AndroidCall::Intent(AndroidIntentInput::ExplicitActivity {
            package_name,
            class_name,
            data_uri,
            extras,
            ..
        }) => {
            validate_package_name(package_name)?;
            validate_class_name(class_name)?;
            data_uri.as_deref().map_or(Ok(()), validate_uri)?;
            let Some(extras) = extras else {
                return Ok(());
            };
            if extras.len() > EXTRAS_MAX_KEYS {
                return Err(DomainError::invalid("intent extras exceed the key limit"));
            }
            for (key, value) in extras {
                if key.is_empty() || key.len() > EXTRA_KEY_MAX_BYTES {
                    return Err(DomainError::invalid("intent extra key is out of bounds"));
                }
                if matches!(value, IntentExtraValue::String(text) if text.len() > EXTRA_STRING_MAX_BYTES)
                {
                    return Err(DomainError::invalid("intent extra string is out of bounds"));
                }
            }
            Ok(())
        }
        AndroidCall::Clipboard(AndroidClipboardInput::Write { text }) => {
            if text.len() > ANDROID_CLIPBOARD_TEXT_MAX_BYTES {
                return Err(DomainError::invalid("clipboard text exceeds its bound"));
            }
            Ok(())
        }
        AndroidCall::Clipboard(
            AndroidClipboardInput::Read {} | AndroidClipboardInput::Clear {},
        ) => Ok(()),
        AndroidCall::Notification(AndroidNotificationInput::List { limit }) => {
            if !(1..=100).contains(limit) {
                return Err(DomainError::invalid(
                    "notification list limit is out of range",
                ));
            }
            Ok(())
        }
        AndroidCall::Notification(AndroidNotificationInput::InvokeAction {
            action_index, ..
        }) => {
            if *action_index > 31 {
                return Err(DomainError::invalid(
                    "notification action index is out of range",
                ));
            }
            Ok(())
        }
        AndroidCall::Notification(
            AndroidNotificationInput::Get { .. } | AndroidNotificationInput::Dismiss { .. },
        ) => Ok(()),
    }
}

pub fn validate_package_name(value: &str) -> Result<(), DomainError> {
    if value.is_empty() || value.len() > PACKAGE_NAME_MAX_BYTES || value.contains('\0') {
        return Err(DomainError::invalid("package name is out of bounds"));
    }
    Ok(())
}

fn validate_class_name(value: &str) -> Result<(), DomainError> {
    if value.is_empty() || value.len() > CLASS_NAME_MAX_BYTES || value.contains('\0') {
        return Err(DomainError::invalid("class name is out of bounds"));
    }
    Ok(())
}

fn validate_uri(value: &str) -> Result<(), DomainError> {
    if value.len() > URI_MAX_BYTES || value.contains('\0') {
        return Err(DomainError::invalid("URI is out of bounds"));
    }
    Ok(())
}

/// Parses one S-SHIZUKU-006/S-MAGISK-005 package-shell inventory. Only exact
/// `package:<name> versionCode:<u64>` records are identity evidence.
pub fn parse_package_inventory(
    stdout: &[u8],
    system: bool,
) -> Result<Vec<PrivilegedPackageRecord>, DomainError> {
    let malformed = || DomainError::new(ErrorCode::IoError, "package inventory is malformed");
    if stdout.len() > ANDROID_PACKAGE_LIST_MAX_STDOUT_BYTES {
        return Err(DomainError::new(
            ErrorCode::IoError,
            "package inventory exceeds its output bound",
        ));
    }
    let text = std::str::from_utf8(stdout).map_err(|_| malformed())?;
    let mut records = Vec::new();
    for line in text.split('\n').filter(|line| !line.is_empty()) {
        let rest = line.strip_prefix("package:").ok_or_else(malformed)?;
        let (name, version) = rest.split_once(" versionCode:").ok_or_else(malformed)?;
        if name.contains(' ')
            || version.is_empty()
            || !version.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(malformed());
        }
        validate_package_name(name).map_err(|_| malformed())?;
        records.push(PrivilegedPackageRecord {
            package_name: name.to_owned(),
            version_code: version.parse().map_err(|_| malformed())?,
            system,
        });
    }
    canonical_inventory(records)
}

/// Sorts and de-duplicates an inventory; the same package with a different
/// classification or version is conflicting evidence rather than a merge.
pub fn canonical_inventory(
    mut records: Vec<PrivilegedPackageRecord>,
) -> Result<Vec<PrivilegedPackageRecord>, DomainError> {
    records.sort_by(|left, right| left.package_name.cmp(&right.package_name));
    let mut canonical: Vec<PrivilegedPackageRecord> = Vec::with_capacity(records.len());
    for record in records {
        match canonical.last() {
            Some(last) if last.package_name == record.package_name => {
                if *last != record {
                    return Err(DomainError::new(
                        ErrorCode::IoError,
                        "package inventory classification conflicts",
                    ));
                }
            }
            _ => canonical.push(record),
        }
    }
    Ok(canonical)
}

fn android_action(call: &AndroidCall) -> &'static str {
    match call {
        AndroidCall::Package(_) => "package",
        AndroidCall::Launch(_) => "launch",
        AndroidCall::Intent(_) => "intent",
        AndroidCall::Clipboard(_) => "clipboard",
        AndroidCall::Notification(_) => "notification",
    }
}

fn android_route(call: &AndroidCall, host: RuntimeHost) -> AndroidRoute {
    match call {
        AndroidCall::Package(AndroidPackageInput::Inspect { .. }) => {
            AndroidRoute::PackageInspect(match host {
                RuntimeHost::ApkRuntime => PackageInspectFact::ExactSuccess,
                RuntimeHost::MagiskBackend => PackageInspectFact::Unknown,
            })
        }
        AndroidCall::Package(AndroidPackageInput::List { .. }) => AndroidRoute::PackageList,
        AndroidCall::Package(AndroidPackageInput::ForceStop { .. }) => {
            AndroidRoute::PackageForceStop
        }
        AndroidCall::Launch(_) | AndroidCall::Intent(_) => AndroidRoute::LaunchOrIntent,
        AndroidCall::Clipboard(_) => AndroidRoute::Clipboard,
        AndroidCall::Notification(_) => AndroidRoute::Notification,
    }
}

fn execute_android<C, P>(
    capabilities: &C,
    own_package: &str,
    primitives: &P,
    notifications: &Mutex<NotificationRefCache>,
    execution: &AdmittedExecution,
    claim: &LocalExecutionClaim,
) -> Result<ExecutionCompletion, ExecutionFailure>
where
    C: CapabilityPort,
    P: AndroidPrimitivePort,
{
    let ExecutionPayload::AndroidCall(envelope) = &execution.payload else {
        return Err(execution_failure(
            ErrorCode::Unsupported,
            "Android surface received a non-Android request",
            true,
        ));
    };
    validate_android_input(&envelope.call).map_err(verified_failure)?;
    let current = capabilities.current().map_err(verified_failure)?;
    let route = android_route(&envelope.call, execution.executor.host);
    if !route_is_current(&current, route, &execution.executor) {
        return Err(execution_failure(
            ErrorCode::StaleAuthority,
            "Android executor fence or generation is stale",
            true,
        ));
    }
    claim.checkpoint().map_err(verified_failure)?;
    let result = match &envelope.call {
        AndroidCall::Package(input) => encode(execute_package(
            primitives,
            own_package,
            &current,
            execution,
            claim,
            envelope,
            input,
        )?)?,
        AndroidCall::Launch(input) => {
            primitives.launch(execution, input, claim)?;
            encode(match input {
                AndroidLaunchInput::Package { package_name } => LaunchResult {
                    launched: True,
                    package_name: package_name.clone(),
                    component: None,
                },
                AndroidLaunchInput::Component {
                    package_name,
                    class_name,
                } => LaunchResult {
                    launched: True,
                    package_name: package_name.clone(),
                    component: Some(ComponentName {
                        package_name: package_name.clone(),
                        class_name: class_name.clone(),
                    }),
                },
            })?
        }
        AndroidCall::Intent(input) => {
            primitives.start_intent(execution, input, claim)?;
            encode(match input {
                AndroidIntentInput::View { package_name, .. } => AndroidIntentResult {
                    started: True,
                    operation: IntentOperation::View,
                    package_name: package_name.clone(),
                    component: None,
                },
                AndroidIntentInput::ExplicitActivity {
                    package_name,
                    class_name,
                    ..
                } => AndroidIntentResult {
                    started: True,
                    operation: IntentOperation::ExplicitActivity,
                    package_name: Some(package_name.clone()),
                    component: Some(ComponentName {
                        package_name: package_name.clone(),
                        class_name: class_name.clone(),
                    }),
                },
            })?
        }
        AndroidCall::Clipboard(input) => encode(match input {
            AndroidClipboardInput::Read {} => match primitives.clipboard_read(execution, claim)? {
                Some(text) if text.len() > ANDROID_CLIPBOARD_TEXT_MAX_BYTES => {
                    return Err(execution_failure(
                        ErrorCode::ResourceLimit,
                        "clipboard text exceeds its bound",
                        true,
                    ));
                }
                Some(text) if !text.is_empty() => AndroidClipboardResult::Read {
                    has_text: true,
                    text: Some(text),
                },
                _ => AndroidClipboardResult::Read {
                    has_text: false,
                    text: None,
                },
            },
            AndroidClipboardInput::Write { text } => {
                primitives.clipboard_write(execution, text, claim)?;
                AndroidClipboardResult::Write { written: True }
            }
            AndroidClipboardInput::Clear {} => {
                primitives.clipboard_clear(execution, claim)?;
                AndroidClipboardResult::Clear { cleared: True }
            }
        })?,
        AndroidCall::Notification(input) => encode(execute_notification(
            primitives,
            notifications,
            execution,
            claim,
            envelope.admitted_at_ms,
            input,
        )?)?,
    };
    let encoded_bytes = serde_json::to_vec(&result)
        .map_err(|_| {
            verified_failure(DomainError::new(
                ErrorCode::InternalError,
                "Android result encoding failed",
            ))
        })?
        .len() as u64;
    if encoded_bytes > UI_ENVELOPE_LIMIT_BYTES as u64 {
        return Err(execution_failure(
            ErrorCode::ResourceLimit,
            "Android result exceeds the protocol frame limit",
            true,
        ));
    }
    Ok(ExecutionCompletion {
        fence: execution_fence(execution),
        capability_generation: execution.executor.capability_generation,
        outcome: ExecutionOutcome::SynchronousCompleted {
            result,
            encoded_bytes,
        },
        cleanup_verified: claim.cleanup_is_verified(),
    })
}

fn execute_package<P: AndroidPrimitivePort>(
    primitives: &P,
    own_package: &str,
    current: &CapabilitySnapshot,
    execution: &AdmittedExecution,
    claim: &LocalExecutionClaim,
    envelope: &AndroidExecutionEnvelope,
    input: &AndroidPackageInput,
) -> Result<AndroidPackageResult, ExecutionFailure> {
    match input {
        AndroidPackageInput::Inspect { package_name } => {
            let package = match execution.executor.provider {
                ProviderToken::AppFramework => {
                    match primitives.framework_package_inspect(execution, package_name, claim)? {
                        FrameworkPackageInspection::Visible(fact) => {
                            framework_fact(fact, package_name)?
                        }
                        FrameworkPackageInspection::VisibilityOrAbsent => {
                            let source = envelope.privileged_inspect.as_ref().ok_or_else(|| {
                                execution_failure(
                                    ErrorCode::IoError,
                                    "privileged package source was not admitted",
                                    true,
                                )
                            })?;
                            let Some(executor) = source.executor.as_ref() else {
                                return Err(execution_failure(
                                    ErrorCode::CapabilityUnavailable,
                                    "no privileged package source can distinguish hidden from absent",
                                    true,
                                ));
                            };
                            if !route_is_current(
                                current,
                                AndroidRoute::PackageInspect(
                                    PackageInspectFact::VisibilityOrAbsent,
                                ),
                                executor,
                            ) {
                                return Err(execution_failure(
                                    ErrorCode::StaleAuthority,
                                    "privileged package source is stale",
                                    true,
                                ));
                            }
                            claim.checkpoint().map_err(verified_failure)?;
                            let privileged = AdmittedExecution {
                                executor: executor.clone(),
                                ..execution.clone()
                            };
                            inventory_package(primitives, &privileged, package_name, claim)?
                        }
                    }
                }
                ProviderToken::Shizuku | ProviderToken::MagiskNative => {
                    inventory_package(primitives, execution, package_name, claim)?
                }
                _ => {
                    return Err(execution_failure(
                        ErrorCode::StaleAuthority,
                        "package inspect provider is invalid",
                        true,
                    ));
                }
            };
            Ok(AndroidPackageResult::Inspect { package })
        }
        AndroidPackageInput::List {
            include_system,
            after_package,
            limit,
        } => {
            require_privileged_package_source(execution)?;
            let inventory = validated_inventory(
                primitives.package_inventory(execution, *include_system, claim)?,
                *include_system,
            )?;
            let remaining: Vec<_> = inventory
                .into_iter()
                .filter(|record| {
                    after_package
                        .as_deref()
                        .is_none_or(|after| record.package_name.as_str() > after)
                })
                .collect();
            let limit = *limit as usize;
            let truncated = remaining.len() > limit;
            let packages: Vec<PackageFact> = remaining
                .into_iter()
                .take(limit)
                .map(package_fact)
                .collect();
            let next_after_package = truncated
                .then(|| packages.last().map(|fact| fact.package_name.clone()))
                .flatten();
            Ok(AndroidPackageResult::List {
                packages,
                truncated,
                next_after_package,
            })
        }
        AndroidPackageInput::ForceStop { package_name } => {
            require_privileged_package_source(execution)?;
            if package_name == own_package {
                return Err(execution_failure(
                    ErrorCode::InvalidArgument,
                    "force-stop cannot target the running DroidBridge package",
                    true,
                ));
            }
            claim.checkpoint().map_err(verified_failure)?;
            primitives.force_stop(execution, package_name, claim)?;
            Ok(AndroidPackageResult::ForceStop {
                package_name: package_name.clone(),
                completed: True,
            })
        }
    }
}

fn require_privileged_package_source(
    execution: &AdmittedExecution,
) -> Result<(), ExecutionFailure> {
    match execution.executor.provider {
        ProviderToken::Shizuku | ProviderToken::MagiskNative => Ok(()),
        _ => Err(execution_failure(
            ErrorCode::CapabilityUnavailable,
            "package inventory and force-stop require privileged execution",
            true,
        )),
    }
}

fn framework_fact(fact: PackageFact, package_name: &str) -> Result<PackageFact, ExecutionFailure> {
    if fact.package_name != package_name
        || fact
            .version_name
            .as_ref()
            .is_some_and(|name| name.len() > VERSION_NAME_MAX_BYTES)
    {
        return Err(execution_failure(
            ErrorCode::IoError,
            "framework package fact is invalid",
            true,
        ));
    }
    Ok(fact)
}

fn inventory_package<P: AndroidPrimitivePort>(
    primitives: &P,
    execution: &AdmittedExecution,
    package_name: &str,
    claim: &LocalExecutionClaim,
) -> Result<PackageFact, ExecutionFailure> {
    validated_inventory(primitives.package_inventory(execution, true, claim)?, true)?
        .into_iter()
        .find(|record| record.package_name == package_name)
        .map(package_fact)
        .ok_or_else(|| execution_failure(ErrorCode::NotFound, "package is absent for user 0", true))
}

fn validated_inventory(
    records: Vec<PrivilegedPackageRecord>,
    include_system: bool,
) -> Result<Vec<PrivilegedPackageRecord>, ExecutionFailure> {
    if records.iter().any(|record| {
        validate_package_name(&record.package_name).is_err() || (!include_system && record.system)
    }) {
        return Err(execution_failure(
            ErrorCode::IoError,
            "package inventory is invalid",
            true,
        ));
    }
    canonical_inventory(records).map_err(verified_failure)
}

fn package_fact(record: PrivilegedPackageRecord) -> PackageFact {
    PackageFact {
        package_name: record.package_name,
        version_name: None,
        version_code: Some(record.version_code),
        enabled: None,
        system: Some(record.system),
        launchable: None,
    }
}

fn execute_notification<P: AndroidPrimitivePort>(
    primitives: &P,
    notifications: &Mutex<NotificationRefCache>,
    execution: &AdmittedExecution,
    claim: &LocalExecutionClaim,
    now_ms: u64,
    input: &AndroidNotificationInput,
) -> Result<AndroidNotificationResult, ExecutionFailure> {
    match input {
        AndroidNotificationInput::List { limit } => {
            let mut snapshot =
                validated_notifications(primitives.notification_snapshot(execution, claim)?)?;
            snapshot.sort_by(|left, right| {
                right
                    .posted_at_ms
                    .cmp(&left.posted_at_ms)
                    .then_with(|| left.key.cmp(&right.key))
            });
            let page_len = snapshot.len().min(*limit as usize);
            let refs = claim
                .publish(|| {
                    notifications
                        .lock()
                        .map_err(|_| notification_lock_error())?
                        .publish(
                            &execution.executor,
                            &snapshot,
                            &snapshot[..page_len],
                            now_ms,
                        )
                })
                .map_err(verified_failure)?;
            let notifications = snapshot[..page_len]
                .iter()
                .zip(refs)
                .map(|(record, (notification_ref, created_at_ms))| {
                    summary(record, notification_ref, created_at_ms)
                })
                .collect::<Result<_, _>>()?;
            Ok(AndroidNotificationResult::List { notifications })
        }
        AndroidNotificationInput::Get { notification_ref } => {
            let pinned = PinnedNotification::pin(
                notifications,
                notification_ref,
                &execution.executor,
                now_ms,
            )?;
            let snapshot =
                validated_notifications(primitives.notification_snapshot(execution, claim)?)?;
            notifications
                .lock()
                .map_err(|_| verified_failure(notification_lock_error()))?
                .apply_snapshot(&execution.executor, &snapshot, now_ms);
            let Some(record) = snapshot
                .iter()
                .find(|record| record.key == pinned.identity.key)
                .filter(|record| record.generation == pinned.identity.generation)
            else {
                pinned.invalidate();
                return Err(verified_failure(stale_reference()));
            };
            let actions = record
                .actions
                .iter()
                .take(NOTIFICATION_ACTIONS_MAX)
                .enumerate()
                .map(|(index, action)| NotificationAction {
                    index: index as u8,
                    title: bounded_text(
                        action.title.as_deref(),
                        NOTIFICATION_ACTION_TITLE_MAX_BYTES,
                    ),
                    requires_remote_input: action.requires_remote_input,
                })
                .collect();
            Ok(AndroidNotificationResult::Get {
                notification: summary(record, notification_ref.clone(), pinned.created_at_ms)?,
                actions,
                actions_truncated: record.actions.len() > NOTIFICATION_ACTIONS_MAX,
            })
        }
        AndroidNotificationInput::Dismiss { notification_ref } => {
            let pinned = PinnedNotification::pin(
                notifications,
                notification_ref,
                &execution.executor,
                now_ms,
            )?;
            claim.checkpoint().map_err(verified_failure)?;
            primitives
                .notification_dismiss(execution, &pinned.identity, claim)
                .inspect_err(|failure| pinned.invalidate_if_stale(failure))?;
            Ok(AndroidNotificationResult::Dismiss {
                notification_ref: notification_ref.clone(),
                dismissed: True,
            })
        }
        AndroidNotificationInput::InvokeAction {
            notification_ref,
            action_index,
        } => {
            let pinned = PinnedNotification::pin(
                notifications,
                notification_ref,
                &execution.executor,
                now_ms,
            )?;
            claim.checkpoint().map_err(verified_failure)?;
            primitives
                .notification_invoke(execution, &pinned.identity, *action_index, claim)
                .inspect_err(|failure| pinned.invalidate_if_stale(failure))?;
            Ok(AndroidNotificationResult::InvokeAction {
                notification_ref: notification_ref.clone(),
                action_index: *action_index,
                invoked: True,
            })
        }
    }
}

fn validated_notifications(
    records: Vec<AndroidNotificationRecord>,
) -> Result<Vec<AndroidNotificationRecord>, ExecutionFailure> {
    let mut keys = BTreeSet::new();
    for record in &records {
        if record.key.is_empty()
            || record.generation == 0
            || validate_package_name(&record.package_name).is_err()
            || !keys.insert(record.key.as_str())
        {
            return Err(execution_failure(
                ErrorCode::IoError,
                "notification snapshot is invalid",
                true,
            ));
        }
    }
    Ok(records)
}

fn summary(
    record: &AndroidNotificationRecord,
    notification_ref: String,
    created_at_ms: u64,
) -> Result<NotificationSummary, ExecutionFailure> {
    Ok(NotificationSummary {
        notification_ref,
        expires_at: instant(created_at_ms.saturating_add(ANDROID_NOTIFICATION_REF_TTL_MS))?,
        package_name: record.package_name.clone(),
        posted_at: record.posted_at_ms.map(instant).transpose()?,
        title: bounded_text(record.title.as_deref(), NOTIFICATION_TITLE_MAX_BYTES),
        text: bounded_text(record.text.as_deref(), NOTIFICATION_TEXT_MAX_BYTES),
        action_count: u32::try_from(record.actions.len()).unwrap_or(u32::MAX),
    })
}

fn instant(ms: u64) -> Result<String, ExecutionFailure> {
    i64::try_from(ms)
        .ok()
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map(|value| value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .ok_or_else(|| {
            execution_failure(ErrorCode::IoError, "notification instant is invalid", true)
        })
}

fn bounded_text(value: Option<&str>, max_bytes: usize) -> Option<String> {
    let value = value?;
    let mut end = value.len().min(max_bytes);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    Some(value[..end].to_owned())
}

struct PinnedNotification<'a> {
    cache: &'a Mutex<NotificationRefCache>,
    notification_ref: &'a str,
    identity: AndroidNotificationIdentity,
    created_at_ms: u64,
}

impl<'a> PinnedNotification<'a> {
    fn pin(
        cache: &'a Mutex<NotificationRefCache>,
        notification_ref: &'a str,
        executor: &ExecutorRecord,
        now_ms: u64,
    ) -> Result<Self, ExecutionFailure> {
        let (identity, created_at_ms) = cache
            .lock()
            .map_err(|_| verified_failure(notification_lock_error()))?
            .pin(notification_ref, executor, now_ms)
            .map_err(verified_failure)?;
        Ok(Self {
            cache,
            notification_ref,
            identity,
            created_at_ms,
        })
    }

    fn invalidate(&self) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.mark_stale(self.notification_ref);
        }
    }

    fn invalidate_if_stale(&self, failure: &ExecutionFailure) {
        if failure.error.code == ErrorCode::StaleReference {
            self.invalidate();
        }
    }
}

impl Drop for PinnedNotification<'_> {
    fn drop(&mut self) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.unpin(self.notification_ref);
        }
    }
}

#[derive(Clone, Debug)]
struct NotificationRefRecord {
    executor: ExecutorRecord,
    identity: AndroidNotificationIdentity,
    created_at_ms: u64,
    pins: usize,
}

/// R-ANDROID-010 reference ownership. Live refs are bounded and pinned only while an
/// admitted operation uses them; refs invalidated by replacement/removal are kept as
/// bounded tombstones until their own expiry so they answer `STALE_REFERENCE`.
#[derive(Default)]
struct NotificationRefCache {
    live: BTreeMap<String, NotificationRefRecord>,
    stale: BTreeMap<String, u64>,
}

impl NotificationRefCache {
    fn expire(&mut self, now_ms: u64) {
        self.live.retain(|_, record| {
            record
                .created_at_ms
                .saturating_add(ANDROID_NOTIFICATION_REF_TTL_MS)
                > now_ms
        });
        self.stale
            .retain(|_, created| created.saturating_add(ANDROID_NOTIFICATION_REF_TTL_MS) > now_ms);
    }

    fn mark_stale(&mut self, notification_ref: &str) {
        let Some(record) = self.live.remove(notification_ref) else {
            return;
        };
        self.stale
            .insert(notification_ref.to_owned(), record.created_at_ms);
        while self.stale.len() > NOTIFICATION_STALE_LIMIT {
            let oldest = self
                .stale
                .iter()
                .min_by(|left, right| (left.1, left.0).cmp(&(right.1, right.0)))
                .map(|(key, _)| key.clone());
            match oldest {
                Some(key) => {
                    self.stale.remove(&key);
                }
                None => break,
            }
        }
    }

    fn apply_snapshot(
        &mut self,
        executor: &ExecutorRecord,
        snapshot: &[AndroidNotificationRecord],
        now_ms: u64,
    ) {
        self.expire(now_ms);
        let current: BTreeMap<&str, u64> = snapshot
            .iter()
            .map(|record| (record.key.as_str(), record.generation))
            .collect();
        let stale: Vec<String> = self
            .live
            .iter()
            .filter(|(_, record)| {
                record.executor != *executor
                    || current.get(record.identity.key.as_str())
                        != Some(&record.identity.generation)
            })
            .map(|(notification_ref, _)| notification_ref.clone())
            .collect();
        for notification_ref in stale {
            self.mark_stale(&notification_ref);
        }
    }

    fn publish(
        &mut self,
        executor: &ExecutorRecord,
        snapshot: &[AndroidNotificationRecord],
        page: &[AndroidNotificationRecord],
        now_ms: u64,
    ) -> Result<Vec<(String, u64)>, DomainError> {
        self.apply_snapshot(executor, snapshot, now_ms);
        let mut reused = BTreeSet::new();
        let mut assigned: Vec<Option<(String, u64)>> = page
            .iter()
            .map(|entry| {
                self.live
                    .iter()
                    .find(|(_, record)| {
                        record.executor == *executor
                            && record.identity.key == entry.key
                            && record.identity.generation == entry.generation
                    })
                    .map(|(notification_ref, record)| {
                        reused.insert(notification_ref.clone());
                        (notification_ref.clone(), record.created_at_ms)
                    })
            })
            .collect();
        let needed = assigned.iter().filter(|slot| slot.is_none()).count();
        let overflow = (self.live.len() + needed).saturating_sub(ANDROID_NOTIFICATION_REF_LIMIT);
        let mut evictable: Vec<(u64, String)> = self
            .live
            .iter()
            .filter(|(notification_ref, record)| {
                record.pins == 0 && !reused.contains(*notification_ref)
            })
            .map(|(notification_ref, record)| (record.created_at_ms, notification_ref.clone()))
            .collect();
        if overflow > evictable.len() {
            return Err(DomainError::new(
                ErrorCode::ResourceLimit,
                "notification reference capacity is pinned",
            ));
        }
        let mut fresh = (0..needed)
            .map(|_| new_uuid().map(|id| id.as_str().to_owned()))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter();
        evictable.sort();
        for (_, notification_ref) in evictable.into_iter().take(overflow) {
            self.live.remove(&notification_ref);
        }
        for (slot, entry) in assigned.iter_mut().zip(page) {
            if slot.is_none() {
                let notification_ref = fresh.next().ok_or_else(notification_lock_error)?;
                self.live.insert(
                    notification_ref.clone(),
                    NotificationRefRecord {
                        executor: executor.clone(),
                        identity: AndroidNotificationIdentity {
                            key: entry.key.clone(),
                            generation: entry.generation,
                        },
                        created_at_ms: now_ms,
                        pins: 0,
                    },
                );
                *slot = Some((notification_ref, now_ms));
            }
        }
        Ok(assigned.into_iter().flatten().collect())
    }

    fn pin(
        &mut self,
        notification_ref: &str,
        executor: &ExecutorRecord,
        now_ms: u64,
    ) -> Result<(AndroidNotificationIdentity, u64), DomainError> {
        self.expire(now_ms);
        match self.live.get_mut(notification_ref) {
            Some(record) if record.executor == *executor => {
                record.pins += 1;
                Ok((record.identity.clone(), record.created_at_ms))
            }
            Some(_) => {
                self.mark_stale(notification_ref);
                Err(stale_reference())
            }
            None if self.stale.contains_key(notification_ref) => Err(stale_reference()),
            None => Err(DomainError::new(
                ErrorCode::NotFound,
                "notification reference is unknown or expired",
            )),
        }
    }

    fn unpin(&mut self, notification_ref: &str) {
        if let Some(record) = self.live.get_mut(notification_ref) {
            record.pins = record.pins.saturating_sub(1);
        }
    }
}

fn route_is_current(
    current: &CapabilitySnapshot,
    route: AndroidRoute,
    executor: &ExecutorRecord,
) -> bool {
    crate::resolve_execution(current, ExecutorRequest::Android(route))
        .map(|value| ExecutorRecord::from(&value) == *executor)
        .unwrap_or(false)
}

fn encode<T: serde::Serialize>(value: T) -> Result<serde_json::Value, ExecutionFailure> {
    serde_json::to_value(value).map_err(|_| {
        verified_failure(DomainError::new(
            ErrorCode::InternalError,
            "Android result encoding failed",
        ))
    })
}

fn error_code_token(code: ErrorCode) -> String {
    serde_json::to_value(code)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "INTERNAL_ERROR".to_owned())
}

fn stale_reference() -> DomainError {
    DomainError::new(ErrorCode::StaleReference, "notification reference is stale")
}

fn notification_lock_error() -> DomainError {
    DomainError::new(
        ErrorCode::InternalError,
        "notification reference state is unavailable",
    )
}

fn verified_failure(error: DomainError) -> ExecutionFailure {
    ExecutionFailure {
        error,
        cleanup_verified: true,
    }
}

const BRIDGE_COMPLETED: &[u8] = br#"{"completed":true}"#;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields, tag = "status", rename_all = "snake_case")]
enum FrameworkPackageWire {
    Visible { package: PackageFact },
    VisibilityOrAbsent,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageInventoryWire {
    packages: Vec<PackageRecordWire>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageRecordWire {
    package_name: String,
    version_code: u64,
    system: bool,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ClipboardReadWire {
    #[serde(default)]
    text: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NotificationSnapshotWire {
    notifications: Vec<NotificationWire>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NotificationWire {
    key: String,
    generation: u64,
    package_name: String,
    #[serde(default)]
    posted_at_ms: Option<u64>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    text: Option<String>,
    actions: Vec<NotificationActionWire>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NotificationActionWire {
    #[serde(default)]
    title: Option<String>,
    requires_remote_input: bool,
}

fn bridge_call<D: AndroidExecutionDispatch + ?Sized>(
    dispatch: &D,
    primitive: &str,
    payload: serde_json::Value,
    execution: &AdmittedExecution,
) -> Result<Vec<u8>, DomainError> {
    let payload = serde_json::to_vec(&payload).map_err(|_| {
        DomainError::new(
            ErrorCode::InternalError,
            "Android primitive encoding failed",
        )
    })?;
    let result = dispatch.dispatch(primitive, &payload, execution)?;
    if !result.descriptors.is_empty() {
        return Err(DomainError::new(
            ErrorCode::IoError,
            "Android primitive returned unexpected descriptors",
        ));
    }
    Ok(result.payload)
}

fn bridge_completed<D: AndroidExecutionDispatch + ?Sized>(
    dispatch: &D,
    primitive: &str,
    payload: serde_json::Value,
    execution: &AdmittedExecution,
) -> Result<(), DomainError> {
    if bridge_call(dispatch, primitive, payload, execution)? != BRIDGE_COMPLETED {
        return Err(DomainError::new(
            ErrorCode::IoError,
            "Android primitive completion is invalid",
        ));
    }
    Ok(())
}

fn bridge_decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, DomainError> {
    serde_json::from_slice(bytes)
        .map_err(|_| DomainError::new(ErrorCode::IoError, "Android primitive result is invalid"))
}

fn bridge_value<T: serde::Serialize>(value: &T) -> Result<serde_json::Value, DomainError> {
    serde_json::to_value(value).map_err(|_| {
        DomainError::new(
            ErrorCode::InternalError,
            "Android primitive encoding failed",
        )
    })
}

/// Decodes one typed notification snapshot shared by the listener bridge and the
/// Magisk direct-listener helper (S-ANDROID-005).
pub fn decode_notification_snapshot(
    bytes: &[u8],
) -> Result<Vec<AndroidNotificationRecord>, DomainError> {
    let wire: NotificationSnapshotWire = bridge_decode(bytes)?;
    Ok(wire
        .notifications
        .into_iter()
        .map(|entry| AndroidNotificationRecord {
            key: entry.key,
            generation: entry.generation,
            package_name: entry.package_name,
            posted_at_ms: entry.posted_at_ms,
            title: entry.title,
            text: entry.text,
            actions: entry
                .actions
                .into_iter()
                .map(|action| AndroidNotificationActionRecord {
                    title: action.title,
                    requires_remote_input: action.requires_remote_input,
                })
                .collect(),
        })
        .collect())
}

pub fn decode_clipboard_read(bytes: &[u8]) -> Result<Option<String>, DomainError> {
    bridge_decode::<ClipboardReadWire>(bytes).map(|wire| wire.text)
}

pub fn bridge_framework_package_inspect<D: AndroidExecutionDispatch + ?Sized>(
    dispatch: &D,
    execution: &AdmittedExecution,
    package_name: &str,
) -> Result<FrameworkPackageInspection, DomainError> {
    let bytes = bridge_call(
        dispatch,
        "PackageInspect",
        serde_json::json!({ "package_name": package_name }),
        execution,
    )?;
    Ok(match bridge_decode::<FrameworkPackageWire>(&bytes)? {
        FrameworkPackageWire::Visible { package } => FrameworkPackageInspection::Visible(package),
        FrameworkPackageWire::VisibilityOrAbsent => FrameworkPackageInspection::VisibilityOrAbsent,
    })
}

pub fn bridge_launch<D: AndroidExecutionDispatch + ?Sized>(
    dispatch: &D,
    execution: &AdmittedExecution,
    input: &AndroidLaunchInput,
) -> Result<(), DomainError> {
    bridge_completed(dispatch, "LaunchActivity", bridge_value(input)?, execution)
}

pub fn bridge_start_intent<D: AndroidExecutionDispatch + ?Sized>(
    dispatch: &D,
    execution: &AdmittedExecution,
    input: &AndroidIntentInput,
) -> Result<(), DomainError> {
    bridge_completed(dispatch, "IntentStart", bridge_value(input)?, execution)
}

pub fn bridge_clipboard_read<D: AndroidExecutionDispatch + ?Sized>(
    dispatch: &D,
    execution: &AdmittedExecution,
) -> Result<Option<String>, DomainError> {
    decode_clipboard_read(&bridge_call(
        dispatch,
        "ClipboardRead",
        serde_json::json!({}),
        execution,
    )?)
}

pub fn bridge_clipboard_write<D: AndroidExecutionDispatch + ?Sized>(
    dispatch: &D,
    execution: &AdmittedExecution,
    text: &str,
) -> Result<(), DomainError> {
    bridge_completed(
        dispatch,
        "ClipboardWrite",
        serde_json::json!({ "text": text }),
        execution,
    )
}

pub fn bridge_clipboard_clear<D: AndroidExecutionDispatch + ?Sized>(
    dispatch: &D,
    execution: &AdmittedExecution,
) -> Result<(), DomainError> {
    bridge_completed(dispatch, "ClipboardClear", serde_json::json!({}), execution)
}

pub fn bridge_notification_snapshot<D: AndroidExecutionDispatch + ?Sized>(
    dispatch: &D,
    execution: &AdmittedExecution,
) -> Result<Vec<AndroidNotificationRecord>, DomainError> {
    decode_notification_snapshot(&bridge_call(
        dispatch,
        "NotificationSnapshot",
        serde_json::json!({}),
        execution,
    )?)
}

pub fn bridge_notification_dismiss<D: AndroidExecutionDispatch + ?Sized>(
    dispatch: &D,
    execution: &AdmittedExecution,
    identity: &AndroidNotificationIdentity,
) -> Result<(), DomainError> {
    bridge_completed(
        dispatch,
        "NotificationDismiss",
        serde_json::json!({ "key": identity.key, "generation": identity.generation }),
        execution,
    )
}

pub fn bridge_notification_invoke<D: AndroidExecutionDispatch + ?Sized>(
    dispatch: &D,
    execution: &AdmittedExecution,
    identity: &AndroidNotificationIdentity,
    action_index: u8,
) -> Result<(), DomainError> {
    bridge_completed(
        dispatch,
        "NotificationAction",
        serde_json::json!({
            "key": identity.key,
            "generation": identity.generation,
            "action_index": action_index,
        }),
        execution,
    )
}

pub fn bridge_shizuku_package_inventory<D: AndroidExecutionDispatch + ?Sized>(
    dispatch: &D,
    execution: &AdmittedExecution,
    include_system: bool,
) -> Result<Vec<PrivilegedPackageRecord>, DomainError> {
    let bytes = bridge_call(
        dispatch,
        "ShizukuPackagePrimitive",
        serde_json::json!({ "operation": "inventory", "include_system": include_system }),
        execution,
    )?;
    Ok(bridge_decode::<PackageInventoryWire>(&bytes)?
        .packages
        .into_iter()
        .map(|record| PrivilegedPackageRecord {
            package_name: record.package_name,
            version_code: record.version_code,
            system: record.system,
        })
        .collect())
}

pub fn bridge_shizuku_force_stop<D: AndroidExecutionDispatch + ?Sized>(
    dispatch: &D,
    execution: &AdmittedExecution,
    package_name: &str,
) -> Result<(), DomainError> {
    bridge_completed(
        dispatch,
        "ShizukuPackagePrimitive",
        serde_json::json!({ "operation": "force_stop", "package_name": package_name }),
        execution,
    )
}
