use crate::{
    CompanionCapabilityRegistration, HelperFamily, HelperHello, HelperRegistry, HostCoordinator,
    HostPreparation, MagiskExecutorFence, MagiskExecutorHandle, ModuleIdentity, ModuleObservation,
    SourceGeneration, WakeAlarmProbe,
    android::{HelperConnection, HelperPort, MagiskAndroidPort, run_clipboard_child},
    automation_wake::RealtimeAlarmWake,
    command::{CommandQuarantine, MagiskCommandProcessPort, RootCommandGuard},
    companion::CompanionPort,
    helper_family_facts,
    magisk_guard_recovery::{
        FilesystemMagiskGuardRecovery, ProcFacts, execute_guard_recovery, probe_root_guard,
        read_boot_id, read_start_ticks,
    },
    network::{
        MagiskCaptureBackend, MagiskNetworkSource, NativeNetworkDefaultEventSource,
        NativeNetworkPort,
    },
    process_network::ProcessNetworkAttachment,
    unix_transport::{peer_uid, receive_json},
    visual::MagiskVisualPort,
};
use chrono::{SecondsFormat, Utc};
use contract::{
    Availability, CapabilityState, ContextCall, ErrorCode, PublicPayload, RunAs, RuntimeHost,
    UuidV4,
};
use domain::{DomainError, OutstandingWork};
use persistence::{
    JsonPersistencePort, LifetimeLease, RuntimeArtifactPort, RuntimeLive, RuntimeOwner, StateStore,
    await_guard_recovery_plan, verify_magisk_metadata_surface,
};
use runtime::{
    AndroidFrameworkFilesystemPort, AndroidNetworkDefaultEventSource, ApkCapabilityPort,
    ApkRuntimeVertical, AutomationScheduler, BoottimeClock, CapabilityPort,
    CompositeExecutionSurface, HostControlPort, NativeAndroidExecutionSurface,
    NativeCommandExecutionSurface, NativeFilesystemExecutionSurface, NativeNetworkExecutionSurface,
    NativeVisualExecutionSurface, NetworkDefaultChangedEvent, NetworkDefaultEventSource,
    NetworkDefaultSourceRegistration, NetworkEventDelivery, ProviderToken, RecoveryProof,
    RuntimeCore, VerticalEnvironment,
};
use std::{
    fs, io,
    os::fd::{AsRawFd, FromRawFd},
    os::unix::fs::{FileTypeExt, PermissionsExt},
    os::unix::net::{UnixListener, UnixStream},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

pub(crate) const VERSION_CODE: u64 = 3000;

pub(crate) struct MagiskHost {
    store: Arc<StateStore>,
    lease: Arc<LifetimeLease>,
    instance_id: UuidV4,
    vertical: ApkRuntimeVertical,
    coordinator: HostCoordinator,
    helper: Option<FrameworkHelper>,
    helper_port: HelperPort,
    family_probes: [bool; 3],
    published_denials: [bool; 3],
    guard_ready: bool,
    module_ready: bool,
    wake_alarm_ready: bool,
    capability_generation: SourceGeneration,
    helper_generation: SourceGeneration,
    command_quarantine: Arc<CommandQuarantine>,
    executor: MagiskExecutorHandle,
    companion_network_events: Arc<MagiskCompanionNetworkEventSource>,
    native_network_events: Arc<NativeNetworkDefaultEventSource>,
    companion_network_events_selected: AtomicBool,
    /// This instance's own process binding to the default network the companion reported.
    process_network: ProcessNetworkAttachment,
    /// The fault that ended this instance's resident Automation scheduler, published as explicit
    /// capability loss by the next runtime fact refresh.
    automation_fault: Arc<StdMutex<Option<DomainError>>>,
    /// The ArtifactStore this instance owns, which answers S-MCP-006 internal artifact queries.
    artifacts: RuntimeArtifactPort,
    core: MagiskCore,
    async_runtime: tokio::runtime::Runtime,
}

type MagiskCore = RuntimeCore<
    JsonPersistencePort,
    RuntimeArtifactPort,
    MagiskExecutionSurface,
    ApkCapabilityPort,
    MagiskHostControl,
>;

type MagiskFilesystemSurface = NativeFilesystemExecutionSurface<
    RuntimeArtifactPort,
    ApkCapabilityPort,
    AndroidFrameworkFilesystemPort<CompanionPort>,
>;

/// The identities this host surface owns: the daemon runs `root` itself and forwards the
/// two identities the APK surface owns instead of impersonating them.
const COMMAND_IDENTITIES: &[RunAs] = &[RunAs::Root, RunAs::App, RunAs::Shell];

/// The Magisk host's own capture backend and observation source, so this host owns every
/// `network.inspect` field family and the raw capture/injection primitive (S-NET-001, S-NET-005).
type MagiskNetworkPort =
    NativeNetworkPort<MagiskNetworkSource, MagiskCaptureBackend, RuntimeArtifactPort>;

type MagiskNetworkSurface =
    NativeNetworkExecutionSurface<RuntimeArtifactPort, ApkCapabilityPort, MagiskNetworkPort>;

type MagiskVisualSurface = NativeVisualExecutionSurface<
    RuntimeArtifactPort,
    ApkCapabilityPort,
    MagiskVisualPort<ApkCapabilityPort>,
>;

type MagiskAndroidSurface = NativeAndroidExecutionSurface<ApkCapabilityPort, MagiskAndroidPort>;

type MagiskExecutionSurface = CompositeExecutionSurface<
    MagiskFilesystemSurface,
    NativeCommandExecutionSurface<RuntimeArtifactPort, ApkCapabilityPort, MagiskCommandProcessPort>,
    MagiskNetworkSurface,
    MagiskVisualSurface,
    MagiskAndroidSurface,
>;

struct MagiskCompanionNetworkEventSource {
    delegate: AndroidNetworkDefaultEventSource<CompanionPort, ApkCapabilityPort>,
    connected: AtomicBool,
}

impl MagiskCompanionNetworkEventSource {
    fn new(companion: CompanionPort, capabilities: ApkCapabilityPort) -> Self {
        Self {
            delegate: AndroidNetworkDefaultEventSource::new(companion, capabilities),
            connected: AtomicBool::new(false),
        }
    }
}

impl NetworkDefaultEventSource for MagiskCompanionNetworkEventSource {
    fn start(
        &self,
        registration: &NetworkDefaultSourceRegistration,
        ingress: runtime::NetworkDefaultEventIngress,
    ) -> Result<(), DomainError> {
        if !self.connected.load(Ordering::Acquire) {
            return Err(DomainError::new(
                ErrorCode::CapabilityUnavailable,
                "authenticated companion network source is unavailable",
            ));
        }
        self.delegate.start(registration, ingress)
    }

    fn stop(&self, registration: &NetworkDefaultSourceRegistration) -> Result<(), DomainError> {
        if !self.connected.load(Ordering::Acquire) {
            // The APK connection-loss owner unregisters its callback locally. The old source
            // generation is already invalidated here, so any retained callback is stale.
            return Ok(());
        }
        self.delegate.stop(registration)
    }
}

impl MagiskHost {
    pub(crate) fn activate(
        store: Arc<StateStore>,
        module_root: &Path,
        canonical_base: &Path,
        sdk_int: u32,
        owner: RuntimeOwner,
        companion: CompanionPort,
        task_activity: Arc<crate::app_keepalive::TaskActivityBeacon>,
    ) -> Result<Self, DomainError> {
        let instance_id = new_uuid()?;
        let boot_id = read_boot_id()?;
        let live = RuntimeLive {
            runtime_epoch: owner.runtime_epoch.clone(),
            host: owner.host,
            host_generation: owner.host_generation,
            runtime_instance_id: instance_id.clone(),
            boot_id: boot_id.clone(),
            pid: std::process::id(),
            start_ticks: read_start_ticks(Path::new("/proc/self/stat"))?,
        };
        let lease = Arc::new(store.acquire_lifetime(live)?);
        let state = store.load(&lease)?;
        let recovery = await_guard_recovery_plan(
            &state,
            &instance_id,
            &boot_id,
            &persistence::GuardProofDirectory::new(canonical_base),
            &ProcFacts,
        )?;
        let environment = VerticalEnvironment {
            sdk_int,
            abi: fixed_property("ro.product.cpu.abi")?,
            timezone: fixed_property("persist.sys.timezone")?,
            manufacturer: fixed_property("ro.product.manufacturer")?,
            model: fixed_property("ro.product.model")?,
            device: fixed_property("ro.product.device")?,
            build_fingerprint: fixed_property("ro.build.fingerprint")?,
            version_name: env!("CARGO_PKG_VERSION").to_owned(),
            version_code: VERSION_CODE,
            runtime_epoch: owner.runtime_epoch.clone(),
            host_generation: owner.host_generation,
        };
        let vertical = ApkRuntimeVertical::new_for_host(environment, RuntimeHost::MagiskBackend)?;
        let mut guard_ready = recovery.guards_are_clean()
            && probe_root_guard(
                canonical_base,
                &module_root.join("bin/droidbridge-exec-guard"),
                lease.live(),
            )
            .unwrap_or(false);
        let mut capability_generation = SourceGeneration::initial();
        let helper_generation = SourceGeneration::initial();
        let wake_alarm_ready = probe_wake_alarm().available();
        register(
            &vertical,
            "magisk.module",
            CapabilityState::Available,
            None,
            false,
            capability_generation.current(),
        )?;
        register(
            &vertical,
            "magisk.root",
            capability_state(guard_ready),
            (!guard_ready).then_some("CLEANUP_UNVERIFIED"),
            guard_ready,
            capability_generation.current(),
        )?;
        register(
            &vertical,
            "execution.root_guard",
            capability_state(guard_ready),
            (!guard_ready).then_some("CLEANUP_UNVERIFIED"),
            guard_ready,
            capability_generation.current(),
        )?;
        register(
            &vertical,
            "magisk.wake_alarm",
            capability_state(wake_alarm_ready),
            (!wake_alarm_ready).then_some("WAKE_ALARM_UNAVAILABLE"),
            wake_alarm_ready,
            capability_generation.current(),
        )?;
        let helper = framework_boot_completed()
            .then(|| FrameworkHelper::start(module_root, sdk_int, helper_generation.current()).ok())
            .flatten();
        let helper_port = HelperPort::default();
        let family_probes = helper.as_ref().map_or([false; 3], |helper| {
            helper_port.publish(Arc::clone(&helper.connection), helper.jar.clone());
            probe_families(helper)
        });
        register(
            &vertical,
            "magisk.framework",
            capability_state(helper.is_some()),
            helper.is_none().then_some("HELPER_UNAVAILABLE"),
            helper.is_some(),
            capability_generation.current(),
        )?;
        for fact in helper_family_facts(
            helper.is_some(),
            |family| family_probes[family_index(family)],
            |_| false,
        ) {
            register(
                &vertical,
                fact.family.key(),
                fact.state,
                fact.reason,
                fact.state == CapabilityState::Available,
                capability_generation.current(),
            )?;
        }
        if !guard_ready {
            vertical.set_unavailable("CLEANUP_UNVERIFIED")?;
        }
        let capabilities = vertical.capability_port(instance_id.clone());
        let host_control = MagiskHostControl {
            store: Arc::clone(&store),
            lease: Arc::clone(&lease),
            capabilities: capabilities.clone(),
            task_activity,
            recovery_proof: if recovery.guards_are_clean() {
                RecoveryProof::Clean
            } else {
                RecoveryProof::CleanupUnverified
            },
        };
        host_control.prepare()?;
        host_control.activate(&capabilities.current()?.fence)?;
        let artifacts = RuntimeArtifactPort::new(Arc::clone(&store), Arc::clone(&lease));
        let command_quarantine = Arc::new(CommandQuarantine::default());
        let command_root = Arc::new(RootCommandGuard::new(
            canonical_base.to_path_buf(),
            crate::command::guard_path(module_root),
            owner.runtime_epoch.clone(),
            instance_id.clone(),
            boot_id.clone(),
            Arc::clone(&command_quarantine),
        ));
        let companion_network_events = Arc::new(MagiskCompanionNetworkEventSource::new(
            companion.clone(),
            capabilities.clone(),
        ));
        let native_network_events = Arc::new(NativeNetworkDefaultEventSource::default());
        let process_network = ProcessNetworkAttachment::device();
        let executions = CompositeExecutionSurface::new(
            NativeFilesystemExecutionSurface::new(
                canonical_base.to_path_buf(),
                artifacts.clone(),
                capabilities.clone(),
                ProviderToken::MagiskNative,
            )
            .with_framework(AndroidFrameworkFilesystemPort::new(companion.clone())),
        )
        .with_command(NativeCommandExecutionSurface::new(
            artifacts.clone(),
            capabilities.clone(),
            COMMAND_IDENTITIES,
            MagiskCommandProcessPort::new(command_root.clone(), companion.clone()),
        ))
        .with_network(NativeNetworkExecutionSurface::new(
            artifacts.clone(),
            capabilities.clone(),
            NativeNetworkPort::new(
                MagiskNetworkSource::new(companion.clone()),
                MagiskCaptureBackend,
                artifacts.clone(),
            ),
        ))
        .with_android(
            NativeAndroidExecutionSurface::new(
                capabilities.clone(),
                crate::process::build_identity().package,
            )
            .with_primitives(MagiskAndroidPort::new(
                command_root.clone(),
                companion.clone(),
                helper_port.clone(),
            )),
        )
        .with_visual(
            NativeVisualExecutionSurface::new(artifacts.clone(), capabilities.clone())
                .with_primitives(MagiskVisualPort::new(
                    canonical_base.to_path_buf(),
                    capabilities.clone(),
                    command_root,
                    companion,
                    helper_port.clone(),
                )),
        );
        let core = RuntimeCore::new(
            JsonPersistencePort::new(Arc::clone(&store), Arc::clone(&lease)),
            artifacts.clone(),
            executions,
            capabilities,
            host_control,
        )
        .with_network_default_event_source(native_network_events.clone());
        let async_runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .map_err(|_| DomainError::new(ErrorCode::InternalError, "Runtime executor failed"))?;
        if !recovery.prior_instances().is_empty() {
            let now = Utc::now();
            let terminal_at_ms = u64::try_from(now.timestamp_millis()).map_err(|_| {
                DomainError::new(ErrorCode::InternalError, "system time is invalid")
            })?;
            let ended_at = now.to_rfc3339_opts(SecondsFormat::Millis, true);
            for old_instance_id in recovery.prior_instances() {
                async_runtime.block_on(core.recover_old_instance(
                    old_instance_id,
                    ended_at.clone(),
                    terminal_at_ms,
                ))?;
            }
        }
        let mut mechanics = FilesystemMagiskGuardRecovery::new(canonical_base, &store, &lease);
        if execute_guard_recovery(&recovery, &mut mechanics).is_err() {
            guard_ready = false;
            let generation = capability_generation.advance()?;
            register(
                &vertical,
                "magisk.root",
                CapabilityState::Unavailable,
                Some("CLEANUP_UNVERIFIED"),
                false,
                generation,
            )?;
            register(
                &vertical,
                "execution.root_guard",
                CapabilityState::Unavailable,
                Some("CLEANUP_UNVERIFIED"),
                false,
                generation,
            )?;
            vertical.set_unavailable("CLEANUP_UNVERIFIED")?;
        }
        let executor = MagiskExecutorHandle::new(
            MagiskExecutorFence {
                runtime_epoch: owner.runtime_epoch.clone(),
                host_generation: owner.host_generation,
                runtime_instance_id: instance_id.clone(),
                source_generation: capability_generation.current(),
            },
            helper.as_ref().map(|_| helper_generation.current()),
        )?;
        let automation_fault = Arc::new(StdMutex::new(None));
        spawn_automation_scheduler(
            &async_runtime,
            core.clone(),
            wake_alarm_ready,
            Arc::clone(&automation_fault),
        );
        Ok(Self {
            store,
            lease,
            instance_id: instance_id.clone(),
            vertical,
            coordinator: HostCoordinator::new(
                RuntimeHost::MagiskBackend,
                owner.host_generation,
                instance_id,
            ),
            helper,
            helper_port,
            family_probes,
            published_denials: [false; 3],
            guard_ready,
            module_ready: true,
            wake_alarm_ready,
            capability_generation,
            helper_generation,
            command_quarantine,
            executor,
            companion_network_events,
            native_network_events,
            companion_network_events_selected: AtomicBool::new(false),
            process_network,
            automation_fault,
            artifacts,
            core,
            async_runtime,
        })
    }

    pub(crate) fn instance_id(&self) -> &UuidV4 {
        &self.instance_id
    }

    pub(crate) fn observe_network_default(
        &self,
        registration: NetworkDefaultSourceRegistration,
        event: NetworkDefaultChangedEvent,
    ) -> Result<NetworkEventDelivery, DomainError> {
        self.core.observe_network_default_event(registration, event)
    }

    /// Applies the default network the companion states its own process runs under. The companion
    /// is this host's only authority for the device's default network, so its own attachment is
    /// also the attachment this process takes.
    pub(crate) fn apply_network_attachment(
        &self,
        network_id: Option<&str>,
    ) -> Result<(), DomainError> {
        self.process_network.apply(network_id)
    }

    /// Ends this instance's process binding. The binding is part of the Magisk host instance,
    /// so it must not outlive the instance that acquired it.
    pub(crate) fn clear_process_network(&self) -> Result<(), DomainError> {
        self.process_network.clear()
    }

    pub(crate) fn use_companion_network_events(&self) -> Result<(), DomainError> {
        self.companion_network_events
            .connected
            .store(true, Ordering::Release);
        if self
            .companion_network_events_selected
            .swap(true, Ordering::AcqRel)
        {
            return Ok(());
        }
        self.core.replace_network_default_event_source(
            self.companion_network_events.clone() as Arc<dyn NetworkDefaultEventSource>
        )?;
        // A subscription that failed on the previous source is retried on the new one.
        self.core.canonical_changes().notify_one();
        Ok(())
    }

    pub(crate) fn use_native_network_events(&self) -> Result<(), DomainError> {
        self.companion_network_events
            .connected
            .store(false, Ordering::Release);
        // The deselected companion is the only authority for the handle this process was bound
        // to, so the native source starts from an unbound process.
        self.process_network.clear()?;
        if !self
            .companion_network_events_selected
            .swap(false, Ordering::AcqRel)
        {
            return Ok(());
        }
        self.core.replace_network_default_event_source(
            self.native_network_events.clone() as Arc<dyn NetworkDefaultEventSource>
        )?;
        self.core.canonical_changes().notify_one();
        Ok(())
    }

    pub(crate) fn guard_ready(&self) -> bool {
        self.guard_ready
    }

    pub(crate) fn wake_alarm_ready(&self) -> bool {
        self.wake_alarm_ready
    }

    pub(crate) fn helper_ready(&self) -> bool {
        self.helper.is_some()
    }

    pub(crate) fn prepare_transition(
        &mut self,
        transition_id: UuidV4,
        target_host: RuntimeHost,
    ) -> Result<HostPreparation, DomainError> {
        let outstanding_work = self.outstanding_work()?;
        let store_revision = self.store_revision()?;
        self.coordinator
            .prepare(transition_id, target_host, outstanding_work, store_revision)
    }

    pub(crate) fn abort_transition(&mut self, transition_id: &UuidV4) -> Result<(), DomainError> {
        self.coordinator.abort(transition_id)
    }

    pub(crate) fn release_transition(&mut self, transition_id: &UuidV4) -> Result<(), DomainError> {
        self.coordinator.release(transition_id)
    }

    pub(crate) fn validate_lease(&self) -> Result<(), DomainError> {
        self.store.validate_lease(&self.lease)
    }

    pub(crate) fn set_app_execution_surface(
        &self,
        state: CapabilityState,
    ) -> Result<(), DomainError> {
        self.vertical.set_app_execution_surface(state)
    }

    pub(crate) fn withdraw_capabilities(
        &self,
        keys: &[&str],
        reason: &str,
    ) -> Result<(), DomainError> {
        self.vertical.withdraw_capabilities(keys, reason)
    }

    pub(crate) fn register_companion_capability(
        &self,
        registration: &CompanionCapabilityRegistration,
    ) -> Result<(), DomainError> {
        self.vertical
            .register_capability(
                &registration.key,
                Availability {
                    state: registration.state,
                    reason: registration.reason.clone(),
                },
                registration.source_generation,
                registration.has_executor,
            )
            .map(|_| ())
    }

    pub(crate) fn forward_runtime(
        &self,
        payload: &serde_json::Value,
    ) -> Result<serde_json::Value, DomainError> {
        self.validate_lease()?;
        let encoded = serde_json::to_vec(payload)
            .map_err(|_| DomainError::invalid("RuntimeForward payload is invalid"))?;
        let now = Utc::now();
        let now_ms = u64::try_from(now.timestamp_millis())
            .map_err(|_| DomainError::new(ErrorCode::InternalError, "system time is invalid"))?;
        let admission_open = self.coordinator.admission_open();
        let response = self.async_runtime.block_on(runtime::submit_public(
            &self.core,
            &encoded,
            now.to_rfc3339_opts(SecondsFormat::Millis, true),
            now_ms,
            admission_open,
            |request| {
                std::future::ready(
                    if admission_open
                        || matches!(
                            &request.payload,
                            PublicPayload::Context {
                                call: ContextCall::Status(_)
                            }
                        )
                    {
                        self.vertical.dispatch_installed(request)
                    } else {
                        Err(DomainError::new(
                            ErrorCode::HostTransitionPending,
                            "Runtime host transition is pending",
                        ))
                    },
                )
            },
        ));
        serde_json::from_slice(&response)
            .map_err(|_| io_error("Magisk Runtime response is invalid"))
    }

    /// Answers one S-MCP-006 internal artifact query forwarded by the APK facade. It never enters
    /// public ingress, so it stays answerable while business admission is closed.
    pub(crate) fn answer_artifact_query(
        &self,
        payload: &serde_json::Value,
    ) -> Result<runtime::McpArtifactReply, DomainError> {
        self.validate_lease()?;
        let now_ms = u64::try_from(Utc::now().timestamp_millis())
            .map_err(|_| DomainError::new(ErrorCode::InternalError, "system time is invalid"))?;
        self.artifacts.answer_mcp_query(payload, now_ms)
    }

    pub(crate) fn refresh_runtime_facts(
        &mut self,
        module_root: &Path,
        sdk_int: u32,
        observed_module_ready: bool,
    ) -> Result<(), DomainError> {
        let scheduler_faulted = match self.automation_fault.lock() {
            Ok(fault) => fault.is_some(),
            Err(poisoned) => poisoned.into_inner().is_some(),
        };
        if scheduler_faulted && self.wake_alarm_ready {
            // Time-trigger admission stopped with the scheduler; persisted dues stay unchanged.
            self.wake_alarm_ready = false;
            let generation = self.capability_generation.advance()?;
            register(
                &self.vertical,
                "magisk.wake_alarm",
                CapabilityState::Unavailable,
                Some("WAKE_ALARM_UNAVAILABLE"),
                false,
                generation,
            )?;
        }
        if self.guard_ready && self.command_quarantine.is_flagged() {
            self.guard_ready = false;
            self.vertical.set_unavailable("CLEANUP_UNVERIFIED")?;
            let generation = self.capability_generation.advance()?;
            for key in ["magisk.root", "execution.root_guard"] {
                register(
                    &self.vertical,
                    key,
                    CapabilityState::Unavailable,
                    Some("CLEANUP_UNVERIFIED"),
                    false,
                    generation,
                )?;
            }
            self.refresh_executor()?;
        }
        if self.module_ready != observed_module_ready {
            self.module_ready = observed_module_ready;
            if !observed_module_ready {
                self.guard_ready = false;
                self.vertical.set_unavailable("MODULE_UNAVAILABLE")?;
            }
            let generation = self.capability_generation.advance()?;
            register(
                &self.vertical,
                "magisk.module",
                capability_state(observed_module_ready),
                (!observed_module_ready).then_some("MODULE_UNAVAILABLE"),
                false,
                generation,
            )?;
            for key in ["magisk.root", "execution.root_guard"] {
                register(
                    &self.vertical,
                    key,
                    capability_state(observed_module_ready && self.guard_ready),
                    (!(observed_module_ready && self.guard_ready))
                        .then_some("MODULE_OR_GUARD_UNAVAILABLE"),
                    observed_module_ready && self.guard_ready,
                    generation,
                )?;
            }
            self.refresh_executor()?;
        }

        if self.helper.as_mut().is_some_and(FrameworkHelper::is_alive) {
            let denials = HelperFamily::ALL.map(|family| self.helper_port.operation_denied(family));
            if denials != self.published_denials {
                self.published_denials = denials;
                self.publish_helper_state(true)?;
            }
            return Ok(());
        }
        if self.helper.is_some() {
            self.helper = None;
            self.helper_port.withdraw();
            self.executor = self.executor.without_helper();
            self.publish_helper_state(false)?;
            return Ok(());
        }
        if !self.module_ready || !self.guard_ready || !framework_boot_completed() {
            return Ok(());
        }
        let helper_generation = self.helper_generation.advance()?;
        if let Ok(helper) = FrameworkHelper::start(module_root, sdk_int, helper_generation) {
            self.helper_port
                .publish(Arc::clone(&helper.connection), helper.jar.clone());
            self.family_probes = probe_families(&helper);
            self.published_denials = [false; 3];
            self.helper = Some(helper);
            self.publish_helper_state(true)?;
        }
        Ok(())
    }

    pub(crate) fn companion_available(&self) -> bool {
        self.vertical
            .capability_port(self.instance_id.clone())
            .current()
            .is_ok_and(|snapshot| {
                snapshot.context.app_execution_surface == CapabilityState::Available
            })
    }

    fn outstanding_work(&self) -> Result<OutstandingWork, DomainError> {
        let state = self.store.load(&self.lease)?;
        let tasks = state
            .tasks
            .iter()
            .filter(|task| {
                matches!(
                    task.state,
                    contract::TaskState::Created
                        | contract::TaskState::Queued
                        | contract::TaskState::Running
                )
            })
            .count();
        let automation_executions = state
            .automation_executions
            .iter()
            .filter(|execution| {
                matches!(
                    execution.summary.state,
                    contract::AutomationExecutionState::Queued
                        | contract::AutomationExecutionState::Running
                )
            })
            .count();
        Ok(OutstandingWork {
            tasks: u32::try_from(tasks).unwrap_or(u32::MAX),
            automation_executions: u32::try_from(automation_executions).unwrap_or(u32::MAX),
            synchronous_executions: u32::from(!state.reservations.is_empty()),
        })
    }

    fn store_revision(&self) -> Result<u64, DomainError> {
        Ok(self.store.load(&self.lease)?.store_revision)
    }

    fn publish_helper_state(&mut self, available: bool) -> Result<(), DomainError> {
        let generation = self.capability_generation.advance()?;
        register(
            &self.vertical,
            "magisk.framework",
            capability_state(available),
            (!available).then_some("HELPER_UNAVAILABLE"),
            available,
            generation,
        )?;
        for fact in helper_family_facts(
            available,
            |family| self.family_probes[family_index(family)],
            |family| self.published_denials[family_index(family)],
        ) {
            register(
                &self.vertical,
                fact.family.key(),
                fact.state,
                fact.reason,
                fact.state == CapabilityState::Available,
                generation,
            )?;
        }
        self.refresh_executor()
    }

    fn refresh_executor(&mut self) -> Result<(), DomainError> {
        let mut fence = self.executor.fence().clone();
        fence.source_generation = self.capability_generation.current();
        self.executor = MagiskExecutorHandle::new(
            fence,
            self.helper
                .as_ref()
                .map(|_| self.helper_generation.current()),
        )?;
        Ok(())
    }
}

pub(crate) fn observe_module(
    module_root: &Path,
    canonical_base: &Path,
    identity: &ModuleIdentity,
) -> Result<ModuleObservation, DomainError> {
    let stable = Path::new("/data/adb/modules/droidbridge");
    let debug = Path::new("/data/adb/modules/droidbridge_debug");
    let property = parse_module_property(&module_root.join("module.prop"), identity)?;
    Ok(ModuleObservation {
        stable_present: stable.is_dir() && !stable.join("remove").exists(),
        debug_present: debug.is_dir() && !debug.join("remove").exists(),
        enabled: !module_root.join("disable").exists() && !module_root.join("remove").exists(),
        module_version_code: property,
        daemon_version_code: VERSION_CODE,
        protocol_version: crate::PROTOCOL_VERSION,
        metadata_self_test: verify_magisk_metadata_surface(canonical_base).is_ok(),
        excluded: canonical_base.join("module-exclusion.json").exists(),
    })
}

/// The framework helper probes each family once per generation (S-MAGISK-005), so it starts
/// only after the Android system services it probes have finished booting.
fn framework_boot_completed() -> bool {
    fixed_property("sys.boot_completed").is_ok_and(|value| value == "1")
}

pub(crate) fn fixed_property(name: &str) -> Result<String, DomainError> {
    let output = Command::new("/system/bin/getprop")
        .arg(name)
        .output()
        .map_err(|_| io_error("cannot read Android property"))?;
    if !output.status.success() {
        return Err(io_error("Android property query failed"));
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| io_error("Android property is empty"))
}

fn parse_module_property(path: &Path, identity: &ModuleIdentity) -> Result<u64, DomainError> {
    let contents = fs::read_to_string(path).map_err(|_| io_error("cannot read module property"))?;
    let mut id = None;
    let mut version_code = None;
    for line in contents.lines() {
        if let Some(value) = line.strip_prefix("id=") {
            id = Some(value);
        }
        if let Some(value) = line.strip_prefix("versionCode=") {
            version_code = value.parse().ok();
        }
    }
    if id != Some(identity.module_id) {
        return Err(DomainError::new(
            ErrorCode::ProtocolIncompatible,
            "module id does not match daemon build",
        ));
    }
    version_code.ok_or_else(|| {
        DomainError::new(ErrorCode::ProtocolIncompatible, "module version is missing")
    })
}

/// Starts this instance's resident Automation scheduler on the Runtime reactor (S-LIFE-003,
/// S-AUTO-001): it publishes `runtime.ready` once, then projects persisted dues onto the wake
/// alarm timerfd when `magisk.wake_alarm` is available, or runs event Automations only. The task
/// ends with the reactor when the host is released.
fn spawn_automation_scheduler(
    async_runtime: &tokio::runtime::Runtime,
    core: MagiskCore,
    wake_alarm_ready: bool,
    fault: Arc<StdMutex<Option<DomainError>>>,
) {
    let scheduler = AutomationScheduler::new(core, Arc::new(BoottimeClock));
    async_runtime.spawn(async move {
        let ended = async {
            scheduler.publish_runtime_ready().await?;
            if wake_alarm_ready {
                let wake = RealtimeAlarmWake::new()?;
                scheduler.run(&wake).await
            } else {
                scheduler.run_events_only().await
            }
        }
        .await;
        if let Err(error) = ended {
            match fault.lock() {
                Ok(mut slot) => *slot = Some(error),
                Err(poisoned) => *poisoned.into_inner() = Some(error),
            }
        }
    });
}

fn probe_wake_alarm() -> WakeAlarmProbe {
    let mut probe = WakeAlarmProbe::default();
    let descriptor = unsafe {
        libc::timerfd_create(
            libc::CLOCK_REALTIME_ALARM,
            libc::TFD_NONBLOCK | libc::TFD_CLOEXEC,
        )
    };
    if descriptor < 0 {
        return probe;
    }
    let _descriptor = unsafe { fs::File::from_raw_fd(descriptor) };
    probe.created = true;
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    if unsafe { libc::clock_gettime(libc::CLOCK_REALTIME, &mut now) } != 0 {
        return probe;
    }
    probe.clock_read = true;
    let armed = libc::itimerspec {
        it_interval: libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        },
        it_value: libc::timespec {
            tv_sec: match now.tv_sec.checked_add(60) {
                Some(value) => value,
                None => return probe,
            },
            tv_nsec: now.tv_nsec,
        },
    };
    if unsafe {
        libc::timerfd_settime(
            descriptor,
            libc::TFD_TIMER_ABSTIME | libc::TFD_TIMER_CANCEL_ON_SET,
            &armed,
            std::ptr::null_mut(),
        )
    } != 0
    {
        return probe;
    }
    probe.armed = true;
    let disarmed = libc::itimerspec {
        it_interval: libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        },
        it_value: libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        },
    };
    if unsafe { libc::timerfd_settime(descriptor, 0, &disarmed, std::ptr::null_mut()) } == 0 {
        probe.disarmed = true;
    }
    probe
}

struct FrameworkHelper {
    child: Child,
    connection: Arc<HelperConnection>,
    jar: PathBuf,
    socket_path: PathBuf,
}

struct FrameworkHelperLaunch {
    child: Option<Child>,
    socket_path: Option<PathBuf>,
}

impl FrameworkHelperLaunch {
    fn finish(mut self) -> (Child, PathBuf) {
        (
            self.child.take().expect("helper child is present"),
            self.socket_path.take().expect("helper socket is present"),
        )
    }
}

impl Drop for FrameworkHelperLaunch {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(socket_path) = self.socket_path.as_ref() {
            let _ = fs::remove_file(socket_path);
        }
    }
}

impl FrameworkHelper {
    fn start(module_root: &Path, sdk_int: u32, generation: u64) -> Result<Self, DomainError> {
        let mut registry = HelperRegistry::new(sdk_int, generation)?;
        let run_directory = module_root.join("run");
        fs::create_dir_all(&run_directory)
            .map_err(|_| io_error("cannot create helper run directory"))?;
        fs::set_permissions(&run_directory, fs::Permissions::from_mode(0o700))
            .map_err(|_| io_error("cannot protect helper run directory"))?;
        let socket_path = run_directory.join(format!("framework-api{sdk_int}.sock"));
        if socket_path.exists() {
            let metadata = fs::symlink_metadata(&socket_path)
                .map_err(|_| io_error("cannot inspect stale helper socket"))?;
            if !metadata.file_type().is_socket() {
                return Err(io_error("helper socket path is not a socket"));
            }
            fs::remove_file(&socket_path)
                .map_err(|_| io_error("cannot remove stale helper socket"))?;
        }
        let listener =
            UnixListener::bind(&socket_path).map_err(|_| io_error("cannot bind helper socket"))?;
        fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))
            .map_err(|_| io_error("cannot protect helper socket"))?;
        listener
            .set_nonblocking(true)
            .map_err(|_| io_error("cannot bound helper accept"))?;
        let jar = module_root.join("framework").join(registry.jar_name());
        let listener_fd = listener.as_raw_fd();
        let mut command = Command::new("/system/bin/app_process");
        command
            .env("CLASSPATH", &jar)
            .arg("/system/bin")
            .arg("com.droidbridge.helper.DroidBridgeFrameworkHelper")
            .arg("3")
            .arg(sdk_int.to_string())
            .arg(generation.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(listener_fd, 3) < 0 || libc::fcntl(3, libc::F_SETFD, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = match command.spawn() {
            Ok(child) => child,
            Err(_) => {
                let _ = fs::remove_file(&socket_path);
                return Err(io_error("cannot launch framework helper"));
            }
        };
        drop(listener);
        let mut launch = FrameworkHelperLaunch {
            child: Some(child),
            socket_path: Some(socket_path),
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut stream = loop {
            let socket_path = launch
                .socket_path
                .as_ref()
                .expect("helper socket is present");
            match UnixStream::connect(socket_path) {
                Ok(stream) => break stream,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
                    ) =>
                {
                    if launch
                        .child
                        .as_mut()
                        .expect("helper child is present")
                        .try_wait()
                        .ok()
                        .flatten()
                        .is_some()
                        || Instant::now() >= deadline
                    {
                        return Err(io_error("framework helper did not accept"));
                    }
                    thread::sleep(Duration::from_millis(20));
                }
                Err(_) => return Err(io_error("framework helper connection failed")),
            }
        };
        let uid =
            peer_uid(&stream).map_err(|_| io_error("cannot authenticate framework helper"))?;
        let hello: HelperHello = receive_json(&mut stream)?;
        registry.accept_hello(uid, hello)?;
        let (child, socket_path) = launch.finish();
        Ok(Self {
            child,
            connection: Arc::new(HelperConnection::new(stream)),
            jar,
            socket_path,
        })
    }

    fn is_alive(&mut self) -> bool {
        !self.connection.is_broken() && self.child.try_wait().is_ok_and(|status| status.is_none())
    }
}

/// Runs each S-MAGISK-005 family probe once for this helper generation. A failed probe
/// records only its own family.
fn probe_families(helper: &FrameworkHelper) -> [bool; 3] {
    HelperFamily::ALL.map(|family| match family {
        HelperFamily::Launch => helper
            .connection
            .request(&serde_json::json!({"operation": "probe_launch"}))
            .is_ok(),
        HelperFamily::Notifications => helper
            .connection
            .request(&serde_json::json!({"operation": "probe_notifications"}))
            .is_ok(),
        HelperFamily::Clipboard => {
            run_clipboard_child(&helper.jar, "probe", &serde_json::json!({}), None).is_ok()
        }
    })
}

const fn family_index(family: HelperFamily) -> usize {
    family.index()
}

impl Drop for FrameworkHelper {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_file(&self.socket_path);
    }
}

#[derive(Clone)]
struct MagiskHostControl {
    store: Arc<StateStore>,
    lease: Arc<LifetimeLease>,
    capabilities: runtime::ApkCapabilityPort,
    recovery_proof: RecoveryProof,
    task_activity: Arc<crate::app_keepalive::TaskActivityBeacon>,
}

impl HostControlPort for MagiskHostControl {
    fn cleanup_unverified(
        &self,
        fence: &domain::AdmissionFence,
        _execution_id: &UuidV4,
    ) -> Result<(), DomainError> {
        self.activate(fence)?;
        self.capabilities.withdraw_readiness()
    }

    fn prepare(&self) -> Result<(), DomainError> {
        self.store.validate_lease(&self.lease)
    }

    fn activate(&self, fence: &domain::AdmissionFence) -> Result<(), DomainError> {
        self.store.validate_lease(&self.lease)?;
        let live = self.lease.live();
        if live.runtime_epoch != fence.runtime_epoch
            || live.host_generation != fence.host_generation
            || live.runtime_instance_id != fence.runtime_instance_id
        {
            return Err(DomainError::new(
                ErrorCode::StaleAuthority,
                "Magisk Runtime activation fence is stale",
            ));
        }
        Ok(())
    }

    fn recover(&self, _: &UuidV4) -> Result<RecoveryProof, DomainError> {
        self.store.validate_lease(&self.lease)?;
        if self.recovery_proof == RecoveryProof::CleanupUnverified {
            self.capabilities.withdraw_readiness()?;
        }
        Ok(self.recovery_proof)
    }

    /// The App executes the Android primitives these Tasks need, so the count is carried to it
    /// and held there. Publishing never fails a committed mutation: the beacon only records the
    /// count, and the wake it leads to is reported on this daemon's own log.
    fn task_activity_changed(&self, active_tasks: usize, canonical_revision: u64) {
        self.task_activity.publish(
            self.lease.live().runtime_epoch.as_str(),
            active_tasks as u64,
            canonical_revision,
        );
    }
}

fn register(
    vertical: &ApkRuntimeVertical,
    key: &str,
    state: CapabilityState,
    reason: Option<&str>,
    has_executor: bool,
    source_generation: u64,
) -> Result<(), DomainError> {
    vertical.register_capability(
        key,
        Availability {
            state,
            reason: reason.map(str::to_owned),
        },
        source_generation,
        has_executor,
    )?;
    Ok(())
}

fn new_uuid() -> Result<UuidV4, DomainError> {
    UuidV4::parse(uuid::Uuid::new_v4().hyphenated().to_string())
        .map_err(|_| DomainError::new(ErrorCode::InternalError, "UUID generation failed"))
}

const fn capability_state(available: bool) -> CapabilityState {
    if available {
        CapabilityState::Available
    } else {
        CapabilityState::Unavailable
    }
}

const fn io_error(reason: &'static str) -> DomainError {
    DomainError::new(ErrorCode::IoError, reason)
}
