use crate::app_guard_recovery::{AppCleanupVerification, reconcile_app_recovery};
use crate::automation_wake::ApkAlarmWake;
use crate::{
    AndroidFrameworkFilesystemDispatcher, AndroidFrameworkFilesystemPort,
    AndroidShizukuFilesystemPort, ApkCommandProcessPort, ApkCore, ApkNetworkPort, ApkVisualPort,
    GetifaddrsInterfaces, NativeHost, ProcFacts, StartResult, guard, host_slot, io_error, new_uuid,
    read_boot_id, read_start_ticks,
};
use contract::{ErrorCode, RunAs, RuntimeHost, RuntimeReadiness, UuidV4};
use domain::{AdmissionFence, DomainError};
use persistence::{
    CanonicalState, FaultFileStore, FaultRecord, FaultRole, GuardProofDirectory,
    JsonPersistencePort, LifetimeLease, PendingDeadOwnerTakeover, RuntimeArtifactPort, RuntimeLive,
    RuntimeOwner, RuntimeTransitionIntent, StateStore, TransitionRecovery,
    await_guard_recovery_plan,
};
use runtime::{
    AndroidNetworkDefaultEventSource, ApkCapabilityPort, ApkRuntimeVertical, AutomationScheduler,
    BoottimeClock, CapabilityPort, CompositeExecutionSurface, HostControlPort,
    NativeCommandExecutionSurface, NativeFilesystemExecutionSurface, NativeNetworkExecutionSurface,
    NativeVisualExecutionSurface, ProviderToken, RecoveryProof, RuntimeCore, VerticalEnvironment,
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Clone)]
pub(super) struct AppHostControl {
    store: Arc<StateStore>,
    lease: Arc<LifetimeLease>,
    recovery_proof: RecoveryProof,
    capabilities: ApkCapabilityPort,
}

impl HostControlPort for AppHostControl {
    fn cleanup_unverified(
        &self,
        fence: &AdmissionFence,
        _execution_id: &UuidV4,
    ) -> Result<(), DomainError> {
        self.activate(fence)?;
        self.capabilities.withdraw_readiness()
    }

    fn prepare(&self) -> Result<(), DomainError> {
        self.store.validate_lease(&self.lease)
    }

    fn activate(&self, fence: &AdmissionFence) -> Result<(), DomainError> {
        self.store.validate_lease(&self.lease)?;
        let live = self.lease.live();
        if live.runtime_epoch != fence.runtime_epoch
            || live.host_generation != fence.host_generation
            || live.runtime_instance_id != fence.runtime_instance_id
        {
            return Err(DomainError::new(
                ErrorCode::StaleAuthority,
                "Runtime activation fence is stale",
            ));
        }
        Ok(())
    }

    fn recover(&self, _old_instance_id: &UuidV4) -> Result<RecoveryProof, DomainError> {
        self.store.validate_lease(&self.lease)?;
        if self.recovery_proof == RecoveryProof::CleanupUnverified {
            self.capabilities.withdraw_readiness()?;
        }
        Ok(self.recovery_proof)
    }

    fn task_activity_changed(&self, active_tasks: usize, canonical_revision: u64) {
        if let Err(error) = crate::publish_task_activity(
            active_tasks,
            canonical_revision,
            &self.lease.live().runtime_epoch,
        ) {
            eprintln!(
                "DroidBridge task activity projection failed: {:?}",
                error.code
            );
        }
    }
}

pub(super) fn start_host(
    base: PathBuf,
    environment_json: &str,
) -> Result<StartResult, DomainError> {
    let mut slot = host_slot()
        .lock()
        .map_err(|_| DomainError::new(ErrorCode::InternalError, "native host lock failed"))?;
    if let Some(host) = slot.as_ref() {
        return existing_host_result(host);
    }
    let environment: VerticalEnvironment = serde_json::from_str(environment_json)
        .map_err(|_| DomainError::invalid("invalid platform environment"))?;
    fs::create_dir_all(&base).map_err(io_error)?;
    let store = Arc::new(StateStore::new(base.clone()));
    let owner_path = base.join("runtime-owner.json");
    if !owner_path.exists() {
        let owner = RuntimeOwner {
            schema_version: 1,
            runtime_epoch: new_uuid()?,
            host: RuntimeHost::ApkRuntime,
            host_generation: 1,
        };
        store.initialize(&owner, &CanonicalState::default())?;
    }
    FaultFileStore::initialize_all_by_apk(&base)?;
    let owner = store.read_owner()?;
    if owner.host != RuntimeHost::ApkRuntime {
        return Err(DomainError::new(
            ErrorCode::CapabilityUnavailable,
            "APK Runtime is not the authoritative host",
        ));
    }
    let boot_id = read_boot_id()?;
    let runtime_instance_id = new_uuid()?;
    let live = RuntimeLive {
        runtime_epoch: owner.runtime_epoch.clone(),
        host: owner.host,
        host_generation: owner.host_generation,
        runtime_instance_id: runtime_instance_id.clone(),
        boot_id: boot_id.clone(),
        pid: std::process::id(),
        start_ticks: read_start_ticks(Path::new("/proc/self/stat"))?,
    };
    let lease = Arc::new(store.acquire_lifetime(live)?);
    activate_app_host(
        &mut slot,
        base,
        environment,
        store,
        owner,
        lease,
        boot_id,
        runtime_instance_id,
        None,
    )
}

pub(super) fn recover_dead_magisk_host(
    base: PathBuf,
    environment_json: &str,
) -> Result<StartResult, DomainError> {
    let mut slot = host_slot()
        .lock()
        .map_err(|_| DomainError::new(ErrorCode::InternalError, "native host lock failed"))?;
    if let Some(host) = slot.as_ref() {
        return existing_host_result(host);
    }
    let environment: VerticalEnvironment = serde_json::from_str(environment_json)
        .map_err(|_| DomainError::invalid("invalid platform environment"))?;
    let store = Arc::new(StateStore::new(base.clone()));
    let owner = store.read_owner()?;
    let observed_transition = store.observe_transition()?;
    let (intent, resume) = match (owner.host, observed_transition) {
        (RuntimeHost::MagiskBackend, None) => {
            let previous_live = read_previous_live(&base)?.ok_or_else(|| {
                DomainError::new(ErrorCode::IoError, "dead Magisk owner has no live identity")
            })?;
            let target_generation = owner.host_generation.checked_add(1).ok_or_else(|| {
                DomainError::new(ErrorCode::ResourceLimit, "host generation exhausted")
            })?;
            (
                RuntimeTransitionIntent {
                    schema_version: 1,
                    transition_id: new_uuid()?,
                    runtime_epoch: owner.runtime_epoch.clone(),
                    from_host: owner.host,
                    from_generation: owner.host_generation,
                    from_instance_id: previous_live.runtime_instance_id,
                    target_host: RuntimeHost::ApkRuntime,
                    target_generation,
                },
                false,
            )
        }
        (
            RuntimeHost::MagiskBackend,
            Some((TransitionRecovery::RemoveUncommittedIntent, intent)),
        ) if intent.from_host == RuntimeHost::MagiskBackend
            && intent.target_host == RuntimeHost::ApkRuntime =>
        {
            (intent, false)
        }
        (RuntimeHost::ApkRuntime, Some((TransitionRecovery::ActivateCommittedTarget, intent)))
            if intent.from_host == RuntimeHost::MagiskBackend
                && intent.target_host == RuntimeHost::ApkRuntime =>
        {
            (intent, true)
        }
        _ => {
            return Err(DomainError::new(
                ErrorCode::StaleAuthority,
                "dead Magisk takeover state is not recoverable",
            ));
        }
    };
    let boot_id = read_boot_id()?;
    let runtime_instance_id = new_uuid()?;
    let target_live = RuntimeLive {
        runtime_epoch: intent.runtime_epoch.clone(),
        host: RuntimeHost::ApkRuntime,
        host_generation: intent.target_generation,
        runtime_instance_id: runtime_instance_id.clone(),
        boot_id: boot_id.clone(),
        pid: std::process::id(),
        start_ticks: read_start_ticks(Path::new("/proc/self/stat"))?,
    };
    let pending = if resume {
        store.resume_dead_owner_takeover(&intent, target_live, &boot_id, &ProcFacts)?
    } else {
        store.begin_dead_owner_takeover(&intent, target_live, &boot_id, &ProcFacts)?
    };
    let lease = Arc::clone(pending.lease());
    FaultFileStore::initialize_all_by_apk(&base)?;
    let target_owner = store.read_owner()?;
    activate_app_host(
        &mut slot,
        base,
        environment,
        store,
        target_owner,
        lease,
        boot_id,
        runtime_instance_id,
        Some(pending),
    )
}

#[allow(clippy::too_many_arguments)]
fn activate_app_host(
    slot: &mut Option<Arc<NativeHost>>,
    base: PathBuf,
    mut environment: VerticalEnvironment,
    store: Arc<StateStore>,
    owner: RuntimeOwner,
    lease: Arc<LifetimeLease>,
    boot_id: UuidV4,
    runtime_instance_id: UuidV4,
    takeover: Option<PendingDeadOwnerTakeover>,
) -> Result<StartResult, DomainError> {
    let recovery = if let Some(pending) = takeover.as_ref() {
        pending.recovery_plan().clone()
    } else {
        let state = store.load(&lease)?;
        await_guard_recovery_plan(
            &state,
            &runtime_instance_id,
            &boot_id,
            &GuardProofDirectory::new(&base),
            &ProcFacts,
        )?
    };
    environment.runtime_epoch = owner.runtime_epoch.clone();
    environment.host_generation = owner.host_generation;
    let product_version = environment.version_name.clone();
    let runtime = ApkRuntimeVertical::new(environment)?;
    guard::publish_scope(guard::GuardScope::new(
        base.clone(),
        owner.runtime_epoch.clone(),
        runtime_instance_id.clone(),
    )?)?;
    let capabilities = runtime.capability_port(runtime_instance_id.clone());
    let host_control = AppHostControl {
        store: Arc::clone(&store),
        lease: Arc::clone(&lease),
        capabilities: capabilities.clone(),
        recovery_proof: RecoveryProof::Clean,
    };
    host_control.prepare()?;
    host_control.activate(&capabilities.current()?.fence)?;
    let artifacts = RuntimeArtifactPort::new(Arc::clone(&store), Arc::clone(&lease));
    let executions = CompositeExecutionSurface::new(
        NativeFilesystemExecutionSurface::new(
            base.clone(),
            artifacts.clone(),
            capabilities.clone(),
            ProviderToken::AppNative,
        )
        .with_framework(AndroidFrameworkFilesystemPort::new(
            AndroidFrameworkFilesystemDispatcher,
        ))
        .with_primitives(AndroidShizukuFilesystemPort),
    )
    .with_command(NativeCommandExecutionSurface::new(
        artifacts.clone(),
        capabilities.clone(),
        &[RunAs::App, RunAs::Shell],
        ApkCommandProcessPort,
    ))
    .with_network(NativeNetworkExecutionSurface::new(
        artifacts.clone(),
        capabilities.clone(),
        ApkNetworkPort::new(
            AndroidFrameworkFilesystemDispatcher,
            GetifaddrsInterfaces,
            AndroidShizukuFilesystemPort,
        ),
    ))
    .with_visual(
        NativeVisualExecutionSurface::new(artifacts.clone(), capabilities.clone())
            .with_primitives(ApkVisualPort::new(base.clone(), capabilities.clone())),
    )
    .with_android(
        runtime::NativeAndroidExecutionSurface::new(
            capabilities.clone(),
            crate::android::running_package()?,
        )
        .with_primitives(crate::android::ApkAndroidPort),
    );
    let network_event_source = Arc::new(AndroidNetworkDefaultEventSource::new(
        AndroidFrameworkFilesystemDispatcher,
        capabilities.clone(),
    ));
    let core = RuntimeCore::new(
        JsonPersistencePort::new(Arc::clone(&store), Arc::clone(&lease)),
        artifacts.clone(),
        executions,
        capabilities,
        host_control,
    )
    .with_network_default_event_source(network_event_source);
    let async_runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .map_err(|_| DomainError::new(ErrorCode::InternalError, "Runtime executor failed"))?;
    let verification = reconcile_app_recovery(
        &base,
        &store,
        &lease,
        &runtime,
        &core,
        &async_runtime,
        &recovery,
    )?;
    let lease = match takeover {
        Some(pending) => verification.complete_takeover(&store, pending, &ProcFacts)?,
        None => lease,
    };
    let committed = store.load(&lease)?;
    let active_tasks = committed
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
    crate::publish_task_activity(
        active_tasks,
        committed.store_revision,
        &lease.live().runtime_epoch,
    )?;
    publish_ready_host(
        slot,
        base,
        store,
        lease,
        runtime,
        core,
        artifacts,
        async_runtime,
        boot_id,
        runtime_instance_id,
        product_version,
        owner,
        verification,
    )
}

#[allow(clippy::too_many_arguments)]
fn publish_ready_host(
    slot: &mut Option<Arc<NativeHost>>,
    base: PathBuf,
    store: Arc<StateStore>,
    lease: Arc<LifetimeLease>,
    runtime: ApkRuntimeVertical,
    core: ApkCore,
    artifacts: RuntimeArtifactPort,
    async_runtime: tokio::runtime::Runtime,
    boot_id: UuidV4,
    runtime_instance_id: UuidV4,
    product_version: String,
    owner: RuntimeOwner,
    _cleanup: AppCleanupVerification,
) -> Result<StartResult, DomainError> {
    store.validate_lease(&lease)?;
    let automation_wake = Arc::new(ApkAlarmWake::new(
        AndroidFrameworkFilesystemDispatcher,
        runtime.capability_port(runtime_instance_id.clone()),
    ));
    spawn_automation_scheduler(
        &async_runtime,
        core.clone(),
        Arc::clone(&automation_wake),
        AutomationFaultContext {
            base: base.clone(),
            product_version: product_version.clone(),
            boot_id: boot_id.clone(),
            runtime_instance_id: runtime_instance_id.clone(),
        },
    );
    // A durable S-UPD-002 maintenance record keeps business admission closed across restarts.
    let admission_open = !base.join(crate::UPDATE_MAINTENANCE_RECORD).exists();
    *slot = Some(Arc::new(NativeHost {
        base,
        store,
        _lease: lease,
        runtime,
        core,
        artifacts,
        async_runtime,
        boot_id,
        runtime_instance_id: runtime_instance_id.clone(),
        product_version,
        admission_open: AtomicBool::new(admission_open),
        automation_wake,
    }));
    Ok(StartResult {
        ready: true,
        runtime_epoch: owner.runtime_epoch,
        host_generation: owner.host_generation,
        runtime_instance_id,
    })
}

fn existing_host_result(host: &NativeHost) -> Result<StartResult, DomainError> {
    host.store.validate_lease(&host._lease)?;
    let capability = host
        .runtime
        .capability_port(host.runtime_instance_id.clone())
        .current()?;
    if crate::guard::is_quarantined()
        || !host.admission_open.load(Ordering::SeqCst)
        || capability.context.readiness != RuntimeReadiness::Ready
    {
        return Err(DomainError::new(
            ErrorCode::CapabilityUnavailable,
            "APK Runtime is not ready",
        ));
    }
    Ok(start_result(host._lease.live()))
}

fn start_result(live: &RuntimeLive) -> StartResult {
    StartResult {
        ready: true,
        runtime_epoch: live.runtime_epoch.clone(),
        host_generation: live.host_generation,
        runtime_instance_id: live.runtime_instance_id.clone(),
    }
}

/// The host identity a scheduler fault is recorded under.
pub(super) struct AutomationFaultContext {
    pub(super) base: PathBuf,
    pub(super) product_version: String,
    pub(super) boot_id: UuidV4,
    pub(super) runtime_instance_id: UuidV4,
}

/// Starts this APK Runtime instance's resident Automation scheduler on its reactor (S-LIFE-003,
/// S-AUTO-001): it publishes `runtime.ready` once, then projects persisted dues onto the single
/// exact alarm. The task ends with the reactor when the host stops; a fault that ends it earlier
/// is appended to the host fault file rather than dropped.
fn spawn_automation_scheduler(
    async_runtime: &tokio::runtime::Runtime,
    core: ApkCore,
    wake: Arc<crate::ApkAutomationWake>,
    fault: AutomationFaultContext,
) {
    let scheduler = AutomationScheduler::new(core, Arc::new(BoottimeClock));
    async_runtime.spawn(async move {
        let ended = async {
            scheduler.publish_runtime_ready().await?;
            scheduler.run(wake.as_ref()).await
        }
        .await;
        if let Err(error) = ended {
            // The fault file is the last channel a detached scheduler has; if it cannot be
            // written either, the stopped scheduler still leaves persisted dues unchanged.
            let _recorded = record_scheduler_fault(&fault, &error);
        }
    });
}

fn record_scheduler_fault(
    fault: &AutomationFaultContext,
    error: &DomainError,
) -> Result<(), DomainError> {
    let now = chrono::Utc::now();
    let now_ms = u64::try_from(now.timestamp_millis())
        .map_err(|_| DomainError::new(ErrorCode::InternalError, "clock is before epoch"))?;
    FaultFileStore::new(&fault.base, FaultRole::Host).append(
        FaultRecord {
            record_id: new_uuid()?,
            at: now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            component: "automation_scheduler".to_owned(),
            code: crate::error_code_token(error.code).to_owned(),
            phase: "automation_scheduler_run".to_owned(),
            product_version: fault.product_version.clone(),
            boot_id: fault.boot_id.clone(),
            runtime_instance_id: Some(fault.runtime_instance_id.clone()),
            execution_id: None,
            exit_code: None,
            signal: None,
            repeat_count: 1,
        },
        now_ms,
    )?;
    Ok(())
}

fn read_previous_live(base: &Path) -> Result<Option<RuntimeLive>, DomainError> {
    let path = base.join("runtime-live.json");
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| DomainError::new(ErrorCode::IoError, "runtime live record is invalid")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_error(error)),
    }
}
