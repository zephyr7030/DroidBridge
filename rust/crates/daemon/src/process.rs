use crate::{
    COMPANION_CAPABILITY_KEYS, CanonicalDirectoryIdentity, CompanionCapabilityRegistration,
    CompanionLink, DaemonRole, EndpointRole, Handshake, ModuleIdentity, ModuleObservation,
    Operation, PROTOCOL_VERSION, WireEnvelope,
    app_keepalive::AppKeepAlive,
    canonical_directory_matches,
    companion::{CompanionChannel, CompanionEvent, CompanionPort},
    decode_companion_capability_snapshot,
    magisk_host::{MagiskHost, VERSION_CODE, fixed_property, observe_module},
    unix_transport::{connect_abstract, peer_uid, receive_json, send_json},
};
use contract::{CapabilityState, ErrorCode, RuntimeHost, UuidV4};
use domain::{AdmissionFence, DomainError};
use persistence::{CanonicalState, RuntimeOwner, StateStore, TransitionRecovery, read_json};
use runtime::{NetworkDefaultChangedEvent, NetworkDefaultSourceRegistration, NetworkEventDelivery};
use serde_json::{Value, json};
use std::{
    fs,
    os::fd::OwnedFd,
    os::unix::fs::MetadataExt,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

const CONNECT_DELAYS_SECONDS: [u64; 6] = [1, 2, 4, 8, 16, 30];

pub fn main() -> ExitCode {
    match Daemon::discover().and_then(Daemon::run) {
        Ok(()) => ExitCode::SUCCESS,
        // The supervisor records only the exit code; the reason goes to the daemon's stderr log.
        Err(error) => {
            eprintln!("droidbridged: exiting: {:?} {}", error.code, error.reason);
            ExitCode::from(1)
        }
    }
}

struct Daemon {
    identity: ModuleIdentity,
    module_root: PathBuf,
    canonical_base: PathBuf,
    canonical_identity: CanonicalDirectoryIdentity,
    store: Arc<StateStore>,
    sdk_int: u32,
    observation: ModuleObservation,
    host: Option<MagiskHost>,
    target_preparation: Option<UuidV4>,
    companion: CompanionLink,
    companion_port: CompanionPort,
    companion_capabilities: Vec<CompanionCapabilityRegistration>,
    maintenance: crate::maintenance::MaintenanceAttempts,
    task_activity: Arc<crate::app_keepalive::TaskActivityBeacon>,
}

impl Daemon {
    fn discover() -> Result<Self, DomainError> {
        let executable = fs::canonicalize(
            std::env::current_exe().map_err(|_| io_error("cannot resolve daemon executable"))?,
        )
        .map_err(|_| io_error("cannot canonicalize daemon executable"))?;
        let module_root = executable
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| io_error("daemon is outside a module root"))?
            .to_path_buf();
        let identity = build_identity();
        if executable != module_root.join("bin").join("droidbridged")
            || module_root.file_name().and_then(|value| value.to_str()) != Some(identity.module_id)
        {
            return Err(DomainError::new(
                ErrorCode::PermissionDenied,
                "daemon executable is outside its fixed module identity",
            ));
        }
        let canonical_base = PathBuf::from("/data/user_de/0")
            .join(identity.package)
            .join("files/droidbridge");
        if !canonical_base.is_dir() {
            return Err(DomainError::new(
                ErrorCode::NotFound,
                "App canonical store is not initialized",
            ));
        }
        let canonical_identity = canonical_directory_identity(&canonical_base)?;
        let sdk_int = fixed_property("ro.build.version.sdk")?
            .parse()
            .ok()
            .filter(|value| (33..=37).contains(value))
            .ok_or_else(|| {
                DomainError::new(
                    ErrorCode::CapabilityUnavailable,
                    "device SDK has no fixed helper",
                )
            })?;
        let observation = observe_module(&module_root, &canonical_base, &identity)?;
        let store = Arc::new(StateStore::new(canonical_base.clone()));
        let task_activity = crate::app_keepalive::TaskActivityBeacon::new(identity.package);
        let mut daemon = Self {
            identity,
            module_root,
            canonical_base,
            canonical_identity,
            store,
            sdk_int,
            observation,
            host: None,
            target_preparation: None,
            companion: CompanionLink::default(),
            companion_port: CompanionPort::default(),
            companion_capabilities: Vec::new(),
            maintenance: crate::maintenance::MaintenanceAttempts::default(),
            task_activity,
        };
        let owner = daemon.store.read_owner()?;
        if DaemonRole::from_owner(owner.host).may_create_core() {
            daemon.activate_current_owner()?;
        }
        Ok(daemon)
    }

    fn run(mut self) -> Result<(), DomainError> {
        let mut failure_index = 0_usize;
        let mut keepalive = AppKeepAlive::new(self.identity.package);
        loop {
            if !self.canonical_directory_is_current() {
                return Ok(());
            }
            let started = Instant::now();
            if let Ok(mut stream) = connect_abstract(self.identity.socket_name) {
                keepalive.observe_present();
                // The App that answers here serves the Android primitives this daemon's Tasks
                // execute, so it is told what it must hold for as long as this connection lives.
                self.task_activity.set_connected(true);
                if let Err(error) = self.serve_connection(&mut stream) {
                    // The only record of a companion connection that ended, and the only way a
                    // rejected handshake is visible at all; the supervisor keeps this stderr.
                    eprintln!(
                        "droidbridged: companion connection ended: {:?} {}",
                        error.code, error.reason
                    );
                }
                self.task_activity.set_connected(false);
                self.companion_port.withdraw()?;
                self.set_companion(false)?;
                // The backoff is not shortened for keep-alive: reconnecting within a second of a
                // Runtime restart overlapped the App's start and failed in-flight daemon Tasks with
                // REVISION_CONFLICT (network.capture stop, device gate), so a revived App waits for
                // the ordinary retry.
                if started.elapsed() >= Duration::from_secs(300) {
                    failure_index = 0;
                }
            } else {
                // No Runtime listens: an enabled agent connection, or a Task this daemon still
                // owns, needs the App started again.
                keepalive.observe_absent(&self.canonical_base, self.task_activity.wake_wanted());
            }
            if !self.canonical_directory_is_current() {
                return Ok(());
            }
            let delay = CONNECT_DELAYS_SECONDS[failure_index.min(CONNECT_DELAYS_SECONDS.len() - 1)];
            failure_index = (failure_index + 1).min(CONNECT_DELAYS_SECONDS.len() - 1);
            thread::sleep(Duration::from_secs(delay));
        }
    }

    fn serve_connection(&mut self, stream: &mut UnixStream) -> Result<(), DomainError> {
        self.require_canonical_directory()?;
        let owner = self.store.read_owner()?;
        let daemon_handshake = Handshake {
            protocol_version: PROTOCOL_VERSION,
            role: EndpointRole::Droidbridged,
            package: self.identity.package.to_owned(),
            user_id: 0,
            runtime_epoch: owner.runtime_epoch.clone(),
            host: owner.host,
            host_generation: owner.host_generation,
            runtime_instance_id: self.host.as_ref().map(|host| host.instance_id().clone()),
        };
        send_json(stream, &daemon_handshake)?;
        let app_handshake: Handshake = receive_json(stream)?;
        // The App's uid is read from the package data directory the system created for it, not
        // from the canonical base inside it: a base recreated by root after App data was cleared
        // carries root as its owner, and would lock the App out for good.
        let package_directory = self
            .canonical_base
            .ancestors()
            .nth(2)
            .ok_or_else(|| io_error("App canonical directory has no package directory"))?;
        let expected_uid = fs::metadata(package_directory)
            .map_err(|_| io_error("cannot inspect App package directory"))?
            .uid();
        app_handshake.validate_peer(
            peer_uid(stream).map_err(|_| io_error("cannot authenticate App socket"))?,
            expected_uid,
            EndpointRole::ApkRuntime,
            &self.identity,
        )?;
        self.require_canonical_directory()?;
        let current_owner = self.store.read_owner()?;
        if app_handshake.runtime_epoch != current_owner.runtime_epoch
            || app_handshake.host != current_owner.host
            || app_handshake.host_generation != current_owner.host_generation
        {
            return Err(DomainError::new(
                ErrorCode::StaleAuthority,
                "App handshake owner fence is stale",
            ));
        }
        self.set_companion(false)?;
        let channel = CompanionChannel::start(stream)?;
        // The delegation surface is live only for the life of this authenticated
        // connection, so it is published before any companion-fenced business work can
        // be admitted and withdrawn with the connection.
        self.companion_port.publish(Arc::clone(&channel))?;
        self.bind_companion_fence(&current_owner)?;
        let status_request = WireEnvelope::request(
            new_uuid()?,
            current_owner.runtime_epoch.clone(),
            current_owner.host_generation,
            self.host.as_ref().map(|host| host.instance_id().clone()),
            Operation::HostStatus,
            self.status_payload()?,
            Vec::new(),
        );
        channel.request_control(&status_request)?;
        let capability_request = WireEnvelope::request(
            new_uuid()?,
            current_owner.runtime_epoch,
            current_owner.host_generation,
            self.host.as_ref().map(|host| host.instance_id().clone()),
            Operation::CapabilitySnapshot,
            json!({}),
            Vec::new(),
        );
        channel.request_control(&capability_request)?;

        loop {
            let event = channel.next_event()?;
            self.require_canonical_directory()?;
            match event {
                CompanionEvent::Response(envelope) => {
                    if envelope.operation == Operation::CapabilitySnapshot {
                        self.companion_capabilities =
                            decode_companion_capability_snapshot(&envelope.payload)?;
                        self.set_companion(true)?;
                    }
                    continue;
                }
                CompanionEvent::Request(received) => {
                    let installs = matches!(
                        received.envelope.operation,
                        Operation::MaintenanceInstallApk | Operation::MaintenanceInstallModule
                    );
                    if !received.descriptors.is_empty() && !installs {
                        return Err(DomainError::new(
                            ErrorCode::ProtocolIncompatible,
                            "operation does not accept descriptors",
                        ));
                    }
                    let request = received.envelope;
                    let received_descriptors = received.descriptors;
                    self.validate_request_fence(&request)?;
                    if request.operation == Operation::HostStatus && self.host.is_some() {
                        let capability_request = WireEnvelope::request(
                            new_uuid()?,
                            request.runtime_epoch.clone(),
                            request.host_generation,
                            self.host.as_ref().map(|host| host.instance_id().clone()),
                            Operation::CapabilitySnapshot,
                            json!({}),
                            Vec::new(),
                        );
                        channel.request_control(&capability_request)?;
                    }
                    let (response_payload, descriptors) =
                        self.handle_request_with_descriptors(&request, received_descriptors)?;
                    let response = WireEnvelope::response(
                        new_uuid()?,
                        &request,
                        self.host.as_ref().map(|host| host.instance_id().clone()),
                        response_payload,
                        descriptors.iter().map(|(role, _)| role.clone()).collect(),
                    );
                    channel.respond(&response, descriptors)?;
                }
            }
        }
    }

    fn canonical_directory_is_current(&self) -> bool {
        canonical_directory_matches(
            self.canonical_identity,
            canonical_directory_identity(&self.canonical_base).ok(),
        )
    }

    fn require_canonical_directory(&self) -> Result<(), DomainError> {
        if self.canonical_directory_is_current() {
            Ok(())
        } else {
            Err(DomainError::new(
                ErrorCode::StaleAuthority,
                "App canonical directory identity changed",
            ))
        }
    }

    fn validate_request_fence(&self, request: &WireEnvelope) -> Result<(), DomainError> {
        let owner = self.store.read_owner()?;
        if request.runtime_epoch != owner.runtime_epoch
            || request.host_generation != owner.host_generation
            || matches!(
                request.operation,
                Operation::CompanionExecute | Operation::CompanionCancel
            )
        {
            return Err(DomainError::new(
                ErrorCode::StaleAuthority,
                "daemon request fence is stale or has invalid direction",
            ));
        }
        if matches!(
            request.operation,
            Operation::RuntimeForward
                | Operation::RuntimeCancel
                | Operation::NetworkDefaultChanged
                | Operation::NetworkAttachment
        ) && request.runtime_instance_id
            != self.host.as_ref().map(|host| host.instance_id().clone())
        {
            return Err(DomainError::new(
                ErrorCode::StaleAuthority,
                "daemon Runtime instance fence is stale",
            ));
        }
        let role = DaemonRole::from_owner(owner.host);
        if !role.permits(request.operation) {
            return Err(DomainError::new(
                ErrorCode::ProtocolIncompatible,
                "operation is illegal for the daemon role",
            ));
        }
        Ok(())
    }

    fn handle_request_with_descriptors(
        &mut self,
        request: &WireEnvelope,
        received: Vec<OwnedFd>,
    ) -> Result<(Value, Vec<(String, OwnedFd)>), DomainError> {
        let install = match request.operation {
            Operation::MaintenanceInstallApk => Some(crate::maintenance::InstallKind::Apk),
            Operation::MaintenanceInstallModule => Some(crate::maintenance::InstallKind::Module),
            _ => None,
        };
        if let Some(kind) = install {
            let reply = self.maintenance.install(
                kind,
                &self.canonical_base,
                &self.module_root,
                &self.identity,
                request,
                received,
            );
            return Ok((reply, Vec::new()));
        }
        if request.operation == Operation::RuntimeForward
            && runtime::McpArtifactQuery::is_artifact_query(&request.payload)
        {
            return Ok(self.answer_artifact_query(&request.payload));
        }
        self.handle_request(request)
            .map(|payload| (payload, Vec::new()))
    }

    /// Answers one S-MCP-006 internal artifact query. A host failure is a typed reply the facade
    /// maps to `-32603`, never a reason to drop the companion connection.
    fn answer_artifact_query(&self, payload: &Value) -> (Value, Vec<(String, OwnedFd)>) {
        let answered = self
            .host
            .as_ref()
            .ok_or_else(|| {
                DomainError::new(
                    ErrorCode::CapabilityUnavailable,
                    "Magisk Runtime is unavailable",
                )
            })
            .and_then(|host| host.answer_artifact_query(payload));
        match answered {
            Ok(reply) => (
                reply.payload,
                reply
                    .descriptor
                    .map(|file| vec![("mcp_artifact".to_owned(), OwnedFd::from(file))])
                    .unwrap_or_default(),
            ),
            Err(error) => (error_payload(error.code), Vec::new()),
        }
    }

    fn handle_request(&mut self, request: &WireEnvelope) -> Result<Value, DomainError> {
        match request.operation {
            Operation::HostStatus => self.status_payload(),
            Operation::HostPrepareTransition => self.prepare_transition(&request.payload),
            Operation::HostAbortTransition => self.abort_transition(&request.payload),
            Operation::HostRelease => self.release_transition(&request.payload),
            Operation::HostActivate => self.activate_transition(&request.payload),
            Operation::RuntimeForward => self.forward_runtime(&request.payload),
            Operation::RuntimeCancel => Ok(json!({"cancelled":false})),
            Operation::CapabilitySnapshot => self.capability_payload(),
            Operation::NetworkDefaultChanged => self.network_default_changed(request),
            Operation::NetworkAttachment => self.network_attachment(request),
            Operation::DiagnosticsSnapshot => Ok(json!({"faults":[]})),
            Operation::MaintenanceStatus => Ok(self
                .maintenance
                .status(&self.canonical_base, &request.payload)),
            Operation::MaintenanceInstallApk | Operation::MaintenanceInstallModule => {
                Err(DomainError::new(
                    ErrorCode::InternalError,
                    "maintenance installs are routed with their descriptors",
                ))
            }
            Operation::CompanionExecute | Operation::CompanionCancel => Err(DomainError::new(
                ErrorCode::ProtocolIncompatible,
                "companion operation has invalid direction",
            )),
        }
    }

    fn network_default_changed(&self, request: &WireEnvelope) -> Result<Value, DomainError> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Payload {
            subscription_generation: u64,
            source_generation: u64,
            #[serde(default)]
            network_id: Option<String>,
            #[serde(default)]
            transport: Option<String>,
        }

        let payload: Payload = decode_payload(&request.payload)?;
        let instance = request.runtime_instance_id.clone().ok_or_else(|| {
            DomainError::new(
                ErrorCode::ProtocolIncompatible,
                "network event instance fence is absent",
            )
        })?;
        let host = self.host.as_ref().ok_or_else(|| {
            DomainError::new(
                ErrorCode::CapabilityUnavailable,
                "Magisk Runtime is unavailable",
            )
        })?;
        let delivery = host.observe_network_default(
            NetworkDefaultSourceRegistration {
                fence: AdmissionFence {
                    runtime_epoch: request.runtime_epoch.clone(),
                    host_generation: request.host_generation,
                    runtime_instance_id: instance,
                },
                subscription_generation: payload.subscription_generation,
                source_generation: payload.source_generation,
            },
            NetworkDefaultChangedEvent::new(payload.network_id, payload.transport),
        )?;
        if delivery == NetworkEventDelivery::IgnoredStale {
            return Ok(error_payload(ErrorCode::StaleAuthority));
        }
        Ok(json!({"accepted":true}))
    }

    /// The companion asserts the default network its own process runs under, so the network this
    /// process runs under follows it. This is a host fact, not a Runtime event: the Automation
    /// event plane carries `network.default_changed` only while an Automation requires it, and
    /// this process must follow the device's network whenever it is the Magisk host.
    fn network_attachment(&self, request: &WireEnvelope) -> Result<Value, DomainError> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Payload {
            #[serde(default)]
            network_id: Option<String>,
        }

        let payload: Payload = decode_payload(&request.payload)?;
        let host = self.host.as_ref().ok_or_else(|| {
            DomainError::new(
                ErrorCode::CapabilityUnavailable,
                "Magisk Runtime is unavailable",
            )
        })?;
        host.apply_network_attachment(payload.network_id.as_deref())?;
        Ok(json!({"accepted":true}))
    }

    fn prepare_transition(&mut self, payload: &Value) -> Result<Value, DomainError> {
        let request: contract::HostPrepareTransition = decode_payload(payload)?;
        let owner = self.store.read_owner()?;
        if request.runtime_epoch != owner.runtime_epoch
            || request.from_host != owner.host
            || request.from_generation != owner.host_generation
            || request.target_host == owner.host
        {
            return Ok(error_payload(ErrorCode::StaleAuthority));
        }
        if owner.host == RuntimeHost::ApkRuntime {
            if request.target_host != RuntimeHost::MagiskBackend
                || self.refresh_observation().is_err()
                || self
                    .observation
                    .readiness(&self.identity, VERSION_CODE)
                    .is_err()
            {
                return Ok(error_payload(ErrorCode::CapabilityUnavailable));
            }
            self.target_preparation = Some(request.transition_id.clone());
            let state: CanonicalState = read_json(&self.canonical_base.join("runtime-state.json"))?;
            return Ok(json!({"prepared":true,"store_revision":state.store_revision}));
        }
        let host = self.host.as_mut().ok_or_else(|| {
            DomainError::new(
                ErrorCode::CapabilityUnavailable,
                "Magisk Runtime is unavailable",
            )
        })?;
        if !host.guard_ready() {
            return Ok(error_payload(ErrorCode::IoError));
        }
        if &request.from_instance_id != host.instance_id() {
            return Ok(error_payload(ErrorCode::StaleAuthority));
        }
        match host.prepare_transition(request.transition_id, request.target_host) {
            Ok(prepared) => Ok(json!({
                "prepared":true,
                "store_revision":prepared.store_revision,
            })),
            Err(error) => Ok(error_payload(error.code)),
        }
    }

    fn abort_transition(&mut self, payload: &Value) -> Result<Value, DomainError> {
        let request: contract::TransitionId = decode_payload(payload)?;
        if self.target_preparation.as_ref() == Some(&request.transition_id) {
            self.target_preparation = None;
            return Ok(json!({"aborted":true}));
        }
        let result = self
            .host
            .as_mut()
            .ok_or_else(|| {
                DomainError::new(ErrorCode::StaleAuthority, "no source Runtime is prepared")
            })?
            .abort_transition(&request.transition_id);
        Ok(match result {
            Ok(()) => json!({"aborted":true}),
            Err(error) => error_payload(error.code),
        })
    }

    fn release_transition(&mut self, payload: &Value) -> Result<Value, DomainError> {
        let request: contract::TransitionId = decode_payload(payload)?;
        let host = self.host.as_mut().ok_or_else(|| {
            DomainError::new(ErrorCode::StaleAuthority, "Magisk Runtime is not active")
        })?;
        // The instance ends here, and the companion's authority over this process's default
        // network ends with it. The binding is cleared first because releasing the transition is
        // one-way: a refused clear must leave the release retryable.
        if let Err(error) = host.clear_process_network() {
            return Ok(error_payload(error.code));
        }
        if let Err(error) = host.release_transition(&request.transition_id) {
            return Ok(error_payload(error.code));
        }
        self.host = None;
        let owner = self.store.read_owner()?;
        self.bind_companion_fence(&owner)?;
        Ok(json!({"released":true}))
    }

    fn activate_transition(&mut self, payload: &Value) -> Result<Value, DomainError> {
        let request: contract::HostActivate = decode_payload(payload)?;
        let owner = self.store.read_owner()?;
        let transition = self.store.observe_transition()?;
        if request.runtime_epoch != owner.runtime_epoch
            || request.host_generation != owner.host_generation
            || request.target_host != owner.host
            || owner.host != RuntimeHost::MagiskBackend
            || !transition.is_some_and(|(recovery, intent)| {
                recovery == TransitionRecovery::ActivateCommittedTarget
                    && intent.transition_id == request.transition_id
                    && intent.runtime_epoch == request.runtime_epoch
                    && intent.target_host == request.target_host
                    && intent.target_generation == request.host_generation
            })
            || self
                .target_preparation
                .as_ref()
                .is_some_and(|value| value != &request.transition_id)
        {
            return Ok(error_payload(ErrorCode::StaleAuthority));
        }
        self.activate_current_owner()?;
        self.target_preparation = None;
        let instance = self.host.as_ref().map(|host| host.instance_id().clone());
        Ok(json!({"ready":true,"runtime_instance_id":instance}))
    }

    fn activate_current_owner(&mut self) -> Result<(), DomainError> {
        if self.host.is_some() {
            return Ok(());
        }
        self.refresh_observation()?;
        self.observation.readiness(&self.identity, VERSION_CODE)?;
        let owner = self.store.read_owner()?;
        if owner.host != RuntimeHost::MagiskBackend {
            return Err(DomainError::new(
                ErrorCode::StaleAuthority,
                "Magisk Runtime is not the selected owner",
            ));
        }
        let host = MagiskHost::activate(
            Arc::clone(&self.store),
            &self.module_root,
            &self.canonical_base,
            self.sdk_int,
            owner.clone(),
            self.companion_port.clone(),
            Arc::clone(&self.task_activity),
        )?;
        host.set_app_execution_surface(self.companion.capability_state())?;
        self.host = Some(host);
        // The fence is bound before any companion-derived capability becomes available, so
        // no request can be admitted for a delegation the connection cannot present.
        self.bind_companion_fence(&owner)?;
        if self.companion.capability_state() == CapabilityState::Available {
            self.apply_companion_capabilities()?;
            self.host
                .as_ref()
                .expect("activated Magisk host")
                .use_companion_network_events()?;
        }
        Ok(())
    }

    /// Binds the live companion connection to the daemon's current Runtime identity: the
    /// connection may delegate only for the instance this daemon currently holds, so the
    /// fence follows the owner record and the active host instance.
    fn bind_companion_fence(&self, owner: &RuntimeOwner) -> Result<(), DomainError> {
        self.companion_port.set_fence(
            owner.runtime_epoch.clone(),
            owner.host_generation,
            self.host.as_ref().map(|host| host.instance_id().clone()),
        )
    }

    fn forward_runtime(&self, payload: &Value) -> Result<Value, DomainError> {
        let host = self.host.as_ref().ok_or_else(|| {
            DomainError::new(
                ErrorCode::CapabilityUnavailable,
                "Magisk Runtime is unavailable",
            )
        })?;
        host.forward_runtime(payload)
    }

    fn set_companion(&mut self, connected: bool) -> Result<(), DomainError> {
        if connected {
            self.companion.observe_connected();
        } else {
            self.companion.observe_disconnected();
            self.companion_capabilities.clear();
        }
        if let Some(host) = self.host.as_ref() {
            if connected {
                self.apply_companion_capabilities()?;
                host.use_companion_network_events()?;
                return Ok(());
            }
            host.withdraw_capabilities(COMPANION_CAPABILITY_KEYS, "COMPANION_UNAVAILABLE")?;
            host.set_app_execution_surface(self.companion.capability_state())?;
            host.use_native_network_events()?;
        }
        Ok(())
    }

    fn apply_companion_capabilities(&self) -> Result<(), DomainError> {
        let Some(host) = self.host.as_ref() else {
            return Ok(());
        };
        for registration in &self.companion_capabilities {
            host.register_companion_capability(registration)?;
        }
        host.set_app_execution_surface(CapabilityState::Available)
    }

    fn status_payload(&mut self) -> Result<Value, DomainError> {
        let observation_ready = self
            .refresh_observation()
            .and_then(|()| self.observation.readiness(&self.identity, VERSION_CODE))
            .is_ok();
        if let Some(host) = self.host.as_mut() {
            host.refresh_runtime_facts(&self.module_root, self.sdk_int, observation_ready)?;
        }
        let owner = self.store.read_owner().ok();
        let role = owner
            .as_ref()
            .map(|value| DaemonRole::from_owner(value.host));
        let ready = role.is_some_and(|value| {
            value.ready(
                observation_ready,
                self.host.as_ref().is_some_and(MagiskHost::guard_ready),
            )
        });
        Ok(json!({
            "role": match role {
                Some(DaemonRole::RuntimeHost) => "runtime_host",
                _ => "backend_only",
            },
            "ready": ready,
            "transition_cleanup_ready": self.host.as_ref().is_none_or(MagiskHost::guard_ready),
            "module_id": self.identity.module_id,
            "module_version": env!("CARGO_PKG_VERSION"),
            "version_code": VERSION_CODE,
            "protocol_version": PROTOCOL_VERSION,
            "runtime_instance_id": self.host.as_ref().map(|host| host.instance_id().clone()),
            "helper_ready": self.host.as_ref().is_some_and(MagiskHost::helper_ready),
        }))
    }

    fn capability_payload(&mut self) -> Result<Value, DomainError> {
        let module_ready = self
            .refresh_observation()
            .and_then(|()| self.observation.readiness(&self.identity, VERSION_CODE))
            .is_ok();
        if let Some(host) = self.host.as_mut() {
            host.refresh_runtime_facts(&self.module_root, self.sdk_int, module_ready)?;
        }
        let root_ready = module_ready && self.host.as_ref().is_some_and(MagiskHost::guard_ready);
        let helper_ready = module_ready && self.host.as_ref().is_some_and(MagiskHost::helper_ready);
        let companion_ready = self
            .host
            .as_ref()
            .is_some_and(MagiskHost::companion_available);
        let wake_alarm_ready =
            module_ready && self.host.as_ref().is_some_and(MagiskHost::wake_alarm_ready);
        Ok(json!({
            "magisk.module": state_token(capability_state(module_ready)),
            "magisk.root": state_token(capability_state(root_ready)),
            "magisk.framework": state_token(capability_state(helper_ready)),
            "magisk.wake_alarm": state_token(capability_state(wake_alarm_ready)),
            "execution.root_guard": state_token(capability_state(root_ready)),
            "app_execution_surface": state_token(capability_state(companion_ready)),
            "shizuku.shell": state_token(CapabilityState::Unavailable),
        }))
    }

    fn refresh_observation(&mut self) -> Result<(), DomainError> {
        self.observation = observe_module(&self.module_root, &self.canonical_base, &self.identity)?;
        Ok(())
    }
}

fn canonical_directory_identity(path: &Path) -> Result<CanonicalDirectoryIdentity, DomainError> {
    let metadata =
        fs::metadata(path).map_err(|_| io_error("cannot inspect App canonical directory"))?;
    if !metadata.is_dir() {
        return Err(io_error("App canonical directory is not a directory"));
    }
    Ok(CanonicalDirectoryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        uid: metadata.uid(),
    })
}

fn new_uuid() -> Result<UuidV4, DomainError> {
    UuidV4::parse(uuid::Uuid::new_v4().hyphenated().to_string())
        .map_err(|_| DomainError::new(ErrorCode::InternalError, "UUID generation failed"))
}

fn decode_payload<T: serde::de::DeserializeOwned>(payload: &Value) -> Result<T, DomainError> {
    serde_json::from_value(payload.clone()).map_err(|_| DomainError::invalid("invalid IPC payload"))
}

fn error_payload(code: ErrorCode) -> Value {
    json!({"error":{"code":code,"retryable":false}})
}

const fn capability_state(available: bool) -> CapabilityState {
    if available {
        CapabilityState::Available
    } else {
        CapabilityState::Unavailable
    }
}

const fn state_token(state: CapabilityState) -> &'static str {
    match state {
        CapabilityState::Available => "available",
        CapabilityState::Unavailable => "unavailable",
        CapabilityState::Unknown => "unknown",
    }
}

const fn io_error(reason: &'static str) -> DomainError {
    DomainError::new(ErrorCode::IoError, reason)
}

#[cfg(feature = "debug-module")]
pub(crate) const fn build_identity() -> ModuleIdentity {
    ModuleIdentity::debug()
}

#[cfg(not(feature = "debug-module"))]
pub(crate) const fn build_identity() -> ModuleIdentity {
    ModuleIdentity::stable()
}
