//! Admission driver for one active Runtime instance (S-LIFE-003, S-AUTO-001): host wakes admit
//! persisted dues, the ready host generation publishes `runtime.ready` once, and default-network
//! changes admit `network.default_changed` Automations. Each admitted execution runs on its own
//! task; its settlement, not this driver, is the canonical outcome.

use crate::{
    AdmittedAutomationExecution, ArtifactPort, AutomationAdmission, AutomationClock,
    CapabilityPort, ExecutionPort, FilesystemPreflightPort, HostControlPort,
    NETWORK_DEFAULT_CHANGED_EVENT, NetworkDefaultChangedEvent, NetworkDefaultSubscription,
    PersistencePort, PortFuture, RUNTIME_READY_EVENT, RuntimeCore,
};
use chrono::DateTime;
use contract::{AutomationId, ErrorCode, ScalarValue};
use domain::DomainError;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

/// One earliest persisted due as a host wake projection arms it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AutomationWakeDue {
    pub instant: String,
    pub unix_millis: i64,
}

/// A host's single time-wake projection of the persisted due truth: the APK exact alarm or the
/// daemon's CLOCK_REALTIME_ALARM timerfd (S-LIFE-003). It copies no schedule.
pub trait AutomationWakeProjection: Send + Sync {
    /// Arms the single wake at `due`, replacing an earlier arm, or disarms it for `None`. Returns
    /// `false` when the host's explicitly unavailable wake capability left it unapplied, so the
    /// same due is applied again after the next canonical change.
    fn arm(&self, due: Option<&AutomationWakeDue>) -> Result<bool, DomainError>;

    /// Resolves once the armed wake fired or the host wall clock was set. Dropping the future
    /// before it resolves must lose no delivery.
    fn wait<'a>(&'a self) -> PortFuture<'a, Result<(), DomainError>>;
}

pub struct AutomationScheduler<P, A, E, C, H, K> {
    core: RuntimeCore<P, A, E, C, H>,
    clock: Arc<K>,
    runtime_ready_published: AtomicBool,
    busy_dropped: AtomicU64,
    rejected: AtomicU64,
    network_events_unavailable: StdMutex<Option<DomainError>>,
}

impl<P, A, E, C, H, K> AutomationScheduler<P, A, E, C, H, K>
where
    P: PersistencePort + 'static,
    A: ArtifactPort + Clone + 'static,
    E: ExecutionPort + FilesystemPreflightPort + 'static,
    C: CapabilityPort + 'static,
    H: HostControlPort + 'static,
    K: AutomationClock + 'static,
{
    pub fn new(core: RuntimeCore<P, A, E, C, H>, clock: Arc<K>) -> Self {
        Self {
            core,
            clock,
            runtime_ready_published: AtomicBool::new(false),
            busy_dropped: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            network_events_unavailable: StdMutex::new(None),
        }
    }

    /// Admits every persisted due at the current wall time, starts each admitted execution, and
    /// returns the next due the host wake projection must arm.
    pub async fn wake(&self) -> Result<Option<String>, DomainError> {
        let (timestamp, _) = self.clock.wall()?;
        let mut blocked: Vec<AutomationId> = Vec::new();
        for automation_id in self.core.due_automation_ids(&timestamp).await? {
            let admission = self
                .core
                .admit_due_automation(&automation_id, timestamp.clone())
                .await?;
            if matches!(admission, AutomationAdmission::Rejected(_)) {
                blocked.push(automation_id);
            }
            self.start(admission);
        }
        self.core
            .earliest_automation_due_excluding(&timestamp, &blocked)
            .await
    }

    /// The host's resident scheduling loop. Each pass admits dues, re-arms the projection only
    /// when the next due changed, and keeps the sole default-network subscription exactly while
    /// an enabled network Automation requires it; it then waits for the wake, a canonical change
    /// or a network event. No pass is periodic. It returns only on a fault, which the host owns.
    pub async fn run<W: AutomationWakeProjection>(
        &self,
        projection: &W,
    ) -> Result<(), DomainError> {
        self.run_loop(Some(projection)).await
    }

    /// The resident loop of a host without a time-wake capability: persisted dues stay unchanged
    /// and time-trigger admission is unavailable, while event Automations still run (S-LIFE-003).
    pub async fn run_events_only(&self) -> Result<(), DomainError> {
        self.run_loop(None::<&UnarmedWake>).await
    }

    async fn run_loop<W: AutomationWakeProjection>(
        &self,
        projection: Option<&W>,
    ) -> Result<(), DomainError> {
        let changes = self.core.canonical_changes();
        let mut armed: Option<Option<String>> = None;
        let mut network: Option<NetworkDefaultSubscription> = None;
        loop {
            if let Some(projection) = projection {
                let next = self.wake().await?;
                if armed.as_ref() != Some(&next) {
                    let due = next.as_deref().map(wake_due).transpose()?;
                    armed = projection.arm(due.as_ref())?.then_some(next);
                }
            }
            self.reconcile_network_subscription(&mut network).await?;
            tokio::select! {
                fired = wait_for_wake(projection) => {
                    fired?;
                    // A fired or clock-cancelled wake is re-armed from canonical truth.
                    armed = None;
                }
                () = changes.notified() => {}
                event = next_network_event(&mut network) => match event {
                    Some(event) => {
                        self.observe_network_default_changed(event).await?;
                    }
                    // The plane invalidated this subscription; the next pass resubscribes.
                    None => network = None,
                },
            }
        }
    }

    /// Publishes `runtime.ready:{host,host_generation}` once for this ready Runtime instance.
    pub async fn publish_runtime_ready(&self) -> Result<usize, DomainError> {
        if self.runtime_ready_published.swap(true, Ordering::AcqRel) {
            return Ok(0);
        }
        let capability = self.core.capability_snapshot()?;
        let host = match serde_json::to_value(capability.context.host) {
            Ok(serde_json::Value::String(host)) => host,
            _ => {
                return Err(DomainError::new(
                    ErrorCode::InternalError,
                    "Runtime host has no wire token",
                ));
            }
        };
        let generation = i64::try_from(capability.fence.host_generation).map_err(|_| {
            DomainError::new(ErrorCode::InternalError, "host generation is out of range")
        })?;
        let facts = BTreeMap::from([
            ("host".to_owned(), ScalarValue::String(host)),
            (
                "host_generation".to_owned(),
                ScalarValue::Integer(generation),
            ),
        ]);
        self.admit_event(RUNTIME_READY_EVENT, facts).await
    }

    /// Admits `network.default_changed` Automations for one delivered default-network change.
    pub async fn observe_network_default_changed(
        &self,
        event: NetworkDefaultChangedEvent,
    ) -> Result<usize, DomainError> {
        let mut facts = BTreeMap::new();
        if let Some(network_id) = event.network_id {
            facts.insert("network_id".to_owned(), ScalarValue::String(network_id));
        }
        if let Some(transport) = event.transport {
            facts.insert("transport".to_owned(), ScalarValue::String(transport));
        }
        self.admit_event(NETWORK_DEFAULT_CHANGED_EVENT, facts).await
    }

    /// Event arrivals dropped because their Automation was busy, aggregated into one bounded count.
    pub fn busy_dropped(&self) -> u64 {
        self.busy_dropped.load(Ordering::Acquire)
    }

    /// Admissions rejected for capacity or readiness, aggregated into one bounded count.
    pub fn rejected(&self) -> u64 {
        self.rejected.load(Ordering::Acquire)
    }

    /// The explicit reason the required default-network subscription is not active, if any
    /// (S-AUTO-001); an enabled network Automation never waits on a silently missing source.
    pub fn network_events_unavailable(&self) -> Option<DomainError> {
        self.network_events_unavailable.lock().map_or_else(
            |poisoned| poisoned.into_inner().clone(),
            |guard| guard.clone(),
        )
    }

    async fn admit_event(
        &self,
        event: &str,
        facts: BTreeMap<String, ScalarValue>,
    ) -> Result<usize, DomainError> {
        let (timestamp, _) = self.clock.wall()?;
        let mut admitted = 0;
        for admission in self
            .core
            .admit_event_automations(event, facts, timestamp)
            .await?
        {
            if matches!(admission, AutomationAdmission::Admitted(_)) {
                admitted += 1;
            }
            self.start(admission);
        }
        Ok(admitted)
    }

    async fn reconcile_network_subscription(
        &self,
        network: &mut Option<NetworkDefaultSubscription>,
    ) -> Result<(), DomainError> {
        let required = self.core.requires_network_default_events().await?;
        if required && network.is_none() {
            let subscribed = self
                .core
                .network_default_event_plane()
                .retry_cleanup()
                .and_then(|()| self.core.subscribe_network_default_events());
            match subscribed {
                Ok(subscription) => {
                    *network = Some(subscription);
                    self.set_network_events_unavailable(None);
                }
                // Retried on the next canonical change or host source notification.
                Err(error) => self.set_network_events_unavailable(Some(error)),
            }
        } else if !required {
            if let Some(mut subscription) = network.take()
                && let Err(error) = subscription.close()
            {
                // The plane quarantines the failed cleanup; the next subscribe retries it first.
                self.set_network_events_unavailable(Some(error));
                return Ok(());
            }
            self.set_network_events_unavailable(None);
        }
        Ok(())
    }

    fn set_network_events_unavailable(&self, error: Option<DomainError>) {
        match self.network_events_unavailable.lock() {
            Ok(mut guard) => *guard = error,
            Err(poisoned) => *poisoned.into_inner() = error,
        }
    }

    fn start(&self, admission: AutomationAdmission) {
        match admission {
            AutomationAdmission::Admitted(execution) => self.spawn(*execution),
            AutomationAdmission::BusyDropped => {
                self.busy_dropped.fetch_add(1, Ordering::AcqRel);
            }
            AutomationAdmission::Rejected(_) => {
                self.rejected.fetch_add(1, Ordering::AcqRel);
            }
            AutomationAdmission::NotDue => {}
        }
    }

    fn spawn(&self, execution: AdmittedAutomationExecution) {
        let core = self.core.clone();
        let clock = Arc::clone(&self.clock);
        tokio::spawn(async move {
            // The execution settles itself in the canonical store; a run that cannot settle is
            // left non-terminal for the next activation's recovery to interrupt (S-PERSIST-005).
            let _ = core
                .run_automation_execution(execution, clock.as_ref())
                .await;
        });
    }
}

/// The type parameter of a loop that has no time-wake projection; it is never constructed.
enum UnarmedWake {}

impl AutomationWakeProjection for UnarmedWake {
    fn arm(&self, _due: Option<&AutomationWakeDue>) -> Result<bool, DomainError> {
        match *self {}
    }

    fn wait<'a>(&'a self) -> PortFuture<'a, Result<(), DomainError>> {
        match *self {}
    }
}

async fn wait_for_wake<W: AutomationWakeProjection>(
    projection: Option<&W>,
) -> Result<(), DomainError> {
    match projection {
        Some(projection) => projection.wait().await,
        None => std::future::pending().await,
    }
}

async fn next_network_event(
    network: &mut Option<NetworkDefaultSubscription>,
) -> Option<NetworkDefaultChangedEvent> {
    match network {
        Some(subscription) => subscription.recv().await,
        None => std::future::pending().await,
    }
}

fn wake_due(instant: &str) -> Result<AutomationWakeDue, DomainError> {
    let parsed = DateTime::parse_from_rfc3339(instant)
        .map_err(|_| DomainError::new(ErrorCode::IoError, "persisted Automation due is invalid"))?;
    Ok(AutomationWakeDue {
        instant: instant.to_owned(),
        unix_millis: parsed.timestamp_millis(),
    })
}
