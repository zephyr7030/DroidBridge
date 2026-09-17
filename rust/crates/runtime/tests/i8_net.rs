//! I8-NET: the two host Network surfaces over one shared semantic handler.
//!
//! Every test drives `NativeNetworkExecutionSurface` exactly as the APK and daemon hosts do,
//! with a scripted network primitive standing in for the daemon's native providers and the
//! App/framework plus read-only Shizuku supplement. That isolates the shared handler's public
//! meaning from the host mechanics, which is what the node's handoff gate claims: one
//! executor selection, one S-NET-001 source plan, one diagnose result projection, one capture
//! Task owner, one packet/PCAP wire owner, the API37 LAN gate on the App executor only, the
//! capture/inject bounds, verified cleanup, and the bounded default-network event.

use contract::{
    Automation, AutomationAction, AutomationTrigger, Availability, CapabilityState,
    CaptureReadSource, DiagnosticOutcome, DnsEntry, DnsRecordType, ErrorCode, EthernetBuild,
    FileTarget, FileTargetType, GrantFacts, InterfaceAddress, InterfaceEntry, MotherTool,
    NetworkBuild, NetworkCall, NetworkCaptureInput, NetworkDiagnoseInput, NetworkDiagnoseResult,
    NetworkInspectInput, NetworkPacketInput, NetworkScope, PacketDecodeSource, PacketInjectResult,
    PacketSource, RequestId, RouteEntry, RuntimeHost, RuntimeReadiness, ScalarValue, SocketEntry,
    SocketProtocol, TaskSnapshot, TaskState, TaskTerminalResult, TransportBuild, UuidV4,
};
use domain::{
    AdmissionFence, CapabilityContext, DomainError, ExecutorRequest, NetworkRoute, Preflight,
    Provider, ProviderGenerations, ResolverFacts, validate_automation,
};
use runtime::{
    AdmittedExecution, ArtifactPort, CapabilityPort, CapabilitySnapshot, CaptureSettlement,
    CompositeExecutionSurface, Established, ExecutionCancelOutcome, ExecutionCompletion,
    ExecutionFailure, ExecutionOutcome, ExecutionPayload, ExecutionPort, ExecutorRecord,
    FilesystemCandidate, FilesystemPreflightPort, LocalExecutionClaim,
    NETWORK_DEFAULT_CHANGED_EVENT, NETWORK_EVENT_CHANNEL_CAPACITY, NETWORK_EVENT_FACT_BYTES,
    NETWORK_MAX_CAPTURE_BYTES, NETWORK_MAX_CAPTURE_PACKETS, NETWORK_MAX_INJECT_COUNT,
    NETWORK_MAX_PACKET_BYTES, NETWORK_MAX_READ_PACKETS, NETWORK_MAX_SCOPE_ENTRIES,
    NativeNetworkExecutionSurface, NetworkDefaultChangedEvent, NetworkDefaultEventIngress,
    NetworkDefaultEventPlane, NetworkDefaultEventSource, NetworkDefaultSourceRegistration,
    NetworkEventDelivery, NetworkFamily, NetworkFamilyPlan, NetworkFamilySource,
    NetworkInspectSettlement, NetworkPrimitiveOutcome, NetworkPrimitivePort,
    NetworkPrimitiveRequest, NetworkPrimitiveSettlement, PCAP_LINKTYPE_ETHERNET, PCAP_LINKTYPE_RAW,
    PortFuture, ProviderToken, RecoveryProof, RuntimeCore, TaskAdmission,
    UnavailableExecutionDelegate,
    fakes::{FakeArtifacts, FakeCapabilities, FakeHostControl, FakePersistence},
    network_dns_probe, network_executor_request, network_settlement_bound_bytes,
    network_source_plan, network_tcp_probe, network_tls_client_config, network_tls_probe,
    pcap_file_header, pcap_record_header, read_pcap, validate_network_input,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Write},
    net::{Ipv4Addr, TcpListener},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration as StdDuration, Instant},
};

const TIMESTAMP: &str = "2026-09-12T00:00:00.000Z";
const NOW_MS: u64 = 1_789_171_200_000;
const APK_INSTANCE: u64 = 0x11;
const MAGISK_INSTANCE: u64 = 0x22;

fn uuid(prefix: u32, value: u64) -> UuidV4 {
    UuidV4::parse(format!("{prefix:08x}-0000-4000-8000-{value:012x}")).unwrap()
}

fn request_id(value: u64) -> RequestId {
    RequestId::parse(format!("10000000-0000-4000-8000-{value:012x}")).unwrap()
}

const fn availability(state: CapabilityState) -> Availability {
    Availability {
        state,
        reason: None,
    }
}

/// The provider facts one host sees plus the two grants the LAN rule and the Magisk
/// ownership rule key on; every other grant is `available` in every fixture.
#[derive(Clone, Copy)]
struct Facts {
    host: RuntimeHost,
    app_native: CapabilityState,
    shizuku: CapabilityState,
    magisk_native: CapabilityState,
    app_execution_surface: CapabilityState,
    local_network: CapabilityState,
    magisk_root: CapabilityState,
    sdk_int: u32,
    host_generation: u64,
    shizuku_generation: u64,
    magisk_generation: u64,
    instance: u64,
}

fn apk_facts(instance: u64) -> Facts {
    Facts {
        host: RuntimeHost::ApkRuntime,
        app_native: CapabilityState::Available,
        shizuku: CapabilityState::Available,
        magisk_native: CapabilityState::Available,
        app_execution_surface: CapabilityState::Available,
        local_network: CapabilityState::Available,
        magisk_root: CapabilityState::Available,
        sdk_int: 37,
        host_generation: 4,
        shizuku_generation: 9_001,
        magisk_generation: 9_002,
        instance,
    }
}

fn magisk_facts(instance: u64) -> Facts {
    Facts {
        host: RuntimeHost::MagiskBackend,
        app_native: CapabilityState::Available,
        shizuku: CapabilityState::Available,
        magisk_native: CapabilityState::Available,
        app_execution_surface: CapabilityState::Available,
        local_network: CapabilityState::Available,
        magisk_root: CapabilityState::Available,
        sdk_int: 37,
        host_generation: 7,
        shizuku_generation: 9_001,
        magisk_generation: 9_002,
        instance,
    }
}

fn grant_facts(facts: Facts) -> GrantFacts {
    let state = CapabilityState::Available;
    GrantFacts {
        android_local_network: availability(facts.local_network),
        android_notifications: availability(state),
        android_notification_listener: availability(state),
        automation_exact_alarm: availability(state),
        visual_accessibility: availability(state),
        visual_media_projection_session: availability(state),
        shizuku_shell: availability(state),
        magisk_module: availability(state),
        magisk_root: availability(facts.magisk_root),
        magisk_framework: availability(state),
        magisk_launch: availability(state),
        magisk_clipboard: availability(state),
        magisk_notifications: availability(state),
        magisk_wake_alarm: availability(state),
        execution_app_guard: availability(state),
        execution_shell_guard: availability(state),
        execution_root_guard: availability(state),
    }
}

fn capability(facts: Facts) -> CapabilitySnapshot {
    CapabilitySnapshot {
        grants: grant_facts(facts),
        context: CapabilityContext {
            sdk_int: facts.sdk_int,
            host: facts.host,
            readiness: RuntimeReadiness::Ready,
            app_execution_surface: facts.app_execution_surface,
        },
        resolver_facts: ResolverFacts {
            app_native: facts.app_native,
            app_framework: facts.app_execution_surface,
            shizuku: facts.shizuku,
            magisk_native: facts.magisk_native,
            magisk_framework: CapabilityState::Available,
            magisk_launch: CapabilityState::Available,
            magisk_clipboard: CapabilityState::Available,
            magisk_notifications: CapabilityState::Available,
            accessibility: CapabilityState::Available,
            media_projection: CapabilityState::Available,
            notification_listener: CapabilityState::Available,
            generations: ProviderGenerations {
                app_native: facts.host_generation,
                app_framework: facts.host_generation,
                shizuku: facts.shizuku_generation,
                magisk_native: facts.magisk_generation,
                magisk_framework: facts.host_generation,
                accessibility: facts.host_generation,
                media_projection: facts.host_generation,
                notification_listener: facts.host_generation,
            },
        },
        fence: AdmissionFence {
            runtime_epoch: uuid(0x8100_0000, 1),
            host_generation: facts.host_generation,
            runtime_instance_id: uuid(0x8100_0000, facts.instance),
        },
    }
}

/// The route R-NET-003 and R-NET-004 fix for one Network action, and the capability
/// projection S-AUTH-NET-001 applies before resolving the executor.
const fn network_route(call: &NetworkCall) -> NetworkRoute {
    match call {
        NetworkCall::Inspect(_)
        | NetworkCall::Diagnose(_)
        | NetworkCall::Capture(NetworkCaptureInput::Read { .. })
        | NetworkCall::Packet(NetworkPacketInput::Decode { .. })
        | NetworkCall::Packet(NetworkPacketInput::Build { .. }) => NetworkRoute::InspectOrDiagnose,
        NetworkCall::Capture(_) => NetworkRoute::Capture,
        NetworkCall::Packet(NetworkPacketInput::Inject { .. }) => NetworkRoute::Inject,
    }
}

const fn projects_app_surface(call: &NetworkCall) -> bool {
    matches!(
        network_route(call),
        NetworkRoute::InspectOrDiagnose | NetworkRoute::ReadOnlyRouteSupplement
    )
}

/// Admits one Network call exactly as `RuntimeCore` does, so the executor record carries the
/// provider, class and generation the handler later re-checks.
fn admitted(
    facts: Facts,
    call: &NetworkCall,
    task_id: Option<UuidV4>,
    instance: u64,
) -> AdmittedExecution {
    let capability = capability(facts);
    let mut resolver_facts = capability.resolver_facts;
    if projects_app_surface(call) {
        resolver_facts.app_native = capability.context.app_execution_surface;
        resolver_facts.generations.app_native = capability.fence.host_generation;
    }
    let executor = domain::resolve_executor(
        capability.context.host,
        capability.fence.clone(),
        resolver_facts,
        ExecutorRequest::Network(network_route(call)),
    )
    .expect("the fixture admits the requested route");
    AdmittedExecution {
        execution_id: uuid(0x8500_0000, instance),
        task_id,
        executor: ExecutorRecord::from(&executor),
        payload: ExecutionPayload::NetworkCall(call.clone()),
    }
}

fn inspect_call(scope: NetworkScope, max_entries: u32) -> NetworkCall {
    NetworkCall::Inspect(NetworkInspectInput { scope, max_entries })
}

fn capture_start_call() -> NetworkCall {
    NetworkCall::Capture(NetworkCaptureInput::Start {
        interface: "wlan0".to_owned(),
        filter: Some("tcp port 443".to_owned()),
        max_packets: 10_000,
        max_bytes: 67_108_864,
        max_duration_ms: 60_000,
        persist_to: None,
    })
}

fn capture_read_file_call() -> NetworkCall {
    NetworkCall::Capture(NetworkCaptureInput::Read {
        source: CaptureReadSource::File {
            file: FileTarget {
                target_type: FileTargetType::Path,
                value: "/data/local/tmp/capture.pcap".to_owned(),
            },
        },
        offset_packet: 0,
        max_packets: 200,
        include_payload: true,
    })
}

fn diagnose_call(input: NetworkDiagnoseInput) -> NetworkCall {
    NetworkCall::Diagnose(input)
}

fn tcp_diagnose(host: &str) -> NetworkCall {
    diagnose_call(NetworkDiagnoseInput::Tcp {
        host: host.to_owned(),
        port: 443,
        timeout_ms: 5_000,
    })
}

fn decode_raw_call(bytes: &[u8]) -> NetworkCall {
    NetworkCall::Packet(NetworkPacketInput::Decode {
        source: PacketDecodeSource::Raw {
            raw_base64: base64(bytes),
        },
    })
}

fn inject_call(packet: &[u8], count: u32) -> NetworkCall {
    NetworkCall::Packet(NetworkPacketInput::Inject {
        interface: "wlan0".to_owned(),
        packet: PacketSource::Raw {
            raw_base64: base64(packet),
        },
        count,
        interval_ms: 10,
    })
}

fn base64(bytes: &[u8]) -> String {
    use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
    BASE64.encode(bytes)
}

fn from_base64(encoded: &str) -> Vec<u8> {
    use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
    BASE64.decode(encoded).unwrap()
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Kind {
    Inspect,
    Diagnose,
    CaptureStart,
    CaptureStop,
    CaptureRead,
    Inject,
}

const fn kind_of(request: &NetworkPrimitiveRequest) -> Kind {
    match request {
        NetworkPrimitiveRequest::Inspect { .. } => Kind::Inspect,
        NetworkPrimitiveRequest::Diagnose(..) => Kind::Diagnose,
        NetworkPrimitiveRequest::CaptureStart { .. } => Kind::CaptureStart,
        NetworkPrimitiveRequest::CaptureStop { .. } => Kind::CaptureStop,
        NetworkPrimitiveRequest::CaptureFileBytes { .. } => Kind::CaptureRead,
        NetworkPrimitiveRequest::PacketInject { .. } => Kind::Inject,
    }
}

/// One scripted network primitive. It records every request and admitted execution, returns
/// the scripted settlement for that request kind, and can hold a request open until its
/// claim is cancelled or the test releases it — which is what a real capture does while its
/// owner watches the claim.
#[derive(Clone, Default)]
struct ScriptedPort {
    state: Arc<PortState>,
}

#[derive(Default)]
struct PortState {
    scripts: Mutex<BTreeMap<Kind, Result<NetworkPrimitiveSettlement, ExecutionFailure>>>,
    requests: Mutex<Vec<NetworkPrimitiveRequest>>,
    executions: Mutex<Vec<AdmittedExecution>>,
    entered: Mutex<usize>,
    held: Mutex<BTreeSet<Kind>>,
    released: Mutex<Option<Result<NetworkPrimitiveSettlement, ExecutionFailure>>>,
    release: Mutex<bool>,
}

impl ScriptedPort {
    fn new() -> Self {
        Self::default()
    }

    fn script(&self, kind: Kind, result: Result<NetworkPrimitiveSettlement, ExecutionFailure>) {
        self.state
            .scripts
            .lock()
            .expect("script lock")
            .insert(kind, result);
    }

    fn hold(&self, kind: Kind) {
        self.state.held.lock().expect("hold lock").insert(kind);
    }

    fn release(&self, result: Result<NetworkPrimitiveSettlement, ExecutionFailure>) {
        *self.state.released.lock().expect("release lock") = Some(result);
        *self.state.release.lock().expect("release flag lock") = true;
    }

    fn requests(&self) -> Vec<NetworkPrimitiveRequest> {
        self.state.requests.lock().expect("request lock").clone()
    }

    fn request(&self, kind: Kind) -> NetworkPrimitiveRequest {
        self.requests()
            .into_iter()
            .find(|request| kind_of(request) == kind)
            .unwrap_or_else(|| panic!("the port never received a {kind:?} request"))
    }

    fn executions(&self) -> Vec<AdmittedExecution> {
        self.state
            .executions
            .lock()
            .expect("execution lock")
            .clone()
    }

    fn entered(&self) -> usize {
        *self.state.entered.lock().expect("entry lock")
    }
}

impl NetworkPrimitivePort for ScriptedPort {
    fn run(
        &self,
        execution: &AdmittedExecution,
        request: NetworkPrimitiveRequest,
        claim: &LocalExecutionClaim,
    ) -> Result<NetworkPrimitiveSettlement, ExecutionFailure> {
        let kind = kind_of(&request);
        self.state
            .requests
            .lock()
            .expect("request lock")
            .push(request);
        self.state
            .executions
            .lock()
            .expect("execution lock")
            .push(execution.clone());
        *self.state.entered.lock().expect("entry lock") += 1;
        if self.state.held.lock().expect("hold lock").contains(&kind) {
            loop {
                if claim.checkpoint().is_err() {
                    return Err(ExecutionFailure {
                        error: DomainError::new(
                            ErrorCode::Cancelled,
                            "the held network primitive observed its cancellation",
                        ),
                        cleanup_verified: true,
                    });
                }
                if *self.state.release.lock().expect("release flag lock") {
                    let released = self.state.released.lock().expect("release lock").take();
                    if let Some(settlement) = released {
                        return settlement;
                    }
                }
                std::thread::sleep(StdDuration::from_millis(5));
            }
        }
        match self.state.scripts.lock().expect("script lock").get(&kind) {
            Some(Ok(settlement)) => Ok(settlement.clone()),
            Some(Err(failure)) => Err(failure.clone()),
            None => Err(ExecutionFailure {
                error: DomainError::new(
                    ErrorCode::InternalError,
                    "the scripted network primitive was not given a settlement",
                ),
                cleanup_verified: true,
            }),
        }
    }
}

/// A host this node does not own: the composite requires one filesystem delegate, and the
/// network payload never reaches it.
#[derive(Clone, Copy)]
struct NoFilesystem;

impl ExecutionPort for NoFilesystem {
    fn claim_and_start<'a>(
        &'a self,
        _execution: AdmittedExecution,
    ) -> PortFuture<'a, Result<ExecutionCompletion, ExecutionFailure>> {
        Box::pin(async move {
            Err(ExecutionFailure {
                error: DomainError::new(
                    ErrorCode::Unsupported,
                    "this fixture owns no filesystem execution",
                ),
                cleanup_verified: true,
            })
        })
    }

    fn cancel<'a>(
        &'a self,
        _execution_id: &'a UuidV4,
    ) -> PortFuture<'a, Result<ExecutionCancelOutcome, DomainError>> {
        Box::pin(async move { Ok(ExecutionCancelOutcome::CompletionWon) })
    }
}

impl FilesystemPreflightPort for NoFilesystem {
    fn preflight(
        &self,
        _candidate: FilesystemCandidate,
        _call: &contract::FilesystemCall,
    ) -> Result<Preflight, DomainError> {
        Ok(Preflight::Positive)
    }
}

type NetworkSurface = NativeNetworkExecutionSurface<FakeArtifacts, FakeCapabilities, ScriptedPort>;
type Core = RuntimeCore<
    FakePersistence,
    FakeArtifacts,
    CompositeExecutionSurface<NoFilesystem, UnavailableExecutionDelegate, NetworkSurface>,
    FakeCapabilities,
    FakeHostControl,
>;

struct Host {
    core: Core,
    artifacts: FakeArtifacts,
    capabilities: FakeCapabilities,
    control: FakeHostControl,
    port: ScriptedPort,
}

fn network_host(facts: Facts) -> Host {
    let artifacts = FakeArtifacts::default();
    let capabilities = FakeCapabilities::new(capability(facts));
    let control =
        FakeHostControl::new(RecoveryProof::Clean).with_capabilities(capabilities.clone());
    let port = ScriptedPort::new();
    let network =
        NativeNetworkExecutionSurface::new(artifacts.clone(), capabilities.clone(), port.clone());
    let core = RuntimeCore::new(
        FakePersistence::default(),
        artifacts.clone(),
        CompositeExecutionSurface::new(NoFilesystem).with_network(network),
        capabilities.clone(),
        control.clone(),
    );
    Host {
        core,
        artifacts,
        capabilities,
        control,
        port,
    }
}

impl Host {
    /// A surface over this host's own artifact store, so one execution can be admitted
    /// directly without inventing a second store.
    fn artifacts_surface(&self) -> NetworkSurface {
        NativeNetworkExecutionSurface::new(
            self.artifacts.clone(),
            self.capabilities.clone(),
            self.port.clone(),
        )
    }
}

fn network_request(id: u64, action: &str, input: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "protocol_version": 1,
        "request_id": format!("10000000-0000-4000-8000-{id:012x}"),
        "payload": {"tool": "network", "action": action, "input": input}
    })
}

async fn submit(host: &Host, request: serde_json::Value) -> serde_json::Value {
    let encoded = runtime::submit_public(
        &host.core,
        &serde_json::to_vec(&request).unwrap(),
        TIMESTAMP.to_owned(),
        NOW_MS,
        true,
        |_| async { panic!("network escaped its canonical ingress handler") },
    )
    .await;
    serde_json::from_slice(&encoded).unwrap()
}

async fn submit_network(
    host: &Host,
    id: u64,
    action: &str,
    input: serde_json::Value,
) -> serde_json::Value {
    submit(host, network_request(id, action, input)).await
}

fn error_code(response: &serde_json::Value) -> &str {
    assert_eq!(
        response["outcome"], "error",
        "expected a structured error, found {response}"
    );
    response["error"]["code"]
        .as_str()
        .expect("every error response carries a code")
}

fn succeeded(response: &serde_json::Value) -> &serde_json::Value {
    assert_eq!(
        response["outcome"], "success",
        "expected a success envelope, found {response}"
    );
    &response["result"]
}

fn task_id_of(response: &serde_json::Value) -> UuidV4 {
    UuidV4::parse(
        response["capture_id"]
            .as_str()
            .expect("a capture start answers with its capture identity")
            .to_owned(),
    )
    .unwrap()
}

/// The synchronous public result of one admitted Network call, or a failure for anything
/// that is not a synchronous network result.
fn synchronous(completion: &ExecutionCompletion) -> &serde_json::Value {
    match &completion.outcome {
        ExecutionOutcome::SynchronousCompleted { result, .. } => result,
        other => panic!("expected a synchronous network result, found {other:?}"),
    }
}

async fn start(
    surface: &NetworkSurface,
    execution: AdmittedExecution,
) -> Result<ExecutionCompletion, ExecutionFailure> {
    surface.claim_and_start(execution).await
}

/// A surface plus the artifact store it publishes into, so a test can open the bytes the
/// Runtime built without asking the surface for its own private store.
struct Built {
    surface: NetworkSurface,
    artifacts: FakeArtifacts,
    port: ScriptedPort,
}

fn built(facts: Facts) -> Built {
    let artifacts = FakeArtifacts::default();
    let port = ScriptedPort::new();
    let surface = NativeNetworkExecutionSurface::new(
        artifacts.clone(),
        FakeCapabilities::new(capability(facts)),
        port.clone(),
    );
    Built {
        surface,
        artifacts,
        port,
    }
}

async fn wait_for(port: &ScriptedPort, kind: Kind) {
    let deadline = Instant::now() + StdDuration::from_secs(10);
    loop {
        if port
            .requests()
            .iter()
            .any(|request| kind_of(request) == kind)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the port never received a {kind:?} request"
        );
        tokio::time::sleep(StdDuration::from_millis(5)).await;
    }
}

async fn terminal_task(host: &Host, task_id: &UuidV4) -> TaskSnapshot {
    let deadline = Instant::now() + StdDuration::from_secs(10);
    loop {
        let snapshot = host.core.get_task(task_id, NOW_MS).await.unwrap();
        if matches!(
            snapshot.state,
            TaskState::Completed
                | TaskState::Failed
                | TaskState::Cancelled
                | TaskState::Interrupted
        ) {
            return snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "the capture Task never settled: {:?}",
            snapshot.state
        );
        tokio::time::sleep(StdDuration::from_millis(5)).await;
    }
}

fn settled(outcome: NetworkPrimitiveOutcome, cleanup_verified: bool) -> NetworkPrimitiveSettlement {
    NetworkPrimitiveSettlement {
        outcome,
        cleanup_verified,
    }
}

fn inspect_settlement(
    interfaces: Option<Established<InterfaceEntry>>,
    routes: Option<Established<RouteEntry>>,
    dns: Option<Established<DnsEntry>>,
    sockets: Option<Established<SocketEntry>>,
) -> NetworkPrimitiveSettlement {
    settled(
        NetworkPrimitiveOutcome::Inspect(NetworkInspectSettlement {
            interfaces,
            routes,
            dns,
            sockets,
        }),
        true,
    )
}

fn interface(name: &str) -> InterfaceEntry {
    InterfaceEntry {
        name: name.to_owned(),
        index: Some(2),
        up: Some(true),
        mtu: Some(1_500),
        addresses: vec![InterfaceAddress {
            address: "192.168.1.20".to_owned(),
            prefix_length: Some(24),
        }],
    }
}

fn route(destination: &str) -> RouteEntry {
    RouteEntry {
        destination: destination.to_owned(),
        gateway: Some("192.168.1.1".to_owned()),
        interface: Some("wlan0".to_owned()),
        metric: Some(600),
    }
}

fn dns_entry(server: &str) -> DnsEntry {
    DnsEntry {
        server: server.to_owned(),
    }
}

fn socket(port: u16) -> SocketEntry {
    SocketEntry {
        protocol: SocketProtocol::Tcp,
        local_address: "127.0.0.1".to_owned(),
        local_port: Some(port),
        remote_address: None,
        remote_port: None,
        state: Some("listen".to_owned()),
        uid: Some(2_000),
    }
}

fn capture_settlement(packets: u64, bytes: u64, cancelled: bool) -> CaptureSettlement {
    CaptureSettlement {
        cancelled,
        packets_captured: packets,
        bytes_captured: bytes,
        capture_ref: Some("dbref:capture:00000000-0000-4000-8000-000000000001".to_owned()),
        destination: None,
        cleanup_verified: true,
    }
}

fn pcap(records: &[(u32, u32, &[u8])]) -> Vec<u8> {
    pcap_of(PCAP_LINKTYPE_ETHERNET, records)
}

fn pcap_of(link_type: u32, records: &[(u32, u32, &[u8])]) -> Vec<u8> {
    let mut file = pcap_file_header(link_type).to_vec();
    for (seconds, microseconds, bytes) in records {
        file.extend_from_slice(&pcap_record_header(
            *seconds,
            *microseconds,
            bytes.len() as u32,
            bytes.len() as u32,
        ));
        file.extend_from_slice(bytes);
    }
    file
}

/// One 46-byte IPv4 TCP packet with a known payload: 20 bytes of IPv4, 20 of TCP and
/// `i8-net`. Its total-length field matches the buffer, so it is read as bare IP rather than
/// as an Ethernet frame.
fn built_tcp_packet() -> Vec<u8> {
    let mut packet = Vec::new();
    packet.extend_from_slice(&[0x45, 0x00]); // version/ihl, dscp
    packet.extend_from_slice(&46u16.to_be_bytes()); // total length: 20 + 20 + 6
    packet.extend_from_slice(&1u16.to_be_bytes()); // identification
    packet.extend_from_slice(&0x4000u16.to_be_bytes()); // don't fragment
    packet.push(64); // ttl
    packet.push(6); // tcp
    packet.extend_from_slice(&[0, 0]); // header checksum, not read back
    packet.extend_from_slice(&[10, 0, 0, 1]);
    packet.extend_from_slice(&[10, 0, 0, 2]);
    packet.extend_from_slice(&1234u16.to_be_bytes());
    packet.extend_from_slice(&443u16.to_be_bytes());
    packet.extend_from_slice(&7u32.to_be_bytes());
    packet.extend_from_slice(&0u32.to_be_bytes());
    packet.push(0x50); // data offset
    packet.push(0x02); // syn
    packet.extend_from_slice(&1024u16.to_be_bytes());
    packet.extend_from_slice(&[0, 0]); // checksum
    packet.extend_from_slice(&[0, 0]); // urgent pointer
    packet.extend_from_slice(b"i8-net");
    packet
}

fn internet_checksum(bytes: &[u8]) -> u16 {
    let mut sum = 0u32;
    for pair in bytes.chunks(2) {
        let word = if pair.len() == 2 {
            u16::from_be_bytes([pair[0], pair[1]])
        } else {
            u16::from(pair[0]) << 8
        };
        sum += u32::from(word);
    }
    while sum > 0xffff {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

fn hex_sha256(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Gate 01: S-AUTH-NET-001 owns executor selection for every Network action, and the
/// selected executor is the authenticated surface the facts establish.
#[tokio::test]
async fn i8_net_g01_executor_selection_is_owned_by_one_authority() {
    let apk = apk_facts(APK_INSTANCE);
    let magisk = magisk_facts(MAGISK_INSTANCE);

    let routed = [
        (
            inspect_call(NetworkScope::All, 200),
            NetworkRoute::InspectOrDiagnose,
        ),
        (tcp_diagnose("example.com"), NetworkRoute::InspectOrDiagnose),
        (capture_read_file_call(), NetworkRoute::InspectOrDiagnose),
        (decode_raw_call(b"raw"), NetworkRoute::InspectOrDiagnose),
        (capture_start_call(), NetworkRoute::Capture),
        (inject_call(b"raw", 1), NetworkRoute::Inject),
    ];
    for (call, route) in &routed {
        assert_eq!(network_route(call), *route, "the fixture route drifted");
        assert_eq!(
            network_executor_request(&capability(magisk), call).unwrap(),
            ExecutorRequest::Network(*route),
            "the Magisk host must own {route:?}"
        );
        let magisk_executor = admitted(magisk, call, None, 1).executor;
        assert_eq!(magisk_executor.provider, ProviderToken::MagiskNative);
        assert_eq!(
            magisk_executor.execution_class,
            contract::ExecutionClass::Magisk
        );
    }

    for (call, route) in &routed {
        let routed_request = network_executor_request(&capability(apk), call);
        match route {
            NetworkRoute::InspectOrDiagnose => {
                assert_eq!(
                    routed_request.unwrap(),
                    ExecutorRequest::Network(NetworkRoute::InspectOrDiagnose)
                );
                let executor = admitted(apk, call, None, 1).executor;
                assert_eq!(executor.provider, ProviderToken::AppNative);
                assert_eq!(executor.execution_class, contract::ExecutionClass::App);
            }
            _ => assert_eq!(
                routed_request.unwrap_err().code,
                ErrorCode::CapabilityUnavailable,
                "the APK host cannot own {route:?}"
            ),
        }
    }

    // The App path is the authenticated companion surface: without it the App executor
    // does not exist, so the call is refused rather than sent to another provider.
    let unauthenticated = Facts {
        app_execution_surface: CapabilityState::Unavailable,
        ..apk_facts(APK_INSTANCE)
    };
    assert_eq!(
        network_executor_request(
            &capability(unauthenticated),
            &inspect_call(NetworkScope::All, 200)
        )
        .unwrap_err()
        .code,
        ErrorCode::CapabilityUnavailable
    );
    assert_eq!(
        domain::resolve_executor(
            capability(unauthenticated).context.host,
            capability(unauthenticated).fence.clone(),
            capability(unauthenticated).resolver_facts,
            ExecutorRequest::Network(NetworkRoute::ReadOnlyRouteSupplement),
        )
        .map(|executor| executor.provider())
        .unwrap_or(Provider::AppNative),
        Provider::Shizuku,
        "the read-only route supplement is the Shizuku UID2000 provider"
    );

    // A host that is not ready has no executor at all.
    let mut unavailable = capability(apk);
    unavailable.context.readiness = RuntimeReadiness::Unavailable;
    assert_eq!(
        network_executor_request(&unavailable, &inspect_call(NetworkScope::All, 200))
            .unwrap_err()
            .code,
        ErrorCode::CapabilityUnavailable
    );

    // One admitted operation settles as the executor it was admitted under.
    let port = ScriptedPort::new();
    port.script(
        Kind::Inspect,
        Ok(inspect_settlement(
            Some(Established {
                entries: vec![interface("wlan0")],
                truncated: false,
            }),
            None,
            None,
            None,
        )),
    );
    let surface = NativeNetworkExecutionSurface::new(
        FakeArtifacts::default(),
        FakeCapabilities::new(capability(apk)),
        port.clone(),
    );
    let completion = start(
        &surface,
        admitted(apk, &inspect_call(NetworkScope::Interfaces, 200), None, 7),
    )
    .await
    .unwrap();
    assert!(synchronous(&completion)["availability"]["interfaces"].is_object());
    assert_eq!(port.entered(), 1);
    assert_eq!(
        port.executions()[0].executor.provider,
        ProviderToken::AppNative,
        "one execution is served by the one selected executor"
    );
}

/// Gate 02: S-NET-001 fixes each family's provider before any query, and R-NET-002 reports
/// that assignment with the source's own truncation.
#[tokio::test]
async fn i8_net_g02_inspect_families_follow_one_source_plan() {
    let apk = apk_facts(APK_INSTANCE);
    let magisk = magisk_facts(MAGISK_INSTANCE);

    let daemon = network_source_plan(&capability(magisk), ProviderToken::MagiskNative);
    for family in [
        NetworkFamily::Interfaces,
        NetworkFamily::Routes,
        NetworkFamily::Dns,
        NetworkFamily::Sockets,
    ] {
        assert_eq!(
            daemon.family(family),
            NetworkFamilyPlan::Source(NetworkFamilySource::Daemon)
        );
    }

    let app = network_source_plan(&capability(apk), ProviderToken::AppNative);
    assert_eq!(
        app.family(NetworkFamily::Interfaces),
        NetworkFamilyPlan::Source(NetworkFamilySource::AppFramework)
    );
    assert_eq!(
        app.family(NetworkFamily::Dns),
        NetworkFamilyPlan::Source(NetworkFamilySource::AppFramework)
    );
    assert_eq!(
        app.family(NetworkFamily::Routes),
        NetworkFamilyPlan::Source(NetworkFamilySource::ShizukuSupplement)
    );
    assert_eq!(
        app.family(NetworkFamily::Sockets),
        NetworkFamilyPlan::Source(NetworkFamilySource::ShizukuSupplement)
    );

    let no_companion = network_source_plan(
        &capability(Facts {
            app_execution_surface: CapabilityState::Unavailable,
            ..apk_facts(APK_INSTANCE)
        }),
        ProviderToken::AppNative,
    );
    assert_eq!(
        no_companion.family(NetworkFamily::Interfaces),
        NetworkFamilyPlan::Unavailable("COMPANION_UNAVAILABLE")
    );
    assert_eq!(
        no_companion.family(NetworkFamily::Dns),
        NetworkFamilyPlan::Unavailable("COMPANION_UNAVAILABLE")
    );
    assert_eq!(
        no_companion.family(NetworkFamily::Routes),
        NetworkFamilyPlan::Source(NetworkFamilySource::ShizukuSupplement),
        "routes keep the supplement when the companion is gone"
    );

    let no_supplement = network_source_plan(
        &capability(Facts {
            shizuku: CapabilityState::Unavailable,
            ..apk_facts(APK_INSTANCE)
        }),
        ProviderToken::AppNative,
    );
    assert_eq!(
        no_supplement.family(NetworkFamily::Sockets),
        NetworkFamilyPlan::Unavailable("SHIZUKU_UNAVAILABLE")
    );
    assert_eq!(
        no_supplement.family(NetworkFamily::Routes),
        NetworkFamilyPlan::Source(NetworkFamilySource::AppFramework),
        "routes fall back to the App family only when no supplement exists"
    );

    // The plan travels with the admitted request and the response reports it per family.
    let host = network_host(apk);
    host.port.script(
        Kind::Inspect,
        Ok(inspect_settlement(
            Some(Established {
                entries: vec![interface("wlan0"), interface("rmnet0"), interface("lo")],
                truncated: false,
            }),
            Some(Established {
                entries: vec![route("0.0.0.0/0"), route("192.168.1.0/24")],
                truncated: false,
            }),
            Some(Established {
                entries: vec![dns_entry("192.168.1.1")],
                truncated: false,
            }),
            Some(Established {
                entries: vec![socket(4_000), socket(4_001), socket(4_002), socket(4_003)],
                truncated: true,
            }),
        )),
    );
    let response = submit_network(
        &host,
        0x201,
        "inspect",
        serde_json::json!({"scope": "all", "max_entries": 2}),
    )
    .await;
    let result = succeeded(&response);

    let request = host.port.request(Kind::Inspect);
    let NetworkPrimitiveRequest::Inspect {
        plan,
        scope,
        max_entries,
    } = request
    else {
        panic!("the inspect request lost its plan");
    };
    assert_eq!(
        plan,
        network_source_plan(&capability(apk), ProviderToken::AppNative)
    );
    assert_eq!(scope, NetworkScope::All);
    assert_eq!(max_entries, 2);

    assert_eq!(result["interfaces"].as_array().unwrap().len(), 2);
    assert_eq!(result["interfaces"][0]["name"], "wlan0");
    assert_eq!(
        result["interfaces"][0]["addresses"][0]["address"],
        "192.168.1.20"
    );
    assert_eq!(result["interfaces"][0]["addresses"][0]["prefix_length"], 24);
    assert_eq!(result["truncated"]["interfaces"], true);
    assert_eq!(result["truncated"]["routes"], false);
    assert_eq!(result["routes"].as_array().unwrap().len(), 2);
    assert_eq!(result["dns"][0]["server"], "192.168.1.1");
    assert_eq!(result["sockets"].as_array().unwrap().len(), 2);
    assert_eq!(
        result["truncated"]["sockets"], true,
        "the source's own truncation is reported"
    );
    assert_eq!(result["availability"]["interfaces"]["state"], "available");
    assert!(result["availability"]["interfaces"].get("reason").is_none());
    assert_eq!(result["availability"]["routes"]["state"], "available");

    // A family the plan cannot serve is `unavailable` with its reason and no data array,
    // and a planned source that could not answer is `unknown` with no data array.
    let host = network_host(Facts {
        shizuku: CapabilityState::Unavailable,
        ..apk_facts(APK_INSTANCE)
    });
    host.port.script(
        Kind::Inspect,
        Ok(inspect_settlement(
            Some(Established {
                entries: vec![interface("wlan0")],
                truncated: false,
            }),
            Some(Established {
                entries: vec![route("0.0.0.0/0")],
                truncated: false,
            }),
            None,
            None,
        )),
    );
    let response = submit_network(
        &host,
        0x202,
        "inspect",
        serde_json::json!({"scope": "all", "max_entries": 200}),
    )
    .await;
    let result = succeeded(&response);
    assert_eq!(result["availability"]["dns"]["state"], "unknown");
    assert_eq!(result["availability"]["dns"]["reason"], "SOURCE_UNRESOLVED");
    assert!(result.get("dns").is_none());
    assert!(result["truncated"].get("dns").is_none());
    assert_eq!(result["availability"]["sockets"]["state"], "unavailable");
    assert_eq!(
        result["availability"]["sockets"]["reason"],
        "SHIZUKU_UNAVAILABLE"
    );
    assert!(result.get("sockets").is_none());
    assert!(result["truncated"].get("sockets").is_none());
    // Routes fell back to the App family, which answered.
    assert_eq!(result["availability"]["routes"]["state"], "available");
    assert_eq!(result["routes"].as_array().unwrap().len(), 1);

    // Scope narrows the reported families.
    let response =
        submit_network(&host, 0x203, "inspect", serde_json::json!({"scope": "dns"})).await;
    let result = succeeded(&response);
    assert!(result["availability"].get("interfaces").is_none());
    assert!(result["availability"].get("routes").is_none());
    assert!(result["availability"].get("sockets").is_none());
    assert_eq!(result["availability"]["dns"]["state"], "unknown");
    assert!(result.get("interfaces").is_none());

    // The response stays inside the one Contract frame even with a large entry budget.
    assert!(network_settlement_bound_bytes() <= runtime::UI_ENVELOPE_LIMIT_BYTES as u64);
    let wide = network_host(magisk);
    wide.port.script(
        Kind::Inspect,
        Ok(inspect_settlement(
            Some(Established {
                entries: (0..NETWORK_MAX_SCOPE_ENTRIES)
                    .map(|index| interface(&format!("if{index}")))
                    .collect(),
                truncated: false,
            }),
            None,
            None,
            None,
        )),
    );
    let response = submit_network(
        &wide,
        0x204,
        "inspect",
        serde_json::json!({"scope": "interfaces", "max_entries": NETWORK_MAX_SCOPE_ENTRIES}),
    )
    .await;
    let result = succeeded(&response);
    let fitted = result["interfaces"].as_array().unwrap().len();
    assert!(fitted < NETWORK_MAX_SCOPE_ENTRIES as usize);
    assert_eq!(result["truncated"]["interfaces"], true);
    assert!(serde_json::to_vec(result).unwrap().len() < runtime::UI_ENVELOPE_LIMIT_BYTES);
}

/// Gate 03: S-NET-002 keeps the complete request inside the one selected executor, and a
/// failed admitted test is never retried through another one.
#[tokio::test]
async fn i8_net_g03_each_selected_executor_owns_its_complete_request() {
    let apk = apk_facts(APK_INSTANCE);

    // The APK host cannot own raw capture or injection, and refuses before any side effect.
    for (id, action, input) in [
        (
            0x301,
            "capture",
            serde_json::json!({"operation": "start", "interface": "wlan0"}),
        ),
        (
            0x302,
            "packet",
            serde_json::json!({
                "operation": "inject",
                "interface": "wlan0",
                "packet": {"raw_base64": base64(b"raw")},
            }),
        ),
    ] {
        let host = network_host(apk);
        let response = submit_network(&host, id, action, input).await;
        assert_eq!(error_code(&response), "CAPABILITY_UNAVAILABLE");
        assert_eq!(
            host.port.entered(),
            0,
            "{action} reached a provider on the APK host"
        );
        assert_eq!(
            host.core.list_tasks(None, 10, NOW_MS).await.unwrap().len(),
            0,
            "{action} created a Task on the APK host"
        );
    }

    // The Magisk host owns the complete request, including raw capture and injection, and
    // the App-local-network grant is irrelevant to that admitted execution.
    let host = network_host(Facts {
        local_network: CapabilityState::Unavailable,
        ..magisk_facts(MAGISK_INSTANCE)
    });
    host.port.hold(Kind::CaptureStart);
    let response = submit_network(
        &host,
        0x303,
        "capture",
        serde_json::json!({
            "operation": "start",
            "interface": "wlan0",
            "filter": "tcp port 443",
            "max_packets": 10_000,
            "max_bytes": 67_108_864,
            "max_duration_ms": 60_000
        }),
    )
    .await;
    let result = succeeded(&response);
    assert_eq!(result["operation"], "start");
    let capture_id = task_id_of(result);
    assert_eq!(result["task_id"], result["capture_id"]);
    wait_for(&host.port, Kind::CaptureStart).await;
    let NetworkPrimitiveRequest::CaptureStart {
        capture_id: request_id_value,
        interface,
        filter,
        max_packets,
        max_bytes,
        max_duration_ms,
        persist_to,
    } = host.port.request(Kind::CaptureStart)
    else {
        panic!("the capture start reached the wrong primitive");
    };
    assert_eq!(request_id_value, capture_id);
    assert_eq!(interface, "wlan0");
    assert_eq!(filter.as_deref(), Some("tcp port 443"));
    assert_eq!(max_packets, 10_000);
    assert_eq!(max_bytes, 67_108_864);
    assert_eq!(max_duration_ms, 60_000);
    assert!(persist_to.is_none());
    assert_eq!(
        host.port.executions()[0].task_id.clone(),
        Some(capture_id.clone()),
        "the capture Task identity reaches the owner"
    );
    host.port.release(Ok(settled(
        NetworkPrimitiveOutcome::CaptureSettled(capture_settlement(12, 4_096, false)),
        true,
    )));
    let snapshot = terminal_task(&host, &capture_id).await;
    assert_eq!(snapshot.state, TaskState::Completed);
    assert_eq!(snapshot.action, "capture");
    assert_eq!(snapshot.tool, MotherTool::Network);
    let crate::TaskTerminalResult::NetworkCapture(capture) = snapshot.result.unwrap() else {
        panic!("the capture Task kept no capture result");
    };
    assert_eq!(capture.capture_id, capture_id);
    assert_eq!(capture.packets_captured, 12);
    assert_eq!(capture.bytes_captured, 4_096);

    // A LAN target on the Magisk host is not subject to the App grant.
    let response = submit_network(
        &host,
        0x304,
        "diagnose",
        serde_json::json!({"test": "tcp", "host": "192.168.1.10", "port": 445}),
    )
    .await;
    assert_eq!(
        error_code(&response),
        "INTERNAL_ERROR",
        "the scripted diagnose has no settlement"
    );
    assert_eq!(
        host.port.entered(),
        2,
        "the daemon path served the LAN target"
    );

    // One failed admitted diagnose settles as its own failure, once, with no second
    // executor asked to retry it.
    let host = network_host(apk);
    host.port.script(
        Kind::Diagnose,
        Err(ExecutionFailure {
            error: DomainError::new(ErrorCode::IoError, "the daemon probe could not start"),
            cleanup_verified: true,
        }),
    );
    let call = tcp_diagnose("example.com");
    let response = submit_network(
        &host,
        0x305,
        "diagnose",
        serde_json::json!({"test": "tcp", "host": "example.com", "port": 443}),
    )
    .await;
    assert_eq!(error_code(&response), "IO_ERROR");
    assert_eq!(
        host.port.entered(),
        1,
        "a failed admitted test is never retried through another executor"
    );
    assert_eq!(host.port.requests().len(), 1);
    assert_eq!(
        network_executor_request(&capability(apk), &call).unwrap(),
        ExecutorRequest::Network(NetworkRoute::InspectOrDiagnose)
    );

    // A negative probe answer is not a failure: the admitted test completed.
    let host = network_host(apk);
    host.port.script(
        Kind::Diagnose,
        Ok(settled(
            NetworkPrimitiveOutcome::Diagnose(NetworkDiagnoseResult::Dns {
                outcome: DiagnosticOutcome::NotFound,
                duration_ms: 3,
                name: "example.invalid".to_owned(),
                record_type: DnsRecordType::A,
                addresses: Vec::new(),
            }),
            true,
        )),
    );
    let response = submit_network(
        &host,
        0x306,
        "diagnose",
        serde_json::json!({"test": "dns", "name": "example.invalid"}),
    )
    .await;
    let result = succeeded(&response);
    assert_eq!(result["test"], "dns");
    assert_eq!(result["outcome"], "not_found");
    assert_eq!(host.port.entered(), 1);
}

/// Gate 04: R-NET-003/R-NET-004 give one capture Task one owner, one identity and one
/// retained terminal result inside the stop bound.
#[tokio::test]
async fn i8_net_g04_capture_task_lifecycle_has_one_owner() {
    let magisk = magisk_facts(MAGISK_INSTANCE);

    // A running capture answers a stop with its own terminal result once it settles.
    let host = network_host(magisk);
    host.port.hold(Kind::CaptureStart);
    host.port.script(
        Kind::CaptureStop,
        Ok(settled(NetworkPrimitiveOutcome::CaptureStopRequested, true)),
    );
    let started = submit_network(
        &host,
        0x401,
        "capture",
        serde_json::json!({"operation": "start", "interface": "wlan0", "max_packets": 32}),
    )
    .await;
    let capture_id = task_id_of(succeeded(&started));
    wait_for(&host.port, Kind::CaptureStart).await;

    let stop_core = host.core.clone();
    let stop = {
        let request = network_request(
            0x402,
            "capture",
            serde_json::json!({"operation": "stop", "capture_id": capture_id.as_str()}),
        );
        tokio::spawn(async move {
            runtime::submit_public(
                &stop_core,
                &serde_json::to_vec(&request).unwrap(),
                TIMESTAMP.to_owned(),
                NOW_MS,
                true,
                |_| async { panic!("network escaped its canonical ingress handler") },
            )
            .await
        })
    };
    wait_for(&host.port, Kind::CaptureStop).await;
    let NetworkPrimitiveRequest::CaptureStop {
        capture_id: stopped_id,
    } = host.port.request(Kind::CaptureStop)
    else {
        panic!("the stop reached the wrong primitive");
    };
    assert_eq!(stopped_id, capture_id);
    host.port.release(Ok(settled(
        NetworkPrimitiveOutcome::CaptureSettled(capture_settlement(32, 8_192, false)),
        true,
    )));
    let response: serde_json::Value = serde_json::from_slice(&stop.await.unwrap()).unwrap();
    let result = succeeded(&response);
    assert_eq!(result["operation"], "capture_result");
    assert_eq!(result["capture_id"], capture_id.as_str());
    assert_eq!(result["packets_captured"], 32);
    assert_eq!(result["bytes_captured"], 8_192);
    assert_eq!(
        result["capture_ref"],
        "dbref:capture:00000000-0000-4000-8000-000000000001"
    );

    // The retained terminal result answers every later stop without a new side effect.
    let entered = host.port.entered();
    let retained = submit_network(
        &host,
        0x403,
        "capture",
        serde_json::json!({"operation": "stop", "capture_id": capture_id.as_str()}),
    )
    .await;
    assert_eq!(succeeded(&retained), result);
    assert_eq!(host.port.entered(), entered);

    // An identity that owns no capture Task is not found, and so is an unknown identity.
    let other = host
        .core
        .admit_task(TaskAdmission {
            request_id: request_id(0x404),
            payload_sha256: "0".repeat(64),
            task_id: uuid(0x8600_0000, 1),
            execution_id: uuid(0x8500_0000, 0x404),
            tool: MotherTool::Network,
            action: "packet".to_owned(),
            route: ExecutorRequest::Network(NetworkRoute::InspectOrDiagnose),
            payload: ExecutionPayload::NetworkCall(decode_raw_call(b"raw")),
            created_at: TIMESTAMP.to_owned(),
            settlement_bound_bytes: network_settlement_bound_bytes(),
            now_ms: NOW_MS,
        })
        .await
        .unwrap();
    let other_id = match other {
        runtime::TaskAdmissionResult::Admitted(snapshot) => snapshot.task_id,
        runtime::TaskAdmissionResult::Replay(snapshot) => snapshot.task_id,
    };
    let wrong_tool = submit_network(
        &host,
        0x405,
        "capture",
        serde_json::json!({"operation": "stop", "capture_id": other_id.as_str()}),
    )
    .await;
    assert_eq!(error_code(&wrong_tool), "NOT_FOUND");
    let unknown = submit_network(
        &host,
        0x406,
        "capture",
        serde_json::json!({"operation": "stop", "capture_id": uuid(0x8600_0000, 9).as_str()}),
    )
    .await;
    assert_eq!(error_code(&unknown), "NOT_FOUND");

    // A cancelled capture settles as cancelled and keeps no retained capture result.
    let host = network_host(magisk);
    host.port.hold(Kind::CaptureStart);
    let started = submit_network(
        &host,
        0x407,
        "capture",
        serde_json::json!({"operation": "start", "interface": "wlan0"}),
    )
    .await;
    let cancelled_id = task_id_of(succeeded(&started));
    wait_for(&host.port, Kind::CaptureStart).await;
    host.core
        .cancel_task(&cancelled_id, TIMESTAMP.to_owned(), NOW_MS)
        .await
        .unwrap();
    let snapshot = terminal_task(&host, &cancelled_id).await;
    assert_eq!(snapshot.state, TaskState::Cancelled);
    assert!(snapshot.cancel_requested);
    assert!(snapshot.result.is_none());
    let stopped = submit_network(
        &host,
        0x408,
        "capture",
        serde_json::json!({"operation": "stop", "capture_id": cancelled_id.as_str()}),
    )
    .await;
    assert_eq!(error_code(&stopped), "CANCELLED");
}

/// Gate 04: R-NET-004 gives a capture that never settles exactly the stop bound before
/// `TIMEOUT`, and the capture itself stays normally settleable.
#[tokio::test]
async fn i8_net_g04_capture_stop_times_out_inside_its_bound() {
    let host = network_host(magisk_facts(MAGISK_INSTANCE));
    host.port.hold(Kind::CaptureStart);
    host.port.script(
        Kind::CaptureStop,
        Ok(settled(NetworkPrimitiveOutcome::CaptureStopRequested, true)),
    );
    let started = submit_network(
        &host,
        0x411,
        "capture",
        serde_json::json!({"operation": "start", "interface": "wlan0"}),
    )
    .await;
    let capture_id = task_id_of(succeeded(&started));
    wait_for(&host.port, Kind::CaptureStart).await;

    let began = Instant::now();
    let stop = submit_network(
        &host,
        0x412,
        "capture",
        serde_json::json!({"operation": "stop", "capture_id": capture_id.as_str()}),
    )
    .await;
    assert_eq!(error_code(&stop), "TIMEOUT");
    let waited = began.elapsed();
    assert!(
        waited >= StdDuration::from_millis(runtime::NETWORK_CAPTURE_STOP_WAIT_MS),
        "the stop returned before its bound: {waited:?}"
    );
    assert!(
        waited < StdDuration::from_millis(runtime::NETWORK_CAPTURE_STOP_WAIT_MS + 5_000),
        "the stop waited past its bound: {waited:?}"
    );

    // The capture itself is untouched by the failed stop: it still settles normally.
    host.port.release(Ok(settled(
        NetworkPrimitiveOutcome::CaptureSettled(capture_settlement(1, 64, false)),
        true,
    )));
    let snapshot = terminal_task(&host, &capture_id).await;
    assert_eq!(snapshot.state, TaskState::Completed);
}

/// Gate 05: R-NET-006/R-NET-007 give packet build and decode one wire owner with computed
/// headers and no option or raw-header override.
#[tokio::test]
async fn i8_net_g05_packet_build_and_decode_share_one_wire_owner() {
    let apk = apk_facts(APK_INSTANCE);
    let built_surface = built(apk);
    let surface = built_surface.surface.clone();
    let execution = admitted(
        apk,
        &NetworkCall::Packet(NetworkPacketInput::Build {
            ethernet: Some(EthernetBuild {
                src_mac: "02:00:00:00:00:01".to_owned(),
                dst_mac: "02:00:00:00:00:02".to_owned(),
            }),
            network: NetworkBuild::Ipv4 {
                src: "10.0.0.1".to_owned(),
                dst: "10.0.0.2".to_owned(),
                ttl: 64,
                identification: 7,
                dont_fragment: true,
            },
            transport: TransportBuild::Tcp {
                src_port: 12_345,
                dst_port: 443,
                sequence: 1_000,
                acknowledgement: 2_000,
                flags: vec!["syn".to_owned(), "ack".to_owned()],
                window: 65_535,
            },
            payload_base64: Some(base64(b"droidbridge")),
        }),
        None,
        0x501,
    );
    let completion = start(&surface, execution).await.unwrap();
    let result = synchronous(&completion);
    assert_eq!(result["length"], 14 + 20 + 20 + 11);
    let packet_ref = result["packet_ref"].as_str().unwrap();
    assert!(packet_ref.starts_with("dbref:packet:"));
    assert_eq!(
        built_surface.port.entered(),
        0,
        "wire mechanics need no host primitive"
    );

    // The wire is what the request stated, and the builder computed every header field it
    // was never given: the IPv4 header checksum and the TCP checksum over its pseudo-header.
    let bytes = built_surface.artifacts.open(packet_ref).unwrap();
    assert_eq!(bytes.len() as u64, result["length"].as_u64().unwrap());
    assert_eq!(
        result["sha256"].as_str().unwrap(),
        hex_sha256(&bytes),
        "the published packet is the bytes the result describes"
    );
    assert_eq!(
        &bytes[0..6],
        &[0x02, 0, 0, 0, 0, 2],
        "Ethernet carries the destination first"
    );
    assert_eq!(&bytes[6..12], &[0x02, 0, 0, 0, 0, 1]);
    assert_eq!(u16::from_be_bytes([bytes[12], bytes[13]]), 0x0800);
    let ipv4 = &bytes[14..34];
    assert_eq!(ipv4[0], 0x45);
    assert_eq!(
        u16::from_be_bytes([ipv4[2], ipv4[3]]),
        20 + 20 + 11,
        "the IPv4 total length covers its own header plus the TCP segment"
    );
    assert_eq!(u16::from_be_bytes([ipv4[4], ipv4[5]]), 7);
    assert_eq!(u16::from_be_bytes([ipv4[6], ipv4[7]]), 0x4000);
    assert_eq!(ipv4[8], 64);
    assert_eq!(ipv4[9], 6);
    assert_eq!(
        internet_checksum(ipv4),
        0,
        "the IPv4 header checksum is computed"
    );
    let tcp = &bytes[34..];
    assert_eq!(u16::from_be_bytes([tcp[0], tcp[1]]), 12_345);
    assert_eq!(u16::from_be_bytes([tcp[2], tcp[3]]), 443);
    assert_eq!(u32::from_be_bytes([tcp[4], tcp[5], tcp[6], tcp[7]]), 1_000);
    assert_eq!(tcp[12], 0x50);
    assert_eq!(tcp[13], 0x12, "syn|ack is the canonical two-flag mask");
    assert_eq!(u16::from_be_bytes([tcp[14], tcp[15]]), 65_535);
    assert_eq!(&tcp[20..], b"droidbridge");
    let mut pseudo = Vec::new();
    pseudo.extend_from_slice(&[10, 0, 0, 1]);
    pseudo.extend_from_slice(&[10, 0, 0, 2]);
    pseudo.push(0);
    pseudo.push(6);
    pseudo.extend_from_slice(&(tcp.len() as u16).to_be_bytes());
    pseudo.extend_from_slice(tcp);
    assert_eq!(
        internet_checksum(&pseudo),
        0,
        "the TCP checksum is computed"
    );

    // Decoding the same bytes reports exactly those fields and the payload.
    let completion = start(
        &surface,
        admitted(apk, &decode_raw_call(&bytes), None, 0x502),
    )
    .await
    .unwrap();
    let decoded = synchronous(&completion);
    assert_eq!(decoded["length"], bytes.len());
    assert_eq!(decoded["ethernet"]["src_mac"], "02:00:00:00:00:01");
    assert_eq!(decoded["ethernet"]["dst_mac"], "02:00:00:00:00:02");
    assert_eq!(decoded["ethernet"]["ether_type"], 2_048);
    assert_eq!(decoded["ipv4"]["src"], "10.0.0.1");
    assert_eq!(decoded["ipv4"]["dst"], "10.0.0.2");
    assert_eq!(decoded["ipv4"]["ttl"], 64);
    assert_eq!(decoded["ipv4"]["protocol"], 6);
    assert_eq!(decoded["ipv4"]["identification"], 7);
    assert_eq!(decoded["ipv4"]["dont_fragment"], true);
    assert_eq!(decoded["tcp"]["src_port"], 12_345);
    assert_eq!(decoded["tcp"]["dst_port"], 443);
    assert_eq!(decoded["tcp"]["sequence"], 1_000);
    assert_eq!(decoded["tcp"]["acknowledgement"], 2_000);
    assert_eq!(decoded["tcp"]["flags"], serde_json::json!(["syn", "ack"]));
    assert_eq!(decoded["tcp"]["window"], 65_535);
    assert_eq!(decoded["payload_preview_base64"], base64(b"droidbridge"));
    assert_eq!(decoded["payload_total_bytes"], 11);
    assert_eq!(decoded["payload_truncated"], false);

    // A bare IPv4 packet decodes without an invented Ethernet header, an IPv6 UDP packet
    // decodes its own header, and a large payload preview is bounded.
    let ipv4_surface = built(apk);
    let surface = ipv4_surface.surface.clone();
    let completion = start(
        &surface,
        admitted(apk, &decode_raw_call(&built_tcp_packet()), None, 0x503),
    )
    .await
    .unwrap();
    let decoded = synchronous(&completion);
    assert!(decoded.get("ethernet").is_none());
    assert_eq!(decoded["ipv4"]["src"], "10.0.0.1");
    assert_eq!(decoded["payload_preview_base64"], base64(b"i8-net"));
    assert_eq!(ipv4_surface.port.entered(), 0);

    let completion = start(
        &surface,
        admitted(
            apk,
            &NetworkCall::Packet(NetworkPacketInput::Build {
                ethernet: None,
                network: NetworkBuild::Ipv6 {
                    src: "fd00::1".to_owned(),
                    dst: "fd00::2".to_owned(),
                    hop_limit: 32,
                },
                transport: TransportBuild::Udp {
                    src_port: 53,
                    dst_port: 53,
                },
                payload_base64: None,
            }),
            None,
            0x504,
        ),
    )
    .await
    .unwrap();
    let built_udp = synchronous(&completion);
    let bytes = ipv4_surface
        .artifacts
        .open(built_udp["packet_ref"].as_str().unwrap())
        .unwrap();
    assert_eq!(bytes[0], 0x60);
    assert_eq!(bytes[6], 17, "UDP is the next header");
    assert_eq!(bytes[7], 32);
    let completion = start(
        &surface,
        admitted(apk, &decode_raw_call(&bytes), None, 0x505),
    )
    .await
    .unwrap();
    let decoded = synchronous(&completion);
    assert_eq!(decoded["ipv6"]["src"], "fd00::1");
    assert_eq!(decoded["ipv6"]["dst"], "fd00::2");
    assert_eq!(decoded["ipv6"]["hop_limit"], 32);
    assert_eq!(decoded["ipv6"]["next_header"], 17);
    assert_eq!(decoded["udp"]["src_port"], 53);
    assert_eq!(decoded["payload_total_bytes"], 0);

    let completion = start(
        &surface,
        admitted(
            apk,
            &NetworkCall::Packet(NetworkPacketInput::Build {
                ethernet: None,
                network: NetworkBuild::Ipv4 {
                    src: "10.0.0.1".to_owned(),
                    dst: "10.0.0.2".to_owned(),
                    ttl: 1,
                    identification: 0,
                    dont_fragment: false,
                },
                transport: TransportBuild::Icmp {
                    icmp_type: 8,
                    code: 0,
                },
                payload_base64: Some(base64(&vec![0x41; 9_000])),
            }),
            None,
            0x506,
        ),
    )
    .await
    .unwrap();
    let bytes = ipv4_surface
        .artifacts
        .open(synchronous(&completion)["packet_ref"].as_str().unwrap())
        .unwrap();
    let completion = start(
        &surface,
        admitted(apk, &decode_raw_call(&bytes), None, 0x507),
    )
    .await
    .unwrap();
    let decoded = synchronous(&completion);
    assert_eq!(decoded["icmp"]["icmp_type"], 8);
    assert_eq!(decoded["icmp"]["code"], 0);
    assert_eq!(decoded["payload_total_bytes"], 9_000);
    assert_eq!(decoded["payload_truncated"], true);
    assert_eq!(
        from_base64(decoded["payload_preview_base64"].as_str().unwrap()).len(),
        runtime::NETWORK_PAYLOAD_PREVIEW_BYTES
    );

    // A capture packet is addressed by its published capture and its own index.
    let host = network_host(apk);
    let published = host
        .artifacts
        .publish_as("capture", &pcap(&[(1, 2, &built_tcp_packet())]))
        .unwrap();
    let completion = start(
        &host.artifacts_surface(),
        admitted(
            apk,
            &NetworkCall::Packet(NetworkPacketInput::Decode {
                source: PacketDecodeSource::Capture {
                    capture_ref: published.artifact_ref.clone(),
                    index: 0,
                },
            }),
            None,
            0x508,
        ),
    )
    .await
    .unwrap();
    assert_eq!(synchronous(&completion)["tcp"]["dst_port"], 443);
    assert_eq!(host.port.entered(), 0);
}

/// Gate 05: R-NET-008 injects one request per repetition with local acceptance only.
#[tokio::test]
async fn i8_net_g05_inject_repeats_once_per_request() {
    let magisk = magisk_facts(MAGISK_INSTANCE);
    let port = ScriptedPort::new();
    port.script(
        Kind::Inject,
        Ok(settled(
            NetworkPrimitiveOutcome::PacketInjected(PacketInjectResult {
                requested_packets: 3,
                accepted_packets: 3,
                bytes_accepted: 3 * 4,
            }),
            true,
        )),
    );
    let surface = NativeNetworkExecutionSurface::new(
        FakeArtifacts::default(),
        FakeCapabilities::new(capability(magisk)),
        port.clone(),
    );
    let completion = start(
        &surface,
        admitted(magisk, &inject_call(b"ping", 3), None, 0x511),
    )
    .await
    .unwrap();
    let result = synchronous(&completion);
    assert_eq!(result["requested_packets"], 3);
    assert_eq!(result["accepted_packets"], 3);
    assert_eq!(result["bytes_accepted"], 12);
    assert_eq!(
        port.entered(),
        1,
        "one inject request carries its repetition count"
    );
    let NetworkPrimitiveRequest::PacketInject {
        interface,
        packet,
        count,
        interval_ms,
    } = port.request(Kind::Inject)
    else {
        panic!("the inject reached the wrong primitive");
    };
    assert_eq!(interface, "wlan0");
    assert_eq!(packet, b"ping");
    assert_eq!(count, 3);
    assert_eq!(interval_ms, 10);
    assert_eq!(port.requests().len(), 1);
}

/// Gate 06: S-NET-005 keeps one capture format owner, so every `capture.read` is the same
/// reader whether the bytes came from a file or from the artifact store.
#[tokio::test]
async fn i8_net_g06_one_capture_format_owner() {
    let magisk = magisk_facts(MAGISK_INSTANCE);
    let bytes = pcap(&[
        (1_789_171_200, 500_000, &built_tcp_packet()),
        (1_789_171_201, 250_000, &[0x45, 0x00]),
    ]);

    // The declared format is the one classic-PCAP little-endian microsecond record, and the link
    // type it carries is the one the caller states for this stream.
    let header = pcap_file_header(PCAP_LINKTYPE_ETHERNET);
    assert_eq!(
        u32::from_le_bytes(header[0..4].try_into().unwrap()),
        runtime::PCAP_MAGIC_MICROSECOND
    );
    assert_eq!(u16::from_le_bytes(header[4..6].try_into().unwrap()), 2);
    assert_eq!(
        u32::from_le_bytes(header[16..20].try_into().unwrap()),
        runtime::PCAP_SNAPLEN
    );
    assert_eq!(
        u32::from_le_bytes(header[20..24].try_into().unwrap()),
        PCAP_LINKTYPE_ETHERNET
    );
    assert_eq!(
        u32::from_le_bytes(
            pcap_file_header(PCAP_LINKTYPE_RAW)[20..24]
                .try_into()
                .unwrap()
        ),
        PCAP_LINKTYPE_RAW
    );
    let records = read_pcap(&bytes).unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].index, 0);
    assert_eq!(records[0].seconds, 1_789_171_200);
    assert_eq!(records[0].microseconds, 500_000);
    assert_eq!(records[0].bytes, built_tcp_packet());
    assert_eq!(records[1].index, 1);
    assert_eq!(records[0].original_len, built_tcp_packet().len() as u32);
    assert!(
        read_pcap(&pcap_file_header(PCAP_LINKTYPE_ETHERNET))
            .unwrap()
            .is_empty()
    );
    // A stream a TUN device produced declares raw IP, and its records decode as bare IP.
    let raw = pcap_of(
        PCAP_LINKTYPE_RAW,
        &[(1_789_171_200, 500_000, &built_tcp_packet())],
    );
    let records = read_pcap(&raw).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].bytes, built_tcp_packet());

    // A caller-named file is read by the Runtime from bytes the host supplied.
    let host = network_host(magisk);
    host.port.script(
        Kind::CaptureRead,
        Ok(settled(
            NetworkPrimitiveOutcome::CaptureFileBytes(bytes.clone()),
            true,
        )),
    );
    let response = submit_network(
        &host,
        0x601,
        "capture",
        serde_json::json!({
            "operation": "read",
            "file": {"type": "path", "value": "/data/local/tmp/capture.pcap"},
            "offset_packet": 0,
            "max_packets": 200,
            "include_payload": true
        }),
    )
    .await;
    let result = succeeded(&response);
    let NetworkPrimitiveRequest::CaptureFileBytes { target } = host.port.request(Kind::CaptureRead)
    else {
        panic!("the file read reached the wrong primitive");
    };
    assert_eq!(target.value, "/data/local/tmp/capture.pcap");
    assert_eq!(target.target_type, FileTargetType::Path);
    assert_eq!(result["packets"].as_array().unwrap().len(), 2);
    assert_eq!(result["packets"][0]["index"], 0);
    assert_eq!(result["packets"][0]["protocol"], "tcp");
    assert_eq!(result["packets"][0]["src_ip"], "10.0.0.1");
    assert_eq!(result["packets"][0]["src_port"], 1234);
    assert_eq!(result["packets"][0]["length"], built_tcp_packet().len());
    assert_eq!(
        result["packets"][0]["timestamp"],
        "2026-09-12T00:00:00.500Z"
    );
    assert_eq!(
        result["packets"][0]["payload_preview_base64"],
        base64(b"i8-net")
    );
    assert_eq!(result["packets"][0]["payload_total_bytes"], 6);
    assert_eq!(result["truncated"], false);
    assert!(result.get("next_offset_packet").is_none());

    // The cursor and the truncation fact agree, so following the cursor ends cleanly.
    let response = submit_network(
        &host,
        0x602,
        "capture",
        serde_json::json!({
            "operation": "read",
            "file": {"type": "path", "value": "/data/local/tmp/capture.pcap"},
            "offset_packet": 1,
            "max_packets": 200,
            "include_payload": false
        }),
    )
    .await;
    let result = succeeded(&response);
    assert_eq!(result["packets"].as_array().unwrap().len(), 1);
    assert_eq!(result["packets"][0]["index"], 1);
    assert!(result["packets"][0].get("payload_preview_base64").is_none());
    assert!(result["packets"][0].get("payload_total_bytes").is_none());
    assert_eq!(result["truncated"], false);

    // A published capture is read inside the Runtime, with no host primitive at all.
    let host = network_host(magisk);
    let published = host.artifacts.publish_as("capture", &bytes).unwrap();
    let response = submit_network(
        &host,
        0x603,
        "capture",
        serde_json::json!({
            "operation": "read",
            "capture_ref": published.artifact_ref,
            "offset_packet": 0,
            "max_packets": 200
        }),
    )
    .await;
    let result = succeeded(&response);
    assert_eq!(result["packets"].as_array().unwrap().len(), 2);
    assert_eq!(host.port.entered(), 0);
    let missing = submit_network(
        &host,
        0x604,
        "capture",
        serde_json::json!({
            "operation": "read",
            "capture_ref": "dbref:capture:00000000-0000-4000-8000-0000000000ff"
        }),
    )
    .await;
    assert_eq!(error_code(&missing), "NOT_FOUND");

    // A file that is not the declared format is a capture failure, never a partial read.
    let mut corrupt = bytes.clone();
    corrupt[0] = 0xff;
    host.port.script(
        Kind::CaptureRead,
        Ok(settled(
            NetworkPrimitiveOutcome::CaptureFileBytes(corrupt),
            true,
        )),
    );
    let response = submit_network(
        &host,
        0x605,
        "capture",
        serde_json::json!({
            "operation": "read",
            "file": {"type": "path", "value": "/data/local/tmp/capture.pcap"}
        }),
    )
    .await;
    assert_eq!(error_code(&response), "CAPTURE_FAILED");
    let truncated = &bytes[..bytes.len() - 4];
    host.port.script(
        Kind::CaptureRead,
        Ok(settled(
            NetworkPrimitiveOutcome::CaptureFileBytes(truncated.to_vec()),
            true,
        )),
    );
    let response = submit_network(
        &host,
        0x606,
        "capture",
        serde_json::json!({
            "operation": "read",
            "file": {"type": "path", "value": "/data/local/tmp/capture.pcap"}
        }),
    )
    .await;
    assert_eq!(error_code(&response), "CAPTURE_FAILED");
}

/// Gate 07: R-NET-011 requires the API37 local-network grant before an APK-served LAN side
/// effect, and only for the App executor.
#[tokio::test]
async fn i8_net_g07_api37_lan_access_is_a_grant_on_the_app_executor_only() {
    let ungranted = Facts {
        local_network: CapabilityState::Unavailable,
        magisk_root: CapabilityState::Unavailable,
        ..apk_facts(APK_INSTANCE)
    };
    assert_eq!(
        domain::derive_capabilities(&grant_facts(ungranted), capability(ungranted).context)
            .unwrap()
            .network_local
            .state,
        CapabilityState::Unavailable
    );
    let granted = Facts {
        ..apk_facts(APK_INSTANCE)
    };
    assert_eq!(
        domain::derive_capabilities(&grant_facts(granted), capability(granted).context)
            .unwrap()
            .network_local
            .state,
        CapabilityState::Available
    );

    // An APK-served operation against a LAN address is refused before its side effect.
    for target in [
        "192.168.1.10",
        "10.1.2.3",
        "172.20.0.9",
        "169.254.10.10",
        "fd00::1",
        "fe80::1",
    ] {
        let host = network_host(ungranted);
        let response = submit_network(
            &host,
            0x701,
            "diagnose",
            serde_json::json!({"test": "tcp", "host": target, "port": 445}),
        )
        .await;
        assert_eq!(
            error_code(&response),
            "CAPABILITY_UNAVAILABLE",
            "{target} is a LAN target"
        );
        assert_eq!(host.port.entered(), 0, "{target} reached a provider");
    }

    // A granted App executor performs the same operation.
    let host = network_host(granted);
    host.port.script(
        Kind::Diagnose,
        Ok(settled(
            NetworkPrimitiveOutcome::Diagnose(NetworkDiagnoseResult::Tcp {
                outcome: DiagnosticOutcome::Refused,
                duration_ms: 1,
                host: "192.168.1.10".to_owned(),
                port: 445,
                remote_ip: None,
            }),
            true,
        )),
    );
    let response = submit_network(
        &host,
        0x702,
        "diagnose",
        serde_json::json!({"test": "tcp", "host": "192.168.1.10", "port": 445}),
    )
    .await;
    assert_eq!(succeeded(&response)["outcome"], "refused");

    // Public-Internet and same-device loopback targets are not LAN features.
    for target in [
        "8.8.8.8",
        "1.1.1.1",
        "127.0.0.1",
        "2001:4860:4860::8888",
        "example.com",
    ] {
        let host = network_host(ungranted);
        host.port.script(
            Kind::Diagnose,
            Ok(settled(
                NetworkPrimitiveOutcome::Diagnose(NetworkDiagnoseResult::Tcp {
                    outcome: DiagnosticOutcome::Unreachable,
                    duration_ms: 1,
                    host: target.to_owned(),
                    port: 443,
                    remote_ip: None,
                }),
                true,
            )),
        );
        let response = submit_network(
            &host,
            0x703,
            "diagnose",
            serde_json::json!({"test": "tcp", "host": target, "port": 443}),
        )
        .await;
        assert_eq!(
            succeeded(&response)["outcome"],
            "unreachable",
            "{target} is not a LAN feature"
        );
        assert_eq!(host.port.entered(), 1);
    }

    // Route and TLS targets obey the same rule; a test that names no target does not.
    let host = network_host(ungranted);
    let routed = submit_network(
        &host,
        0x704,
        "diagnose",
        serde_json::json!({"test": "route", "destination_ip": "192.168.1.10"}),
    )
    .await;
    assert_eq!(error_code(&routed), "CAPABILITY_UNAVAILABLE");
    let tls = submit_network(
        &host,
        0x705,
        "diagnose",
        serde_json::json!({"test": "tls", "host": "192.168.1.10"}),
    )
    .await;
    assert_eq!(error_code(&tls), "CAPABILITY_UNAVAILABLE");
    let connectivity = submit_network(
        &host,
        0x706,
        "diagnose",
        serde_json::json!({"test": "connectivity"}),
    )
    .await;
    assert_eq!(
        error_code(&connectivity),
        "INTERNAL_ERROR",
        "the scripted connectivity probe has no settlement"
    );
    assert_eq!(
        host.port.entered(),
        1,
        "only the target-less connectivity test reached a provider"
    );

    // Android 16 and below have no such constraint.
    let legacy = Facts {
        sdk_int: 36,
        ..ungranted
    };
    let host = network_host(legacy);
    host.port.script(
        Kind::Diagnose,
        Ok(settled(
            NetworkPrimitiveOutcome::Diagnose(NetworkDiagnoseResult::Tcp {
                outcome: DiagnosticOutcome::Success,
                duration_ms: 1,
                host: "192.168.1.10".to_owned(),
                port: 445,
                remote_ip: Some("192.168.1.10".to_owned()),
            }),
            true,
        )),
    );
    let response = submit_network(
        &host,
        0x707,
        "diagnose",
        serde_json::json!({"test": "tcp", "host": "192.168.1.10", "port": 445}),
    )
    .await;
    assert_eq!(succeeded(&response)["outcome"], "success");

    // The Magisk executor is never subject to the App grant.
    let host = network_host(Facts {
        local_network: CapabilityState::Unavailable,
        ..magisk_facts(MAGISK_INSTANCE)
    });
    host.port.script(
        Kind::Diagnose,
        Ok(settled(
            NetworkPrimitiveOutcome::Diagnose(NetworkDiagnoseResult::Tcp {
                outcome: DiagnosticOutcome::Success,
                duration_ms: 1,
                host: "192.168.1.10".to_owned(),
                port: 445,
                remote_ip: Some("192.168.1.10".to_owned()),
            }),
            true,
        )),
    );
    let response = submit_network(
        &host,
        0x708,
        "diagnose",
        serde_json::json!({"test": "tcp", "host": "192.168.1.10", "port": 445}),
    )
    .await;
    assert_eq!(succeeded(&response)["outcome"], "success");
    assert_eq!(host.port.entered(), 1);

    // A LAN operation on the APK host that reaches neither executor is still refused for
    // the executor it would have used.
    let unauthenticated = Facts {
        app_execution_surface: CapabilityState::Unavailable,
        ..ungranted
    };
    let host = network_host(unauthenticated);
    let response = submit_network(
        &host,
        0x709,
        "diagnose",
        serde_json::json!({"test": "tcp", "host": "192.168.1.10", "port": 445}),
    )
    .await;
    assert_eq!(error_code(&response), "CAPABILITY_UNAVAILABLE");
    assert_eq!(host.port.entered(), 0);
}

/// Gate 08: R-NET-009 gives every test one tagged result with one outcome vocabulary, and
/// the shared probes report the performed negative answers as completed tests.
#[tokio::test]
async fn i8_net_g08_diagnose_reports_the_five_test_shapes() {
    let apk = apk_facts(APK_INSTANCE);

    // The probe input reaches the executor exactly as parsed, defaults included.
    let cases: [(&str, serde_json::Value, NetworkDiagnoseInput); 5] = [
        (
            "connectivity",
            serde_json::json!({"test": "connectivity"}),
            NetworkDiagnoseInput::Connectivity {},
        ),
        (
            "dns",
            serde_json::json!({"test": "dns", "name": "example.com"}),
            NetworkDiagnoseInput::Dns {
                name: "example.com".to_owned(),
                record_type: DnsRecordType::A,
            },
        ),
        (
            "tcp",
            serde_json::json!({"test": "tcp", "host": "example.com", "port": 443}),
            NetworkDiagnoseInput::Tcp {
                host: "example.com".to_owned(),
                port: 443,
                timeout_ms: 5_000,
            },
        ),
        (
            "tls",
            serde_json::json!({"test": "tls", "host": "example.com"}),
            NetworkDiagnoseInput::Tls {
                host: "example.com".to_owned(),
                port: 443,
                server_name: None,
                timeout_ms: 5_000,
            },
        ),
        (
            "route",
            serde_json::json!({"test": "route", "destination_ip": "8.8.8.8"}),
            NetworkDiagnoseInput::Route {
                destination_ip: "8.8.8.8".to_owned(),
            },
        ),
    ];
    for (index, (test, input, expected)) in cases.iter().enumerate() {
        let host = network_host(apk);
        host.port.script(
            Kind::Diagnose,
            Ok(settled(
                NetworkPrimitiveOutcome::Diagnose(diagnose_result(test)),
                true,
            )),
        );
        let response = submit_network(&host, 0x800 + index as u64, "diagnose", input.clone()).await;
        let result = succeeded(&response);
        assert_eq!(result["test"], *test);
        assert!(result["duration_ms"].is_u64());
        let NetworkPrimitiveRequest::Diagnose(reached, _) = host.port.request(Kind::Diagnose)
        else {
            panic!("the {test} test did not reach the diagnose executor");
        };
        assert_eq!(
            reached, *expected,
            "the {test} test reached the executor with different parameters"
        );
        assert_eq!(
            result
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>(),
            diagnose_keys(test),
            "the {test} result changed its field set"
        );
    }

    // A performed negative answer is a completed test with that outcome, never an error and
    // never a fabricated success; `certificate_verified` is true only on success.
    for outcome in [
        DiagnosticOutcome::Success,
        DiagnosticOutcome::NotFound,
        DiagnosticOutcome::Refused,
        DiagnosticOutcome::Timeout,
        DiagnosticOutcome::Unreachable,
        DiagnosticOutcome::DnsError,
        DiagnosticOutcome::TlsError,
        DiagnosticOutcome::NoRoute,
    ] {
        let host = network_host(apk);
        host.port.script(
            Kind::Diagnose,
            Ok(settled(
                NetworkPrimitiveOutcome::Diagnose(NetworkDiagnoseResult::Tls {
                    outcome,
                    duration_ms: 4,
                    host: "example.com".to_owned(),
                    port: 443,
                    server_name: "example.com".to_owned(),
                    remote_ip: Some("93.184.216.34".to_owned()),
                    certificate_verified: outcome == DiagnosticOutcome::Success,
                }),
                true,
            )),
        );
        let response = submit_network(
            &host,
            0x810,
            "diagnose",
            serde_json::json!({"test": "tls", "host": "example.com"}),
        )
        .await;
        let result = succeeded(&response);
        assert_eq!(result["test"], "tls");
        assert_eq!(result["outcome"], diagnostic_token(outcome));
        assert_eq!(
            result["certificate_verified"],
            outcome == DiagnosticOutcome::Success
        );
        assert_eq!(result["server_name"], "example.com");
        assert_eq!(result["remote_ip"], "93.184.216.34");
    }

    // An inability to start the assigned executor stays a structured error.
    let host = network_host(apk);
    host.port.script(
        Kind::Diagnose,
        Err(ExecutionFailure {
            error: DomainError::new(
                ErrorCode::CapabilityUnavailable,
                "the assigned executor could not start the probe",
            ),
            cleanup_verified: true,
        }),
    );
    let response = submit_network(
        &host,
        0x811,
        "diagnose",
        serde_json::json!({"test": "dns", "name": "example.com"}),
    )
    .await;
    assert_eq!(error_code(&response), "CAPABILITY_UNAVAILABLE");

    // The one shared probe layer both hosts call reports the same vocabulary from the local
    // network: a resolution failure, an absent family, a refused port and a failed
    // handshake are all performed tests.
    let dns = network_dns_probe("127.0.0.1", DnsRecordType::A);
    let NetworkDiagnoseResult::Dns {
        outcome,
        name,
        record_type,
        addresses,
        ..
    } = dns
    else {
        panic!("the dns probe answered another test");
    };
    assert_eq!(outcome, DiagnosticOutcome::Success);
    assert_eq!(name, "127.0.0.1");
    assert_eq!(record_type, DnsRecordType::A);
    assert_eq!(addresses, vec!["127.0.0.1".to_owned()]);
    let absent = network_dns_probe("127.0.0.1", DnsRecordType::Aaaa);
    let NetworkDiagnoseResult::Dns {
        outcome, addresses, ..
    } = absent
    else {
        panic!("the dns probe answered another test");
    };
    assert_eq!(outcome, DiagnosticOutcome::NotFound);
    assert!(addresses.is_empty());
    let unresolved = network_dns_probe("i8-net..invalid", DnsRecordType::A);
    let NetworkDiagnoseResult::Dns {
        outcome, addresses, ..
    } = unresolved
    else {
        panic!("the dns probe answered another test");
    };
    assert_eq!(
        outcome,
        DiagnosticOutcome::DnsError,
        "a name the resolver refuses is a negative diagnostic"
    );
    assert!(addresses.is_empty());

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let open_port = listener.local_addr().unwrap().port();
    let tcp = network_tcp_probe("127.0.0.1", open_port, 5_000);
    let NetworkDiagnoseResult::Tcp {
        outcome,
        remote_ip,
        port,
        ..
    } = tcp
    else {
        panic!("the tcp probe answered another test");
    };
    assert_eq!(outcome, DiagnosticOutcome::Success);
    assert_eq!(remote_ip.as_deref(), Some("127.0.0.1"));
    assert_eq!(port, open_port);
    drop(listener);
    let refused = network_tcp_probe("127.0.0.1", open_port, 5_000);
    let NetworkDiagnoseResult::Tcp {
        outcome, remote_ip, ..
    } = refused
    else {
        panic!("the tcp probe answered another test");
    };
    assert_eq!(outcome, DiagnosticOutcome::Refused);
    assert!(remote_ip.is_none());

    // TLS verifies against `server_name` or `host`, and the pinned configuration is the one
    // builder: no verification bypass exists, so a non-TLS peer is a performed `tls_error`
    // and an invalid server name is a structured refusal.
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let plain_port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        if let Ok((mut socket, _)) = listener.accept() {
            let mut buffer = [0u8; 1_024];
            let _ = socket.read(&mut buffer);
            let _ = socket.write_all(b"not a tls record at all");
        }
    });
    let tls = network_tls_probe("127.0.0.1", plain_port, None, 5_000).unwrap();
    let NetworkDiagnoseResult::Tls {
        outcome,
        server_name,
        certificate_verified,
        remote_ip,
        ..
    } = tls
    else {
        panic!("the tls probe answered another test");
    };
    assert_eq!(outcome, DiagnosticOutcome::TlsError);
    assert_eq!(
        server_name, "127.0.0.1",
        "an omitted server name is exactly the host"
    );
    assert!(!certificate_verified);
    assert_eq!(remote_ip.as_deref(), Some("127.0.0.1"));
    server.join().unwrap();
    assert_eq!(
        network_tls_probe("127.0.0.1", plain_port, Some("not a name"), 5_000)
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
    let config = network_tls_client_config();
    assert!(config.enable_sni);
    assert!(config.alpn_protocols.is_empty());
    assert!(
        Arc::ptr_eq(
            config.crypto_provider(),
            rustls::crypto::CryptoProvider::get_default()
                .expect("the TLS owner installs one crypto provider as the process default")
        ),
        "the client config must trust the provider this owner pins"
    );
}

fn diagnose_result(test: &str) -> NetworkDiagnoseResult {
    match test {
        "connectivity" => NetworkDiagnoseResult::Connectivity {
            outcome: DiagnosticOutcome::Success,
            duration_ms: 1,
            active_network_present: Some(true),
            default_route_present: Some(true),
            dns_configured: Some(true),
        },
        "dns" => NetworkDiagnoseResult::Dns {
            outcome: DiagnosticOutcome::Success,
            duration_ms: 1,
            name: "example.com".to_owned(),
            record_type: DnsRecordType::A,
            addresses: vec!["93.184.216.34".to_owned()],
        },
        "tcp" => NetworkDiagnoseResult::Tcp {
            outcome: DiagnosticOutcome::Success,
            duration_ms: 1,
            host: "example.com".to_owned(),
            port: 443,
            remote_ip: Some("93.184.216.34".to_owned()),
        },
        "tls" => NetworkDiagnoseResult::Tls {
            outcome: DiagnosticOutcome::Success,
            duration_ms: 1,
            host: "example.com".to_owned(),
            port: 443,
            server_name: "example.com".to_owned(),
            remote_ip: Some("93.184.216.34".to_owned()),
            certificate_verified: true,
        },
        "route" => NetworkDiagnoseResult::Route {
            outcome: DiagnosticOutcome::Success,
            duration_ms: 1,
            destination_ip: "8.8.8.8".to_owned(),
            interface: Some("wlan0".to_owned()),
            gateway: Some("192.168.1.1".to_owned()),
        },
        other => panic!("no fixture for {other}"),
    }
}

fn diagnose_keys(test: &str) -> BTreeSet<&'static str> {
    match test {
        "connectivity" => BTreeSet::from([
            "test",
            "outcome",
            "duration_ms",
            "active_network_present",
            "default_route_present",
            "dns_configured",
        ]),
        "dns" => BTreeSet::from([
            "test",
            "outcome",
            "duration_ms",
            "name",
            "record_type",
            "addresses",
        ]),
        "tcp" => BTreeSet::from([
            "test",
            "outcome",
            "duration_ms",
            "host",
            "port",
            "remote_ip",
        ]),
        "tls" => BTreeSet::from([
            "test",
            "outcome",
            "duration_ms",
            "host",
            "port",
            "server_name",
            "remote_ip",
            "certificate_verified",
        ]),
        "route" => BTreeSet::from([
            "test",
            "outcome",
            "duration_ms",
            "destination_ip",
            "interface",
            "gateway",
        ]),
        other => panic!("no fixture for {other}"),
    }
}

const fn diagnostic_token(outcome: DiagnosticOutcome) -> &'static str {
    match outcome {
        DiagnosticOutcome::Success => "success",
        DiagnosticOutcome::NotFound => "not_found",
        DiagnosticOutcome::Refused => "refused",
        DiagnosticOutcome::Timeout => "timeout",
        DiagnosticOutcome::Unreachable => "unreachable",
        DiagnosticOutcome::DnsError => "dns_error",
        DiagnosticOutcome::TlsError => "tls_error",
        DiagnosticOutcome::NoRoute => "no_route",
    }
}

/// Gate 09: R-NET-002..009 bounds are enforced before executor resolution, so an
/// out-of-range request never reaches a provider and never becomes a primitive outcome.
#[tokio::test]
async fn i8_net_g09_capture_and_inject_bounds_precede_execution() {
    let apk = apk_facts(APK_INSTANCE);
    let magisk = magisk_facts(MAGISK_INSTANCE);
    let over_packet = base64(&vec![0x41; NETWORK_MAX_PACKET_BYTES + 1]);
    let over_payload = base64(&vec![0x41; NETWORK_MAX_PACKET_BYTES + 1]);

    let rejected: Vec<(&str, &str, serde_json::Value)> = vec![
        (
            "inspect max_entries 0",
            "inspect",
            serde_json::json!({"scope": "all", "max_entries": 0}),
        ),
        (
            "inspect max_entries over",
            "inspect",
            serde_json::json!({"scope": "all", "max_entries": NETWORK_MAX_SCOPE_ENTRIES + 1}),
        ),
        (
            "capture interface empty",
            "capture",
            serde_json::json!({"operation": "start", "interface": ""}),
        ),
        (
            "capture interface NUL",
            "capture",
            serde_json::json!({"operation": "start", "interface": "wlan\u{0}0"}),
        ),
        (
            "capture filter NUL",
            "capture",
            serde_json::json!({"operation": "start", "interface": "wlan0", "filter": "tcp\u{0}"}),
        ),
        (
            "capture max_packets 0",
            "capture",
            serde_json::json!({"operation": "start", "interface": "wlan0", "max_packets": 0}),
        ),
        (
            "capture max_packets over",
            "capture",
            serde_json::json!({
                "operation": "start",
                "interface": "wlan0",
                "max_packets": NETWORK_MAX_CAPTURE_PACKETS + 1
            }),
        ),
        (
            "capture max_bytes 0",
            "capture",
            serde_json::json!({"operation": "start", "interface": "wlan0", "max_bytes": 0}),
        ),
        (
            "capture max_bytes over",
            "capture",
            serde_json::json!({
                "operation": "start",
                "interface": "wlan0",
                "max_bytes": NETWORK_MAX_CAPTURE_BYTES + 1
            }),
        ),
        (
            "capture max_duration_ms 0",
            "capture",
            serde_json::json!({"operation": "start", "interface": "wlan0", "max_duration_ms": 0}),
        ),
        (
            "capture max_duration_ms over",
            "capture",
            serde_json::json!({
                "operation": "start",
                "interface": "wlan0",
                "max_duration_ms": runtime::NETWORK_MAX_CAPTURE_DURATION_MS + 1
            }),
        ),
        (
            "read max_packets 0",
            "capture",
            serde_json::json!({
                "operation": "read",
                "file": {"type": "path", "value": "/tmp/c.pcap"},
                "max_packets": 0
            }),
        ),
        (
            "read max_packets over",
            "capture",
            serde_json::json!({
                "operation": "read",
                "file": {"type": "path", "value": "/tmp/c.pcap"},
                "max_packets": NETWORK_MAX_READ_PACKETS + 1
            }),
        ),
        (
            "read capture_ref is not a capture",
            "capture",
            serde_json::json!({
                "operation": "read",
                "capture_ref": "/tmp/c.pcap",
                "max_packets": 10
            }),
        ),
        (
            "decode raw empty",
            "packet",
            serde_json::json!({"operation": "decode", "raw_base64": ""}),
        ),
        (
            "decode raw over",
            "packet",
            serde_json::json!({"operation": "decode", "raw_base64": over_packet}),
        ),
        (
            "decode packet_ref is not a packet",
            "packet",
            serde_json::json!({"operation": "decode", "packet_ref": "dbref:capture:0"}),
        ),
        (
            "build src_mac invalid",
            "packet",
            serde_json::json!({
                "operation": "build",
                "network": {"type": "ipv4", "src": "10.0.0.1", "dst": "10.0.0.2"},
                "transport": {"type": "udp", "src_port": 1, "dst_port": 2},
                "ethernet": {"src_mac": "02:00:00:00:00", "dst_mac": "02:00:00:00:00:02"}
            }),
        ),
        (
            "build address invalid",
            "packet",
            serde_json::json!({
                "operation": "build",
                "network": {"type": "ipv4", "src": "10.0.0.256", "dst": "10.0.0.2"},
                "transport": {"type": "udp", "src_port": 1, "dst_port": 2}
            }),
        ),
        (
            "build flag unknown",
            "packet",
            serde_json::json!({
                "operation": "build",
                "network": {"type": "ipv4", "src": "10.0.0.1", "dst": "10.0.0.2"},
                "transport": {"type": "tcp", "src_port": 1, "dst_port": 2, "flags": ["urgent"]}
            }),
        ),
        (
            "build payload over",
            "packet",
            serde_json::json!({
                "operation": "build",
                "network": {"type": "ipv4", "src": "10.0.0.1", "dst": "10.0.0.2"},
                "transport": {"type": "udp", "src_port": 1, "dst_port": 2},
                "payload_base64": over_payload
            }),
        ),
        (
            "build carries a raw header field",
            "packet",
            serde_json::json!({
                "operation": "build",
                "network": {"type": "ipv4", "src": "10.0.0.1", "dst": "10.0.0.2", "checksum": 0},
                "transport": {"type": "udp", "src_port": 1, "dst_port": 2}
            }),
        ),
        (
            "inject interface empty",
            "packet",
            serde_json::json!({
                "operation": "inject",
                "interface": "",
                "packet": {"raw_base64": base64(b"raw")}
            }),
        ),
        (
            "inject count 0",
            "packet",
            serde_json::json!({
                "operation": "inject",
                "interface": "wlan0",
                "packet": {"raw_base64": base64(b"raw")},
                "count": 0
            }),
        ),
        (
            "inject count over",
            "packet",
            serde_json::json!({
                "operation": "inject",
                "interface": "wlan0",
                "packet": {"raw_base64": base64(b"raw")},
                "count": NETWORK_MAX_INJECT_COUNT + 1
            }),
        ),
        (
            "inject interval over",
            "packet",
            serde_json::json!({
                "operation": "inject",
                "interface": "wlan0",
                "packet": {"raw_base64": base64(b"raw")},
                "interval_ms": runtime::NETWORK_MAX_INJECT_INTERVAL_MS + 1
            }),
        ),
        (
            "inject packet empty",
            "packet",
            serde_json::json!({
                "operation": "inject",
                "interface": "wlan0",
                "packet": {"raw_base64": ""}
            }),
        ),
        (
            "inject packet over",
            "packet",
            serde_json::json!({
                "operation": "inject",
                "interface": "wlan0",
                "packet": {"raw_base64": over_packet}
            }),
        ),
        (
            "inject packet_ref is not a packet",
            "packet",
            serde_json::json!({
                "operation": "inject",
                "interface": "wlan0",
                "packet": {"packet_ref": "dbref:capture:0"}
            }),
        ),
        (
            "diagnose name empty",
            "diagnose",
            serde_json::json!({"test": "dns", "name": ""}),
        ),
        (
            "diagnose name NUL",
            "diagnose",
            serde_json::json!({"test": "dns", "name": "a\u{0}b"}),
        ),
        (
            "diagnose port 0",
            "diagnose",
            serde_json::json!({"test": "tcp", "host": "example.com", "port": 0}),
        ),
        (
            "diagnose timeout under",
            "diagnose",
            serde_json::json!({"test": "tcp", "host": "example.com", "port": 443, "timeout_ms": 99}),
        ),
        (
            "diagnose timeout over",
            "diagnose",
            serde_json::json!({"test": "tcp", "host": "example.com", "port": 443, "timeout_ms": 60_001}),
        ),
        (
            "diagnose tls server_name empty",
            "diagnose",
            serde_json::json!({"test": "tls", "host": "example.com", "server_name": ""}),
        ),
        (
            "diagnose route empty",
            "diagnose",
            serde_json::json!({"test": "route", "destination_ip": ""}),
        ),
    ];

    for (index, (label, action, input)) in rejected.iter().enumerate() {
        // The APK host cannot own capture or injection at all, so an out-of-range request
        // still answering INVALID_ARGUMENT proves the bounds ran first.
        let host = network_host(apk);
        let response = submit_network(&host, 0x900 + index as u64, action, input.clone()).await;
        assert_eq!(error_code(&response), "INVALID_ARGUMENT", "{label}");
        assert_eq!(host.port.entered(), 0, "{label} reached a provider");
        refused_call(action, input);
    }

    // Exactly at every bound the request is admitted and reaches the provider.
    let host = network_host(magisk);
    host.port.script(
        Kind::Inspect,
        Ok(inspect_settlement(None, None, None, None)),
    );
    host.port.script(
        Kind::CaptureStart,
        Ok(settled(
            NetworkPrimitiveOutcome::CaptureSettled(capture_settlement(0, 0, false)),
            true,
        )),
    );
    host.port.script(
        Kind::Inject,
        Ok(settled(
            NetworkPrimitiveOutcome::PacketInjected(contract::PacketInjectResult {
                requested_packets: NETWORK_MAX_INJECT_COUNT as u64,
                accepted_packets: NETWORK_MAX_INJECT_COUNT as u64,
                bytes_accepted: u64::from(NETWORK_MAX_INJECT_COUNT)
                    * NETWORK_MAX_PACKET_BYTES as u64,
            }),
            true,
        )),
    );
    let response = submit_network(
        &host,
        0x980,
        "inspect",
        serde_json::json!({"scope": "all", "max_entries": 1}),
    )
    .await;
    assert_eq!(
        succeeded(&response)["availability"]["interfaces"]["state"],
        "unknown"
    );
    let response = submit_network(
        &host,
        0x981,
        "capture",
        serde_json::json!({
            "operation": "start",
            "interface": "wlan0",
            "max_packets": NETWORK_MAX_CAPTURE_PACKETS,
            "max_bytes": NETWORK_MAX_CAPTURE_BYTES,
            "max_duration_ms": runtime::NETWORK_MAX_CAPTURE_DURATION_MS
        }),
    )
    .await;
    let capture_id = task_id_of(succeeded(&response));
    let snapshot = terminal_task(&host, &capture_id).await;
    assert_eq!(snapshot.state, TaskState::Completed);
    let response = submit_network(
        &host,
        0x982,
        "packet",
        serde_json::json!({
            "operation": "inject",
            "interface": "wlan0",
            "packet": {"raw_base64": base64(&vec![0x41; NETWORK_MAX_PACKET_BYTES])},
            "count": NETWORK_MAX_INJECT_COUNT,
            "interval_ms": runtime::NETWORK_MAX_INJECT_INTERVAL_MS
        }),
    )
    .await;
    assert_eq!(
        succeeded(&response)["requested_packets"],
        NETWORK_MAX_INJECT_COUNT
    );
    assert_eq!(host.port.entered(), 3);
}

/// One refused validation vector through the typed call. A vector the contract's own wire
/// rules refuse never becomes a call; every other vector must be refused by the single
/// validation entry point, which is what makes the bounds unreachable from a primitive.
fn refused_call(action: &str, input: &serde_json::Value) {
    let Ok(call) = serde_json::from_value::<NetworkCall>(serde_json::json!({
        "action": action,
        "input": input
    })) else {
        return;
    };
    assert!(
        validate_network_input(&call).is_err(),
        "the vector reached the handler but passed validation: {action} {input}"
    );
}

/// Gate 10: verified cleanup is part of one execution's settlement, and an unverified
/// cleanup withdraws readiness through the host-control owner.
#[tokio::test]
async fn i8_net_g10_network_cleanup_is_verified_or_the_execution_fails() {
    let magisk = magisk_facts(MAGISK_INSTANCE);

    // A synchronous operation whose primitive could not verify cleanup fails as such and
    // reports the unverified cleanup to the host.
    let host = network_host(magisk);
    host.port.script(
        Kind::Inspect,
        Ok(inspect_settlement(
            Some(Established {
                entries: Vec::new(),
                truncated: false,
            }),
            None,
            None,
            None,
        )),
    );
    let response = submit_network(
        &host,
        0xA01,
        "inspect",
        serde_json::json!({"scope": "interfaces"}),
    )
    .await;
    assert_eq!(
        succeeded(&response)["interfaces"].as_array().unwrap().len(),
        0
    );
    assert!(host.control.cleanup_reports().is_empty());

    let host = network_host(magisk);
    host.port.script(
        Kind::Inspect,
        Ok(settled(
            NetworkPrimitiveOutcome::Inspect(NetworkInspectSettlement::default()),
            false,
        )),
    );
    let response = submit_network(
        &host,
        0xA02,
        "inspect",
        serde_json::json!({"scope": "interfaces"}),
    )
    .await;
    assert_eq!(error_code(&response), "IO_ERROR");
    assert_eq!(host.control.cleanup_reports().len(), 1);
    assert_eq!(
        host.control.cleanup_reports()[0].0,
        capability(magisk).fence,
        "the unverified cleanup is reported against the admission fence"
    );
    assert_eq!(
        host.capabilities.current().unwrap().context.readiness,
        RuntimeReadiness::Unavailable,
        "an unverified cleanup withdraws readiness"
    );

    // A capture Task carries the same rule into its terminal state.
    let host = network_host(magisk);
    host.port.script(
        Kind::CaptureStart,
        Ok(settled(
            NetworkPrimitiveOutcome::CaptureSettled(capture_settlement(4, 256, false)),
            false,
        )),
    );
    let started = submit_network(
        &host,
        0xA03,
        "capture",
        serde_json::json!({"operation": "start", "interface": "wlan0"}),
    )
    .await;
    let capture_id = task_id_of(succeeded(&started));
    let snapshot = terminal_task(&host, &capture_id).await;
    assert_eq!(
        snapshot.state,
        TaskState::Interrupted,
        "an unverified cleanup is not a truthful failure"
    );
    assert_eq!(snapshot.error.as_ref().unwrap().code, ErrorCode::IoError);
    assert!(snapshot.result.is_none());
    assert_eq!(host.control.cleanup_reports().len(), 1);
}

#[derive(Default)]
struct RecordingEventSource {
    starts: Mutex<Vec<NetworkDefaultSourceRegistration>>,
    stops: Mutex<Vec<NetworkDefaultSourceRegistration>>,
    ingress: Mutex<Option<NetworkDefaultEventIngress>>,
    fail_stop: AtomicBool,
}

impl RecordingEventSource {
    fn emit(&self, event: NetworkDefaultChangedEvent) -> Result<NetworkEventDelivery, DomainError> {
        self.ingress
            .lock()
            .unwrap()
            .as_ref()
            .expect("source is started")
            .observe(event)
    }

    fn ingress(&self) -> NetworkDefaultEventIngress {
        self.ingress
            .lock()
            .unwrap()
            .as_ref()
            .expect("source is started")
            .clone()
    }
}

impl NetworkDefaultEventSource for RecordingEventSource {
    fn start(
        &self,
        registration: &NetworkDefaultSourceRegistration,
        ingress: NetworkDefaultEventIngress,
    ) -> Result<(), DomainError> {
        self.starts.lock().unwrap().push(registration.clone());
        *self.ingress.lock().unwrap() = Some(ingress);
        Ok(())
    }

    fn stop(&self, registration: &NetworkDefaultSourceRegistration) -> Result<(), DomainError> {
        self.stops.lock().unwrap().push(registration.clone());
        if self.fail_stop.load(Ordering::Acquire) {
            return Err(DomainError::new(
                ErrorCode::IoError,
                "scripted source cleanup failed",
            ));
        }
        *self.ingress.lock().unwrap() = None;
        Ok(())
    }
}

/// Gate 11: the I9-facing event plane uses one bounded in-memory receiver, establishes a
/// baseline before delivery, and never creates an ArtifactStore/`data` representation.
#[tokio::test]
async fn i8_net_g11_default_network_event_plane_is_bounded_and_not_an_artifact() {
    let plane = NetworkDefaultEventPlane::new();
    let source = Arc::new(RecordingEventSource::default());
    let fence = capability(apk_facts(APK_INSTANCE)).fence;
    let mut subscription = plane
        .subscribe(
            fence,
            Arc::clone(&source) as Arc<dyn NetworkDefaultEventSource>,
        )
        .unwrap();

    let wifi = NetworkDefaultChangedEvent::new(Some("wifi-1".to_owned()), Some("wifi".to_owned()));
    assert_eq!(
        source.emit(wifi.clone()).unwrap(),
        NetworkEventDelivery::Baseline
    );
    assert!(matches!(
        subscription.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));
    assert_eq!(source.emit(wifi).unwrap(), NetworkEventDelivery::Unchanged);

    let cell =
        NetworkDefaultChangedEvent::new(Some("cell-7".to_owned()), Some("cellular".to_owned()));
    assert_eq!(
        source.emit(cell.clone()).unwrap(),
        NetworkEventDelivery::Delivered
    );
    assert_eq!(subscription.recv().await, Some(cell));

    let lost = NetworkDefaultChangedEvent::new(None, None);
    assert_eq!(
        source.emit(lost.clone()).unwrap(),
        NetworkEventDelivery::Delivered
    );
    assert_eq!(subscription.recv().await, Some(lost));

    for index in 0..NETWORK_EVENT_CHANNEL_CAPACITY {
        let event = NetworkDefaultChangedEvent::new(Some(format!("net-{index}")), None);
        assert_eq!(source.emit(event).unwrap(), NetworkEventDelivery::Delivered);
    }
    assert_eq!(
        source
            .emit(NetworkDefaultChangedEvent::new(
                Some("newest-drop".to_owned()),
                None
            ))
            .unwrap(),
        NetworkEventDelivery::DroppedSaturated
    );
    assert_eq!(plane.saturation_dropped(), 1);

    assert_eq!(
        source
            .emit(NetworkDefaultChangedEvent::new(Some(String::new()), None))
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        source
            .emit(NetworkDefaultChangedEvent::new(
                Some("x".repeat(NETWORK_EVENT_FACT_BYTES + 1)),
                None,
            ))
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );

    assert_eq!(NETWORK_DEFAULT_CHANGED_EVENT, "network.default_changed");
    for r#match in [
        None,
        Some(BTreeMap::from([(
            "network_id".to_owned(),
            ScalarValue::String("wifi-1".to_owned()),
        )])),
        Some(BTreeMap::from([(
            "transport".to_owned(),
            ScalarValue::String("wifi".to_owned()),
        )])),
    ] {
        validate_automation(&event_automation(NETWORK_DEFAULT_CHANGED_EVENT, r#match)).unwrap();
    }
    assert_eq!(
        validate_automation(&event_automation("network.default_lost", None))
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );

    subscription.close().unwrap();
}

/// Gate 12: one generation-fenced subscription owns source start, replacement and cleanup;
/// stale callbacks cannot cross replacement, and failed cleanup blocks a second source.
#[tokio::test]
async fn i8_net_g12_subscription_lifecycle_is_single_owned_and_generation_fenced() {
    let plane = NetworkDefaultEventPlane::new();
    let first = Arc::new(RecordingEventSource::default());
    let second = Arc::new(RecordingEventSource::default());
    let fence = capability(apk_facts(APK_INSTANCE)).fence;
    let mut subscription = plane
        .subscribe(
            fence.clone(),
            Arc::clone(&first) as Arc<dyn NetworkDefaultEventSource>,
        )
        .unwrap();
    let stale_ingress = first.ingress();

    assert_eq!(
        plane
            .subscribe(fence.clone(), Arc::new(RecordingEventSource::default()),)
            .unwrap_err()
            .code,
        ErrorCode::AlreadyExists
    );

    plane
        .replace_source(
            subscription.generation(),
            Arc::clone(&second) as Arc<dyn NetworkDefaultEventSource>,
        )
        .unwrap();
    assert_eq!(first.stops.lock().unwrap().len(), 1);
    assert_eq!(second.starts.lock().unwrap().len(), 1);
    assert_eq!(
        stale_ingress
            .observe(NetworkDefaultChangedEvent::new(
                Some("stale".to_owned()),
                None
            ))
            .unwrap(),
        NetworkEventDelivery::IgnoredStale
    );
    assert_eq!(
        second
            .emit(NetworkDefaultChangedEvent::new(
                Some("replacement".to_owned()),
                None
            ))
            .unwrap(),
        NetworkEventDelivery::Baseline
    );

    second.fail_stop.store(true, Ordering::Release);
    assert_eq!(subscription.close().unwrap_err().code, ErrorCode::IoError);
    assert_eq!(
        plane
            .subscribe(fence.clone(), Arc::new(RecordingEventSource::default()),)
            .unwrap_err()
            .code,
        ErrorCode::AlreadyExists
    );
    second.fail_stop.store(false, Ordering::Release);
    plane.retry_cleanup().unwrap();

    let replacement = Arc::new(RecordingEventSource::default());
    let subscription = plane
        .subscribe(
            fence,
            Arc::clone(&replacement) as Arc<dyn NetworkDefaultEventSource>,
        )
        .unwrap();
    drop(subscription);
    assert_eq!(replacement.stops.lock().unwrap().len(), 1);
}

fn event_automation(name: &str, r#match: Option<BTreeMap<String, ScalarValue>>) -> Automation {
    Automation {
        automation_id: uuid(0x8200_0000, 1),
        name: "default-network-watch".to_owned(),
        enabled: true,
        trigger: AutomationTrigger::Event {
            name: name.to_owned(),
            r#match,
        },
        action: AutomationAction::Delay {
            duration_ms: 60_000,
        },
        state: BTreeMap::new(),
        revision: 1,
        created_at: TIMESTAMP.to_owned(),
        updated_at: TIMESTAMP.to_owned(),
    }
}
