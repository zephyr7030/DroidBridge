//! The one shared Network semantic handler (S-AUTH-NET-001).
//!
//! Both host surfaces admit every Network action through this module, so the public
//! result and error semantics, the field-family source assignment and the capture
//! ownership are singular. Whichever host owns the requested operation supplies the
//! primitive; the handler itself never selects an executor.

use crate::{
    AdmittedExecution, AndroidExecutionDispatch, AndroidPrimitiveResult, ArtifactPort,
    CapabilityPort, CapabilitySnapshot, ExecutionCancelOutcome, ExecutionCompletion,
    ExecutionFailure, ExecutionOutcome, ExecutionPayload, ExecutionPort, ExecutorRecord,
    HostControlPort, LocalExecutionClaim, LocalExecutionClaims, PersistencePort, PortFuture,
    ProviderToken, RESERVE_FLOOR_BYTES, RuntimeCore, SynchronousAdmission, TaskAdmission,
    TaskAdmissionResult, UI_ENVELOPE_LIMIT_BYTES,
    command::{execution_failure, execution_fence, new_uuid},
};
use contract::{
    Availability, CapabilityState, CaptureId, CaptureReadResult, CaptureReadSource, CaptureResult,
    CaptureResultOperation, CaptureStartOperation, CaptureStartResult, DiagnosticOutcome, DnsEntry,
    DnsRecordType, ErrorCode, EthernetBuild, EthernetHeader, ExecutionClass, FileTarget,
    IcmpHeader, InterfaceEntry, Ipv4Header, Ipv6Header, MotherTool, NetworkBuild, NetworkCall,
    NetworkCaptureInput, NetworkDiagnoseInput, NetworkDiagnoseResult, NetworkFamilyAvailability,
    NetworkFamilyTruncated, NetworkInspectInput, NetworkInspectResult, NetworkPacketInput,
    NetworkScope, PacketBuildResult, PacketDecodeResult, PacketDecodeSource, PacketInjectResult,
    PacketProtocol, PacketSource, PacketSummary, RequestId, RouteEntry, SocketEntry, TaskSnapshot,
    TaskState, TaskTerminalResult, TcpHeader, TransportBuild, UdpHeader, UuidV4,
};
use domain::{DomainError, ExecutorRequest, NetworkRoute, Provider, derive_capabilities};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// The S-CONTRACT-002 inline overhead reserved around one encoded result.
const MAX_INLINE_ENVELOPE_OVERHEAD: usize = 1_024;

/// R-NET-002 `max_entries` upper bound.
pub const NETWORK_MAX_SCOPE_ENTRIES: u32 = 5_000;
/// R-NET-002 `max_entries` default.
pub const NETWORK_DEFAULT_SCOPE_ENTRIES: u32 = 200;
/// R-NET-003 `max_packets` lower bound.
pub const NETWORK_MIN_CAPTURE_PACKETS: u64 = 1;
/// R-NET-003 `max_packets` upper bound.
pub const NETWORK_MAX_CAPTURE_PACKETS: u64 = 1_000_000;
/// S-NET-003 `max_bytes` cap, exactly the S-ART-002 capture-artifact maximum.
pub const NETWORK_MAX_CAPTURE_BYTES: u64 = 268_435_456;
/// R-NET-003 `max_duration_ms` upper bound.
pub const NETWORK_MAX_CAPTURE_DURATION_MS: u64 = 3_600_000;
/// R-NET-004 bound on the capture Task becoming terminal after a stop request.
pub const NETWORK_CAPTURE_STOP_WAIT_MS: u64 = 10_000;
/// R-NET-005 `max_packets` upper bound.
pub const NETWORK_MAX_READ_PACKETS: u32 = 5_000;
/// R-NET-005/006 decoded payload preview bound.
pub const NETWORK_PAYLOAD_PREVIEW_BYTES: usize = 4_096;
/// R-NET-006/007/008 packet byte bounds.
pub const NETWORK_MIN_PACKET_BYTES: usize = 1;
/// R-NET-006/007/008 packet byte bounds.
pub const NETWORK_MAX_PACKET_BYTES: usize = 131_072;
/// R-NET-008 `count` upper bound.
pub const NETWORK_MAX_INJECT_COUNT: u32 = 100;
/// R-NET-008 `interval_ms` upper bound.
pub const NETWORK_MAX_INJECT_INTERVAL_MS: u64 = 60_000;
/// R-NET-009 `timeout_ms` lower bound.
pub const NETWORK_MIN_DIAGNOSE_TIMEOUT_MS: u64 = 100;
/// R-NET-009 `timeout_ms` upper bound.
pub const NETWORK_MAX_DIAGNOSE_TIMEOUT_MS: u64 = 60_000;

/// S-NET-005 classic-PCAP little-endian microsecond file header length.
pub const PCAP_FILE_HEADER_BYTES: usize = 24;
/// S-NET-005 classic-PCAP record header length.
pub const PCAP_RECORD_HEADER_BYTES: usize = 16;
/// S-NET-003 non-promiscuous capture snaplen.
pub const PCAP_SNAPLEN: u32 = 65_535;
/// S-NET-005 fixed LINKTYPE_ETHERNET.
pub const PCAP_LINKTYPE_ETHERNET: u32 = 1;
/// S-NET-005 LINKTYPE_RAW: every record is one bare IPv4 or IPv6 packet, which is what a TUN
/// interface reports. libpcap's own `DLT_RAW` maps to this file link type, not to its number.
pub const PCAP_LINKTYPE_RAW: u32 = 101;
/// S-NET-005 fixed little-endian microsecond magic.
pub const PCAP_MAGIC_MICROSECOND: u32 = 0xa1b2_c3d4;

/// The R-NET-006 canonical TCP flag order.
pub const TCP_FLAG_ORDER: [&str; 8] = ["fin", "syn", "rst", "psh", "ack", "urg", "ece", "cwr"];

/// R-NET-002 reports one entry per requested family; this is the family's slot in the
/// per-response entry budget.
#[repr(usize)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetworkFamily {
    Interfaces = 0,
    Routes = 1,
    Dns = 2,
    Sockets = 3,
}

/// The provider S-NET-001 fixes for one field family on one host.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetworkFamilySource {
    /// The independent Magisk daemon's native providers.
    Daemon,
    /// The authenticated APK companion's App/Android-framework facts.
    AppFramework,
    /// The read-only Shizuku `UID2000` procfs supplement.
    ShizukuSupplement,
}

/// The S-NET-001 assignment of one field family, fixed before any side effect.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetworkFamilyPlan {
    Source(NetworkFamilySource),
    Unavailable(&'static str),
}

/// The S-NET-001 source assignment of one `network.inspect` request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NetworkSourcePlan {
    interfaces: NetworkFamilyPlan,
    routes: NetworkFamilyPlan,
    dns: NetworkFamilyPlan,
    sockets: NetworkFamilyPlan,
    supplement_generation: Option<u64>,
}

impl NetworkSourcePlan {
    /// The Shizuku executor generation the read-only supplement was resolved at, so its reads
    /// reach that session and never the executor that admitted the network request.
    pub const fn supplement_generation(&self) -> Option<u64> {
        self.supplement_generation
    }

    pub const fn family(&self, family: NetworkFamily) -> NetworkFamilyPlan {
        match family {
            NetworkFamily::Interfaces => self.interfaces,
            NetworkFamily::Routes => self.routes,
            NetworkFamily::Dns => self.dns,
            NetworkFamily::Sockets => self.sockets,
        }
    }
}

/// The R-NET-002 families one scope requests, in the order the response reports them.
pub const fn scope_families(scope: NetworkScope) -> &'static [NetworkFamily] {
    match scope {
        NetworkScope::Interfaces => &[NetworkFamily::Interfaces],
        NetworkScope::Routes => &[NetworkFamily::Routes],
        NetworkScope::Dns => &[NetworkFamily::Dns],
        NetworkScope::Sockets => &[NetworkFamily::Sockets],
        NetworkScope::All => &[
            NetworkFamily::Interfaces,
            NetworkFamily::Routes,
            NetworkFamily::Dns,
            NetworkFamily::Sockets,
        ],
    }
}

/// S-NET-001 fixes each field family's provider from the already-projected capability
/// facts and the executor S-AUTH-NET-001 selected, so the assignment cannot drift after a
/// failed query. Magisk owns every family on the Magisk host; the APK path never consults
/// Shizuku for a family Magisk owns, and never claims an owner it cannot reach.
pub fn network_source_plan(
    capability: &CapabilitySnapshot,
    provider: ProviderToken,
) -> NetworkSourcePlan {
    if provider == ProviderToken::MagiskNative {
        return NetworkSourcePlan {
            interfaces: NetworkFamilyPlan::Source(NetworkFamilySource::Daemon),
            routes: NetworkFamilyPlan::Source(NetworkFamilySource::Daemon),
            dns: NetworkFamilyPlan::Source(NetworkFamilySource::Daemon),
            sockets: NetworkFamilyPlan::Source(NetworkFamilySource::Daemon),
            supplement_generation: None,
        };
    }
    let companion = capability.context.app_execution_surface == CapabilityState::Available;
    let supplement = crate::resolve_execution(
        capability,
        ExecutorRequest::Network(NetworkRoute::ReadOnlyRouteSupplement),
    )
    .ok()
    .map(|executor| executor.capability_generation());
    let app_family = if companion {
        NetworkFamilyPlan::Source(NetworkFamilySource::AppFramework)
    } else {
        NetworkFamilyPlan::Unavailable("COMPANION_UNAVAILABLE")
    };
    let supplement_family = if supplement.is_some() {
        NetworkFamilyPlan::Source(NetworkFamilySource::ShizukuSupplement)
    } else {
        NetworkFamilyPlan::Unavailable("SHIZUKU_UNAVAILABLE")
    };
    NetworkSourcePlan {
        interfaces: app_family,
        routes: match supplement_family {
            NetworkFamilyPlan::Source(_) => supplement_family,
            NetworkFamilyPlan::Unavailable(_) => app_family,
        },
        dns: app_family,
        sockets: supplement_family,
        supplement_generation: supplement,
    }
}

/// The registered Automation event family I8-NET produces (S-HANDOFF-011).
pub const NETWORK_DEFAULT_CHANGED_EVENT: &str = "network.default_changed";

/// The bound on either identity fact the registered event carries.
pub const NETWORK_EVENT_FACT_BYTES: usize = 128;

/// S-NET-006's exact bounded FIFO capacity.
pub const NETWORK_EVENT_CHANNEL_CAPACITY: usize = 256;

/// The bounded `event.network_default_changed.v1` record. S-HANDOFF-011 makes this event a
/// bounded announcement that carries no copied network authority, so it holds only the
/// observed identity facts of the new default network and never a grant, capability, route
/// table or address list. A fact the observing source cannot establish is omitted.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkDefaultChangedEvent {
    pub name: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
}

impl NetworkDefaultChangedEvent {
    pub fn new(network_id: Option<String>, transport: Option<String>) -> Self {
        Self {
            name: NETWORK_DEFAULT_CHANGED_EVENT,
            network_id,
            transport,
        }
    }

    fn validate(&self) -> Result<(), DomainError> {
        for fact in [&self.network_id, &self.transport] {
            if let Some(fact) = fact
                && (fact.is_empty() || fact.len() > NETWORK_EVENT_FACT_BYTES)
            {
                return Err(DomainError::invalid(
                    "network default-change fact is out of bounds",
                ));
            }
        }
        Ok(())
    }
}

/// The complete fence and two monotonic generations carried by one selected source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkDefaultSourceRegistration {
    pub fence: domain::AdmissionFence,
    pub subscription_generation: u64,
    pub source_generation: u64,
}

/// The selected App-companion or daemon-native observer. `stop` does not return success
/// until the callback/observer has been unregistered and can no longer enqueue new work.
pub trait NetworkDefaultEventSource: Send + Sync {
    fn start(
        &self,
        registration: &NetworkDefaultSourceRegistration,
        ingress: NetworkDefaultEventIngress,
    ) -> Result<(), DomainError>;

    fn stop(&self, registration: &NetworkDefaultSourceRegistration) -> Result<(), DomainError>;
}

/// The shared source-control implementation for an Android framework callback, used by
/// both the APK Runtime and the Magisk Runtime's authenticated companion path.
pub struct AndroidNetworkDefaultEventSource<D, C> {
    dispatch: D,
    capabilities: C,
}

impl<D: Clone, C: Clone> Clone for AndroidNetworkDefaultEventSource<D, C> {
    fn clone(&self) -> Self {
        Self {
            dispatch: self.dispatch.clone(),
            capabilities: self.capabilities.clone(),
        }
    }
}

impl<D, C> AndroidNetworkDefaultEventSource<D, C> {
    pub const fn new(dispatch: D, capabilities: C) -> Self {
        Self {
            dispatch,
            capabilities,
        }
    }
}

impl<D, C> NetworkDefaultEventSource for AndroidNetworkDefaultEventSource<D, C>
where
    D: AndroidExecutionDispatch,
    C: CapabilityPort,
{
    fn start(
        &self,
        registration: &NetworkDefaultSourceRegistration,
        _ingress: NetworkDefaultEventIngress,
    ) -> Result<(), DomainError> {
        let result = self.dispatch_control("NetworkDefaultSubscribe", registration)?;
        let reply: NetworkDefaultSubscribeReply = decode_network_control_reply(&result.payload)?;
        if !reply.subscribed {
            return Err(DomainError::new(
                ErrorCode::IoError,
                "network subscribe result is invalid",
            ));
        }
        Ok(())
    }

    fn stop(&self, registration: &NetworkDefaultSourceRegistration) -> Result<(), DomainError> {
        let result = self.dispatch_control("NetworkDefaultUnsubscribe", registration)?;
        let reply: NetworkDefaultUnsubscribeReply = decode_network_control_reply(&result.payload)?;
        if !reply.unsubscribed {
            return Err(DomainError::new(
                ErrorCode::IoError,
                "network unsubscribe result is invalid",
            ));
        }
        Ok(())
    }
}

impl<D, C> AndroidNetworkDefaultEventSource<D, C>
where
    D: AndroidExecutionDispatch,
    C: CapabilityPort,
{
    fn dispatch_control(
        &self,
        primitive: &str,
        registration: &NetworkDefaultSourceRegistration,
    ) -> Result<AndroidPrimitiveResult, DomainError> {
        let capability = self.capabilities.current()?;
        if capability.fence != registration.fence {
            return Err(DomainError::new(
                ErrorCode::StaleAuthority,
                "network event source fence is stale",
            ));
        }
        if capability.context.app_execution_surface != CapabilityState::Available {
            return Err(DomainError::new(
                ErrorCode::CapabilityUnavailable,
                "Android network callback source is unavailable",
            ));
        }
        let capability_generation = capability.resolver_facts.generations.app_framework;
        if capability_generation == 0 {
            return Err(DomainError::new(
                ErrorCode::CapabilityUnavailable,
                "Android network callback generation is unavailable",
            ));
        }
        let execution = AdmittedExecution {
            execution_id: new_uuid()?,
            task_id: None,
            executor: ExecutorRecord {
                host: capability.context.host,
                provider: ProviderToken::AppFramework,
                execution_class: ExecutionClass::AndroidFramework,
                capability_generation,
                fence: contract::Fence {
                    runtime_epoch: registration.fence.runtime_epoch.clone(),
                    host_generation: registration.fence.host_generation,
                    runtime_instance_id: registration.fence.runtime_instance_id.clone(),
                },
            },
            payload: ExecutionPayload::OpaqueOperation("network.default_changed".to_owned()),
        };
        let payload = serde_json::to_vec(&NetworkDefaultControlRequest {
            subscription_generation: registration.subscription_generation,
            source_generation: registration.source_generation,
        })
        .map_err(|_| {
            DomainError::new(
                ErrorCode::InternalError,
                "network source request encoding failed",
            )
        })?;
        let result = self.dispatch.dispatch(primitive, &payload, &execution)?;
        if !result.descriptors.is_empty() {
            return Err(DomainError::new(
                ErrorCode::IoError,
                "network source control returned descriptors",
            ));
        }
        Ok(result)
    }
}

#[derive(serde::Serialize)]
#[serde(deny_unknown_fields)]
struct NetworkDefaultControlRequest {
    subscription_generation: u64,
    source_generation: u64,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NetworkDefaultSubscribeReply {
    subscribed: bool,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NetworkDefaultUnsubscribeReply {
    unsubscribed: bool,
}

fn decode_network_control_reply<T: serde::de::DeserializeOwned>(
    bytes: &[u8],
) -> Result<T, DomainError> {
    serde_json::from_slice(bytes).map_err(|_| {
        DomainError::new(
            ErrorCode::IoError,
            "network source control result is invalid",
        )
    })
}

/// Observable result of one source callback. This is source-lifecycle evidence only; the
/// Automation consumer receives only `NetworkDefaultChangedEvent` values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetworkEventDelivery {
    Baseline,
    Unchanged,
    Delivered,
    DroppedSaturated,
    IgnoredStale,
}

/// One coalesced, bounded diagnostic fact for a completed channel-saturation episode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NetworkEventSaturationDiagnostic {
    pub dropped_events: u64,
}

#[derive(Default)]
struct NetworkEventPlaneState {
    subscription_generation: u64,
    source_generation: u64,
    slot: NetworkEventPlaneSlot,
    pending_saturation: u64,
}

#[derive(Default)]
enum NetworkEventPlaneSlot {
    #[default]
    Vacant,
    Active(NetworkEventActive),
    Transitioning,
    CleanupBlocked(NetworkEventCleanup),
}

struct NetworkEventActive {
    registration: NetworkDefaultSourceRegistration,
    source: Arc<dyn NetworkDefaultEventSource>,
    sender: mpsc::Sender<NetworkDefaultChangedEvent>,
    baseline: Option<NetworkDefaultChangedEvent>,
    saturation_dropped: u64,
}

struct NetworkEventCleanup {
    registration: NetworkDefaultSourceRegistration,
    source: Arc<dyn NetworkDefaultEventSource>,
    continuation: NetworkEventCleanupContinuation,
}

enum NetworkEventCleanupContinuation {
    Unsubscribe,
    Replace {
        sender: mpsc::Sender<NetworkDefaultChangedEvent>,
        fence: domain::AdmissionFence,
        source: Arc<dyn NetworkDefaultEventSource>,
    },
}

/// The only callback ingress into one active event plane. It captures every generation,
/// making a retained callback harmless after unsubscribe, source replacement or host loss.
#[derive(Clone)]
pub struct NetworkDefaultEventIngress {
    plane: Weak<Mutex<NetworkEventPlaneState>>,
    registration: NetworkDefaultSourceRegistration,
}

impl NetworkDefaultEventIngress {
    pub fn registration(&self) -> &NetworkDefaultSourceRegistration {
        &self.registration
    }

    pub fn observe(
        &self,
        observed: NetworkDefaultChangedEvent,
    ) -> Result<NetworkEventDelivery, DomainError> {
        let Some(plane) = self.plane.upgrade() else {
            return Ok(NetworkEventDelivery::IgnoredStale);
        };
        let mut state = plane
            .lock()
            .map_err(|_| DomainError::new(ErrorCode::InternalError, "event plane lock failed"))?;
        let NetworkEventPlaneSlot::Active(active) = &mut state.slot else {
            return Ok(NetworkEventDelivery::IgnoredStale);
        };
        if active.registration != self.registration {
            return Ok(NetworkEventDelivery::IgnoredStale);
        }
        observed.validate()?;
        if active.baseline.is_none() {
            active.baseline = Some(observed);
            return Ok(NetworkEventDelivery::Baseline);
        }
        if active.baseline.as_ref() == Some(&observed) {
            return Ok(NetworkEventDelivery::Unchanged);
        }
        active.baseline = Some(observed.clone());
        let (delivery, recovered_saturation) = match active.sender.try_send(observed) {
            Ok(()) => {
                let recovered = std::mem::take(&mut active.saturation_dropped);
                (NetworkEventDelivery::Delivered, recovered)
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                active.saturation_dropped = active.saturation_dropped.saturating_add(1);
                (NetworkEventDelivery::DroppedSaturated, 0)
            }
            Err(mpsc::error::TrySendError::Closed(_)) => (NetworkEventDelivery::IgnoredStale, 0),
        };
        if recovered_saturation > 0 {
            state.pending_saturation = state
                .pending_saturation
                .saturating_add(recovered_saturation);
        }
        Ok(delivery)
    }
}

/// One Runtime-instance-owned, non-persistent event plane. Its API has no ArtifactPort, so
/// neither an event nor its baseline can become a `data` artifact by construction.
#[derive(Clone, Default)]
pub struct NetworkDefaultEventPlane {
    inner: Arc<Mutex<NetworkEventPlaneState>>,
}

impl NetworkDefaultEventPlane {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn subscribe(
        &self,
        fence: domain::AdmissionFence,
        source: Arc<dyn NetworkDefaultEventSource>,
    ) -> Result<NetworkDefaultSubscription, DomainError> {
        if fence.host_generation == 0 {
            return Err(DomainError::invalid("network event fence is invalid"));
        }
        let (registration, receiver) = {
            let mut state = self.lock()?;
            if !matches!(state.slot, NetworkEventPlaneSlot::Vacant) {
                return Err(network_event_subscription_exists());
            }
            let subscription_generation = next_generation(state.subscription_generation)?;
            let source_generation = next_generation(state.source_generation)?;
            state.subscription_generation = subscription_generation;
            state.source_generation = source_generation;
            let registration = NetworkDefaultSourceRegistration {
                fence,
                subscription_generation,
                source_generation,
            };
            let (sender, receiver) = mpsc::channel(NETWORK_EVENT_CHANNEL_CAPACITY);
            state.slot = NetworkEventPlaneSlot::Active(NetworkEventActive {
                registration: registration.clone(),
                source: Arc::clone(&source),
                sender,
                baseline: None,
                saturation_dropped: 0,
            });
            (registration, receiver)
        };
        let ingress = NetworkDefaultEventIngress {
            plane: Arc::downgrade(&self.inner),
            registration: registration.clone(),
        };
        if let Err(start_error) = source.start(&registration, ingress) {
            return match self.unsubscribe(registration.subscription_generation) {
                Ok(()) => Err(start_error),
                Err(cleanup_error) => Err(cleanup_error),
            };
        }
        Ok(NetworkDefaultSubscription {
            plane: self.clone(),
            generation: registration.subscription_generation,
            receiver,
            closed: false,
        })
    }

    pub fn active_subscription_generation(&self) -> Result<Option<u64>, DomainError> {
        let state = self.lock()?;
        Ok(match &state.slot {
            NetworkEventPlaneSlot::Active(active) => {
                Some(active.registration.subscription_generation)
            }
            _ => None,
        })
    }

    pub fn observe(
        &self,
        registration: NetworkDefaultSourceRegistration,
        observed: NetworkDefaultChangedEvent,
    ) -> Result<NetworkEventDelivery, DomainError> {
        NetworkDefaultEventIngress {
            plane: Arc::downgrade(&self.inner),
            registration,
        }
        .observe(observed)
    }

    pub fn replace_source(
        &self,
        subscription_generation: u64,
        replacement: Arc<dyn NetworkDefaultEventSource>,
    ) -> Result<(), DomainError> {
        let cleanup = {
            let mut state = self.lock()?;
            let slot = std::mem::take(&mut state.slot);
            match slot {
                NetworkEventPlaneSlot::Active(active)
                    if active.registration.subscription_generation == subscription_generation =>
                {
                    flush_saturation(&mut state, active.saturation_dropped);
                    let registration = active.registration.clone();
                    state.slot = NetworkEventPlaneSlot::Transitioning;
                    NetworkEventCleanup {
                        registration,
                        source: active.source,
                        continuation: NetworkEventCleanupContinuation::Replace {
                            sender: active.sender,
                            fence: active.registration.fence,
                            source: replacement,
                        },
                    }
                }
                other => {
                    state.slot = other;
                    return Err(network_event_stale_subscription());
                }
            }
        };
        self.finish_cleanup(cleanup)
    }

    pub fn retry_cleanup(&self) -> Result<(), DomainError> {
        let cleanup = {
            let mut state = self.lock()?;
            let slot = std::mem::take(&mut state.slot);
            match slot {
                NetworkEventPlaneSlot::CleanupBlocked(cleanup) => {
                    state.slot = NetworkEventPlaneSlot::Transitioning;
                    cleanup
                }
                other => {
                    state.slot = other;
                    return Ok(());
                }
            }
        };
        self.finish_cleanup(cleanup)
    }

    pub fn saturation_dropped(&self) -> u64 {
        let Ok(state) = self.inner.lock() else {
            return 0;
        };
        let active = match &state.slot {
            NetworkEventPlaneSlot::Active(active) => active.saturation_dropped,
            _ => 0,
        };
        state.pending_saturation.saturating_add(active)
    }

    pub fn take_saturation_diagnostic(&self) -> Option<NetworkEventSaturationDiagnostic> {
        let Ok(mut state) = self.inner.lock() else {
            return None;
        };
        (state.pending_saturation > 0).then(|| NetworkEventSaturationDiagnostic {
            dropped_events: std::mem::take(&mut state.pending_saturation),
        })
    }

    fn unsubscribe(&self, subscription_generation: u64) -> Result<(), DomainError> {
        let cleanup = {
            let mut state = self.lock()?;
            let slot = std::mem::take(&mut state.slot);
            match slot {
                NetworkEventPlaneSlot::Active(active)
                    if active.registration.subscription_generation == subscription_generation =>
                {
                    flush_saturation(&mut state, active.saturation_dropped);
                    state.slot = NetworkEventPlaneSlot::Transitioning;
                    NetworkEventCleanup {
                        registration: active.registration,
                        source: active.source,
                        continuation: NetworkEventCleanupContinuation::Unsubscribe,
                    }
                }
                NetworkEventPlaneSlot::CleanupBlocked(mut cleanup)
                    if cleanup.registration.subscription_generation == subscription_generation =>
                {
                    cleanup.continuation = NetworkEventCleanupContinuation::Unsubscribe;
                    state.slot = NetworkEventPlaneSlot::Transitioning;
                    cleanup
                }
                NetworkEventPlaneSlot::Vacant => return Ok(()),
                other => {
                    state.slot = other;
                    return Err(network_event_stale_subscription());
                }
            }
        };
        self.finish_cleanup(cleanup)
    }

    fn finish_cleanup(&self, cleanup: NetworkEventCleanup) -> Result<(), DomainError> {
        if let Err(error) = cleanup.source.stop(&cleanup.registration) {
            self.lock()?.slot = NetworkEventPlaneSlot::CleanupBlocked(cleanup);
            return Err(error);
        }
        match cleanup.continuation {
            NetworkEventCleanupContinuation::Unsubscribe => {
                self.lock()?.slot = NetworkEventPlaneSlot::Vacant;
                Ok(())
            }
            NetworkEventCleanupContinuation::Replace {
                sender,
                fence,
                source,
            } => self.start_replacement(
                cleanup.registration.subscription_generation,
                fence,
                sender,
                source,
            ),
        }
    }

    fn start_replacement(
        &self,
        subscription_generation: u64,
        fence: domain::AdmissionFence,
        sender: mpsc::Sender<NetworkDefaultChangedEvent>,
        source: Arc<dyn NetworkDefaultEventSource>,
    ) -> Result<(), DomainError> {
        let registration = {
            let mut state = self.lock()?;
            let source_generation = next_generation(state.source_generation)?;
            state.source_generation = source_generation;
            let registration = NetworkDefaultSourceRegistration {
                fence,
                subscription_generation,
                source_generation,
            };
            state.slot = NetworkEventPlaneSlot::Active(NetworkEventActive {
                registration: registration.clone(),
                source: Arc::clone(&source),
                sender,
                baseline: None,
                saturation_dropped: 0,
            });
            registration
        };
        let ingress = NetworkDefaultEventIngress {
            plane: Arc::downgrade(&self.inner),
            registration: registration.clone(),
        };
        if let Err(start_error) = source.start(&registration, ingress) {
            return match self.unsubscribe(subscription_generation) {
                Ok(()) => Err(start_error),
                Err(cleanup_error) => Err(cleanup_error),
            };
        }
        Ok(())
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, NetworkEventPlaneState>, DomainError> {
        self.inner
            .lock()
            .map_err(|_| DomainError::new(ErrorCode::InternalError, "event plane lock failed"))
    }
}

/// The Automation engine's sole receiver. Dropping it follows the same invalidate-then-stop
/// path as explicit close; failed source cleanup remains quarantined inside the plane.
pub struct NetworkDefaultSubscription {
    plane: NetworkDefaultEventPlane,
    generation: u64,
    receiver: mpsc::Receiver<NetworkDefaultChangedEvent>,
    closed: bool,
}

impl std::fmt::Debug for NetworkDefaultSubscription {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NetworkDefaultSubscription")
            .field("generation", &self.generation)
            .field("closed", &self.closed)
            .finish()
    }
}

impl NetworkDefaultSubscription {
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub async fn recv(&mut self) -> Option<NetworkDefaultChangedEvent> {
        self.receiver.recv().await
    }

    pub fn try_recv(&mut self) -> Result<NetworkDefaultChangedEvent, mpsc::error::TryRecvError> {
        self.receiver.try_recv()
    }

    pub fn close(&mut self) -> Result<(), DomainError> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        self.receiver.close();
        self.plane.unsubscribe(self.generation)
    }
}

impl Drop for NetworkDefaultSubscription {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

fn flush_saturation(state: &mut NetworkEventPlaneState, dropped: u64) {
    state.pending_saturation = state.pending_saturation.saturating_add(dropped);
}

fn next_generation(current: u64) -> Result<u64, DomainError> {
    current.checked_add(1).ok_or_else(|| {
        DomainError::new(
            ErrorCode::ResourceLimit,
            "network event generation exhausted",
        )
    })
}

fn network_event_subscription_exists() -> DomainError {
    DomainError::new(
        ErrorCode::AlreadyExists,
        "network event subscription already exists",
    )
}

fn network_event_stale_subscription() -> DomainError {
    DomainError::new(
        ErrorCode::StaleAuthority,
        "network event subscription is stale",
    )
}

/// Entries one authoritative query established, with the query's own truncation fact.
#[derive(Clone, Debug, PartialEq)]
pub struct Established<T> {
    pub entries: Vec<T>,
    pub truncated: bool,
}

/// The per-family inspect settlement. `None` means the assigned provider could not
/// establish the family, which the response reports as `unknown` with its data omitted.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NetworkInspectSettlement {
    pub interfaces: Option<Established<InterfaceEntry>>,
    pub routes: Option<Established<RouteEntry>>,
    pub dns: Option<Established<DnsEntry>>,
    pub sockets: Option<Established<SocketEntry>>,
}

/// The terminal settlement of one capture Task (S-NET-003). The capture's owner publishes
/// its complete record under `capture_ref` and commits `destination`, so the Runtime never
/// becomes a second owner of the capture bytes.
#[derive(Clone, Debug, PartialEq)]
pub struct CaptureSettlement {
    pub cancelled: bool,
    pub packets_captured: u64,
    pub bytes_captured: u64,
    pub capture_ref: Option<String>,
    pub destination: Option<FileTarget>,
    pub cleanup_verified: bool,
}

/// One already-admitted network request handed to the host surface that owns it.
#[derive(Clone, Debug, PartialEq)]
pub enum NetworkPrimitiveRequest {
    Inspect {
        plan: NetworkSourcePlan,
        scope: NetworkScope,
        max_entries: u32,
    },
    Diagnose(NetworkDiagnoseInput, NetworkSourcePlan),
    CaptureStart {
        capture_id: CaptureId,
        interface: String,
        filter: Option<String>,
        max_packets: u64,
        max_bytes: u64,
        max_duration_ms: u64,
        persist_to: Option<FileTarget>,
    },
    CaptureStop {
        capture_id: CaptureId,
    },
    /// One capture stream read from a caller-named file: the host that already owns a
    /// filesystem primitive supplies the bytes, the Runtime owns the PCAP format.
    CaptureFileBytes {
        target: FileTarget,
    },
    PacketInject {
        interface: String,
        packet: Vec<u8>,
        count: u32,
        interval_ms: u64,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum NetworkPrimitiveOutcome {
    Inspect(NetworkInspectSettlement),
    Diagnose(NetworkDiagnoseResult),
    CaptureSettled(CaptureSettlement),
    CaptureStopRequested,
    CaptureFileBytes(Vec<u8>),
    PacketInjected(PacketInjectResult),
}

#[derive(Clone, Debug, PartialEq)]
pub struct NetworkPrimitiveSettlement {
    pub outcome: NetworkPrimitiveOutcome,
    pub cleanup_verified: bool,
}

/// The single network primitive both host surfaces implement. The APK surface backs
/// inspect/diagnose with App/Android-framework providers and the read-only Shizuku
/// supplement; the Magisk surface backs every family with daemon-native providers and owns
/// raw capture/injection. No host implements a second capture abstraction (S-NET-005).
pub trait NetworkPrimitivePort: Send + Sync {
    fn run(
        &self,
        execution: &AdmittedExecution,
        request: NetworkPrimitiveRequest,
        claim: &LocalExecutionClaim,
    ) -> Result<NetworkPrimitiveSettlement, ExecutionFailure>;
}

pub async fn handle_network_public<P, A, E, C, H>(
    core: &RuntimeCore<P, A, E, C, H>,
    request_id: RequestId,
    payload_sha256: String,
    call: NetworkCall,
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
    if let NetworkCall::Capture(NetworkCaptureInput::Stop { capture_id }) = &call {
        return stop_capture(
            core,
            request_id,
            payload_sha256,
            capture_id.clone(),
            timestamp,
            now_ms,
        )
        .await;
    }
    let capability = core.capability_snapshot()?;
    let route = network_executor_request(&capability, &call)?;
    require_local_network_authority(&capability, route, &call)?;
    let execution_id = new_uuid()?;
    let payload = ExecutionPayload::NetworkCall(call.clone());
    if is_network_task(&call) {
        let task_id = new_uuid()?;
        let admission = core
            .admit_task(TaskAdmission {
                request_id,
                payload_sha256,
                task_id: task_id.clone(),
                execution_id,
                tool: MotherTool::Network,
                action: network_action(&call).to_owned(),
                route,
                payload,
                created_at: timestamp.clone(),
                settlement_bound_bytes: network_settlement_bound_bytes(),
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
        return serde_json::to_value(CaptureStartResult {
            operation: CaptureStartOperation::Start,
            capture_id: admitted_task_id.clone(),
            task_id: admitted_task_id,
        })
        .map_err(|_| DomainError::new(ErrorCode::InternalError, "Task result encoding failed"));
    }
    core.run_synchronous(
        SynchronousAdmission {
            request_id,
            payload_sha256,
            execution_id,
            operation: format!("network.{}", network_action(&call)),
            route,
            payload,
            settlement_bound_bytes: network_settlement_bound_bytes(),
            now_ms,
        },
        timestamp,
        now_ms,
    )
    .await
    .map_err(|error| DomainError::new(error.code, "network execution failed"))
}

/// S-NET-003 keeps one Contract response inside the S-CONTRACT-002 frame limit, so one
/// admitted network operation can never settle more than that frame.
pub const fn network_settlement_bound_bytes() -> u64 {
    UI_ENVELOPE_LIMIT_BYTES as u64
}

pub const fn network_action(call: &NetworkCall) -> &'static str {
    match call {
        NetworkCall::Inspect(_) => "inspect",
        NetworkCall::Capture(_) => "capture",
        NetworkCall::Packet(_) => "packet",
        NetworkCall::Diagnose(_) => "diagnose",
    }
}

/// R-NET-003 is the only Network action with a Task identity; every other action settles
/// inside its own response.
const fn is_network_task(call: &NetworkCall) -> bool {
    matches!(
        call,
        NetworkCall::Capture(NetworkCaptureInput::Start { .. })
    )
}

/// R-NET-004: a retained terminal capture keeps answering with its own terminal result, an
/// unknown or expired identity is `NOT_FOUND`, and a capture still running is asked to stop
/// once and then given at most 10,000 ms to settle before `TIMEOUT`. The stop request is
/// answered before executor resolution, so a retained result stays readable on a host that
/// cannot itself capture.
async fn stop_capture<P, A, E, C, H>(
    core: &RuntimeCore<P, A, E, C, H>,
    request_id: RequestId,
    payload_sha256: String,
    capture_id: CaptureId,
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
    let snapshot = capture_task(core, &capture_id, now_ms).await?;
    if let Some(value) = retained_capture_result(&snapshot)? {
        return Ok(value);
    }
    if task_is_terminal(snapshot.state) {
        return Err(capture_stop_failure(&snapshot));
    }
    let capability = core.capability_snapshot()?;
    let call = NetworkCall::Capture(NetworkCaptureInput::Stop {
        capture_id: capture_id.clone(),
    });
    let route = network_executor_request(&capability, &call)?;
    let execution_id = new_uuid()?;
    core.run_synchronous(
        SynchronousAdmission {
            request_id,
            payload_sha256,
            execution_id,
            operation: format!("network.{}", network_action(&call)),
            route,
            payload: ExecutionPayload::NetworkCall(call),
            settlement_bound_bytes: network_settlement_bound_bytes(),
            now_ms,
        },
        timestamp,
        now_ms,
    )
    .await
    .map_err(|error| DomainError::new(error.code, "network capture stop failed"))?;
    let deadline = Instant::now() + Duration::from_millis(NETWORK_CAPTURE_STOP_WAIT_MS);
    loop {
        let snapshot = capture_task(core, &capture_id, now_ms).await?;
        if let Some(value) = retained_capture_result(&snapshot)? {
            return Ok(value);
        }
        if task_is_terminal(snapshot.state) {
            return Err(capture_stop_failure(&snapshot));
        }
        if Instant::now() >= deadline {
            return Err(DomainError::new(
                ErrorCode::Timeout,
                "network capture did not settle inside the stop bound",
            ));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn capture_task<P, A, E, C, H>(
    core: &RuntimeCore<P, A, E, C, H>,
    capture_id: &CaptureId,
    now_ms: u64,
) -> Result<TaskSnapshot, DomainError>
where
    P: PersistencePort + 'static,
    A: ArtifactPort + Clone + 'static,
    E: ExecutionPort + 'static,
    C: CapabilityPort + 'static,
    H: HostControlPort + 'static,
{
    let snapshot = core.get_task(capture_id, now_ms).await?;
    if snapshot.tool != MotherTool::Network || snapshot.action != "capture" {
        return Err(DomainError::new(
            ErrorCode::NotFound,
            "capture identity is not a capture Task",
        ));
    }
    Ok(snapshot)
}

fn retained_capture_result(
    snapshot: &TaskSnapshot,
) -> Result<Option<serde_json::Value>, DomainError> {
    match &snapshot.result {
        Some(TaskTerminalResult::NetworkCapture(result)) => {
            serde_json::to_value(result).map(Some).map_err(|_| {
                DomainError::new(ErrorCode::InternalError, "capture result encoding failed")
            })
        }
        Some(_) => Err(capture_stop_failure(snapshot)),
        None => Ok(None),
    }
}

fn capture_stop_failure(snapshot: &TaskSnapshot) -> DomainError {
    match &snapshot.error {
        Some(error) => DomainError::new(error.code, "network capture has no retained result"),
        None => DomainError::new(
            ErrorCode::CaptureFailed,
            "network capture has no retained result",
        ),
    }
}

const fn task_is_terminal(state: TaskState) -> bool {
    matches!(
        state,
        TaskState::Completed | TaskState::Failed | TaskState::Cancelled | TaskState::Interrupted
    )
}

/// The R-NET-002..009 bounds. Every bound is enforced before executor resolution, so an
/// out-of-range request never reaches a network primitive.
pub fn validate_network_input(call: &NetworkCall) -> Result<(), DomainError> {
    match call {
        NetworkCall::Inspect(NetworkInspectInput { max_entries, .. }) => {
            if !(1..=NETWORK_MAX_SCOPE_ENTRIES).contains(max_entries) {
                return Err(DomainError::invalid(
                    "network.inspect max_entries is out of bounds",
                ));
            }
        }
        NetworkCall::Capture(input) => validate_capture_input(input)?,
        NetworkCall::Packet(input) => validate_packet_input(input)?,
        NetworkCall::Diagnose(input) => validate_diagnose_input(input)?,
    }
    Ok(())
}

fn validate_capture_input(input: &NetworkCaptureInput) -> Result<(), DomainError> {
    match input {
        NetworkCaptureInput::Start {
            interface,
            filter,
            max_packets,
            max_bytes,
            max_duration_ms,
            ..
        } => {
            require_name(
                interface,
                "network.capture start interface is out of bounds",
            )?;
            if let Some(filter) = filter {
                require_name(filter, "network.capture start filter is out of bounds")?;
            }
            if !(NETWORK_MIN_CAPTURE_PACKETS..=NETWORK_MAX_CAPTURE_PACKETS).contains(max_packets) {
                return Err(DomainError::invalid(
                    "network.capture start max_packets is out of bounds",
                ));
            }
            if !(1..=NETWORK_MAX_CAPTURE_BYTES).contains(max_bytes) {
                return Err(DomainError::invalid(
                    "network.capture start max_bytes is out of bounds",
                ));
            }
            if !(1..=NETWORK_MAX_CAPTURE_DURATION_MS).contains(max_duration_ms) {
                return Err(DomainError::invalid(
                    "network.capture start max_duration_ms is out of bounds",
                ));
            }
        }
        NetworkCaptureInput::Stop { .. } => {}
        NetworkCaptureInput::Read {
            source,
            max_packets,
            ..
        } => {
            if !(1..=NETWORK_MAX_READ_PACKETS).contains(max_packets) {
                return Err(DomainError::invalid(
                    "network.capture read max_packets is out of bounds",
                ));
            }
            if let CaptureReadSource::CaptureRef { capture_ref } = source {
                require_ref(
                    capture_ref,
                    "capture",
                    "network.capture read capture_ref is invalid",
                )?;
            }
        }
    }
    Ok(())
}

fn validate_packet_input(input: &NetworkPacketInput) -> Result<(), DomainError> {
    match input {
        NetworkPacketInput::Decode { source } => match source {
            PacketDecodeSource::Raw { raw_base64 } => {
                let bytes =
                    decode_base64(raw_base64, "network.packet decode raw_base64 is invalid")?;
                require_packet_bytes(
                    bytes.len(),
                    "network.packet decode packet length is out of bounds",
                )?;
            }
            PacketDecodeSource::PacketRef { packet_ref } => {
                require_ref(
                    packet_ref,
                    "packet",
                    "network.packet decode packet_ref is invalid",
                )?;
            }
            PacketDecodeSource::Capture { capture_ref, .. } => {
                require_ref(
                    capture_ref,
                    "capture",
                    "network.packet decode capture_ref is invalid",
                )?;
            }
        },
        NetworkPacketInput::Build {
            ethernet,
            network,
            transport,
            payload_base64,
        } => {
            if let Some(EthernetBuild { src_mac, dst_mac }) = ethernet {
                parse_mac(src_mac, "network.packet build src_mac is invalid")?;
                parse_mac(dst_mac, "network.packet build dst_mac is invalid")?;
            }
            match network {
                NetworkBuild::Ipv4 { src, dst, .. } => {
                    parse_ipv4(src, "network.packet build src is invalid")?;
                    parse_ipv4(dst, "network.packet build dst is invalid")?;
                }
                NetworkBuild::Ipv6 { src, dst, .. } => {
                    parse_ipv6(src, "network.packet build src is invalid")?;
                    parse_ipv6(dst, "network.packet build dst is invalid")?;
                }
            }
            if let TransportBuild::Tcp { flags, .. } = transport {
                tcp_flag_mask(flags)?;
            }
            if let Some(payload_base64) = payload_base64 {
                let payload = decode_base64(
                    payload_base64,
                    "network.packet build payload_base64 is invalid",
                )?;
                if payload.len() > NETWORK_MAX_PACKET_BYTES {
                    return Err(DomainError::invalid(
                        "network.packet build payload is out of bounds",
                    ));
                }
            }
        }
        NetworkPacketInput::Inject {
            interface,
            packet,
            count,
            interval_ms,
        } => {
            require_name(
                interface,
                "network.packet inject interface is out of bounds",
            )?;
            if !(1..=NETWORK_MAX_INJECT_COUNT).contains(count) {
                return Err(DomainError::invalid(
                    "network.packet inject count is out of bounds",
                ));
            }
            if *interval_ms > NETWORK_MAX_INJECT_INTERVAL_MS {
                return Err(DomainError::invalid(
                    "network.packet inject interval_ms is out of bounds",
                ));
            }
            match packet {
                PacketSource::Raw { raw_base64 } => {
                    let bytes =
                        decode_base64(raw_base64, "network.packet inject raw_base64 is invalid")?;
                    require_packet_bytes(
                        bytes.len(),
                        "network.packet inject packet length is out of bounds",
                    )?;
                }
                PacketSource::Ref { packet_ref } => {
                    require_ref(
                        packet_ref,
                        "packet",
                        "network.packet inject packet_ref is invalid",
                    )?;
                }
            }
        }
    }
    Ok(())
}

fn validate_diagnose_input(input: &NetworkDiagnoseInput) -> Result<(), DomainError> {
    match input {
        NetworkDiagnoseInput::Connectivity {} => {}
        NetworkDiagnoseInput::Dns { name, .. } => {
            require_name(name, "network.diagnose name is out of bounds")?;
        }
        NetworkDiagnoseInput::Tcp {
            host,
            port,
            timeout_ms,
        } => {
            require_name(host, "network.diagnose host is out of bounds")?;
            if *port == 0 || !diagnose_timeout_in_bounds(*timeout_ms) {
                return Err(DomainError::invalid(
                    "network.diagnose tcp parameters are out of bounds",
                ));
            }
        }
        NetworkDiagnoseInput::Tls {
            host,
            port,
            server_name,
            timeout_ms,
        } => {
            require_name(host, "network.diagnose host is out of bounds")?;
            if let Some(server_name) = server_name {
                require_name(server_name, "network.diagnose server_name is out of bounds")?;
            }
            if *port == 0 || !diagnose_timeout_in_bounds(*timeout_ms) {
                return Err(DomainError::invalid(
                    "network.diagnose tls parameters are out of bounds",
                ));
            }
        }
        NetworkDiagnoseInput::Route { destination_ip } => {
            require_name(
                destination_ip,
                "network.diagnose destination_ip is out of bounds",
            )?;
        }
    }
    Ok(())
}

fn require_name(value: &str, reason: &'static str) -> Result<(), DomainError> {
    if value.is_empty() || value.contains('\0') {
        return Err(DomainError::invalid(reason));
    }
    Ok(())
}

/// A family's data comes from the artifact store or a capture, so a reference names the
/// kind the request is about rather than any opaque string.
fn require_ref(value: &str, kind: &str, reason: &'static str) -> Result<(), DomainError> {
    let prefix = format!("dbref:{kind}:");
    if !value.starts_with(&prefix) {
        return Err(DomainError::invalid(reason));
    }
    Ok(())
}

const fn diagnose_timeout_in_bounds(timeout_ms: u64) -> bool {
    timeout_ms >= NETWORK_MIN_DIAGNOSE_TIMEOUT_MS && timeout_ms <= NETWORK_MAX_DIAGNOSE_TIMEOUT_MS
}

fn require_packet_bytes(length: usize, reason: &'static str) -> Result<(), DomainError> {
    if !(NETWORK_MIN_PACKET_BYTES..=NETWORK_MAX_PACKET_BYTES).contains(&length) {
        return Err(DomainError::invalid(reason));
    }
    Ok(())
}

/// The route S-AUTH-NET-001 admits for one Network call, fixed before any query, connect or
/// injection reaches a provider.
pub fn network_executor_request(
    capability: &CapabilitySnapshot,
    call: &NetworkCall,
) -> Result<ExecutorRequest, DomainError> {
    validate_network_input(call)?;
    if capability.context.readiness != contract::RuntimeReadiness::Ready {
        return Err(DomainError::new(
            ErrorCode::CapabilityUnavailable,
            "Runtime is not ready for network execution",
        ));
    }
    let request = ExecutorRequest::Network(match call {
        NetworkCall::Inspect(_)
        | NetworkCall::Diagnose(_)
        | NetworkCall::Capture(NetworkCaptureInput::Read { .. })
        | NetworkCall::Packet(NetworkPacketInput::Decode { .. })
        | NetworkCall::Packet(NetworkPacketInput::Build { .. }) => NetworkRoute::InspectOrDiagnose,
        NetworkCall::Capture(_) => NetworkRoute::Capture,
        NetworkCall::Packet(NetworkPacketInput::Inject { .. }) => NetworkRoute::Inject,
    });
    crate::resolve_execution(capability, request)?;
    Ok(request)
}

/// R-NET-011: an APK-served operation whose stated target is a LAN address needs the local
/// network authority on Android 17/API37 before its side effect. A Magisk-served operation
/// is never subject to the App grant, and neither same-device loopback nor public-Internet
/// access is a LAN feature. This is the single site for that rule: it runs on one capability
/// observation before task admission and before any primitive, so no admitted execution can
/// bypass it.
fn require_local_network_authority(
    capability: &CapabilitySnapshot,
    route: ExecutorRequest,
    call: &NetworkCall,
) -> Result<(), DomainError> {
    if crate::resolve_execution(capability, route)?.provider() != Provider::AppNative
        || !targets_local_network(call)
    {
        return Ok(());
    }
    if derive_capabilities(&capability.grants, capability.context)?
        .network_local
        .state
        == CapabilityState::Available
    {
        return Ok(());
    }
    Err(DomainError::new(
        ErrorCode::CapabilityUnavailable,
        "Android 17 local-network access is not available to the App executor",
    ))
}

/// Only a literal LAN target address is API37 LAN access. Loopback is the same-device MCP
/// listener, a hostname is resolved by the provider, and `connectivity`/`dns` name no target
/// at all.
fn targets_local_network(call: &NetworkCall) -> bool {
    let target = match call {
        NetworkCall::Diagnose(NetworkDiagnoseInput::Tcp { host, .. })
        | NetworkCall::Diagnose(NetworkDiagnoseInput::Tls { host, .. }) => host,
        NetworkCall::Diagnose(NetworkDiagnoseInput::Route { destination_ip }) => destination_ip,
        _ => return false,
    };
    target.parse::<IpAddr>().is_ok_and(is_lan_address)
}

fn is_lan_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(ipv4) => ipv4.is_private() || ipv4.is_link_local(),
        IpAddr::V6(ipv6) => {
            let segments = ipv6.segments();
            segments[0] & 0xffc0 == 0xfe80 || segments[0] & 0xfe00 == 0xfc00
        }
    }
}

#[derive(Clone)]
pub struct NativeNetworkExecutionSurface<A, C, P> {
    artifacts: A,
    capabilities: C,
    port: P,
    claims: LocalExecutionClaims,
}

impl<A, C, P> NativeNetworkExecutionSurface<A, C, P> {
    pub fn new(artifacts: A, capabilities: C, port: P) -> Self {
        Self {
            artifacts,
            capabilities,
            port,
            claims: LocalExecutionClaims::default(),
        }
    }
}

impl<A, C, P> ExecutionPort for NativeNetworkExecutionSurface<A, C, P>
where
    A: ArtifactPort + Clone + 'static,
    C: CapabilityPort + Clone + 'static,
    P: NetworkPrimitivePort + Clone + 'static,
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
        let port = self.port.clone();
        Box::pin(async move {
            let result = execute_network(artifacts, capabilities, port, &execution, &claim).await;
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

async fn execute_network<A, C, P>(
    artifacts: A,
    capabilities: C,
    port: P,
    execution: &AdmittedExecution,
    claim: &LocalExecutionClaim,
) -> Result<ExecutionCompletion, ExecutionFailure>
where
    A: ArtifactPort,
    C: CapabilityPort,
    P: NetworkPrimitivePort + Clone + 'static,
{
    let call = match &execution.payload {
        ExecutionPayload::NetworkCall(call) => call.clone(),
        _ => {
            return Err(execution_failure(
                ErrorCode::Unsupported,
                "native network surface received a non-network request",
                true,
            ));
        }
    };
    if let Err(error) = validate_network_input(&call) {
        return Err(ExecutionFailure {
            error,
            cleanup_verified: true,
        });
    }
    let current = capabilities.current().map_err(|error| ExecutionFailure {
        error,
        cleanup_verified: true,
    })?;
    if !network_executor_is_current(&current, execution, execution.executor.provider) {
        return Err(execution_failure(
            ErrorCode::StaleAuthority,
            "network executor fence or generation is stale",
            true,
        ));
    }
    claim.checkpoint().map_err(|error| ExecutionFailure {
        error,
        cleanup_verified: true,
    })?;
    if runs_inline(&call) {
        return execute_inline_network(&artifacts, execution, claim, &call).await;
    }
    let plan = network_source_plan(&current, execution.executor.provider);
    let request = match match_primitive(&artifacts, &call, execution, plan) {
        Ok(request) => request,
        Err(failure) => return Err(failure),
    };
    let settlement =
        match run_network_blocking(port, execution.clone(), request, claim.clone()).await {
            Ok(settlement) => settlement,
            Err(failure) => {
                if failure.error.code == ErrorCode::Cancelled {
                    return Ok(network_cancellation(
                        execution,
                        claim,
                        failure.cleanup_verified,
                    ));
                }
                return Err(failure);
            }
        };
    if is_capture_cancellation(&settlement.outcome) {
        return Ok(network_cancellation(
            execution,
            claim,
            settlement.cleanup_verified,
        ));
    }
    let cleanup_verified = settlement.cleanup_verified && claim.cleanup_is_verified();
    if !cleanup_verified {
        return Err(ExecutionFailure {
            error: DomainError::new(
                ErrorCode::IoError,
                "network primitive cleanup is unverified",
            ),
            cleanup_verified: false,
        });
    }
    let outcome = match network_outcome(settlement.outcome, &call, execution, plan) {
        Ok(outcome) => outcome,
        Err(error) => {
            if error.code == ErrorCode::Cancelled {
                return Ok(network_cancellation(execution, claim, cleanup_verified));
            }
            return Err(ExecutionFailure {
                error,
                cleanup_verified,
            });
        }
    };
    if !outcome_fits(&outcome) {
        return Err(ExecutionFailure {
            error: DomainError::new(
                ErrorCode::ResourceLimit,
                "network result exceeds the protocol frame limit",
            ),
            cleanup_verified,
        });
    }
    Ok(ExecutionCompletion {
        fence: execution_fence(execution),
        capability_generation: execution.executor.capability_generation,
        outcome,
        cleanup_verified,
    })
}

fn match_primitive<A: ArtifactPort>(
    artifacts: &A,
    call: &NetworkCall,
    execution: &AdmittedExecution,
    plan: NetworkSourcePlan,
) -> Result<NetworkPrimitiveRequest, ExecutionFailure> {
    let request = match call {
        NetworkCall::Inspect(input) => NetworkPrimitiveRequest::Inspect {
            plan,
            scope: input.scope,
            max_entries: input.max_entries,
        },
        NetworkCall::Diagnose(input) => NetworkPrimitiveRequest::Diagnose(input.clone(), plan),
        NetworkCall::Capture(NetworkCaptureInput::Start {
            interface,
            filter,
            max_packets,
            max_bytes,
            max_duration_ms,
            persist_to,
        }) => {
            let Some(capture_id) = execution.task_id.clone() else {
                return Err(execution_failure(
                    ErrorCode::InternalError,
                    "network capture start requires a Task identity",
                    true,
                ));
            };
            NetworkPrimitiveRequest::CaptureStart {
                capture_id,
                interface: interface.clone(),
                filter: filter.clone(),
                max_packets: *max_packets,
                max_bytes: *max_bytes,
                max_duration_ms: *max_duration_ms,
                persist_to: persist_to.clone(),
            }
        }
        NetworkCall::Capture(NetworkCaptureInput::Stop { capture_id }) => {
            NetworkPrimitiveRequest::CaptureStop {
                capture_id: capture_id.clone(),
            }
        }
        NetworkCall::Capture(NetworkCaptureInput::Read {
            source: CaptureReadSource::File { file },
            ..
        }) => NetworkPrimitiveRequest::CaptureFileBytes {
            target: file.clone(),
        },
        NetworkCall::Packet(NetworkPacketInput::Inject {
            interface,
            packet,
            count,
            interval_ms,
        }) => {
            let packet = match packet {
                PacketSource::Raw { raw_base64 } => {
                    decode_base64(raw_base64, "network.packet inject raw_base64 is invalid")
                }
                PacketSource::Ref { packet_ref } => artifacts.open(packet_ref),
            }
            .map_err(|error| ExecutionFailure {
                error,
                cleanup_verified: true,
            })?;
            require_packet_bytes(
                packet.len(),
                "network.packet inject packet length is out of bounds",
            )
            .map_err(|error| ExecutionFailure {
                error,
                cleanup_verified: true,
            })?;
            NetworkPrimitiveRequest::PacketInject {
                interface: interface.clone(),
                packet,
                count: *count,
                interval_ms: *interval_ms,
            }
        }
        _ => {
            return Err(execution_failure(
                ErrorCode::InternalError,
                "network request has no host primitive",
                true,
            ));
        }
    };
    Ok(request)
}

fn is_capture_cancellation(outcome: &NetworkPrimitiveOutcome) -> bool {
    matches!(
        outcome,
        NetworkPrimitiveOutcome::CaptureSettled(CaptureSettlement {
            cancelled: true,
            ..
        })
    )
}

/// `packet.decode`, `packet.build` and a `capture.read` of a published capture are wire
/// mechanics over bytes the Runtime already owns, so every host handles them identically
/// inside the Runtime (S-NET-004).
const fn runs_inline(call: &NetworkCall) -> bool {
    matches!(
        call,
        NetworkCall::Packet(NetworkPacketInput::Decode { .. })
            | NetworkCall::Packet(NetworkPacketInput::Build { .. })
            | NetworkCall::Capture(NetworkCaptureInput::Read {
                source: CaptureReadSource::CaptureRef { .. },
                ..
            })
    )
}

async fn execute_inline_network<A: ArtifactPort>(
    artifacts: &A,
    execution: &AdmittedExecution,
    claim: &LocalExecutionClaim,
    call: &NetworkCall,
) -> Result<ExecutionCompletion, ExecutionFailure> {
    let value = match call {
        NetworkCall::Packet(NetworkPacketInput::Decode { source }) => {
            let bytes = open_decode_source(artifacts, source)?;
            require_packet_bytes(
                bytes.len(),
                "network.packet decode packet length is out of bounds",
            )
            .map_err(|error| ExecutionFailure {
                error,
                cleanup_verified: true,
            })?;
            serde_json::to_value(decode_packet(&bytes)).map_err(|_| encoding_failure())
        }
        NetworkCall::Packet(NetworkPacketInput::Build {
            ethernet,
            network,
            transport,
            payload_base64,
        }) => {
            let buffer = build_packet(ethernet, network, transport, payload_base64.as_deref())?;
            let metadata = claim
                .publish(|| artifacts.publish_as("packet", &buffer))
                .map_err(|error| ExecutionFailure {
                    error,
                    cleanup_verified: true,
                })?;
            serde_json::to_value(PacketBuildResult {
                packet_ref: metadata.artifact_ref,
                length: buffer.len() as u64,
                sha256: metadata.sha256,
            })
            .map_err(|_| encoding_failure())
        }
        NetworkCall::Capture(NetworkCaptureInput::Read {
            source: CaptureReadSource::CaptureRef { capture_ref },
            offset_packet,
            max_packets,
            include_payload,
        }) => {
            let bytes = artifacts
                .open(capture_ref)
                .map_err(|error| ExecutionFailure {
                    error,
                    cleanup_verified: true,
                })?;
            let records = read_pcap(&bytes).map_err(|error| ExecutionFailure {
                error,
                cleanup_verified: true,
            })?;
            read_capture_result(&records, *offset_packet, *max_packets, *include_payload)
                .and_then(|result| serde_json::to_value(result).map_err(|_| encoding_failure()))
        }
        _ => {
            return Err(execution_failure(
                ErrorCode::InternalError,
                "network request has no inline handler",
                true,
            ));
        }
    }
    .map_err(|error| ExecutionFailure {
        error,
        cleanup_verified: true,
    })?;
    let encoded_bytes = encoded_len(&value).map_err(|error| ExecutionFailure {
        error,
        cleanup_verified: true,
    })?;
    if !encoded_fits(&value) {
        return Err(ExecutionFailure {
            error: DomainError::new(
                ErrorCode::ResourceLimit,
                "network result exceeds the protocol frame limit",
            ),
            cleanup_verified: true,
        });
    }
    Ok(ExecutionCompletion {
        fence: execution_fence(execution),
        capability_generation: execution.executor.capability_generation,
        outcome: ExecutionOutcome::SynchronousCompleted {
            result: value,
            encoded_bytes,
        },
        cleanup_verified: true,
    })
}

fn open_decode_source<A: ArtifactPort>(
    artifacts: &A,
    source: &PacketDecodeSource,
) -> Result<Vec<u8>, ExecutionFailure> {
    let failure = |error| ExecutionFailure {
        error,
        cleanup_verified: true,
    };
    match source {
        PacketDecodeSource::Raw { raw_base64 } => {
            decode_base64(raw_base64, "network.packet decode raw_base64 is invalid")
                .map_err(failure)
        }
        PacketDecodeSource::PacketRef { packet_ref } => artifacts.open(packet_ref).map_err(failure),
        PacketDecodeSource::Capture { capture_ref, index } => {
            let bytes = artifacts.open(capture_ref).map_err(failure)?;
            let records = read_pcap(&bytes).map_err(failure)?;
            records
                .into_iter()
                .find(|record| record.index == *index)
                .map(|record| record.bytes)
                .ok_or_else(|| {
                    failure(DomainError::new(
                        ErrorCode::NotFound,
                        "capture packet index is not present",
                    ))
                })
        }
    }
}

/// One packet occupies its own blocking thread: a capture holds it for as long as
/// `max_duration_ms`, and a caller-named capture file is bounded blocking I/O.
async fn run_network_blocking<P: NetworkPrimitivePort + Clone + 'static>(
    port: P,
    execution: AdmittedExecution,
    request: NetworkPrimitiveRequest,
    claim: LocalExecutionClaim,
) -> Result<NetworkPrimitiveSettlement, ExecutionFailure> {
    tokio::task::spawn_blocking(move || port.run(&execution, request, &claim))
        .await
        .map_err(|_| {
            execution_failure(
                ErrorCode::InternalError,
                "network primitive did not report a settlement",
                false,
            )
        })?
}

fn network_executor_is_current(
    current: &CapabilitySnapshot,
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
            current.context.app_execution_surface == CapabilityState::Available
                && current.resolver_facts.app_native == CapabilityState::Available
                && execution.executor.capability_generation == current.fence.host_generation
        }
        ProviderToken::Shizuku => {
            current.resolver_facts.shizuku == CapabilityState::Available
                && execution.executor.capability_generation
                    == current.resolver_facts.generations.shizuku
        }
        ProviderToken::MagiskNative => {
            current.context.host == contract::RuntimeHost::MagiskBackend
                && current.resolver_facts.magisk_native == CapabilityState::Available
                && execution.executor.capability_generation
                    == current.resolver_facts.generations.magisk_native
        }
        _ => false,
    }
}

fn network_cancellation(
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
                operation: "network.capture".to_owned(),
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

/// The R-NET-002..009 result of one settled primitive. `capture_id` is the capture Task's
/// own identity, so capture ownership stays one-to-one with zero extra state.
fn network_outcome(
    outcome: NetworkPrimitiveOutcome,
    call: &NetworkCall,
    execution: &AdmittedExecution,
    plan: NetworkSourcePlan,
) -> Result<ExecutionOutcome, DomainError> {
    let value = match outcome {
        NetworkPrimitiveOutcome::Inspect(settlement) => {
            encode_result(assemble_inspect(call, plan, settlement)?)
        }
        NetworkPrimitiveOutcome::Diagnose(result) => encode_result(result),
        NetworkPrimitiveOutcome::PacketInjected(result) => encode_result(result),
        NetworkPrimitiveOutcome::CaptureFileBytes(bytes) => {
            let NetworkCall::Capture(NetworkCaptureInput::Read {
                offset_packet,
                max_packets,
                include_payload,
                ..
            }) = call
            else {
                return Err(DomainError::invalid(
                    "network.capture read was not requested",
                ));
            };
            let records = read_pcap(&bytes)?;
            encode_result(read_capture_result(
                &records,
                *offset_packet,
                *max_packets,
                *include_payload,
            )?)
        }
        NetworkPrimitiveOutcome::CaptureStopRequested => {
            encode_result(serde_json::json!({ "operation": "stop" }))
        }
        NetworkPrimitiveOutcome::CaptureSettled(settlement) => {
            let Some(capture_id) = execution.task_id.clone() else {
                return Err(DomainError::new(
                    ErrorCode::InternalError,
                    "network capture settlement requires a Task identity",
                ));
            };
            let terminal = TaskTerminalResult::NetworkCapture(CaptureResult {
                operation: CaptureResultOperation::CaptureResult,
                capture_id,
                packets_captured: settlement.packets_captured,
                bytes_captured: settlement.bytes_captured,
                capture_ref: settlement.capture_ref,
                destination: settlement.destination,
            });
            let encoded_bytes = encoded_len(&terminal)?;
            return Ok(ExecutionOutcome::Completed {
                result: terminal,
                encoded_bytes,
            });
        }
    };
    let value = value?;
    let encoded_bytes = encoded_len(&value)?;
    Ok(ExecutionOutcome::SynchronousCompleted {
        result: value,
        encoded_bytes,
    })
}

fn outcome_fits(outcome: &ExecutionOutcome) -> bool {
    match outcome {
        ExecutionOutcome::SynchronousCompleted { encoded_bytes, .. }
        | ExecutionOutcome::Completed { encoded_bytes, .. }
        | ExecutionOutcome::Failed { encoded_bytes, .. }
        | ExecutionOutcome::Cancelled { encoded_bytes, .. } => {
            *encoded_bytes as usize + MAX_INLINE_ENVELOPE_OVERHEAD <= UI_ENVELOPE_LIMIT_BYTES
        }
    }
}

fn reason_encoding() -> &'static str {
    "network result encoding failed"
}

fn encoding_failure() -> DomainError {
    DomainError::new(ErrorCode::InternalError, reason_encoding())
}

/// S-NET-004's serialization step is the only place a result can fail to encode, so a
/// damaged capture stream or an oversized read window keeps its own error code instead of
/// collapsing into an encoding failure.
fn encode_result<T: serde::Serialize>(value: T) -> Result<serde_json::Value, DomainError> {
    serde_json::to_value(value).map_err(|_| encoding_failure())
}

fn decode_base64(encoded: &str, reason: &'static str) -> Result<Vec<u8>, DomainError> {
    use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
    BASE64
        .decode(encoded)
        .map_err(|_| DomainError::invalid(reason))
}

fn encode_base64(bytes: &[u8]) -> String {
    use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
    BASE64.encode(bytes)
}

fn parse_mac(value: &str, reason: &'static str) -> Result<[u8; 6], DomainError> {
    let parts = value.split(':').collect::<Vec<_>>();
    if parts.len() != 6 {
        return Err(DomainError::invalid(reason));
    }
    let mut mac = [0u8; 6];
    for (index, part) in parts.iter().enumerate() {
        if part.len() != 2
            || !part
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(DomainError::invalid(reason));
        }
        mac[index] = u8::from_str_radix(part, 16).map_err(|_| DomainError::invalid(reason))?;
    }
    Ok(mac)
}

fn format_mac(mac: [u8; 6]) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

fn parse_ipv4(value: &str, reason: &'static str) -> Result<Ipv4Addr, DomainError> {
    value
        .parse::<Ipv4Addr>()
        .map_err(|_| DomainError::invalid(reason))
}

fn parse_ipv6(value: &str, reason: &'static str) -> Result<Ipv6Addr, DomainError> {
    value
        .parse::<Ipv6Addr>()
        .map_err(|_| DomainError::invalid(reason))
}

/// R-NET-007: the flag names are the canonical R-NET-006 set, each at most once, so the
/// builder cannot express a flag the decoder cannot report.
fn tcp_flag_mask(flags: &[String]) -> Result<u8, DomainError> {
    let mut mask = 0u8;
    for flag in flags {
        let Some(index) = TCP_FLAG_ORDER.iter().position(|known| known == flag) else {
            return Err(DomainError::invalid(
                "network.packet build tcp flags are invalid",
            ));
        };
        if mask & (1 << index) != 0 {
            return Err(DomainError::invalid(
                "network.packet build tcp flags are not unique",
            ));
        }
        mask |= 1 << index;
    }
    Ok(mask)
}

/// S-NET-004: etherparse owns the wire structures and every dependent length, protocol
/// number and checksum is computed here, so no caller-supplied header byte survives.
fn build_packet(
    ethernet: &Option<EthernetBuild>,
    network: &NetworkBuild,
    transport: &TransportBuild,
    payload_base64: Option<&str>,
) -> Result<Vec<u8>, ExecutionFailure> {
    let failed = |error| ExecutionFailure {
        error,
        cleanup_verified: true,
    };
    let payload = match payload_base64 {
        Some(encoded) => decode_base64(encoded, "network.packet build payload_base64 is invalid")
            .map_err(failed)?,
        None => Vec::new(),
    };
    let ip_number = match transport {
        TransportBuild::Tcp { .. } => etherparse::IpNumber::TCP,
        TransportBuild::Udp { .. } => etherparse::IpNumber::UDP,
        TransportBuild::Icmp { .. } => match network {
            NetworkBuild::Ipv4 { .. } => etherparse::IpNumber::ICMP,
            NetworkBuild::Ipv6 { .. } => etherparse::IpNumber::IPV6_ICMP,
        },
    };
    let headers = match network {
        NetworkBuild::Ipv4 {
            src,
            dst,
            ttl,
            identification,
            dont_fragment,
        } => {
            let src = parse_ipv4(src, "network.packet build src is invalid").map_err(failed)?;
            let dst = parse_ipv4(dst, "network.packet build dst is invalid").map_err(failed)?;
            let mut header =
                etherparse::Ipv4Header::new(0, *ttl, ip_number, src.octets(), dst.octets())
                    .map_err(|_| {
                        failed(DomainError::invalid(
                            "network.packet build ipv4 header is out of bounds",
                        ))
                    })?;
            header.identification = *identification;
            header.dont_fragment = *dont_fragment;
            etherparse::IpHeaders::Ipv4(header, etherparse::Ipv4Extensions::default())
        }
        NetworkBuild::Ipv6 {
            src,
            dst,
            hop_limit,
        } => {
            let src = parse_ipv6(src, "network.packet build src is invalid").map_err(failed)?;
            let dst = parse_ipv6(dst, "network.packet build dst is invalid").map_err(failed)?;
            etherparse::IpHeaders::Ipv6(
                etherparse::Ipv6Header {
                    traffic_class: 0,
                    flow_label: etherparse::Ipv6FlowLabel::ZERO,
                    payload_length: 0,
                    next_header: ip_number,
                    hop_limit: *hop_limit,
                    source: src.octets(),
                    destination: dst.octets(),
                },
                etherparse::Ipv6Extensions::default(),
            )
        }
    };
    let builder = match ethernet {
        Some(EthernetBuild { src_mac, dst_mac }) => {
            let src =
                parse_mac(src_mac, "network.packet build src_mac is invalid").map_err(failed)?;
            let dst =
                parse_mac(dst_mac, "network.packet build dst_mac is invalid").map_err(failed)?;
            etherparse::PacketBuilder::ethernet2(src, dst).ip(headers)
        }
        None => etherparse::PacketBuilder::ip(headers),
    };
    let mut buffer = Vec::new();
    let built = match transport {
        TransportBuild::Tcp {
            src_port,
            dst_port,
            sequence,
            acknowledgement,
            flags,
            window,
        } => {
            let mask = tcp_flag_mask(flags).map_err(failed)?;
            let mut header = etherparse::TcpHeader::new(*src_port, *dst_port, *sequence, *window);
            header.acknowledgment_number = *acknowledgement;
            header.fin = mask & 1 << 0 != 0;
            header.syn = mask & 1 << 1 != 0;
            header.rst = mask & 1 << 2 != 0;
            header.psh = mask & 1 << 3 != 0;
            header.ack = mask & 1 << 4 != 0;
            header.urg = mask & 1 << 5 != 0;
            header.ece = mask & 1 << 6 != 0;
            header.cwr = mask & 1 << 7 != 0;
            builder
                .tcp_header(header)
                .write_to_vec(&mut buffer, &payload)
        }
        TransportBuild::Udp { src_port, dst_port } => builder
            .udp(*src_port, *dst_port)
            .write_to_vec(&mut buffer, &payload),
        TransportBuild::Icmp { icmp_type, code } => match network {
            NetworkBuild::Ipv4 { .. } => builder
                .icmpv4_raw(*icmp_type, *code, [0u8; 4])
                .write_to_vec(&mut buffer, &payload),
            NetworkBuild::Ipv6 { .. } => builder
                .icmpv6_raw(*icmp_type, *code, [0u8; 4])
                .write_to_vec(&mut buffer, &payload),
        },
    };
    built.map_err(|_| {
        failed(DomainError::invalid(
            "network.packet build packet is not representable",
        ))
    })?;
    Ok(buffer)
}

/// R-NET-006 decodes only the layers the bytes actually establish, over one deterministic
/// link-layer rule, and returns at most the bounded payload preview.
pub fn decode_packet(bytes: &[u8]) -> PacketDecodeResult {
    let mut result = PacketDecodeResult {
        length: bytes.len() as u64,
        ethernet: None,
        ipv4: None,
        ipv6: None,
        tcp: None,
        udp: None,
        icmp: None,
        payload_preview_base64: String::new(),
        payload_total_bytes: 0,
        payload_truncated: false,
    };
    let mut payload = bytes;
    let sliced = match link_layer(bytes) {
        LinkLayer::Ethernet => etherparse::SlicedPacket::from_ethernet(bytes).ok(),
        LinkLayer::Ip => etherparse::SlicedPacket::from_ip(bytes).ok(),
        LinkLayer::Unknown => None,
    };
    if let Some(sliced) = &sliced {
        if let Some(etherparse::LinkSlice::Ethernet2(link)) = &sliced.link {
            result.ethernet = Some(EthernetHeader {
                src_mac: format_mac(link.source()),
                dst_mac: format_mac(link.destination()),
                ether_type: link.ether_type().0,
            });
            payload = link.payload().payload;
        }
        match &sliced.net {
            Some(etherparse::NetSlice::Ipv4(ip)) => {
                let header = ip.header();
                result.ipv4 = Some(Ipv4Header {
                    src: header.source_addr().to_string(),
                    dst: header.destination_addr().to_string(),
                    ttl: header.ttl(),
                    protocol: header.protocol().0,
                    identification: header.identification(),
                    dont_fragment: header.dont_fragment(),
                });
                payload = ip.payload().payload;
            }
            Some(etherparse::NetSlice::Ipv6(ip)) => {
                let header = ip.header();
                result.ipv6 = Some(Ipv6Header {
                    src: header.source_addr().to_string(),
                    dst: header.destination_addr().to_string(),
                    hop_limit: header.hop_limit(),
                    next_header: header.next_header().0,
                });
                payload = ip.payload().payload;
            }
            _ => {}
        }
        match &sliced.transport {
            Some(etherparse::TransportSlice::Tcp(tcp)) => {
                result.tcp = Some(TcpHeader {
                    src_port: tcp.source_port(),
                    dst_port: tcp.destination_port(),
                    sequence: tcp.sequence_number(),
                    acknowledgement: tcp.acknowledgment_number(),
                    flags: tcp_flags(tcp),
                    window: tcp.window_size(),
                });
                payload = tcp.payload();
            }
            Some(etherparse::TransportSlice::Udp(udp)) => {
                result.udp = Some(UdpHeader {
                    src_port: udp.source_port(),
                    dst_port: udp.destination_port(),
                });
                payload = udp.payload();
            }
            Some(etherparse::TransportSlice::Icmpv4(icmp)) => {
                result.icmp = Some(IcmpHeader {
                    icmp_type: icmp.type_u8(),
                    code: icmp.code_u8(),
                });
                payload = icmp.payload();
            }
            Some(etherparse::TransportSlice::Icmpv6(icmp)) => {
                result.icmp = Some(IcmpHeader {
                    icmp_type: icmp.type_u8(),
                    code: icmp.code_u8(),
                });
                payload = icmp.payload();
            }
            _ => {}
        }
    }
    let preview_len = payload.len().min(NETWORK_PAYLOAD_PREVIEW_BYTES);
    result.payload_preview_base64 = encode_base64(&payload[..preview_len]);
    result.payload_total_bytes = payload.len() as u64;
    result.payload_truncated = preview_len < payload.len();
    result
}

fn tcp_flags(tcp: &etherparse::TcpSlice<'_>) -> Vec<String> {
    let set = [
        tcp.fin(),
        tcp.syn(),
        tcp.rst(),
        tcp.psh(),
        tcp.ack(),
        tcp.urg(),
        tcp.ece(),
        tcp.cwr(),
    ];
    TCP_FLAG_ORDER
        .iter()
        .zip(set)
        .filter(|(_, set)| *set)
        .map(|(name, _)| (*name).to_owned())
        .collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LinkLayer {
    Ethernet,
    Ip,
    Unknown,
}

/// A bare IP buffer must never be misread as Ethernet and an Ethernet frame must not be
/// misread as raw IP: a self-consistent raw IP packet wins, then a frame whose EtherType and
/// embedded IP agree, then the remaining IP version nibble.
fn link_layer(bytes: &[u8]) -> LinkLayer {
    let ip_exact = match bytes.first().map(|byte| byte >> 4) {
        Some(4) => {
            bytes.len() >= 20 && u16::from_be_bytes([bytes[2], bytes[3]]) as usize == bytes.len()
        }
        Some(6) => {
            bytes.len() >= 40
                && 40 + u16::from_be_bytes([bytes[4], bytes[5]]) as usize == bytes.len()
        }
        _ => false,
    };
    if ip_exact {
        return LinkLayer::Ip;
    }
    if looks_like_ethernet(bytes) {
        return LinkLayer::Ethernet;
    }
    if matches!(bytes.first().map(|byte| byte >> 4), Some(4) | Some(6)) {
        return LinkLayer::Ip;
    }
    LinkLayer::Unknown
}

fn looks_like_ethernet(bytes: &[u8]) -> bool {
    if bytes.len() < 14 {
        return false;
    }
    let inner = &bytes[14..];
    match u16::from_be_bytes([bytes[12], bytes[13]]) {
        0x0800 => {
            inner.len() >= 20
                && inner[0] >> 4 == 4
                && u16::from_be_bytes([inner[2], inner[3]]) as usize <= inner.len()
        }
        0x86DD => inner.first().map(|byte| byte >> 4) == Some(6),
        0x0806 => true,
        _ => false,
    }
}

/// One classic-PCAP little-endian microsecond record (S-NET-005). This module is the one
/// owner of that format: the daemon's capture writer and every `capture.read` share it.
#[derive(Clone, Debug, PartialEq)]
pub struct PcapRecord {
    pub index: u64,
    pub seconds: u32,
    pub microseconds: u32,
    pub original_len: u32,
    pub bytes: Vec<u8>,
}

/// S-NET-005 classic-PCAP little-endian microsecond file header for a stream whose records
/// carry `link_type`. The link type is the capturing device's own fact, so a capture that
/// declares a link type its records do not have is not a stream this format can express.
pub fn pcap_file_header(link_type: u32) -> [u8; PCAP_FILE_HEADER_BYTES] {
    let mut header = [0u8; PCAP_FILE_HEADER_BYTES];
    header[0..4].copy_from_slice(&PCAP_MAGIC_MICROSECOND.to_le_bytes());
    header[4..6].copy_from_slice(&2u16.to_le_bytes());
    header[6..8].copy_from_slice(&4u16.to_le_bytes());
    header[16..20].copy_from_slice(&PCAP_SNAPLEN.to_le_bytes());
    header[20..24].copy_from_slice(&link_type.to_le_bytes());
    header
}

pub fn pcap_record_header(
    seconds: u32,
    microseconds: u32,
    captured_len: u32,
    original_len: u32,
) -> [u8; PCAP_RECORD_HEADER_BYTES] {
    let mut header = [0u8; PCAP_RECORD_HEADER_BYTES];
    header[0..4].copy_from_slice(&seconds.to_le_bytes());
    header[4..8].copy_from_slice(&microseconds.to_le_bytes());
    header[8..12].copy_from_slice(&captured_len.to_le_bytes());
    header[12..16].copy_from_slice(&original_len.to_le_bytes());
    header
}

/// One complete classic-PCAP little-endian microsecond stream. A record whose header or
/// bytes are incomplete is a damaged capture rather than a short read.
pub fn read_pcap(bytes: &[u8]) -> Result<Vec<PcapRecord>, DomainError> {
    let damaged = |reason: &'static str| DomainError::new(ErrorCode::CaptureFailed, reason);
    if bytes.len() < PCAP_FILE_HEADER_BYTES
        || u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) != PCAP_MAGIC_MICROSECOND
    {
        return Err(damaged("capture stream is not a classic PCAP file"));
    }
    let mut records = Vec::new();
    let mut offset = PCAP_FILE_HEADER_BYTES;
    while offset < bytes.len() {
        if bytes.len() - offset < PCAP_RECORD_HEADER_BYTES {
            return Err(damaged("capture stream has an incomplete record header"));
        }
        let header = &bytes[offset..offset + PCAP_RECORD_HEADER_BYTES];
        let seconds = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
        let microseconds = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
        let captured_len = u32::from_le_bytes([header[8], header[9], header[10], header[11]]);
        let original_len = u32::from_le_bytes([header[12], header[13], header[14], header[15]]);
        offset += PCAP_RECORD_HEADER_BYTES;
        let captured_len = captured_len as usize;
        if bytes.len() - offset < captured_len {
            return Err(damaged("capture stream has an incomplete record"));
        }
        records.push(PcapRecord {
            index: records.len() as u64,
            seconds,
            microseconds,
            original_len,
            bytes: bytes[offset..offset + captured_len].to_vec(),
        });
        offset += captured_len;
    }
    Ok(records)
}

/// The bounded R-NET-005 window over a capture, as far as the frame limit allows.
fn read_capture_result(
    records: &[PcapRecord],
    offset_packet: u64,
    max_packets: u32,
    include_payload: bool,
) -> Result<CaptureReadResult, DomainError> {
    let start = usize::try_from(offset_packet)
        .unwrap_or(usize::MAX)
        .min(records.len());
    let end = start
        .saturating_add(max_packets as usize)
        .min(records.len());
    let packets = records[start..end]
        .iter()
        .map(|record| packet_summary(record, include_payload))
        .collect::<Vec<_>>();
    fit_capture_read(
        packets,
        offset_packet,
        end < records.len(),
        "network.capture read result exceeds the protocol frame limit",
    )
}

/// R-NET-005 keeps one response inside the S-CONTRACT-002 frame: the window carries as many
/// complete summaries as fit, and the continuation facts report the rest. `truncated` and
/// `next_offset_packet` are driven by the same condition, so a caller that follows the
/// cursor to the end never sees a claim of truncation.
fn fit_capture_read(
    packets: Vec<PacketSummary>,
    offset_packet: u64,
    more_beyond: bool,
    reason: &'static str,
) -> Result<CaptureReadResult, DomainError> {
    let total = packets.len();
    let candidate = |count: usize| {
        let more = count < total || more_beyond;
        CaptureReadResult {
            packets: packets[..count].to_vec(),
            next_offset_packet: more.then(|| offset_packet.saturating_add(count as u64)),
            truncated: more,
        }
    };
    let Some(count) = largest_fitting(total, |count| encoded_fits(&candidate(count))) else {
        return Err(DomainError::new(ErrorCode::ResourceLimit, reason));
    };
    Ok(candidate(count))
}

fn packet_summary(record: &PcapRecord, include_payload: bool) -> PacketSummary {
    let decoded = decode_packet(&record.bytes);
    PacketSummary {
        index: record.index,
        timestamp: capture_timestamp(record),
        length: record.bytes.len() as u64,
        protocol: packet_protocol(&decoded),
        src_mac: decoded
            .ethernet
            .as_ref()
            .map(|header| header.src_mac.clone()),
        dst_mac: decoded
            .ethernet
            .as_ref()
            .map(|header| header.dst_mac.clone()),
        src_ip: decoded
            .ipv4
            .as_ref()
            .map(|header| header.src.clone())
            .or_else(|| decoded.ipv6.as_ref().map(|header| header.src.clone())),
        dst_ip: decoded
            .ipv4
            .as_ref()
            .map(|header| header.dst.clone())
            .or_else(|| decoded.ipv6.as_ref().map(|header| header.dst.clone())),
        src_port: decoded
            .tcp
            .as_ref()
            .map(|header| header.src_port)
            .or_else(|| decoded.udp.as_ref().map(|header| header.src_port)),
        dst_port: decoded
            .tcp
            .as_ref()
            .map(|header| header.dst_port)
            .or_else(|| decoded.udp.as_ref().map(|header| header.dst_port)),
        payload_preview_base64: include_payload.then_some(decoded.payload_preview_base64),
        payload_total_bytes: include_payload.then_some(decoded.payload_total_bytes),
        payload_truncated: include_payload.then_some(decoded.payload_truncated),
    }
}

/// The most specific transport the bytes establish, then the network layer, then the link
/// layer. No application protocol is ever guessed.
fn packet_protocol(decoded: &PacketDecodeResult) -> PacketProtocol {
    if decoded.tcp.is_some() {
        PacketProtocol::Tcp
    } else if decoded.udp.is_some() {
        PacketProtocol::Udp
    } else if decoded.icmp.is_some() {
        PacketProtocol::Icmp
    } else if decoded.ipv6.is_some() {
        PacketProtocol::Ipv6
    } else if decoded.ipv4.is_some() {
        PacketProtocol::Ipv4
    } else if decoded.ethernet.is_some() {
        PacketProtocol::Ethernet
    } else {
        PacketProtocol::Other
    }
}

fn capture_timestamp(record: &PcapRecord) -> Option<String> {
    let millis = i64::from(record.seconds)
        .checked_mul(1_000)?
        .checked_add(i64::from(record.microseconds) / 1_000)?;
    Some(
        chrono::DateTime::from_timestamp_millis(millis)?
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    )
}

/// The largest `count` in `0..=upper` whose candidate fits, or `None` when even an empty one
/// does not. Every entry only adds bytes, so `fits` is monotone and the bisection is exact.
fn largest_fitting(upper: usize, mut fits: impl FnMut(usize) -> bool) -> Option<usize> {
    if !fits(0) {
        return None;
    }
    let mut low = 0usize;
    let mut high = upper;
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        if fits(middle) {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    Some(low)
}

/// The S-NET-001 assignment plus the port's settlement become the exact R-NET-002 result: a
/// family is `available` with its possibly empty array only when its assigned provider
/// answered, `unavailable` with its stable reason when the assignment has no provider, and
/// `unknown` with its data omitted when the provider could not establish it. The response is
/// then fitted to the frame by shortening the arrays it reports as truncated.
fn assemble_inspect(
    call: &NetworkCall,
    plan: NetworkSourcePlan,
    settlement: NetworkInspectSettlement,
) -> Result<NetworkInspectResult, DomainError> {
    let NetworkCall::Inspect(NetworkInspectInput {
        scope, max_entries, ..
    }) = call
    else {
        return Err(DomainError::invalid("network.inspect was not requested"));
    };
    InspectData::new(plan, settlement, *max_entries).fit(*scope)
}

struct InspectData {
    plan: NetworkSourcePlan,
    interfaces: Option<Established<InterfaceEntry>>,
    routes: Option<Established<RouteEntry>>,
    dns: Option<Established<DnsEntry>>,
    sockets: Option<Established<SocketEntry>>,
    counts: [usize; 4],
}

impl InspectData {
    fn new(
        plan: NetworkSourcePlan,
        settlement: NetworkInspectSettlement,
        max_entries: u32,
    ) -> Self {
        let mut data = Self {
            plan,
            interfaces: settlement.interfaces,
            routes: settlement.routes,
            dns: settlement.dns,
            sockets: settlement.sockets,
            counts: [0; 4],
        };
        let limit = max_entries as usize;
        for family in scope_families(NetworkScope::All) {
            data.counts[*family as usize] = data.entry_count(*family).min(limit);
        }
        data
    }

    fn entry_count(&self, family: NetworkFamily) -> usize {
        match family {
            NetworkFamily::Interfaces => self
                .interfaces
                .as_ref()
                .map_or(0, |slot| slot.entries.len()),
            NetworkFamily::Routes => self.routes.as_ref().map_or(0, |slot| slot.entries.len()),
            NetworkFamily::Dns => self.dns.as_ref().map_or(0, |slot| slot.entries.len()),
            NetworkFamily::Sockets => self.sockets.as_ref().map_or(0, |slot| slot.entries.len()),
        }
    }

    fn is_established(&self, family: NetworkFamily) -> bool {
        match family {
            NetworkFamily::Interfaces => self.interfaces.is_some(),
            NetworkFamily::Routes => self.routes.is_some(),
            NetworkFamily::Dns => self.dns.is_some(),
            NetworkFamily::Sockets => self.sockets.is_some(),
        }
    }

    fn source_truncated(&self, family: NetworkFamily) -> bool {
        match family {
            NetworkFamily::Interfaces => {
                self.interfaces.as_ref().is_some_and(|slot| slot.truncated)
            }
            NetworkFamily::Routes => self.routes.as_ref().is_some_and(|slot| slot.truncated),
            NetworkFamily::Dns => self.dns.as_ref().is_some_and(|slot| slot.truncated),
            NetworkFamily::Sockets => self.sockets.as_ref().is_some_and(|slot| slot.truncated),
        }
    }

    /// R-NET-002 pairs a data array with an availability state of `available`, so the array
    /// is present exactly when the plan names a provider and that provider answered.
    fn is_present(&self, family: NetworkFamily) -> bool {
        matches!(self.plan.family(family), NetworkFamilyPlan::Source(_))
            && self.is_established(family)
    }

    /// R-NET-002 reports one boolean per present data array and no key for an omitted family,
    /// so the entry budget is per-family and the reduction only ever shortens arrays the frame
    /// cannot hold. Every pass that changes anything strictly reduces the entry total, so the
    /// loop terminates; a result that cannot fit even with no entries is `RESOURCE_LIMIT`.
    fn fit(mut self, scope: NetworkScope) -> Result<NetworkInspectResult, DomainError> {
        while !encoded_fits(&self.build(&self.counts, scope)) {
            let mut progressed = false;
            for family in scope_families(scope) {
                let index = *family as usize;
                if !self.is_present(*family) || self.counts[index] == 0 {
                    continue;
                }
                let current = self.counts[index];
                let fitted = largest_fitting(current, |count| {
                    let mut probe = self.counts;
                    probe[index] = count;
                    encoded_fits(&self.build(&probe, scope))
                });
                if let Some(fitted) = fitted
                    && fitted < current
                {
                    self.counts[index] = fitted;
                    progressed = true;
                }
            }
            if !progressed {
                return Err(DomainError::new(
                    ErrorCode::ResourceLimit,
                    "network.inspect result exceeds the protocol frame limit",
                ));
            }
        }
        Ok(self.build(&self.counts, scope))
    }

    fn build(&self, counts: &[usize; 4], scope: NetworkScope) -> NetworkInspectResult {
        let mut result = NetworkInspectResult {
            interfaces: None,
            routes: None,
            dns: None,
            sockets: None,
            availability: NetworkFamilyAvailability {
                interfaces: None,
                routes: None,
                dns: None,
                sockets: None,
            },
            truncated: NetworkFamilyTruncated {
                interfaces: None,
                routes: None,
                dns: None,
                sockets: None,
            },
        };
        for family in scope_families(scope) {
            let availability = match self.plan.family(*family) {
                NetworkFamilyPlan::Unavailable(reason) => Availability {
                    state: CapabilityState::Unavailable,
                    reason: Some((*reason).to_owned()),
                },
                NetworkFamilyPlan::Source(_) if self.is_established(*family) => Availability {
                    state: CapabilityState::Available,
                    reason: None,
                },
                NetworkFamilyPlan::Source(_) => Availability {
                    state: CapabilityState::Unknown,
                    reason: Some("SOURCE_UNRESOLVED".to_owned()),
                },
            };
            // R-NET-002: only a requested family has a state, a data array is present
            // exactly when that state is `available`, and `truncated` follows its array.
            let available = matches!(availability.state, CapabilityState::Available);
            let index = *family as usize;
            let truncated = available.then(|| {
                self.source_truncated(*family) || counts[index] < self.entry_count(*family)
            });
            match family {
                NetworkFamily::Interfaces => {
                    result.interfaces = sliced(available, self.interfaces.as_ref(), counts[index]);
                    result.availability.interfaces = Some(availability);
                    result.truncated.interfaces = truncated;
                }
                NetworkFamily::Routes => {
                    result.routes = sliced(available, self.routes.as_ref(), counts[index]);
                    result.availability.routes = Some(availability);
                    result.truncated.routes = truncated;
                }
                NetworkFamily::Dns => {
                    result.dns = sliced(available, self.dns.as_ref(), counts[index]);
                    result.availability.dns = Some(availability);
                    result.truncated.dns = truncated;
                }
                NetworkFamily::Sockets => {
                    result.sockets = sliced(available, self.sockets.as_ref(), counts[index]);
                    result.availability.sockets = Some(availability);
                    result.truncated.sockets = truncated;
                }
            }
        }
        result
    }
}

fn sliced<T: Clone>(
    present: bool,
    entries: Option<&Established<T>>,
    count: usize,
) -> Option<Vec<T>> {
    if !present {
        return None;
    }
    entries.map(|slot| slot.entries[..count.min(slot.entries.len())].to_vec())
}

fn encoded_len(value: &impl serde::Serialize) -> Result<u64, DomainError> {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len() as u64)
        .map_err(|_| DomainError::new(ErrorCode::InternalError, reason_encoding()))
}

fn encoded_fits(value: &impl serde::Serialize) -> bool {
    serde_json::to_vec(value).is_ok_and(|encoded| {
        encoded.len() + MAX_INLINE_ENVELOPE_OVERHEAD <= UI_ENVELOPE_LIMIT_BYTES
    })
}

/// S-NET-002's single pinned Rustls configuration: the pinned `ring` provider, the pinned
/// WebPKI root store, no client authentication and no verification bypass. Both Runtime
/// hosts use this one definition, so a host never builds a second TLS policy.
///
/// The root store is the static Mozilla set compiled into the pinned dependency: it moves only
/// with a dependency update, and it never reads the device's system store, so Android
/// enterprise or user-installed CAs do not apply here. No CRL or OCSP source is configured
/// either, so revocation is not checked. Enterprise TLS inspection is therefore a separate
/// product capability with its own auditable trust source, never a per-tool exception here.
pub fn network_tls_client_config() -> Arc<rustls::ClientConfig> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

/// R-NET-009 keeps a performed probe `completed` even when the remote answer is negative,
/// so only a probe that could not be started at all is a structured error. These three
/// probes are the one implementation both host ports call from their own process, which is
/// what makes the Magisk path execute in `droidbridged` and the APK path in the APK runtime.
pub fn network_dns_probe(name: &str, record_type: DnsRecordType) -> NetworkDiagnoseResult {
    let started = Instant::now();
    let (outcome, addresses) = match resolve(name, 0) {
        Err(()) => (DiagnosticOutcome::DnsError, Vec::new()),
        Ok(all) => {
            let mut addresses: Vec<String> = Vec::new();
            for address in all {
                let canonical = address.ip().to_string();
                let wanted = match record_type {
                    DnsRecordType::A => address.is_ipv4(),
                    DnsRecordType::Aaaa => address.is_ipv6(),
                };
                if wanted && !addresses.contains(&canonical) {
                    addresses.push(canonical);
                }
            }
            let outcome = if addresses.is_empty() {
                DiagnosticOutcome::NotFound
            } else {
                DiagnosticOutcome::Success
            };
            (outcome, addresses)
        }
    };
    NetworkDiagnoseResult::Dns {
        outcome,
        duration_ms: elapsed_ms(started),
        name: name.to_owned(),
        record_type,
        addresses,
    }
}

pub fn network_tcp_probe(host: &str, port: u16, timeout_ms: u64) -> NetworkDiagnoseResult {
    let started = Instant::now();
    let (outcome, remote_ip) = match connect_first(host, port, Duration::from_millis(timeout_ms)) {
        Ok(stream) => (DiagnosticOutcome::Success, peer_ip(&stream)),
        Err(outcome) => (outcome, None),
    };
    NetworkDiagnoseResult::Tcp {
        outcome,
        duration_ms: elapsed_ms(started),
        host: host.to_owned(),
        port,
        remote_ip,
    }
}

/// TLS connects to `host:port` and verifies SNI and the certificate chain against
/// `server_name` or `host`. An omitted `server_name` is exactly `host` (R-NET-009).
pub fn network_tls_probe(
    host: &str,
    port: u16,
    server_name: Option<&str>,
    timeout_ms: u64,
) -> Result<NetworkDiagnoseResult, DomainError> {
    let started = Instant::now();
    let name = server_name.unwrap_or(host).to_owned();
    let target = rustls::pki_types::ServerName::try_from(name.clone())
        .map_err(|_| DomainError::invalid("network.diagnose tls server name is invalid"))?;
    let timeout = Duration::from_millis(timeout_ms);
    let (outcome, remote_ip, certificate_verified) = match connect_first(host, port, timeout) {
        Err(outcome) => (outcome, None, false),
        Ok(socket) => {
            let remote_ip = peer_ip(&socket);
            let _ = socket.set_read_timeout(Some(timeout));
            let _ = socket.set_write_timeout(Some(timeout));
            let connection = rustls::ClientConnection::new(network_tls_client_config(), target)
                .map_err(|_| DomainError::invalid("network.diagnose tls server name is invalid"))?;
            let mut stream = rustls::StreamOwned::new(connection, socket);
            match stream.conn.complete_io(&mut stream.sock) {
                Ok(_) => (DiagnosticOutcome::Success, remote_ip, true),
                Err(error) => (tls_failure(&error), remote_ip, false),
            }
        }
    };
    Ok(NetworkDiagnoseResult::Tls {
        outcome,
        duration_ms: elapsed_ms(started),
        host: host.to_owned(),
        port,
        server_name: name,
        remote_ip,
        certificate_verified,
    })
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

fn resolve(host: &str, port: u16) -> Result<Vec<SocketAddr>, ()> {
    (host, port)
        .to_socket_addrs()
        .map(|addresses| addresses.collect())
        .map_err(|_| ())
}

/// One candidate address at a time inside the single probe budget, in the platform
/// resolver's own preference order, so a probe can never outlive its stated timeout.
fn connect_first(host: &str, port: u16, timeout: Duration) -> Result<TcpStream, DiagnosticOutcome> {
    let deadline = Instant::now() + timeout;
    let addresses = resolve(host, port).map_err(|()| DiagnosticOutcome::DnsError)?;
    let mut failure = None;
    for address in addresses {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            failure.get_or_insert(DiagnosticOutcome::Timeout);
            break;
        }
        match TcpStream::connect_timeout(&address, remaining) {
            Ok(stream) => return Ok(stream),
            Err(error) => {
                failure.get_or_insert(connect_failure(&error));
            }
        }
    }
    Err(failure.unwrap_or(DiagnosticOutcome::Unreachable))
}

fn connect_failure(error: &std::io::Error) -> DiagnosticOutcome {
    match error.kind() {
        std::io::ErrorKind::ConnectionRefused => DiagnosticOutcome::Refused,
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => DiagnosticOutcome::Timeout,
        _ => DiagnosticOutcome::Unreachable,
    }
}

fn tls_failure(error: &std::io::Error) -> DiagnosticOutcome {
    match error.kind() {
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => DiagnosticOutcome::Timeout,
        _ => DiagnosticOutcome::TlsError,
    }
}

fn peer_ip(stream: &TcpStream) -> Option<String> {
    stream
        .peer_addr()
        .ok()
        .map(|address| address.ip().to_string())
}
