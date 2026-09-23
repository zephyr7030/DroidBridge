#![deny(unsafe_op_in_unsafe_fn)]

use contract::{CapabilityState, ErrorCode, RuntimeHost, UuidV4};
use domain::{DomainError, OutstandingWork};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

#[cfg(unix)]
mod android;
#[cfg(unix)]
mod visual;

pub const PROTOCOL_VERSION: u32 = 1;

/// The product versionCode this daemon was built as, `major * 1_000_000 + minor * 1_000 + patch`
/// of the crate version. The build requires that version to equal the one `gradle.properties`
/// stamps into the APK and `module.prop`, so the three can only disagree across builds.
pub const VERSION_CODE: u64 = parse_version_part(env!("CARGO_PKG_VERSION_MAJOR")) * 1_000_000
    + parse_version_part(env!("CARGO_PKG_VERSION_MINOR")) * 1_000
    + parse_version_part(env!("CARGO_PKG_VERSION_PATCH"));

const fn parse_version_part(part: &str) -> u64 {
    let bytes = part.as_bytes();
    assert!(!bytes.is_empty(), "version part is empty");
    let mut value = 0_u64;
    let mut index = 0;
    while index < bytes.len() {
        assert!(
            bytes[index].is_ascii_digit(),
            "version part is not a number"
        );
        value = value * 10 + (bytes[index] - b'0') as u64;
        index += 1;
    }
    value
}

/// The three independently probed helper families of S-MAGISK-005.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HelperFamily {
    Launch,
    Clipboard,
    Notifications,
}

impl HelperFamily {
    pub const ALL: [Self; 3] = [Self::Launch, Self::Clipboard, Self::Notifications];

    pub const fn key(self) -> &'static str {
        match self {
            Self::Launch => "magisk.launch",
            Self::Clipboard => "magisk.clipboard",
            Self::Notifications => "magisk.notifications",
        }
    }

    const fn probe_failure(self) -> &'static str {
        match self {
            Self::Launch => "LAUNCH_PROBE_FAILED",
            Self::Clipboard => "CLIPBOARD_PROBE_FAILED",
            Self::Notifications => "NOTIFICATION_PROBE_FAILED",
        }
    }

    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) const fn index(self) -> usize {
        match self {
            Self::Launch => 0,
            Self::Clipboard => 1,
            Self::Notifications => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HelperFamilyFact {
    pub family: HelperFamily,
    pub state: CapabilityState,
    pub reason: Option<&'static str>,
}

/// Projects each family from its own probe and denial facts only; a failed family never
/// changes a sibling. Helper loss alone withdraws every family.
pub fn helper_family_facts(
    helper_ready: bool,
    probe_succeeded: impl Fn(HelperFamily) -> bool,
    operation_denied: impl Fn(HelperFamily) -> bool,
) -> [HelperFamilyFact; 3] {
    HelperFamily::ALL.map(|family| {
        let reason = if !helper_ready {
            Some("HELPER_UNAVAILABLE")
        } else if !probe_succeeded(family) {
            Some(family.probe_failure())
        } else if operation_denied(family) {
            Some("OPERATION_DENIED")
        } else {
            None
        };
        HelperFamilyFact {
            family,
            state: if reason.is_none() {
                CapabilityState::Available
            } else {
                CapabilityState::Unavailable
            },
            reason,
        }
    })
}
pub const MAX_FRAME_BYTES: usize = 262_144;
pub const MAX_FILE_DESCRIPTORS: usize = 4;
pub const MAX_OUTSTANDING: usize = 64;
pub const RESERVED_CONTROL: usize = 4;
pub const MAX_BUSINESS_OUTSTANDING: usize = MAX_OUTSTANDING - RESERVED_CONTROL;
pub const MAX_CONNECTION_MESSAGES: usize = 65_536;
pub const PACKAGE_PRIMITIVE_TIMEOUT_MS: u64 = 15_000;
pub const PACKAGE_PRIMITIVE_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
pub const COMPANION_CAPABILITY_KEYS: &[&str] = &[
    "android.local_network",
    "android.notifications",
    "android.notification_listener",
    "automation.exact_alarm",
    "visual.accessibility",
    "visual.media_projection_session",
    "shizuku.shell",
    "execution.app_guard",
    "execution.shell_guard",
];

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompanionCapabilityRegistration {
    pub key: String,
    pub state: CapabilityState,
    pub reason: Option<String>,
    pub source_generation: u64,
    pub has_executor: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompanionCapabilitySnapshot {
    registrations: Vec<CompanionCapabilityRegistration>,
}

pub fn decode_companion_capability_snapshot(
    payload: &Value,
) -> Result<Vec<CompanionCapabilityRegistration>, DomainError> {
    let snapshot: CompanionCapabilitySnapshot =
        serde_json::from_value(payload.clone()).map_err(|_| {
            DomainError::new(
                ErrorCode::ProtocolIncompatible,
                "invalid companion capability snapshot",
            )
        })?;
    if snapshot.registrations.len() > COMPANION_CAPABILITY_KEYS.len() {
        return Err(DomainError::new(
            ErrorCode::ProtocolIncompatible,
            "companion capability snapshot exceeds the fixed key set",
        ));
    }
    let mut keys = HashSet::new();
    for registration in &snapshot.registrations {
        let reason_valid = match registration.state {
            CapabilityState::Available => registration.reason.is_none(),
            CapabilityState::Unavailable | CapabilityState::Unknown => registration
                .reason
                .as_ref()
                .is_some_and(|reason| !reason.is_empty()),
        };
        if !COMPANION_CAPABILITY_KEYS.contains(&registration.key.as_str())
            || !keys.insert(registration.key.as_str())
            || registration.source_generation == 0
            || (registration.state != CapabilityState::Available && registration.has_executor)
            || !reason_valid
        {
            return Err(DomainError::new(
                ErrorCode::ProtocolIncompatible,
                "invalid companion capability registration",
            ));
        }
    }
    Ok(snapshot.registrations)
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MagiskPrimitiveFamily {
    Process,
    Filesystem,
    NetworkCapture,
    NetworkInjection,
    VisualCapture,
    Input,
    Package,
    PrivilegedAndroid,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MagiskExecutorFence {
    pub runtime_epoch: UuidV4,
    pub host_generation: u64,
    pub runtime_instance_id: UuidV4,
    pub source_generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MagiskExecutorHandle {
    fence: MagiskExecutorFence,
    helper_generation: Option<u64>,
}

impl MagiskExecutorHandle {
    pub fn new(
        fence: MagiskExecutorFence,
        helper_generation: Option<u64>,
    ) -> Result<Self, DomainError> {
        if fence.host_generation == 0
            || fence.source_generation == 0
            || helper_generation == Some(0)
        {
            return Err(DomainError::invalid(
                "invalid Magisk executor generation fence",
            ));
        }
        Ok(Self {
            fence,
            helper_generation,
        })
    }

    pub fn authorize(
        &self,
        expected: &MagiskExecutorFence,
        family: MagiskPrimitiveFamily,
    ) -> Result<(), DomainError> {
        if &self.fence != expected {
            return Err(DomainError::new(
                ErrorCode::StaleAuthority,
                "Magisk primitive fence is stale",
            ));
        }
        if family == MagiskPrimitiveFamily::PrivilegedAndroid && self.helper_generation.is_none() {
            return Err(DomainError::new(
                ErrorCode::CapabilityUnavailable,
                "Magisk framework helper is unavailable",
            ));
        }
        Ok(())
    }

    pub fn without_helper(&self) -> Self {
        Self {
            fence: self.fence.clone(),
            helper_generation: None,
        }
    }

    pub fn fence(&self) -> &MagiskExecutorFence {
        &self.fence
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageListKind {
    ThirdParty,
    System,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrimitiveProcessPlan {
    program: &'static str,
    arguments: Vec<String>,
    family: MagiskPrimitiveFamily,
    fixed_timeout_ms: Option<u64>,
    fixed_output_bytes: Option<usize>,
}

impl PrimitiveProcessPlan {
    pub fn root_shell(command: String) -> Result<Self, DomainError> {
        if command.is_empty() || command.contains('\0') {
            return Err(DomainError::invalid("invalid root command"));
        }
        Ok(Self {
            program: "/system/bin/sh",
            arguments: vec!["-c".to_owned(), command],
            family: MagiskPrimitiveFamily::Process,
            fixed_timeout_ms: None,
            fixed_output_bytes: None,
        })
    }

    pub fn screen_capture() -> Self {
        Self {
            program: "/system/bin/screencap",
            arguments: Vec::new(),
            family: MagiskPrimitiveFamily::VisualCapture,
            fixed_timeout_ms: None,
            fixed_output_bytes: None,
        }
    }

    pub fn input(arguments: Vec<String>) -> Result<Self, DomainError> {
        if arguments.is_empty()
            || arguments.len() > 16
            || arguments
                .iter()
                .any(|value| value.is_empty() || value.len() > 4_096 || value.contains('\0'))
        {
            return Err(DomainError::invalid("invalid input primitive arguments"));
        }
        Ok(Self {
            program: "/system/bin/input",
            arguments,
            family: MagiskPrimitiveFamily::Input,
            fixed_timeout_ms: None,
            fixed_output_bytes: None,
        })
    }

    pub fn package_list(kind: PackageListKind) -> Self {
        let selector = match kind {
            PackageListKind::ThirdParty => "-3",
            PackageListKind::System => "-s",
        };
        Self {
            program: "/system/bin/cmd",
            arguments: [
                "package",
                "list",
                "packages",
                selector,
                "--show-versioncode",
                "--user",
                "0",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            family: MagiskPrimitiveFamily::Package,
            fixed_timeout_ms: Some(PACKAGE_PRIMITIVE_TIMEOUT_MS),
            fixed_output_bytes: Some(PACKAGE_PRIMITIVE_OUTPUT_BYTES),
        }
    }

    pub fn package_force_stop(package_name: String) -> Result<Self, DomainError> {
        if !valid_package_name(&package_name) {
            return Err(DomainError::invalid("invalid package name"));
        }
        Ok(Self {
            program: "/system/bin/am",
            arguments: vec![
                "force-stop".to_owned(),
                "--user".to_owned(),
                "0".to_owned(),
                package_name,
            ],
            family: MagiskPrimitiveFamily::Package,
            fixed_timeout_ms: Some(PACKAGE_PRIMITIVE_TIMEOUT_MS),
            fixed_output_bytes: Some(PACKAGE_PRIMITIVE_OUTPUT_BYTES),
        })
    }

    pub const fn program(&self) -> &'static str {
        self.program
    }

    pub fn arguments(&self) -> &[String] {
        &self.arguments
    }

    pub const fn family(&self) -> MagiskPrimitiveFamily {
        self.family
    }

    pub const fn fixed_timeout_ms(&self) -> Option<u64> {
        self.fixed_timeout_ms
    }

    pub const fn fixed_output_bytes(&self) -> Option<usize> {
        self.fixed_output_bytes
    }
}

fn valid_package_name(value: &str) -> bool {
    if value.is_empty() || value.len() > 255 || !value.contains('.') {
        return false;
    }
    value.split('.').all(|segment| {
        let mut characters = segment.chars();
        characters
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
            && characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointRole {
    ApkRuntime,
    Droidbridged,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Handshake {
    pub protocol_version: u32,
    pub role: EndpointRole,
    pub package: String,
    pub user_id: u32,
    pub runtime_epoch: UuidV4,
    pub host: RuntimeHost,
    pub host_generation: u64,
    pub runtime_instance_id: Option<UuidV4>,
}

impl Handshake {
    pub fn validate_peer(
        &self,
        peer_uid: u32,
        expected_uid: u32,
        expected_role: EndpointRole,
        identity: &ModuleIdentity,
    ) -> Result<(), DomainError> {
        if peer_uid != expected_uid
            || self.protocol_version != PROTOCOL_VERSION
            || self.role != expected_role
            || self.package != identity.package
            || self.user_id != 0
            || self.host_generation == 0
        {
            return Err(DomainError::new(
                ErrorCode::PermissionDenied,
                "daemon handshake authentication failed",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModuleIdentity {
    pub module_id: &'static str,
    pub package: &'static str,
    pub socket_name: &'static str,
}

impl ModuleIdentity {
    pub const fn stable() -> Self {
        Self {
            module_id: "droidbridge",
            package: "com.droidbridge.android",
            socket_name: "droidbridge.com.droidbridge.android.u0.v1",
        }
    }

    pub const fn debug() -> Self {
        Self {
            module_id: "droidbridge_debug",
            package: "com.droidbridge.android.debug",
            socket_name: "droidbridge.com.droidbridge.android.debug.u0.v1",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleObservation {
    pub stable_present: bool,
    pub debug_present: bool,
    pub enabled: bool,
    pub module_version_code: u64,
    pub daemon_version_code: u64,
    pub protocol_version: u32,
    pub metadata_self_test: bool,
    pub excluded: bool,
}

impl ModuleObservation {
    pub fn readiness(
        &self,
        identity: &ModuleIdentity,
        expected_version_code: u64,
    ) -> Result<(), DomainError> {
        if self.stable_present && self.debug_present {
            return Err(DomainError::new(
                ErrorCode::CapabilityUnavailable,
                "stable and debug modules conflict",
            ));
        }
        let present = if identity.module_id == ModuleIdentity::stable().module_id {
            self.stable_present
        } else {
            self.debug_present
        };
        if !present || !self.enabled || self.excluded || !self.metadata_self_test {
            return Err(DomainError::new(
                ErrorCode::CapabilityUnavailable,
                "Magisk backend is not ready",
            ));
        }
        if self.protocol_version != PROTOCOL_VERSION
            || self.module_version_code != expected_version_code
            || self.daemon_version_code != expected_version_code
        {
            return Err(DomainError::new(
                ErrorCode::ProtocolIncompatible,
                "Magisk backend version is incompatible",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DaemonRole {
    BackendOnly,
    RuntimeHost,
}

impl DaemonRole {
    pub const fn from_owner(host: RuntimeHost) -> Self {
        match host {
            RuntimeHost::ApkRuntime => Self::BackendOnly,
            RuntimeHost::MagiskBackend => Self::RuntimeHost,
        }
    }

    pub const fn may_create_core(self) -> bool {
        matches!(self, Self::RuntimeHost)
    }

    pub const fn ready(self, module_ready: bool, runtime_host_ready: bool) -> bool {
        module_ready && (matches!(self, Self::BackendOnly) || runtime_host_ready)
    }

    pub const fn permits(self, operation: Operation) -> bool {
        match self {
            Self::BackendOnly => matches!(
                operation,
                Operation::HostStatus
                    | Operation::HostPrepareTransition
                    | Operation::HostAbortTransition
                    | Operation::HostActivate
                    | Operation::CapabilitySnapshot
                    | Operation::DiagnosticsSnapshot
                    | Operation::MaintenanceStatus
                    | Operation::MaintenanceInstallApk
                    | Operation::MaintenanceInstallModule
            ),
            Self::RuntimeHost => !matches!(
                operation,
                Operation::MaintenanceInstallApk | Operation::MaintenanceInstallModule
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Operation {
    HostStatus,
    HostPrepareTransition,
    HostAbortTransition,
    HostRelease,
    HostActivate,
    RuntimeForward,
    RuntimeCancel,
    CompanionExecute,
    CompanionCancel,
    CapabilitySnapshot,
    NetworkDefaultChanged,
    NetworkAttachment,
    DiagnosticsSnapshot,
    MaintenanceStatus,
    MaintenanceInstallApk,
    MaintenanceInstallModule,
}

impl Operation {
    pub const fn is_control(self) -> bool {
        matches!(
            self,
            Self::HostStatus
                | Self::HostPrepareTransition
                | Self::HostAbortTransition
                | Self::HostRelease
                | Self::HostActivate
                | Self::RuntimeCancel
                | Self::CompanionCancel
                | Self::MaintenanceStatus
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    Request,
    Response,
    Cancel,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireEnvelope {
    pub protocol_version: u32,
    pub kind: MessageKind,
    pub message_id: UuidV4,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<UuidV4>,
    pub runtime_epoch: UuidV4,
    pub host_generation: u64,
    pub runtime_instance_id: Option<UuidV4>,
    pub operation: Operation,
    pub payload: Value,
    pub fd_roles: Vec<String>,
}

impl WireEnvelope {
    pub fn request(
        message_id: UuidV4,
        runtime_epoch: UuidV4,
        host_generation: u64,
        runtime_instance_id: Option<UuidV4>,
        operation: Operation,
        payload: Value,
        fd_roles: Vec<String>,
    ) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            kind: MessageKind::Request,
            message_id,
            reply_to: None,
            runtime_epoch,
            host_generation,
            runtime_instance_id,
            operation,
            payload,
            fd_roles,
        }
    }

    pub fn response(
        message_id: UuidV4,
        request: &Self,
        runtime_instance_id: Option<UuidV4>,
        payload: Value,
        fd_roles: Vec<String>,
    ) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            kind: MessageKind::Response,
            message_id,
            reply_to: Some(request.message_id.clone()),
            runtime_epoch: request.runtime_epoch.clone(),
            host_generation: request.host_generation,
            runtime_instance_id,
            operation: request.operation,
            payload,
            fd_roles,
        }
    }

    pub fn validate(&self, descriptor_count: usize) -> Result<(), DomainError> {
        if self.protocol_version != PROTOCOL_VERSION
            || self.host_generation == 0
            || self.fd_roles.len() > MAX_FILE_DESCRIPTORS
            || self.fd_roles.len() != descriptor_count
            || self.fd_roles.iter().any(|role| !valid_fd_role(role))
            || (self.kind == MessageKind::Request && self.reply_to.is_some())
            || (self.kind != MessageKind::Request && self.reply_to.is_none())
            || (matches!(
                self.operation,
                Operation::RuntimeForward
                    | Operation::RuntimeCancel
                    | Operation::CompanionExecute
                    | Operation::CompanionCancel
                    | Operation::NetworkDefaultChanged
            ) && self.runtime_instance_id.is_none())
        {
            return Err(DomainError::new(
                ErrorCode::ProtocolIncompatible,
                "invalid daemon protocol envelope",
            ));
        }
        Ok(())
    }
}

fn valid_fd_role(role: &str) -> bool {
    matches!(
        role,
        "execution_guard_proof"
            | "verified_apk"
            | "verified_module_zip"
            | "visual_raw_frame"
            | "visual_source_image"
            | "visual_encoded_image"
            | "stdin"
            | "stdout"
            | "stderr"
            | "content"
            | "mcp_artifact"
    )
}

pub fn encode_frame(envelope: &WireEnvelope) -> Result<Vec<u8>, DomainError> {
    envelope.validate(envelope.fd_roles.len())?;
    let body = serde_json::to_vec(envelope).map_err(|_| {
        DomainError::new(ErrorCode::InternalError, "daemon envelope encoding failed")
    })?;
    if body.is_empty() || body.len() > MAX_FRAME_BYTES {
        return Err(DomainError::new(
            ErrorCode::ResourceLimit,
            "daemon envelope exceeds the frame bound",
        ));
    }
    let length = u32::try_from(body.len())
        .map_err(|_| DomainError::new(ErrorCode::ResourceLimit, "daemon frame length overflow"))?;
    let mut frame = Vec::with_capacity(body.len() + 4);
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(&body);
    Ok(frame)
}

pub fn decode_frame(frame: &[u8]) -> Result<WireEnvelope, DomainError> {
    let header: [u8; 4] = frame
        .get(..4)
        .and_then(|value| value.try_into().ok())
        .ok_or_else(|| DomainError::new(ErrorCode::ProtocolIncompatible, "missing frame header"))?;
    let length = u32::from_be_bytes(header) as usize;
    if length == 0 || length > MAX_FRAME_BYTES || frame.len() != length + 4 {
        return Err(DomainError::new(
            ErrorCode::ProtocolIncompatible,
            "invalid daemon frame length",
        ));
    }
    let envelope: WireEnvelope = serde_json::from_slice(&frame[4..]).map_err(|_| {
        DomainError::new(ErrorCode::ProtocolIncompatible, "invalid daemon frame JSON")
    })?;
    envelope.validate(envelope.fd_roles.len())?;
    Ok(envelope)
}

#[derive(Default)]
pub struct ConnectionLedger {
    outstanding: HashMap<UuidV4, PendingRequest>,
    business: HashSet<UuidV4>,
    seen: HashSet<UuidV4>,
    incoming_messages: usize,
    outgoing_messages: usize,
}

#[derive(Clone)]
struct PendingRequest {
    operation: Operation,
    runtime_epoch: UuidV4,
    host_generation: u64,
    runtime_instance_id: Option<UuidV4>,
}

impl ConnectionLedger {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reserve_business_envelope(&mut self, request: &WireEnvelope) -> Result<(), DomainError> {
        if self.business.len() >= MAX_BUSINESS_OUTSTANDING
            || self.outstanding.len() >= MAX_OUTSTANDING
        {
            return Err(DomainError::new(
                ErrorCode::ResourceLimit,
                "daemon business request slots are full",
            ));
        }
        self.reserve(request, true)
    }

    pub fn reserve_control_envelope(&mut self, request: &WireEnvelope) -> Result<(), DomainError> {
        if self.outstanding.len() >= MAX_OUTSTANDING {
            return Err(DomainError::new(
                ErrorCode::ResourceLimit,
                "daemon request slots are full",
            ));
        }
        self.reserve(request, false)
    }

    fn reserve(&mut self, request: &WireEnvelope, business: bool) -> Result<(), DomainError> {
        if request.kind != MessageKind::Request {
            return Err(DomainError::new(
                ErrorCode::ProtocolIncompatible,
                "only requests can reserve daemon correlation state",
            ));
        }
        let message_id = request.message_id.clone();
        self.observe_outgoing(message_id.clone())?;
        if business {
            let inserted = self.business.insert(message_id.clone());
            debug_assert!(inserted);
        }
        let prior = self.outstanding.insert(
            message_id,
            PendingRequest {
                operation: request.operation,
                runtime_epoch: request.runtime_epoch.clone(),
                host_generation: request.host_generation,
                runtime_instance_id: request.runtime_instance_id.clone(),
            },
        );
        debug_assert!(prior.is_none());
        Ok(())
    }

    pub fn complete_envelope(&mut self, response: &WireEnvelope) -> Result<(), DomainError> {
        if response.kind != MessageKind::Response {
            return Err(DomainError::new(
                ErrorCode::ProtocolIncompatible,
                "only responses can complete daemon correlation state",
            ));
        }
        let reply_to = response.reply_to.as_ref().ok_or_else(|| {
            DomainError::new(ErrorCode::ProtocolIncompatible, "response lacks reply id")
        })?;
        let request = self.outstanding.get(reply_to).ok_or_else(|| {
            DomainError::new(
                ErrorCode::ProtocolIncompatible,
                "unknown daemon reply correlation",
            )
        })?;
        if request.operation != response.operation {
            return Err(DomainError::new(
                ErrorCode::ProtocolIncompatible,
                "mismatched daemon reply operation",
            ));
        }
        let instance_must_match = matches!(
            request.operation,
            Operation::RuntimeForward
                | Operation::RuntimeCancel
                | Operation::CompanionExecute
                | Operation::CompanionCancel
        );
        if request.runtime_epoch != response.runtime_epoch
            || request.host_generation != response.host_generation
            || (instance_must_match && request.runtime_instance_id != response.runtime_instance_id)
        {
            return Err(DomainError::new(
                ErrorCode::StaleAuthority,
                "daemon reply owner fence is stale",
            ));
        }
        self.outstanding.remove(reply_to);
        self.business.remove(reply_to);
        Ok(())
    }

    pub fn observe_incoming(&mut self, message_id: UuidV4) -> Result<(), DomainError> {
        Self::record_message(&mut self.seen, &mut self.incoming_messages, message_id)
    }

    pub fn observe_outgoing(&mut self, message_id: UuidV4) -> Result<(), DomainError> {
        Self::record_message(&mut self.seen, &mut self.outgoing_messages, message_id)
    }

    fn record_message(
        seen: &mut HashSet<UuidV4>,
        direction_count: &mut usize,
        message_id: UuidV4,
    ) -> Result<(), DomainError> {
        if *direction_count >= MAX_CONNECTION_MESSAGES {
            return Err(DomainError::new(
                ErrorCode::ResourceLimit,
                "daemon connection message history is exhausted",
            ));
        }
        if !seen.insert(message_id) {
            return Err(DomainError::new(
                ErrorCode::ProtocolIncompatible,
                "duplicate daemon message id",
            ));
        }
        *direction_count += 1;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceGeneration(u64);

impl SourceGeneration {
    pub const fn initial() -> Self {
        Self(1)
    }

    pub const fn current(self) -> u64 {
        self.0
    }

    pub fn advance(&mut self) -> Result<u64, DomainError> {
        self.0 = self.0.checked_add(1).ok_or_else(|| {
            DomainError::new(ErrorCode::ResourceLimit, "capability generation exhausted")
        })?;
        Ok(self.0)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WakeAlarmProbe {
    pub created: bool,
    pub clock_read: bool,
    pub armed: bool,
    pub disarmed: bool,
}

impl WakeAlarmProbe {
    pub const fn available(self) -> bool {
        self.created && self.clock_read && self.armed && self.disarmed
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostPreparation {
    pub transition_id: UuidV4,
    pub store_revision: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PromotionOutcome {
    Prepared(HostPreparation),
    Deferred,
}

impl PromotionOutcome {
    pub const fn is_deferred(&self) -> bool {
        matches!(self, Self::Deferred)
    }
}

pub struct HostCoordinator {
    host: RuntimeHost,
    generation: u64,
    instance: UuidV4,
    preparing: Option<UuidV4>,
    retired: bool,
}

impl HostCoordinator {
    pub fn new(host: RuntimeHost, generation: u64, instance: UuidV4) -> Self {
        Self {
            host,
            generation,
            instance,
            preparing: None,
            retired: false,
        }
    }

    pub const fn admission_open(&self) -> bool {
        self.preparing.is_none() && !self.retired
    }

    pub fn prepare(
        &mut self,
        transition_id: UuidV4,
        target: RuntimeHost,
        outstanding: OutstandingWork,
        store_revision: u64,
    ) -> Result<HostPreparation, DomainError> {
        if self.retired || self.preparing.is_some() || target == self.host {
            return Err(DomainError::new(
                ErrorCode::HostTransitionPending,
                "runtime transition cannot be prepared",
            ));
        }
        if !outstanding.is_idle() {
            return Err(DomainError::new(
                ErrorCode::HostTransitionPending,
                "runtime host is not idle",
            ));
        }
        self.preparing = Some(transition_id.clone());
        Ok(HostPreparation {
            transition_id,
            store_revision,
        })
    }

    pub fn optional_promotion(
        &mut self,
        transition_id: UuidV4,
        outstanding: OutstandingWork,
        store_revision: u64,
    ) -> PromotionOutcome {
        if !outstanding.is_idle() {
            return PromotionOutcome::Deferred;
        }
        self.prepare(
            transition_id,
            RuntimeHost::MagiskBackend,
            outstanding,
            store_revision,
        )
        .map(PromotionOutcome::Prepared)
        .unwrap_or(PromotionOutcome::Deferred)
    }

    pub fn abort(&mut self, transition_id: &UuidV4) -> Result<(), DomainError> {
        if self.preparing.as_ref() != Some(transition_id) || self.retired {
            return Err(DomainError::new(
                ErrorCode::StaleAuthority,
                "transition id is not prepared",
            ));
        }
        self.preparing = None;
        Ok(())
    }

    pub fn release(&mut self, transition_id: &UuidV4) -> Result<(), DomainError> {
        if self.preparing.as_ref() != Some(transition_id) || self.retired {
            return Err(DomainError::new(
                ErrorCode::StaleAuthority,
                "transition id is not prepared",
            ));
        }
        self.retired = true;
        Ok(())
    }

    pub fn identity(&self) -> (RuntimeHost, u64, &UuidV4) {
        (self.host, self.generation, &self.instance)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelperHello {
    pub protocol_version: u32,
    pub sdk_int: u32,
    pub helper_generation: u64,
}

pub struct HelperRegistry {
    sdk_int: u32,
    expected_generation: u64,
    generation: Option<u64>,
}

impl HelperRegistry {
    pub fn new(sdk_int: u32, expected_generation: u64) -> Result<Self, DomainError> {
        if !(33..=37).contains(&sdk_int) {
            return Err(DomainError::new(
                ErrorCode::CapabilityUnavailable,
                "device SDK has no fixed framework helper",
            ));
        }
        if expected_generation == 0 {
            return Err(DomainError::invalid("invalid framework helper generation"));
        }
        Ok(Self {
            sdk_int,
            expected_generation,
            generation: None,
        })
    }

    pub fn jar_name(&self) -> String {
        format!("droidbridge-framework-api{}.jar", self.sdk_int)
    }

    pub fn accept_hello(&mut self, peer_uid: u32, hello: HelperHello) -> Result<(), DomainError> {
        if peer_uid != 0
            || hello.protocol_version != PROTOCOL_VERSION
            || hello.sdk_int != self.sdk_int
            || hello.helper_generation != self.expected_generation
        {
            return Err(DomainError::new(
                ErrorCode::PermissionDenied,
                "framework helper authentication failed",
            ));
        }
        self.generation = Some(hello.helper_generation);
        Ok(())
    }

    pub const fn framework_state(&self) -> CapabilityState {
        if self.generation.is_some() {
            CapabilityState::Available
        } else {
            CapabilityState::Unavailable
        }
    }

    pub fn disconnected(&mut self) {
        self.generation = None;
    }
}

#[derive(Default)]
pub struct CompanionLink {
    connected: bool,
}

impl CompanionLink {
    pub fn observe_connected(&mut self) {
        self.connected = true;
    }

    pub fn observe_disconnected(&mut self) {
        self.connected = false;
    }

    pub const fn capability_state(&self) -> CapabilityState {
        if self.connected {
            CapabilityState::Available
        } else {
            CapabilityState::Unavailable
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MagiskFacts {
    pub magisk_module: CapabilityState,
    pub magisk_root: CapabilityState,
    pub magisk_framework: CapabilityState,
    pub execution_root_guard: CapabilityState,
    pub app_execution_surface: CapabilityState,
    pub shizuku_shell: CapabilityState,
}

impl MagiskFacts {
    pub const fn ready(companion: bool, helper: bool) -> Self {
        Self {
            magisk_module: CapabilityState::Available,
            magisk_root: CapabilityState::Available,
            magisk_framework: if helper {
                CapabilityState::Available
            } else {
                CapabilityState::Unavailable
            },
            execution_root_guard: CapabilityState::Available,
            app_execution_surface: if companion {
                CapabilityState::Available
            } else {
                CapabilityState::Unavailable
            },
            shizuku_shell: CapabilityState::Unavailable,
        }
    }

    pub const fn with_companion(mut self, connected: bool) -> Self {
        self.app_execution_surface = if connected {
            CapabilityState::Available
        } else {
            CapabilityState::Unavailable
        };
        if !connected {
            self.shizuku_shell = CapabilityState::Unavailable;
        }
        self
    }
}

#[cfg(any(unix, test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CanonicalDirectoryIdentity {
    pub(crate) device: u64,
    pub(crate) inode: u64,
    pub(crate) uid: u32,
}

#[cfg(any(unix, test))]
pub(crate) fn canonical_directory_matches(
    expected: CanonicalDirectoryIdentity,
    observed: Option<CanonicalDirectoryIdentity>,
) -> bool {
    observed == Some(expected)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionGuardState {
    Clean,
    Running,
    CleanupUnverified,
}

pub mod network;

#[cfg(any(unix, test))]
mod process_network;

#[cfg(unix)]
pub mod unix_transport;

#[cfg(unix)]
pub mod companion;

#[cfg(unix)]
mod command;

#[cfg(unix)]
mod maintenance;

#[cfg(unix)]
mod magisk_guard_recovery;

#[cfg(unix)]
mod magisk_host;

#[cfg(unix)]
mod automation_wake;

#[cfg(any(unix, test))]
mod app_keepalive;

#[cfg(unix)]
pub mod process;

impl ExecutionGuardState {
    pub fn observe_guard_death(&mut self) {
        if *self == Self::Running {
            *self = Self::CleanupUnverified;
        }
    }

    pub fn observe_reboot(&mut self) {
        *self = Self::Clean;
    }

    pub const fn blocks_admission(self) -> bool {
        matches!(self, Self::CleanupUnverified)
    }
}

#[cfg(test)]
mod tests {
    use super::{CanonicalDirectoryIdentity, canonical_directory_matches};

    #[test]
    fn i7_g01_replaced_or_missing_canonical_directory_deactivates_daemon() {
        let expected = CanonicalDirectoryIdentity {
            device: 11,
            inode: 22,
            uid: 33,
        };

        assert!(canonical_directory_matches(expected, Some(expected)));
        assert!(!canonical_directory_matches(expected, None));
        assert!(!canonical_directory_matches(
            expected,
            Some(CanonicalDirectoryIdentity {
                device: 12,
                ..expected
            }),
        ));
        assert!(!canonical_directory_matches(
            expected,
            Some(CanonicalDirectoryIdentity {
                inode: 23,
                ..expected
            }),
        ));
        assert!(!canonical_directory_matches(
            expected,
            Some(CanonicalDirectoryIdentity {
                uid: 34,
                ..expected
            }),
        ));
    }
}
