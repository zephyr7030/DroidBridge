//! I9 admission driver gates: one persisted due for every host wake, the two registered events,
//! busy-drop semantics and the once-per-instance runtime.ready publication.

use chrono::{DateTime, SecondsFormat, TimeZone, Utc};
use contract::{
    AutomationExecutionState, Availability, CapabilityState, ErrorCode, GrantFacts, RuntimeHost,
    RuntimeReadiness, ScalarValue, UuidV4,
};
use domain::{AdmissionFence, CapabilityContext, DomainError, ProviderGenerations, ResolverFacts};
use runtime::{
    AutomationAdmission, AutomationClock, AutomationScheduler, AutomationWakeDue,
    AutomationWakeProjection, CapabilitySnapshot, NetworkDefaultChangedEvent,
    NetworkDefaultEventIngress, NetworkDefaultEventSource, NetworkDefaultSourceRegistration,
    NetworkEventDelivery, PortFuture, RecoveryProof, RuntimeCore,
    fakes::{FakeArtifacts, FakeCapabilities, FakeExecutions, FakeHostControl, FakePersistence},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;

type TestCore =
    RuntimeCore<FakePersistence, FakeArtifacts, FakeExecutions, FakeCapabilities, FakeHostControl>;

const SAVED: &str = "2026-09-14T08:00:00.000Z";

fn uuid(value: u64) -> UuidV4 {
    UuidV4::parse(format!("99400000-0000-4000-8000-{value:012x}")).unwrap()
}

fn available() -> Availability {
    Availability {
        state: CapabilityState::Available,
        reason: None,
    }
}

fn capability() -> CapabilitySnapshot {
    let state = CapabilityState::Available;
    CapabilitySnapshot {
        grants: GrantFacts {
            android_local_network: available(),
            android_notifications: available(),
            android_notification_listener: available(),
            automation_exact_alarm: available(),
            visual_accessibility: available(),
            visual_media_projection_session: available(),
            shizuku_shell: available(),
            magisk_module: available(),
            magisk_root: available(),
            magisk_framework: available(),
            magisk_launch: available(),
            magisk_clipboard: available(),
            magisk_notifications: available(),
            magisk_wake_alarm: available(),
            execution_app_guard: available(),
            execution_shell_guard: available(),
            execution_root_guard: available(),
        },
        context: CapabilityContext {
            sdk_int: 37,
            host: RuntimeHost::ApkRuntime,
            readiness: RuntimeReadiness::Ready,
            app_execution_surface: state,
        },
        resolver_facts: ResolverFacts {
            app_native: state,
            app_framework: state,
            shizuku: state,
            magisk_native: state,
            magisk_framework: state,
            magisk_launch: state,
            magisk_clipboard: state,
            magisk_notifications: state,
            accessibility: state,
            media_projection: state,
            notification_listener: state,
            generations: ProviderGenerations {
                app_native: 1,
                app_framework: 1,
                shizuku: 1,
                magisk_native: 1,
                magisk_framework: 1,
                accessibility: 1,
                media_projection: 1,
                notification_listener: 1,
            },
        },
        fence: AdmissionFence {
            runtime_epoch: uuid(1),
            host_generation: 3,
            runtime_instance_id: uuid(2),
        },
    }
}

fn make_core() -> (TestCore, FakePersistence) {
    let (core, persistence, _) = make_core_with(None);
    (core, persistence)
}

fn make_core_with(
    network_source: Option<Arc<dyn NetworkDefaultEventSource>>,
) -> (TestCore, FakePersistence, FakeCapabilities) {
    let persistence = FakePersistence::default();
    let capabilities = FakeCapabilities::new(capability());
    let mut core = RuntimeCore::new(
        persistence.clone(),
        FakeArtifacts::default(),
        FakeExecutions::default(),
        capabilities.clone(),
        FakeHostControl::new(RecoveryProof::Clean).with_capabilities(capabilities.clone()),
    );
    if let Some(source) = network_source {
        core = core.with_network_default_event_source(source);
    }
    (core, persistence, capabilities)
}

fn millis(timestamp: &str) -> u64 {
    u64::try_from(
        DateTime::parse_from_rfc3339(timestamp)
            .unwrap()
            .timestamp_millis(),
    )
    .unwrap()
}

fn at_minutes(minutes: i64) -> String {
    (DateTime::parse_from_rfc3339(SAVED)
        .unwrap()
        .with_timezone(&Utc)
        + chrono::Duration::minutes(minutes))
    .to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// A wall clock the test moves explicitly; boot time follows it and sleeps complete at once.
struct ManualClock {
    wall_ms: AtomicU64,
}

impl ManualClock {
    fn at(timestamp: &str) -> Arc<Self> {
        Arc::new(Self {
            wall_ms: AtomicU64::new(millis(timestamp)),
        })
    }

    fn set(&self, timestamp: &str) {
        self.wall_ms.store(millis(timestamp), Ordering::SeqCst);
    }
}

impl AutomationClock for ManualClock {
    fn boot_millis(&self) -> Result<u64, DomainError> {
        Ok(self.wall_ms.load(Ordering::SeqCst))
    }

    fn sleep<'a>(&'a self, duration_ms: u64) -> PortFuture<'a, Result<(), DomainError>> {
        Box::pin(async move {
            tokio::task::yield_now().await;
            self.wall_ms.fetch_add(duration_ms, Ordering::SeqCst);
            Ok(())
        })
    }

    fn wall(&self) -> Result<(String, u64), DomainError> {
        let millis = self.wall_ms.load(Ordering::SeqCst);
        let instant = Utc
            .timestamp_millis_opt(i64::try_from(millis).unwrap())
            .unwrap();
        Ok((instant.to_rfc3339_opts(SecondsFormat::Millis, true), millis))
    }
}

async fn save(
    core: &TestCore,
    request_id: u64,
    name: &str,
    enabled: bool,
    trigger: Value,
) -> UuidV4 {
    let response = mutate(
        core,
        request_id,
        "save",
        json!({
            "name": name,
            "enabled": enabled,
            "trigger": trigger,
            "action": {"type": "delay", "duration_ms": 1},
        }),
    )
    .await;
    UuidV4::parse(
        response["result"]["automation_id"]
            .as_str()
            .unwrap()
            .to_owned(),
    )
    .unwrap()
}

async fn disable(core: &TestCore, request_id: u64, automation_id: &UuidV4) {
    mutate(
        core,
        request_id,
        "set_enabled",
        json!({"automation_id": automation_id, "enabled": false, "expected_revision": 1}),
    )
    .await;
}

async fn mutate(core: &TestCore, request_id: u64, action: &str, input: Value) -> Value {
    let request = json!({
        "protocol_version": 1,
        "request_id": uuid(0x1000 + request_id),
        "payload": {"tool": "automation", "action": action, "input": input},
    });
    let response: Value = serde_json::from_slice(
        &runtime::submit_public(
            core,
            &serde_json::to_vec(&request).unwrap(),
            SAVED.to_owned(),
            millis(SAVED),
            true,
            |_| async { panic!("automation escaped the shared Runtime ingress") },
        )
        .await,
    )
    .unwrap();
    assert_eq!(response["outcome"], "success", "{response}");
    response
}

async fn settled(persistence: &FakePersistence, executions: usize) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let stored = persistence.snapshot();
            if stored.automation_executions.len() >= executions
                && stored
                    .automation_executions
                    .iter()
                    .all(|execution| execution.summary.state == AutomationExecutionState::Completed)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("admitted executions settle");
}

#[tokio::test]
async fn i9_g02_every_host_wake_projects_the_same_persisted_due() {
    let (core, persistence) = make_core();
    let minutely = save(
        &core,
        1,
        "minutely",
        true,
        json!({"type": "interval", "every_ms": 60_000}),
    )
    .await;
    save(
        &core,
        2,
        "hourly",
        true,
        json!({"type": "interval", "every_ms": 3_600_000}),
    )
    .await;
    save(
        &core,
        3,
        "disabled",
        false,
        json!({"type": "interval", "every_ms": 60_000}),
    )
    .await;
    save(
        &core,
        4,
        "event",
        true,
        json!({"type": "event", "name": "runtime.ready"}),
    )
    .await;

    assert_eq!(
        core.earliest_automation_due().await.unwrap(),
        Some(at_minutes(1))
    );
    assert_eq!(
        core.due_automation_ids(&at_minutes(0)).await.unwrap(),
        vec![]
    );
    assert_eq!(
        core.due_automation_ids(&at_minutes(1)).await.unwrap(),
        vec![minutely.clone()]
    );

    // A busy time trigger leaves the earliest-arm candidate set until it settles.
    let AutomationAdmission::Admitted(execution) = core
        .admit_due_automation(&minutely, at_minutes(1))
        .await
        .unwrap()
    else {
        panic!("the minutely due is admitted");
    };
    assert_eq!(
        core.earliest_automation_due().await.unwrap(),
        Some(at_minutes(60))
    );
    assert!(
        core.due_automation_ids(&at_minutes(59))
            .await
            .unwrap()
            .is_empty()
    );

    let clock = ManualClock::at(&at_minutes(1));
    core.run_automation_execution(*execution, clock.as_ref())
        .await
        .unwrap();
    assert_eq!(
        persistence.snapshot().automations[0].next_due_at.as_deref(),
        Some(at_minutes(2).as_str())
    );
    assert_eq!(
        core.earliest_automation_due().await.unwrap(),
        Some(at_minutes(2))
    );
}

#[tokio::test]
async fn i9_g02_scheduler_wake_admits_dues_and_returns_the_next_arm() {
    let (core, persistence) = make_core();
    save(
        &core,
        1,
        "minutely",
        true,
        json!({"type": "interval", "every_ms": 60_000}),
    )
    .await;
    let clock = ManualClock::at(&at_minutes(0));
    let scheduler = AutomationScheduler::new(core.clone(), Arc::clone(&clock));

    assert_eq!(scheduler.wake().await.unwrap(), Some(at_minutes(1)));
    assert!(persistence.snapshot().automation_executions.is_empty());

    // Three missed minutes admit one execution and arm the next boundary after the wake.
    clock.set(&at_minutes(3));
    let next = scheduler.wake().await.unwrap();
    // While the admitted execution runs its Automation is not armed; once it settles the next
    // boundary after the wake is.
    assert!(
        next.is_none() || next.as_deref() == Some(at_minutes(4).as_str()),
        "{next:?}"
    );
    settled(&persistence, 1).await;
    assert_eq!(persistence.snapshot().automation_executions.len(), 1);
    assert_eq!(
        core.earliest_automation_due().await.unwrap(),
        Some(at_minutes(4))
    );
}

#[tokio::test]
async fn i9_g03_only_the_two_registered_events_admit_exact_matches() {
    let (core, persistence) = make_core();
    let ready = save(
        &core,
        1,
        "ready",
        true,
        json!({"type": "event", "name": "runtime.ready", "match": {"host": "apk_runtime", "host_generation": 3}}),
    )
    .await;
    let typed = save(
        &core,
        2,
        "string generation",
        true,
        json!({"type": "event", "name": "runtime.ready", "match": {"host_generation": "3"}}),
    )
    .await;
    let wifi = save(
        &core,
        3,
        "wifi",
        true,
        json!({"type": "event", "name": "network.default_changed", "match": {"transport": "wifi"}}),
    )
    .await;

    let unregistered = core
        .admit_event_automations("visual.window_changed", BTreeMap::new(), at_minutes(1))
        .await
        .unwrap_err();
    assert_eq!(unregistered.code, ErrorCode::InvalidArgument);

    let clock = ManualClock::at(&at_minutes(1));
    let scheduler = AutomationScheduler::new(core.clone(), Arc::clone(&clock));
    // Integer generation 3 never matches the string "3".
    assert_eq!(scheduler.publish_runtime_ready().await.unwrap(), 1);
    assert_eq!(scheduler.publish_runtime_ready().await.unwrap(), 0);

    let cellular =
        NetworkDefaultChangedEvent::new(Some("net-1".to_owned()), Some("cellular".to_owned()));
    assert_eq!(
        scheduler
            .observe_network_default_changed(cellular)
            .await
            .unwrap(),
        0
    );
    let wifi_event =
        NetworkDefaultChangedEvent::new(Some("net-2".to_owned()), Some("wifi".to_owned()));
    assert_eq!(
        scheduler
            .observe_network_default_changed(wifi_event)
            .await
            .unwrap(),
        1
    );
    settled(&persistence, 2).await;

    let stored = persistence.snapshot();
    let owners = stored
        .automation_executions
        .iter()
        .map(|execution| execution.automation_id.clone())
        .collect::<Vec<_>>();
    assert!(owners.contains(&ready) && owners.contains(&wifi));
    assert!(!owners.contains(&typed));
}

#[tokio::test]
async fn i9_g08_busy_event_arrivals_are_dropped_not_queued() {
    let (core, persistence) = make_core();
    let wifi = save(
        &core,
        1,
        "wifi",
        true,
        json!({"type": "event", "name": "network.default_changed"}),
    )
    .await;
    let facts = BTreeMap::from([(
        "transport".to_owned(),
        ScalarValue::String("wifi".to_owned()),
    )]);
    let first = core
        .admit_event_automations("network.default_changed", facts.clone(), at_minutes(1))
        .await
        .unwrap();
    assert!(matches!(
        first.as_slice(),
        [AutomationAdmission::Admitted(_)]
    ));

    let scheduler = AutomationScheduler::new(core.clone(), ManualClock::at(&at_minutes(2)));
    let busy = NetworkDefaultChangedEvent::new(None, Some("wifi".to_owned()));
    assert_eq!(
        scheduler
            .observe_network_default_changed(busy)
            .await
            .unwrap(),
        0
    );
    assert_eq!(scheduler.busy_dropped(), 1);
    let stored = persistence.snapshot();
    assert_eq!(stored.automation_executions.len(), 1);
    assert_eq!(stored.automation_executions[0].automation_id, wifi);
}

/// A host wake projection that records every arm and fires only when the test says so.
#[derive(Default)]
struct RecordingProjection {
    arms: Mutex<Vec<Option<AutomationWakeDue>>>,
    fired: Notify,
}

impl RecordingProjection {
    fn arms(&self) -> Vec<Option<String>> {
        self.arms
            .lock()
            .unwrap()
            .iter()
            .map(|due| due.as_ref().map(|due| due.instant.clone()))
            .collect()
    }

    fn last_arm(&self) -> Option<Option<String>> {
        self.arms().last().cloned()
    }

    /// The host timer expired, or the kernel cancelled it because the wall clock was set.
    fn fire(&self) {
        self.fired.notify_one();
    }
}

impl AutomationWakeProjection for RecordingProjection {
    fn arm(&self, due: Option<&AutomationWakeDue>) -> Result<bool, DomainError> {
        if let Some(due) = due {
            assert_eq!(
                due.unix_millis,
                i64::try_from(millis(&due.instant)).unwrap()
            );
        }
        self.arms.lock().unwrap().push(due.cloned());
        Ok(true)
    }

    fn wait<'a>(&'a self) -> PortFuture<'a, Result<(), DomainError>> {
        Box::pin(async move {
            self.fired.notified().await;
            Ok(())
        })
    }
}

#[derive(Default)]
struct RecordingEventSource {
    starts: AtomicU64,
    stops: AtomicU64,
    ingress: Mutex<Option<NetworkDefaultEventIngress>>,
}

impl RecordingEventSource {
    fn emit(&self, event: NetworkDefaultChangedEvent) -> NetworkEventDelivery {
        self.ingress
            .lock()
            .unwrap()
            .as_ref()
            .expect("source is started")
            .observe(event)
            .unwrap()
    }
}

impl NetworkDefaultEventSource for RecordingEventSource {
    fn start(
        &self,
        _registration: &NetworkDefaultSourceRegistration,
        ingress: NetworkDefaultEventIngress,
    ) -> Result<(), DomainError> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        *self.ingress.lock().unwrap() = Some(ingress);
        Ok(())
    }

    fn stop(&self, _registration: &NetworkDefaultSourceRegistration) -> Result<(), DomainError> {
        self.stops.fetch_add(1, Ordering::SeqCst);
        *self.ingress.lock().unwrap() = None;
        Ok(())
    }
}

type TestScheduler = AutomationScheduler<
    FakePersistence,
    FakeArtifacts,
    FakeExecutions,
    FakeCapabilities,
    FakeHostControl,
    ManualClock,
>;

fn run_scheduler(
    scheduler: &Arc<TestScheduler>,
    projection: &Arc<RecordingProjection>,
) -> tokio::task::JoinHandle<()> {
    let scheduler = Arc::clone(scheduler);
    let projection = Arc::clone(projection);
    tokio::spawn(async move {
        let fault = scheduler.run(projection.as_ref()).await;
        panic!("the resident scheduler loop ended: {fault:?}");
    })
}

async fn eventually(what: &str, check: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !check() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

fn completed_executions(persistence: &FakePersistence) -> usize {
    persistence
        .snapshot()
        .automation_executions
        .iter()
        .filter(|execution| execution.summary.state == AutomationExecutionState::Completed)
        .count()
}

#[tokio::test]
async fn i9_g07_suspend_and_wall_clock_changes_reconcile_the_persisted_due() {
    let (core, persistence) = make_core();
    save(
        &core,
        1,
        "minutely",
        true,
        json!({"type": "interval", "every_ms": 60_000}),
    )
    .await;
    let clock = ManualClock::at(&at_minutes(0));
    let scheduler = Arc::new(AutomationScheduler::new(core.clone(), Arc::clone(&clock)));
    let projection = Arc::new(RecordingProjection::default());
    let loop_task = run_scheduler(&scheduler, &projection);
    eventually("the first arm", || {
        projection.last_arm() == Some(Some(at_minutes(1)))
    })
    .await;

    // The device slept through 89 missed minutes: one wake admits one execution without
    // catch-up and re-arms the first boundary after the wall time it woke at.
    clock.set(&at_minutes(90));
    projection.fire();
    eventually("the suspended due to settle and re-arm", || {
        completed_executions(&persistence) == 1
            && projection.last_arm() == Some(Some(at_minutes(91)))
    })
    .await;
    assert_eq!(persistence.snapshot().automation_executions.len(), 1);
    assert_eq!(
        persistence.snapshot().automations[0].next_due_at.as_deref(),
        Some(at_minutes(91).as_str())
    );

    // The wall clock was set back: the cancelled timer rescans, admits nothing and re-arms the
    // unchanged persisted due rather than a timer-derived one.
    let arms_before = projection.arms().len();
    clock.set(&at_minutes(30));
    projection.fire();
    eventually("the clock-change re-arm", || {
        projection.arms().len() > arms_before
    })
    .await;
    assert_eq!(projection.last_arm(), Some(Some(at_minutes(91))));
    assert_eq!(persistence.snapshot().automation_executions.len(), 1);
    loop_task.abort();
}

#[tokio::test]
async fn i9_g01_canonical_changes_rearm_the_projection_without_a_wake() {
    let (core, _persistence) = make_core();
    let clock = ManualClock::at(&at_minutes(0));
    let scheduler = Arc::new(AutomationScheduler::new(core.clone(), clock));
    let projection = Arc::new(RecordingProjection::default());
    let loop_task = run_scheduler(&scheduler, &projection);
    eventually("the idle disarm", || projection.arms() == vec![None]).await;

    let minutely = save(
        &core,
        1,
        "minutely",
        true,
        json!({"type": "interval", "every_ms": 60_000}),
    )
    .await;
    eventually("the save re-arm", || {
        projection.last_arm() == Some(Some(at_minutes(1)))
    })
    .await;

    disable(&core, 2, &minutely).await;
    eventually("the disable disarm", || projection.last_arm() == Some(None)).await;
    // Each distinct due is armed once; unrelated commits do not re-arm an unchanged due.
    assert_eq!(projection.arms(), vec![None, Some(at_minutes(1)), None]);
    loop_task.abort();
}

#[tokio::test]
async fn i9_g01_rejected_admission_keeps_the_due_and_disarms_its_expired_wake() {
    let (core, persistence, capabilities) = make_core_with(None);
    save(
        &core,
        1,
        "minutely",
        true,
        json!({"type": "interval", "every_ms": 60_000}),
    )
    .await;
    let mut unready = capability();
    unready.context.readiness = RuntimeReadiness::Unavailable;
    capabilities.set(unready);
    let clock = ManualClock::at(&at_minutes(2));
    let scheduler = AutomationScheduler::new(core.clone(), Arc::clone(&clock));

    // The expired due is not re-armed as an immediate wake, and nothing was consumed.
    assert_eq!(scheduler.wake().await.unwrap(), None);
    assert_eq!(scheduler.rejected(), 1);
    let stored = persistence.snapshot();
    assert!(stored.automation_executions.is_empty());
    assert_eq!(
        stored.automations[0].next_due_at.as_deref(),
        Some(at_minutes(1).as_str())
    );

    capabilities.set(capability());
    scheduler.wake().await.unwrap();
    settled(&persistence, 1).await;
    assert_eq!(
        persistence.snapshot().automations[0].next_due_at.as_deref(),
        Some(at_minutes(3).as_str())
    );
}

#[tokio::test]
async fn i9_g03_network_subscription_exists_only_while_a_network_automation_is_enabled() {
    let source = Arc::new(RecordingEventSource::default());
    let (core, persistence, _) = make_core_with(Some(
        Arc::clone(&source) as Arc<dyn NetworkDefaultEventSource>
    ));
    let clock = ManualClock::at(&at_minutes(0));
    let scheduler = Arc::new(AutomationScheduler::new(core.clone(), clock));
    let projection = Arc::new(RecordingProjection::default());
    let loop_task = run_scheduler(&scheduler, &projection);
    eventually("the idle disarm", || projection.arms() == vec![None]).await;
    assert_eq!(source.starts.load(Ordering::SeqCst), 0);

    let wifi = save(
        &core,
        1,
        "wifi",
        true,
        json!({"type": "event", "name": "network.default_changed", "match": {"transport": "wifi"}}),
    )
    .await;
    eventually("the subscription", || {
        source.starts.load(Ordering::SeqCst) == 1
    })
    .await;
    assert_eq!(
        source.emit(NetworkDefaultChangedEvent::new(
            Some("net-1".to_owned()),
            Some("cellular".to_owned())
        )),
        NetworkEventDelivery::Baseline
    );
    assert_eq!(
        source.emit(NetworkDefaultChangedEvent::new(
            Some("net-2".to_owned()),
            Some("wifi".to_owned())
        )),
        NetworkEventDelivery::Delivered
    );
    eventually("the network execution", || {
        completed_executions(&persistence) == 1
    })
    .await;
    assert_eq!(
        persistence.snapshot().automation_executions[0].automation_id,
        wifi
    );
    assert!(scheduler.network_events_unavailable().is_none());

    disable(&core, 2, &wifi).await;
    eventually("the unsubscribe", || {
        source.stops.load(Ordering::SeqCst) == 1
    })
    .await;
    assert_eq!(source.starts.load(Ordering::SeqCst), 1);
    loop_task.abort();
}

#[tokio::test]
async fn i9_g03_missing_network_source_is_reported_explicitly() {
    let (core, _persistence) = make_core();
    let clock = ManualClock::at(&at_minutes(0));
    let scheduler = Arc::new(AutomationScheduler::new(core.clone(), clock));
    let projection = Arc::new(RecordingProjection::default());
    let loop_task = run_scheduler(&scheduler, &projection);

    let wifi = save(
        &core,
        1,
        "wifi",
        true,
        json!({"type": "event", "name": "network.default_changed"}),
    )
    .await;
    eventually("the explicit source loss", || {
        scheduler
            .network_events_unavailable()
            .is_some_and(|error| error.code == ErrorCode::CapabilityUnavailable)
    })
    .await;

    disable(&core, 2, &wifi).await;
    eventually("the cleared requirement", || {
        scheduler.network_events_unavailable().is_none()
    })
    .await;
    loop_task.abort();
}
