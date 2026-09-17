//! I8-NET: the Magisk daemon's own Network primitive port.
//!
//! Every test drives `NativeNetworkPort` — the port the Magisk host installs — over scripted
//! provider bytes and a scripted capture device. That isolates the node's handoff claim from
//! the device and the kernel: one netlink parse, one `/proc/net` decode, one capture registry,
//! one PCAP writer over the Runtime's own format, one cleanup path and one bounded
//! default-network event.

use contract::{
    Availability, CapabilityState, DiagnosticOutcome, ErrorCode, FileTarget, FileTargetType,
    GrantFacts, NetworkDiagnoseInput, NetworkDiagnoseResult, NetworkScope, RouteEntry,
    RuntimeReadiness, UuidV4,
};
use daemon::network::{
    CaptureActivation, CaptureBackend, CaptureDevice, CaptureLimits, CaptureProgress,
    CaptureRegistry, CaptureStep, LinkFacts, NativeNetworkPort, NetworkHostSource, SOCKET_TABLES,
    SourceDump, complete_prefix, dns_servers_from_snapshot, interface_entries, link_facts,
    native_default_event_from_routes, parse_sockets, reply_is_complete, route_entries,
    route_prefix, socket_protocol_for_table,
};
use domain::{AdmissionFence, CapabilityContext, DomainError, ProviderGenerations, ResolverFacts};
use runtime::{
    AdmittedExecution, ArtifactMetadata, ArtifactPort, CapabilitySnapshot, ExecutionFailure,
    ExecutionPayload, LocalExecutionClaim, LocalExecutionClaims, NetworkFamily, NetworkFamilyPlan,
    NetworkFamilySource, NetworkPrimitiveOutcome, NetworkPrimitivePort, NetworkPrimitiveRequest,
    read_pcap,
};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

// ---------------------------------------------------------------------------------------
// Netlink reply builders. Every field is written host-endian, as the kernel writes it.
// ---------------------------------------------------------------------------------------

fn padded(mut message: Vec<u8>) -> Vec<u8> {
    while !message.len().is_multiple_of(4) {
        message.push(0);
    }
    message
}

fn netlink_message(kind: u16, body: &[u8]) -> Vec<u8> {
    let length = (16 + body.len()) as u32;
    let mut message = Vec::new();
    message.extend_from_slice(&length.to_ne_bytes());
    message.extend_from_slice(&kind.to_ne_bytes());
    message.extend_from_slice(&0u16.to_ne_bytes());
    message.extend_from_slice(&1u32.to_ne_bytes());
    message.extend_from_slice(&0u32.to_ne_bytes());
    message.extend_from_slice(body);
    padded(message)
}

fn netlink_done() -> Vec<u8> {
    netlink_message(3, &[])
}

fn attribute(kind: u16, value: &[u8]) -> Vec<u8> {
    let length = (4 + value.len()) as u16;
    let mut attribute = Vec::new();
    attribute.extend_from_slice(&length.to_ne_bytes());
    attribute.extend_from_slice(&kind.to_ne_bytes());
    attribute.extend_from_slice(value);
    padded(attribute)
}

fn name_attribute(name: &str) -> Vec<u8> {
    let mut value = name.as_bytes().to_vec();
    value.push(0);
    attribute(3, &value)
}

fn link_message(index: u32, flags: u32, name: &str, mtu: u32) -> Vec<u8> {
    let mut body = Vec::new();
    body.push(0);
    body.push(0);
    body.extend_from_slice(&1u16.to_ne_bytes());
    body.extend_from_slice(&index.to_ne_bytes());
    body.extend_from_slice(&flags.to_ne_bytes());
    body.extend_from_slice(&0u32.to_ne_bytes());
    body.extend_from_slice(&name_attribute(name));
    body.extend_from_slice(&attribute(4, &mtu.to_ne_bytes()));
    netlink_message(16, &body)
}

fn address_message(
    family: u8,
    prefix: u8,
    index: u32,
    local: &[u8],
    peer: Option<&[u8]>,
) -> Vec<u8> {
    let mut body = vec![family, prefix, 0, 0];
    body.extend_from_slice(&index.to_ne_bytes());
    body.extend_from_slice(&attribute(2, local));
    if let Some(peer) = peer {
        body.extend_from_slice(&attribute(1, peer));
    }
    netlink_message(20, &body)
}

fn route_message(family: u8, prefix: u8, flags: u32, attributes: &[Vec<u8>]) -> Vec<u8> {
    let mut body = vec![family, prefix, 0, 0, 254, 3, 0, 1];
    body.extend_from_slice(&flags.to_ne_bytes());
    for attribute in attributes {
        body.extend_from_slice(attribute);
    }
    netlink_message(24, &body)
}

const IFF_UP: u32 = 0x1;
const IFF_LOOPBACK: u32 = 0x8;
const AF_INET: u8 = 2;
const AF_INET6: u8 = 10;

/// The link and address dumps one ordinary Magisk host sees: a loopback device and an
/// addressed uplink.
fn link_dump() -> Vec<u8> {
    let mut dump = link_message(1, IFF_UP | IFF_LOOPBACK, "lo", 65_536);
    dump.extend_from_slice(&link_message(2, IFF_UP, "wlan0", 1_500));
    dump.extend_from_slice(&netlink_done());
    dump
}

fn address_dump() -> Vec<u8> {
    let mut dump = address_message(AF_INET, 8, 1, &[127, 0, 0, 1], None);
    dump.extend_from_slice(&address_message(AF_INET, 24, 2, &[192, 0, 2, 10], None));
    dump.extend_from_slice(&address_message(
        AF_INET6,
        64,
        2,
        &[0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
        None,
    ));
    dump.extend_from_slice(&netlink_done());
    dump
}

fn route_dump() -> Vec<u8> {
    let mut dump = route_message(
        AF_INET,
        0,
        0,
        &[
            attribute(5, &[192, 0, 2, 1]),
            attribute(4, &2u32.to_ne_bytes()),
            attribute(6, &100u32.to_ne_bytes()),
        ],
    );
    dump.extend_from_slice(&route_message(
        AF_INET,
        24,
        0,
        &[
            attribute(1, &[192, 0, 2, 0]),
            attribute(4, &2u32.to_ne_bytes()),
        ],
    ));
    dump.extend_from_slice(&route_message(
        AF_INET6,
        0,
        0,
        &[
            attribute(5, &[0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]),
            attribute(4, &2u32.to_ne_bytes()),
        ],
    ));
    dump.extend_from_slice(&netlink_done());
    dump
}

/// The row shapes `/proc/net/tcp` and its siblings report, in the kernel's own field order.
fn socket_table(rows: &[&str]) -> String {
    let mut text = String::from(
        "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n",
    );
    for row in rows {
        text.push_str(row);
        text.push('\n');
    }
    text
}

/// One `/proc/net/tcp` row the kernel would print for this socket: the address is the
/// host-endian hex of the four address bytes and the port is big-endian hex.
const LISTEN_ROW: &str = "   0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 1 1 0000000000000000 100 0 0 10 0";
const ESTABLISHED_ROW: &str = "   1: 0A0200C0:01BB 076433C6:15B3 01 00000000:00000000 00:00000000 00000000 10123        0 2 1 0000000000000000 20 4 30 10 -1";
const IPV6_LISTEN_ROW: &str = "   2: B80D0120000000000000000001000000:0050 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 3 1 0000000000000000 100 0 0 10 0";
/// An address field that is neither the 8-character IPv4 width nor the 32-character IPv6
/// width, so the row is refused rather than guessed.
const MALFORMED_ROW: &str = "   3: 0100007F1:0050 0A0200C0:01BB 01 00000000:00000000 00:00000000 00000000  1000 0 4 1 0000000000000000 0 0 0 10 0";
/// A row too short to carry the fields this table reports.
const SHORT_ROW: &str = "   4: 0100007F 00000000:0000 0A";

// ---------------------------------------------------------------------------------------
// The scripted observation source and capture device.
// ---------------------------------------------------------------------------------------

#[derive(Default)]
struct Facts {
    link: Option<SourceDump>,
    address: Option<SourceDump>,
    route: Option<SourceDump>,
    tables: BTreeMap<String, Option<String>>,
    dns: Option<Vec<String>>,
    /// The execution id the DNS query was asked under, so a test can prove this host asks the
    /// companion under the admitted execution instead of minting one of its own.
    dns_execution: Option<String>,
}

/// The daemon's observation seam over scripted bytes. An absent fact is one this host could
/// not establish, which is what the `unknown` family is reported from.
#[derive(Clone, Default)]
struct ScriptedSource {
    facts: Arc<Mutex<Facts>>,
}

impl ScriptedSource {
    fn new() -> Self {
        Self::default()
    }

    fn update(self, edit: impl FnOnce(&mut Facts)) -> Self {
        edit(&mut self.facts.lock().expect("facts lock"));
        self
    }

    fn links(self) -> Self {
        self.update(|facts| {
            facts.link = Some(SourceDump {
                bytes: link_dump(),
                truncated: false,
            });
            facts.address = Some(SourceDump {
                bytes: address_dump(),
                truncated: false,
            });
        })
    }

    fn routes(self) -> Self {
        self.update(|facts| {
            facts.route = Some(SourceDump {
                bytes: route_dump(),
                truncated: false,
            });
        })
    }
}

impl NetworkHostSource for ScriptedSource {
    fn link_dump(&self) -> Result<SourceDump, DomainError> {
        self.facts
            .lock()
            .expect("facts lock")
            .link
            .clone()
            .ok_or_else(|| DomainError::new(ErrorCode::InternalError, "no link dump"))
    }

    fn address_dump(&self) -> Result<SourceDump, DomainError> {
        self.facts
            .lock()
            .expect("facts lock")
            .address
            .clone()
            .ok_or_else(|| DomainError::new(ErrorCode::InternalError, "no address dump"))
    }

    fn route_dump(&self) -> Result<SourceDump, DomainError> {
        self.facts
            .lock()
            .expect("facts lock")
            .route
            .clone()
            .ok_or_else(|| DomainError::new(ErrorCode::InternalError, "no route dump"))
    }

    fn socket_text(&self, table: &str) -> Result<Option<String>, DomainError> {
        Ok(self
            .facts
            .lock()
            .expect("facts lock")
            .tables
            .get(table)
            .cloned()
            .unwrap_or(None))
    }

    fn dns_servers(
        &self,
        execution: &AdmittedExecution,
    ) -> Result<Option<Vec<String>>, DomainError> {
        let mut facts = self.facts.lock().expect("facts lock");
        facts.dns_execution = Some(execution.execution_id.to_string());
        Ok(facts.dns.clone())
    }
}

/// Every publication a test's artifact store recorded, as the kind it was published under and
/// the exact bytes.
type Publications = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

/// The capture artifact store, which records every publication so a test can prove exactly
/// what this daemon produced.
#[derive(Clone, Default)]
struct RecordingArtifacts {
    published: Publications,
}

impl RecordingArtifacts {
    fn published(&self) -> Vec<(String, Vec<u8>)> {
        self.published.lock().expect("artifact lock").clone()
    }

    fn count(&self) -> usize {
        self.published.lock().expect("artifact lock").len()
    }
}

impl ArtifactPort for RecordingArtifacts {
    fn publish(&self, bytes: &[u8]) -> Result<ArtifactMetadata, DomainError> {
        self.publish_as("data", bytes)
    }

    fn publish_as(&self, kind: &str, bytes: &[u8]) -> Result<ArtifactMetadata, DomainError> {
        let mut published = self.published.lock().expect("artifact lock");
        published.push((kind.to_owned(), bytes.to_vec()));
        Ok(ArtifactMetadata {
            artifact_ref: format!("dbref:{kind}:{:08x}", published.len()),
            byte_count: bytes.len() as u64,
            sha256: String::new(),
            mime: None,
        })
    }

    fn publish_for_execution(
        &self,
        _execution_id: &UuidV4,
        kind: &str,
        bytes: &[u8],
    ) -> Result<ArtifactMetadata, DomainError> {
        self.publish_as(kind, bytes)
    }

    fn publish_image_for_execution(
        &self,
        _execution_id: &UuidV4,
        mime: &str,
        bytes: &[u8],
    ) -> Result<ArtifactMetadata, DomainError> {
        let mut metadata = self.publish_as("image", bytes)?;
        metadata.mime = Some(mime.to_owned());
        Ok(metadata)
    }

    fn open(&self, artifact_ref: &str) -> Result<Vec<u8>, DomainError> {
        artifact_ref
            .rsplit(':')
            .next()
            .and_then(|index| usize::from_str_radix(index, 16).ok())
            .and_then(|index| index.checked_sub(1))
            .and_then(|index| {
                self.published
                    .lock()
                    .expect("artifact lock")
                    .get(index)
                    .map(|(_, bytes)| bytes.clone())
            })
            .ok_or_else(|| DomainError::new(ErrorCode::NotFound, "artifact not found"))
    }

    fn metadata(&self, artifact_ref: &str) -> Result<ArtifactMetadata, DomainError> {
        let bytes = self.open(artifact_ref)?;
        Ok(ArtifactMetadata {
            artifact_ref: artifact_ref.to_owned(),
            byte_count: bytes.len() as u64,
            sha256: String::new(),
            mime: None,
        })
    }

    fn delete(&self, _artifact_ref: &str) -> Result<(), DomainError> {
        Ok(())
    }
}

/// One scripted capture step. `Wait` holds the capture inside its own read window until the
/// test releases the gate, which is how a test observes a capture that is still running.
enum Step {
    Packet {
        seconds: u32,
        microseconds: u32,
        original_len: u32,
        bytes: Vec<u8>,
    },
    Timeout,
    End,
    Failed,
    Wait,
}

fn packet(seconds: u32, microseconds: u32, len: usize, tag: u8) -> Step {
    Step::Packet {
        seconds,
        microseconds,
        original_len: len as u32,
        bytes: vec![tag; len],
    }
}

#[derive(Default)]
struct Gate {
    open: Mutex<bool>,
    signal: Condvar,
}

impl Gate {
    fn wait(&self) {
        let mut open = self.open.lock().expect("gate lock");
        while !*open {
            open = self.signal.wait(open).expect("gate wait");
        }
    }

    fn release(&self) {
        *self.open.lock().expect("gate lock") = true;
        self.signal.notify_all();
    }
}

#[derive(Default)]
struct Ledger {
    opens: AtomicUsize,
    closed: AtomicUsize,
    activated: AtomicUsize,
    broken: AtomicUsize,
    delivered: AtomicUsize,
    interfaces: Mutex<Vec<String>>,
    filters: Mutex<Vec<Option<String>>>,
    injected: Mutex<Vec<Vec<u8>>>,
}

impl Ledger {
    fn count(counter: &AtomicUsize) -> usize {
        counter.load(Ordering::SeqCst)
    }

    fn injected(&self) -> Vec<Vec<u8>> {
        self.injected.lock().expect("ledger lock").clone()
    }

    fn filter(&self) -> Option<String> {
        self.filters.lock().expect("ledger lock")[0].clone()
    }
}

struct Script {
    steps: Mutex<VecDeque<Step>>,
    gate: Gate,
    inject_error: bool,
    /// What this device answers at activation, which is the device's own link fact.
    activation: Result<CaptureActivation, String>,
}

impl Script {
    fn new(steps: Vec<Step>) -> Arc<Self> {
        Self::with_activation(
            steps,
            Ok(CaptureActivation::Active(runtime::PCAP_LINKTYPE_ETHERNET)),
        )
    }

    /// A TUN interface: one bare IPv4 or IPv6 packet per record.
    fn raw_ip(steps: Vec<Step>) -> Arc<Self> {
        Self::with_activation(
            steps,
            Ok(CaptureActivation::Active(runtime::PCAP_LINKTYPE_RAW)),
        )
    }

    /// A device whose link type the classic-PCAP format cannot express.
    fn unrepresentable(steps: Vec<Step>) -> Arc<Self> {
        Self::with_activation(steps, Ok(CaptureActivation::NoPcapLinkType))
    }

    /// A device that cannot be activated at all, which is a device fault.
    fn failing_activation(steps: Vec<Step>) -> Arc<Self> {
        Self::with_activation(
            steps,
            Err("pcap_activate reports the interface is not up".to_owned()),
        )
    }

    fn with_activation(
        steps: Vec<Step>,
        activation: Result<CaptureActivation, String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            steps: Mutex::new(steps.into()),
            gate: Gate::default(),
            inject_error: false,
            activation,
        })
    }

    fn refusing_injection() -> Arc<Self> {
        Arc::new(Self {
            steps: Mutex::new(VecDeque::new()),
            gate: Gate::default(),
            inject_error: true,
            activation: Ok(CaptureActivation::Active(runtime::PCAP_LINKTYPE_ETHERNET)),
        })
    }
}

#[derive(Clone)]
struct ScriptBackend {
    script: Arc<Script>,
    ledger: Arc<Ledger>,
}

impl ScriptBackend {
    fn new(script: Arc<Script>, ledger: Arc<Ledger>) -> Self {
        Self { script, ledger }
    }
}

impl CaptureBackend for ScriptBackend {
    fn open(
        &self,
        interface: &str,
        filter: Option<&str>,
    ) -> Result<Box<dyn CaptureDevice>, String> {
        self.ledger.opens.fetch_add(1, Ordering::SeqCst);
        self.ledger
            .interfaces
            .lock()
            .expect("ledger lock")
            .push(interface.to_owned());
        self.ledger
            .filters
            .lock()
            .expect("ledger lock")
            .push(filter.map(str::to_owned));
        Ok(Box::new(ScriptDevice {
            script: Arc::clone(&self.script),
            ledger: Arc::clone(&self.ledger),
            closed: false,
        }))
    }
}

struct ScriptDevice {
    script: Arc<Script>,
    ledger: Arc<Ledger>,
    closed: bool,
}

impl CaptureDevice for ScriptDevice {
    fn activate(&mut self) -> Result<CaptureActivation, String> {
        self.ledger.activated.fetch_add(1, Ordering::SeqCst);
        self.script.activation.clone()
    }

    fn next_packet(&mut self) -> CaptureStep {
        let step = self
            .script
            .steps
            .lock()
            .expect("script lock")
            .pop_front()
            .unwrap_or(Step::Timeout);
        match step {
            Step::Packet {
                seconds,
                microseconds,
                original_len,
                bytes,
            } => {
                self.ledger.delivered.fetch_add(1, Ordering::SeqCst);
                CaptureStep::Packet {
                    seconds,
                    microseconds,
                    original_len,
                    bytes,
                }
            }
            Step::Timeout => CaptureStep::Timeout,
            Step::End => CaptureStep::End,
            Step::Failed => CaptureStep::Failed,
            Step::Wait => {
                self.script.gate.wait();
                CaptureStep::Timeout
            }
        }
    }

    fn break_loop(&mut self) {
        self.ledger.broken.fetch_add(1, Ordering::SeqCst);
    }

    fn inject(&mut self, packet: &[u8]) -> Result<usize, String> {
        self.ledger
            .injected
            .lock()
            .expect("ledger lock")
            .push(packet.to_vec());
        if self.script.inject_error {
            return Err("scripted injection failure".to_owned());
        }
        Ok(packet.len())
    }

    /// The handle may only be closed once, so a second close is reported rather than counted.
    fn close(&mut self) -> Result<(), String> {
        if self.closed {
            return Err("scripted device was closed twice".to_owned());
        }
        self.closed = true;
        self.ledger.closed.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn error(&self) -> Option<String> {
        Some("scripted device error".to_owned())
    }
}

// ---------------------------------------------------------------------------------------
// The host capability fixture the S-NET-001 plan is projected from.
// ---------------------------------------------------------------------------------------

const fn available() -> Availability {
    Availability {
        state: CapabilityState::Available,
        reason: None,
    }
}

fn uuid(value: u64) -> UuidV4 {
    UuidV4::parse(format!("00000000-0000-4000-8000-{value:012x}")).unwrap()
}

fn magisk_capability() -> CapabilitySnapshot {
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
            host: contract::RuntimeHost::MagiskBackend,
            readiness: RuntimeReadiness::Ready,
            app_execution_surface: CapabilityState::Available,
        },
        resolver_facts: ResolverFacts {
            app_native: CapabilityState::Available,
            app_framework: CapabilityState::Available,
            shizuku: CapabilityState::Available,
            magisk_native: CapabilityState::Available,
            magisk_framework: CapabilityState::Available,
            magisk_launch: CapabilityState::Available,
            magisk_clipboard: CapabilityState::Available,
            magisk_notifications: CapabilityState::Available,
            accessibility: CapabilityState::Available,
            media_projection: CapabilityState::Available,
            notification_listener: CapabilityState::Available,
            generations: ProviderGenerations {
                app_native: 4,
                app_framework: 4,
                shizuku: 9,
                magisk_native: 9,
                magisk_framework: 4,
                accessibility: 4,
                media_projection: 4,
                notification_listener: 4,
            },
        },
        fence: AdmissionFence {
            runtime_epoch: uuid(1),
            host_generation: 7,
            runtime_instance_id: uuid(2),
        },
    }
}

/// The plan the Runtime hands this host on the Magisk path: S-NET-001 assigns this daemon every
/// family.
fn magisk_plan() -> runtime::NetworkSourcePlan {
    runtime::network_source_plan(&magisk_capability(), runtime::ProviderToken::MagiskNative)
}

fn executed(execution: u64) -> AdmittedExecution {
    AdmittedExecution {
        execution_id: uuid(0x9000 + execution),
        task_id: None,
        executor: runtime::ExecutorRecord {
            host: contract::RuntimeHost::MagiskBackend,
            provider: runtime::ProviderToken::MagiskNative,
            execution_class: contract::ExecutionClass::Magisk,
            capability_generation: 9,
            fence: contract::Fence {
                runtime_epoch: uuid(1),
                host_generation: 7,
                runtime_instance_id: uuid(2),
            },
        },
        payload: ExecutionPayload::NetworkCall(contract::NetworkCall::Inspect(
            contract::NetworkInspectInput {
                scope: NetworkScope::All,
                max_entries: 200,
            },
        )),
    }
}

type Port = NativeNetworkPort<ScriptedSource, ScriptBackend, RecordingArtifacts>;

fn port(source: ScriptedSource, backend: ScriptBackend, artifacts: RecordingArtifacts) -> Port {
    NativeNetworkPort::new(source, backend, artifacts)
}

fn backend(script: Arc<Script>, ledger: &Arc<Ledger>) -> ScriptBackend {
    ScriptBackend::new(script, Arc::clone(ledger))
}

fn inspect_scope(scope: NetworkScope) -> NetworkPrimitiveRequest {
    NetworkPrimitiveRequest::Inspect {
        plan: magisk_plan(),
        scope,
        max_entries: 200,
    }
}

fn run(
    port: &Port,
    request: NetworkPrimitiveRequest,
    claim: &LocalExecutionClaim,
) -> Result<NetworkPrimitiveOutcome, ExecutionFailure> {
    port.run(&executed(1), request, claim)
        .map(|settlement| settlement.outcome)
}

fn claim_for(claims: &LocalExecutionClaims, execution: u64) -> LocalExecutionClaim {
    claims.claim(uuid(0x9000 + execution)).expect("claim")
}

fn settled(outcome: NetworkPrimitiveOutcome) -> runtime::CaptureSettlement {
    match outcome {
        NetworkPrimitiveOutcome::CaptureSettled(settlement) => settlement,
        other => panic!("capture settled with another outcome: {other:?}"),
    }
}

fn inspected(outcome: NetworkPrimitiveOutcome) -> runtime::NetworkInspectSettlement {
    match outcome {
        NetworkPrimitiveOutcome::Inspect(settlement) => settlement,
        other => panic!("inspect settled with another outcome: {other:?}"),
    }
}

fn capture_request(
    capture_id: &contract::CaptureId,
    limits: CaptureLimits,
    persist_to: Option<FileTarget>,
) -> NetworkPrimitiveRequest {
    NetworkPrimitiveRequest::CaptureStart {
        capture_id: capture_id.clone(),
        interface: "wlan0".to_owned(),
        filter: None,
        max_packets: limits.max_packets,
        max_bytes: limits.max_bytes,
        max_duration_ms: limits.max_duration_ms,
        persist_to,
    }
}

fn stop_request(capture_id: &contract::CaptureId) -> NetworkPrimitiveRequest {
    NetworkPrimitiveRequest::CaptureStop {
        capture_id: capture_id.clone(),
    }
}

fn generous() -> CaptureLimits {
    CaptureLimits {
        max_packets: 1_000,
        max_bytes: 1_000_000,
        max_duration_ms: 60_000,
    }
}

/// Whether one identity no longer names a running capture on this port.
fn retired(port: &Port, claim: &LocalExecutionClaim, capture_id: &contract::CaptureId) -> bool {
    matches!(
        run(port, stop_request(capture_id), claim),
        Err(failure) if failure.error.code == ErrorCode::NotFound
    )
}

/// The complete classic-PCAP stream of `packets`, built from the Runtime's own headers.
fn expected_stream(packets: &[(&[u8], u32, u32, u32)]) -> Vec<u8> {
    expected_stream_of(runtime::PCAP_LINKTYPE_ETHERNET, packets)
}

/// The same stream for a device that reports another link type.
fn expected_stream_of(link_type: u32, packets: &[(&[u8], u32, u32, u32)]) -> Vec<u8> {
    let mut bytes = runtime::pcap_file_header(link_type).to_vec();
    for (packet, seconds, microseconds, original_len) in packets {
        bytes.extend_from_slice(&runtime::pcap_record_header(
            *seconds,
            *microseconds,
            packet.len() as u32,
            *original_len,
        ));
        bytes.extend_from_slice(packet);
    }
    bytes
}

fn wait_for(mut ready: impl FnMut() -> bool, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < deadline, "the capture never {what}");
        thread::sleep(Duration::from_millis(5));
    }
}

// ---------------------------------------------------------------------------------------
// S-NET-001: the daemon's own netlink and procfs providers.
// ---------------------------------------------------------------------------------------

#[test]
fn i8_net_g01_netlink_link_and_address_replies_become_exact_interface_facts() {
    let links = link_facts(&link_dump(), &address_dump()).expect("link facts");
    assert_eq!(links.len(), 2);
    assert_eq!(
        links[0],
        LinkFacts {
            index: 1,
            name: "lo".to_owned(),
            up: true,
            loopback: true,
            mtu: Some(65_536),
            addresses: vec![contract::InterfaceAddress {
                address: "127.0.0.1".to_owned(),
                prefix_length: Some(8),
            }],
        }
    );
    assert_eq!(
        links[1],
        LinkFacts {
            index: 2,
            name: "wlan0".to_owned(),
            up: true,
            loopback: false,
            mtu: Some(1_500),
            addresses: vec![
                contract::InterfaceAddress {
                    address: "192.0.2.10".to_owned(),
                    prefix_length: Some(24),
                },
                contract::InterfaceAddress {
                    address: "2001:db8::1".to_owned(),
                    prefix_length: Some(64),
                },
            ],
        }
    );

    let entries = interface_entries(&links);
    assert_eq!(entries[1].name, "wlan0");
    assert_eq!(entries[1].index, Some(2));
    assert_eq!(entries[1].up, Some(true));
    assert_eq!(entries[1].mtu, Some(1_500));
    assert_eq!(entries[0].addresses.len(), 1);

    // A link the kernel reports without a usable name is not reported at all.
    let mut unnamed = netlink_message(16, &[0u8; 16]);
    unnamed.extend_from_slice(&netlink_done());
    assert!(link_facts(&unnamed, &[]).expect("facts").is_empty());

    // An address for a link this host did not enumerate is not attached to a guessed link.
    let mut detached = address_message(AF_INET, 24, 99, &[192, 0, 2, 10], None);
    detached.extend_from_slice(&netlink_done());
    let links = link_facts(&link_dump(), &detached).expect("facts");
    assert_eq!(links[1].addresses.len(), 0);
}

#[test]
fn i8_net_g02_truncated_or_short_netlink_messages_are_rejected_not_guessed() {
    // A message whose declared length runs past the buffer is a short reply.
    let mut short = link_message(2, IFF_UP, "wlan0", 1_500);
    short.truncate(short.len() - 8);
    assert!(link_facts(&short, &[]).is_err());

    // A tail shorter than one message header is an incomplete reply.
    let mut partial = link_message(2, IFF_UP, "wlan0", 1_500);
    partial.extend_from_slice(&[0u8; 6]);
    assert!(link_facts(&partial, &[]).is_err());

    // A link message shorter than `ifinfomsg` cannot be read at all.
    assert!(link_facts(&netlink_message(16, &[0u8; 12]), &[]).is_err());

    // An attribute whose own length runs past its message is truncated.
    let mut body = vec![0u8; 16];
    body.extend_from_slice(&100u16.to_ne_bytes());
    body.extend_from_slice(&3u16.to_ne_bytes());
    assert!(link_facts(&netlink_message(16, &body), &[]).is_err());

    // A kernel refusal is an error, never an empty reply.
    assert!(link_facts(&netlink_message(2, &(-1i32).to_ne_bytes()), &[]).is_err());
    assert!(link_facts(&netlink_message(2, &(-2i32).to_ne_bytes()), &[]).is_err());

    // A dump without its own end-of-dump message is truncated, and the complete messages
    // before the cut are kept while the partial tail is dropped.
    let complete = link_message(2, IFF_UP, "wlan0", 1_500);
    let mut truncated = complete.clone();
    truncated.extend_from_slice(&complete[..10]);
    assert!(!reply_is_complete(&truncated));
    assert_eq!(complete_prefix(&truncated), complete.as_slice());
    assert!(reply_is_complete(&link_dump()));
}

#[test]
fn i8_net_g03_route_replies_become_exact_route_entries() {
    let links = link_facts(&link_dump(), &[]).expect("link facts");
    let routes = route_entries(&route_dump(), &links).expect("route entries");
    assert_eq!(routes.len(), 3);
    assert_eq!(routes[0].destination, "0.0.0.0/0");
    assert_eq!(routes[0].gateway.as_deref(), Some("192.0.2.1"));
    assert_eq!(routes[0].interface.as_deref(), Some("wlan0"));
    assert_eq!(routes[0].metric, Some(100));
    assert_eq!(route_prefix(&routes[0].destination), Some(0));
    assert_eq!(routes[1].destination, "192.0.2.0/24");
    assert_eq!(routes[1].gateway, None);
    assert_eq!(routes[1].metric, None);
    assert_eq!(routes[2].destination, "::/0");
    assert_eq!(routes[2].gateway.as_deref(), Some("fe80::1"));
    assert_eq!(route_prefix(&routes[2].destination), Some(0));

    // A routing-cache entry is not a routing-table entry.
    let mut cloned = route_message(AF_INET, 24, 0x200, &[attribute(4, &2u32.to_ne_bytes())]);
    cloned.extend_from_slice(&netlink_done());
    assert!(route_entries(&cloned, &links).expect("routes").is_empty());

    // A route whose egress interface this host did not enumerate omits the field instead of
    // naming a guessed one.
    let mut unknown = route_message(AF_INET, 24, 0, &[attribute(4, &99u32.to_ne_bytes())]);
    unknown.extend_from_slice(&netlink_done());
    let entries = route_entries(&unknown, &links).expect("routes");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].destination, "0.0.0.0/24");
    assert_eq!(entries[0].interface, None);

    // A route message short of `rtmsg` is refused rather than read past its end.
    assert!(route_entries(&netlink_message(24, &[0u8; 8]), &links).is_err());
}

#[test]
fn i8_net_g04_proc_net_tables_decode_exact_socket_facts() {
    assert_eq!(
        socket_protocol_for_table("tcp"),
        contract::SocketProtocol::Tcp
    );
    assert_eq!(
        socket_protocol_for_table("tcp6"),
        contract::SocketProtocol::Tcp
    );
    assert_eq!(
        socket_protocol_for_table("udp"),
        contract::SocketProtocol::Udp
    );
    assert_eq!(
        socket_protocol_for_table("udp6"),
        contract::SocketProtocol::Udp
    );
    assert_eq!(
        socket_protocol_for_table("packet"),
        contract::SocketProtocol::Other
    );
    assert_eq!(SOCKET_TABLES, ["tcp", "tcp6", "udp", "udp6"]);

    let text = socket_table(&[
        LISTEN_ROW,
        ESTABLISHED_ROW,
        IPV6_LISTEN_ROW,
        MALFORMED_ROW,
        SHORT_ROW,
    ]);
    let sockets = parse_sockets("tcp", &text);
    assert_eq!(sockets.len(), 3);

    assert_eq!(sockets[0].protocol, contract::SocketProtocol::Tcp);
    assert_eq!(sockets[0].local_address, "127.0.0.1");
    assert_eq!(sockets[0].local_port, Some(8080));
    assert_eq!(sockets[0].remote_address, None);
    assert_eq!(sockets[0].remote_port, None);
    assert_eq!(sockets[0].state.as_deref(), Some("LISTEN"));
    assert_eq!(sockets[0].uid, Some(1000));

    assert_eq!(sockets[1].local_address, "192.0.2.10");
    assert_eq!(sockets[1].local_port, Some(443));
    assert_eq!(sockets[1].remote_address.as_deref(), Some("198.51.100.7"));
    assert_eq!(sockets[1].remote_port, Some(5555));
    assert_eq!(sockets[1].state.as_deref(), Some("ESTABLISHED"));
    assert_eq!(sockets[1].uid, Some(10123));

    assert_eq!(sockets[2].local_address, "2001:db8::1");
    assert_eq!(sockets[2].local_port, Some(80));
    assert_eq!(sockets[2].remote_address, None);
    assert_eq!(sockets[2].state.as_deref(), Some("LISTEN"));
    assert_eq!(sockets[2].uid, Some(0));

    // The same table read as UDP keeps its rows but reports the other protocol.
    let udp = parse_sockets("udp", &text);
    assert_eq!(udp.len(), 3);
    assert_eq!(udp[1].protocol, contract::SocketProtocol::Udp);

    // A state the kernel's own table does not name is omitted rather than invented.
    let unnamed = socket_table(&[
        "   0: 0100007F:0050 00000000:0000 FF 00000000:00000000 00:00000000 00000000  1000 0 1 1 0000000000000000 100 0 0 10 0",
    ]);
    let sockets = parse_sockets("udp", &unnamed);
    assert_eq!(sockets.len(), 1);
    assert_eq!(sockets[0].state, None);
}

#[test]
fn i8_net_g05_inspect_drives_every_family_from_the_daemon_sources_and_omits_unknown_ones() {
    let source = ScriptedSource::new().links().routes().update(|facts| {
        facts
            .tables
            .insert("tcp".to_owned(), Some(socket_table(&[LISTEN_ROW])));
        for table in ["tcp6", "udp", "udp6"] {
            facts
                .tables
                .insert(table.to_owned(), Some(socket_table(&[])));
        }
        facts.dns = Some(vec!["192.0.2.53".to_owned()]);
    });
    let ledger = Arc::new(Ledger::default());
    let artifacts = RecordingArtifacts::default();
    let source = source.clone();
    let observed = port(
        source.clone(),
        backend(Script::new(Vec::new()), &ledger),
        artifacts.clone(),
    );
    let claims = LocalExecutionClaims::default();
    let claim = claim_for(&claims, 1);

    let settlement =
        inspected(run(&observed, inspect_scope(NetworkScope::All), &claim).expect("inspect"));
    let interfaces = settlement.interfaces.expect("interfaces established");
    assert_eq!(interfaces.entries.len(), 2);
    assert_eq!(interfaces.entries[1].name, "wlan0");
    assert!(!interfaces.truncated);
    let routes = settlement.routes.expect("routes established");
    assert_eq!(routes.entries.len(), 3);
    let dns = settlement.dns.expect("dns established");
    assert_eq!(dns.entries.len(), 1);
    assert_eq!(dns.entries[0].server, "192.0.2.53");
    // The companion query carries the admitted execution rather than an identity this host
    // mints for itself.
    assert_eq!(
        source
            .facts
            .lock()
            .expect("facts lock")
            .dns_execution
            .as_deref(),
        Some(executed(1).execution_id.to_string().as_str())
    );
    let sockets = settlement.sockets.expect("sockets established");
    assert_eq!(sockets.entries.len(), 1);
    assert_eq!(sockets.entries[0].local_address, "127.0.0.1");

    // A socket table this host cannot read leaves the union incomplete rather than silently
    // shorter, and a host that reads no table at all establishes no socket fact.
    let partial = ScriptedSource::new().update(|facts| {
        facts
            .tables
            .insert("tcp".to_owned(), Some(socket_table(&[LISTEN_ROW])));
        facts.tables.insert("tcp6".to_owned(), None);
    });
    let observed = port(
        partial,
        backend(Script::new(Vec::new()), &ledger),
        artifacts.clone(),
    );
    let settlement =
        inspected(run(&observed, inspect_scope(NetworkScope::Sockets), &claim).expect("inspect"));
    let sockets = settlement.sockets.expect("sockets established");
    assert_eq!(sockets.entries.len(), 1);
    assert!(sockets.truncated);

    let blind = port(
        ScriptedSource::new(),
        backend(Script::new(Vec::new()), &ledger),
        artifacts.clone(),
    );
    let settlement =
        inspected(run(&blind, inspect_scope(NetworkScope::Sockets), &claim).expect("inspect"));
    assert!(settlement.sockets.is_none());

    // A family this host cannot observe is omitted, which the Runtime reports as `unknown`.
    let settlement =
        inspected(run(&blind, inspect_scope(NetworkScope::All), &claim).expect("inspect"));
    assert!(settlement.interfaces.is_none());
    assert!(settlement.routes.is_none());
    assert!(settlement.dns.is_none());
    assert!(settlement.sockets.is_none());

    // A scope that requests one family observes only that family.
    let settlement =
        inspected(run(&observed, inspect_scope(NetworkScope::Dns), &claim).expect("inspect"));
    assert!(settlement.interfaces.is_none());
    assert!(settlement.routes.is_none());
    assert!(settlement.sockets.is_none());
    assert!(settlement.dns.is_none());

    // The S-NET-001 plan itself hands this host every family on the Magisk path.
    let plan = magisk_plan();
    for family in [
        NetworkFamily::Interfaces,
        NetworkFamily::Routes,
        NetworkFamily::Dns,
        NetworkFamily::Sockets,
    ] {
        assert_eq!(
            plan.family(family),
            NetworkFamilyPlan::Source(NetworkFamilySource::Daemon)
        );
    }

    // The companion's `LinkProperties` answer is the one DNS observation this host has, in the
    // reply shape the APK surface encodes.
    assert_eq!(
        dns_servers_from_snapshot(&serde_json::json!({"dns": [{"server": "192.0.2.53"}]})),
        Some(vec!["192.0.2.53".to_owned()])
    );
    assert_eq!(
        dns_servers_from_snapshot(&serde_json::json!({"dns": []})),
        Some(Vec::new())
    );
    assert_eq!(dns_servers_from_snapshot(&serde_json::json!({})), None);
    // An entry that carries no server is not a fact, so the family stays unestablished instead
    // of being reported with a hole in it.
    assert_eq!(
        dns_servers_from_snapshot(&serde_json::json!({"dns": [{"server": "192.0.2.53"}, {}]})),
        None
    );
}

// ---------------------------------------------------------------------------------------
// S-NET-003: capture identity, bounds and settlement.
// ---------------------------------------------------------------------------------------

#[test]
fn i8_net_g06_capture_registry_binds_one_identity_and_refuses_unknown_or_duplicate() {
    let registry = CaptureRegistry::default();
    let limits = CaptureLimits {
        max_packets: 5,
        max_bytes: 1_000,
        max_duration_ms: 1_000,
    };
    let slot = registry.register(&uuid(7), limits).expect("register");
    assert_eq!(slot.limits(), limits);
    assert_eq!(slot.progress(), CaptureProgress::default());
    assert!(!slot.stop_requested());
    slot.request_stop();
    assert!(slot.stop_requested());

    // One identity owns one capture, and an identity this daemon does not hold is not a target.
    let Err(duplicate) = registry.register(&uuid(7), limits) else {
        panic!("a second capture took a running identity");
    };
    assert_eq!(duplicate.code, ErrorCode::CaptureFailed);
    let Err(unknown) = registry.running(&uuid(8)) else {
        panic!("an unknown identity named a running capture");
    };
    assert_eq!(unknown.code, ErrorCode::NotFound);

    // A stop request for an identity the port does not hold is `NOT_FOUND`.
    let ledger = Arc::new(Ledger::default());
    let script = Script::new(vec![Step::Wait, Step::End]);
    let artifacts = RecordingArtifacts::default();
    let stopped = port(
        ScriptedSource::new(),
        backend(Arc::clone(&script), &ledger),
        artifacts.clone(),
    );
    let claims = LocalExecutionClaims::default();
    let claim = claim_for(&claims, 1);
    let failure = run(&stopped, stop_request(&uuid(8)), &claim)
        .expect_err("an unknown capture identity is not a stop target");
    assert_eq!(failure.error.code, ErrorCode::NotFound);
    assert!(failure.cleanup_verified);

    // While one capture holds an identity, a second capture on the same identity is refused
    // rather than aliasing two captures onto it.
    let capture_id = uuid(9);
    let running = stopped.clone();
    let held = Arc::clone(&script);
    let handle = thread::spawn({
        let capture_id = capture_id.clone();
        move || {
            let claims = LocalExecutionClaims::default();
            let claim = claim_for(&claims, 2);
            running
                .run(
                    &executed(2),
                    capture_request(&capture_id, generous(), None),
                    &claim,
                )
                .map(|settlement| settlement.outcome)
        }
    });
    wait_for(|| Ledger::count(&ledger.opens) == 1, "opened its device");
    let failure = run(
        &stopped,
        capture_request(&capture_id, generous(), None),
        &claim,
    )
    .expect_err("a second capture cannot take a running identity");
    assert_eq!(failure.error.code, ErrorCode::CaptureFailed);
    assert!(failure.cleanup_verified);
    assert_eq!(Ledger::count(&ledger.opens), 1);

    // The running capture still owns the identity and still takes its stop request.
    assert_eq!(
        run(&stopped, stop_request(&capture_id), &claim).expect("stop"),
        NetworkPrimitiveOutcome::CaptureStopRequested
    );
    held.gate.release();
    let settlement = settled(handle.join().expect("capture thread").expect("capture"));
    assert!(!settlement.cancelled);
    assert_eq!(settlement.packets_captured, 0);
    // The identity is released by the settle that owned it, so it names no running capture.
    assert!(retired(&stopped, &claim, &capture_id));
    assert_eq!(Ledger::count(&ledger.closed), 1);
    // The settle that released the identity published the stream it settled with, which for a
    // device that reported no packet is the file header alone.
    let [(kind, bytes)] = artifacts.published().try_into().expect("one publication");
    assert_eq!(kind, "capture");
    assert_eq!(bytes, expected_stream(&[]));
}

#[test]
fn i8_net_g07_capture_byte_packet_and_duration_bounds_stop_at_the_first_exceeded_bound() {
    let claims = LocalExecutionClaims::default();
    let claim = claim_for(&claims, 1);

    // The packet bound stops the capture at the first packet it cannot admit.
    let ledger = Arc::new(Ledger::default());
    let script = Script::new(vec![
        packet(1, 0, 60, 0x11),
        packet(2, 0, 60, 0x22),
        packet(3, 0, 60, 0x33),
    ]);
    let artifacts = RecordingArtifacts::default();
    let observed = port(
        ScriptedSource::new(),
        backend(Arc::clone(&script), &ledger),
        artifacts.clone(),
    );
    let limits = CaptureLimits {
        max_packets: 2,
        max_bytes: 1_000_000,
        max_duration_ms: 60_000,
    };
    let settlement =
        settled(run(&observed, capture_request(&uuid(10), limits, None), &claim).expect("capture"));
    assert!(!settlement.cancelled);
    assert_eq!(settlement.packets_captured, 2);
    assert_eq!(settlement.bytes_captured, 120);
    assert!(settlement.cleanup_verified);
    assert_eq!(Ledger::count(&ledger.closed), 1);
    assert_eq!(artifacts.count(), 1);
    let records = read_pcap(&artifacts.published()[0].1).expect("pcap");
    assert_eq!(records.len(), 2);
    assert_eq!(records[1].bytes, vec![0x22u8; 60]);

    // The byte bound is the whole artifact, so a capture sitting exactly on it emits exactly
    // that many bytes: 24 + 16 + 40 fits, and the next record would not.
    let byte_bound =
        (runtime::PCAP_FILE_HEADER_BYTES + runtime::PCAP_RECORD_HEADER_BYTES + 40) as u64;
    let ledger = Arc::new(Ledger::default());
    let script = Script::new(vec![
        packet(1, 0, 40, 0x31),
        packet(2, 0, 40, 0x32),
        packet(3, 0, 40, 0x33),
    ]);
    let artifacts = RecordingArtifacts::default();
    let observed = port(
        ScriptedSource::new(),
        backend(Arc::clone(&script), &ledger),
        artifacts.clone(),
    );
    let limits = CaptureLimits {
        max_packets: 1_000,
        max_bytes: byte_bound,
        max_duration_ms: 60_000,
    };
    let settlement =
        settled(run(&observed, capture_request(&uuid(11), limits, None), &claim).expect("capture"));
    assert_eq!(settlement.packets_captured, 1);
    assert_eq!(settlement.bytes_captured, 40);
    let bytes = artifacts.published()[0].1.clone();
    assert_eq!(bytes.len() as u64, byte_bound);
    assert_eq!(
        bytes,
        expected_stream(&[(&[0x31u8; 40], 1, 0, 40)]),
        "the artifact is the Runtime's own stream at the bound"
    );
    assert_eq!(read_pcap(&bytes).expect("pcap").len(), 1);

    // The very same packets with room for two records keep both.
    let ledger = Arc::new(Ledger::default());
    let script = Script::new(vec![packet(1, 0, 40, 0x31), packet(2, 0, 40, 0x32)]);
    let artifacts = RecordingArtifacts::default();
    let observed = port(
        ScriptedSource::new(),
        backend(Arc::clone(&script), &ledger),
        artifacts.clone(),
    );
    let limits = CaptureLimits {
        max_packets: 1_000,
        max_bytes: byte_bound + 56,
        max_duration_ms: 60_000,
    };
    let settlement =
        settled(run(&observed, capture_request(&uuid(13), limits, None), &claim).expect("capture"));
    assert_eq!(settlement.packets_captured, 2);
    assert_eq!(artifacts.published()[0].1.len() as u64, byte_bound + 56);

    // The duration bound stops a capture whose device never reports a packet, and what such a
    // capture settled with is published like any other stream: a complete classic-PCAP file whose
    // header is the whole of it, so the reference and the counts describe the same file.
    let ledger = Arc::new(Ledger::default());
    let script = Script::new(vec![Step::Timeout, Step::Timeout, Step::Timeout]);
    let artifacts = RecordingArtifacts::default();
    let observed = port(
        ScriptedSource::new(),
        backend(Arc::clone(&script), &ledger),
        artifacts.clone(),
    );
    let limits = CaptureLimits {
        max_packets: 1_000,
        max_bytes: 1_000_000,
        max_duration_ms: 0,
    };
    let settlement =
        settled(run(&observed, capture_request(&uuid(12), limits, None), &claim).expect("capture"));
    assert!(!settlement.cancelled);
    assert_eq!(settlement.packets_captured, 0);
    assert_eq!(settlement.bytes_captured, 0);
    let [(kind, bytes)] = artifacts.published().try_into().expect("one publication");
    assert_eq!(kind, "capture");
    assert_eq!(bytes, expected_stream(&[]));
    assert_eq!(read_pcap(&bytes).expect("pcap").len(), 0);
    assert_eq!(
        settlement.capture_ref.as_deref(),
        Some("dbref:capture:00000001")
    );
    // No `persist_to` was requested, so there is no file to commit a destination for.
    assert_eq!(settlement.destination, None);
    assert_eq!(Ledger::count(&ledger.closed), 1);
}

#[test]
fn i8_net_g08_capture_writer_emits_the_runtime_classic_pcap_stream() {
    let ledger = Arc::new(Ledger::default());
    let script = Script::new(vec![
        packet(1_700_000_000, 123_456, 200, 0x41),
        packet(1_700_000_001, 654_321, 9, 0x42),
        Step::End,
    ]);
    let artifacts = RecordingArtifacts::default();
    let observed = port(
        ScriptedSource::new(),
        backend(Arc::clone(&script), &ledger),
        artifacts.clone(),
    );
    let claims = LocalExecutionClaims::default();
    let claim = claim_for(&claims, 1);
    let settlement = settled(
        run(
            &observed,
            capture_request(&uuid(20), generous(), None),
            &claim,
        )
        .expect("capture"),
    );
    assert_eq!(settlement.packets_captured, 2);
    assert_eq!(settlement.bytes_captured, 209);
    assert_eq!(
        settlement.capture_ref.as_deref(),
        Some("dbref:capture:00000001")
    );

    let [(kind, bytes)] = artifacts.published().try_into().expect("one publication");
    // The bytes are the one classic-PCAP stream the Runtime owns, byte for byte.
    assert_eq!(kind, "capture");
    assert_eq!(
        bytes,
        expected_stream(&[
            (&[0x41u8; 200], 1_700_000_000, 123_456, 200),
            (&[0x42u8; 9], 1_700_000_001, 654_321, 9),
        ])
    );
    let records = read_pcap(&bytes).expect("pcap");
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].index, 0);
    assert_eq!(records[0].seconds, 1_700_000_000);
    assert_eq!(records[0].microseconds, 123_456);
    assert_eq!(records[0].original_len, 200);
    assert_eq!(records[0].bytes.len(), 200);
    assert_eq!(records[1].index, 1);
    assert_eq!(records[1].bytes, vec![0x42u8; 9]);

    // A packet longer than the snaplen is stored at the snaplen the Runtime declares while
    // its original length is kept, exactly as classic PCAP records a truncated capture.
    let oversized = runtime::PCAP_SNAPLEN as usize + 100;
    let script = Script::new(vec![
        Step::Packet {
            seconds: 5,
            microseconds: 6,
            original_len: oversized as u32,
            bytes: vec![0x51; oversized],
        },
        Step::End,
    ]);
    let artifacts = RecordingArtifacts::default();
    let observed = port(
        ScriptedSource::new(),
        backend(Arc::clone(&script), &ledger),
        artifacts.clone(),
    );
    run(
        &observed,
        capture_request(&uuid(21), generous(), None),
        &claim,
    )
    .expect("capture");
    let records = read_pcap(&artifacts.published()[0].1).expect("pcap");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].bytes.len(), runtime::PCAP_SNAPLEN as usize);
    assert_eq!(records[0].original_len, oversized as u32);
}

#[test]
fn i8_net_g09_stop_request_settles_with_exact_cleanup_and_never_publishes_a_partial_capture() {
    let claims = LocalExecutionClaims::default();
    let claim = claim_for(&claims, 1);

    // R-NET-004: a stop request asks the capture to settle, and the settle publishes the
    // complete record the capture holds.
    let ledger = Arc::new(Ledger::default());
    let script = Script::new(vec![
        packet(10, 20, 48, 0x61),
        packet(11, 21, 48, 0x62),
        Step::Wait,
    ]);
    let artifacts = RecordingArtifacts::default();
    let observed = port(
        ScriptedSource::new(),
        backend(Arc::clone(&script), &ledger),
        artifacts.clone(),
    );
    let capture_id = uuid(30);
    let running = observed.clone();
    let handle = thread::spawn({
        let capture_id = capture_id.clone();
        move || {
            let claims = LocalExecutionClaims::default();
            let claim = claim_for(&claims, 2);
            running
                .run(
                    &executed(2),
                    capture_request(&capture_id, generous(), None),
                    &claim,
                )
                .map(|settlement| settlement.outcome)
        }
    });
    wait_for(
        || Ledger::count(&ledger.delivered) == 2,
        "delivered two packets",
    );
    assert_eq!(
        run(&observed, stop_request(&capture_id), &claim).expect("stop"),
        NetworkPrimitiveOutcome::CaptureStopRequested
    );
    // The stop is a request: the capture still holds its device until it settles.
    assert_eq!(Ledger::count(&ledger.closed), 0);
    script.gate.release();
    let settlement = settled(handle.join().expect("capture thread").expect("capture"));
    assert!(!settlement.cancelled);
    assert_eq!(settlement.packets_captured, 2);
    assert_eq!(settlement.bytes_captured, 96);
    assert!(settlement.cleanup_verified);
    assert_eq!(Ledger::count(&ledger.closed), 1);
    assert_eq!(
        settlement.capture_ref.as_deref(),
        Some("dbref:capture:00000001")
    );
    assert_eq!(
        artifacts.published()[0].1,
        expected_stream(&[(&[0x61u8; 48], 10, 20, 48), (&[0x62u8; 48], 11, 21, 48)])
    );

    // An execution cancellation reaches the capture through its own claim, settles it as
    // cancelled, and publishes no partial capture at all.
    let ledger = Arc::new(Ledger::default());
    let script = Script::new(vec![packet(12, 22, 32, 0x71), Step::Wait]);
    let artifacts = RecordingArtifacts::default();
    let observed = port(
        ScriptedSource::new(),
        backend(Arc::clone(&script), &ledger),
        artifacts.clone(),
    );
    let cancellation = Arc::new(LocalExecutionClaims::default());
    let capture_id = uuid(31);
    let running = observed.clone();
    let cancelling = Arc::clone(&cancellation);
    let handle = thread::spawn(move || {
        let claim = cancelling.claim(uuid(0x9003)).expect("claim");
        running
            .run(
                &executed(3),
                capture_request(&capture_id, generous(), None),
                &claim,
            )
            .map(|settlement| settlement.outcome)
    });
    wait_for(
        || Ledger::count(&ledger.delivered) == 1,
        "delivered one packet",
    );
    assert!(cancellation.request_cancel(&uuid(0x9003)));
    script.gate.release();
    let settlement = settled(handle.join().expect("capture thread").expect("capture"));
    assert!(settlement.cancelled);
    assert_eq!(settlement.packets_captured, 1);
    assert_eq!(settlement.capture_ref, None);
    assert_eq!(settlement.destination, None);
    // The cleanup is still reported as verified, and the device was closed exactly once.
    assert!(settlement.cleanup_verified);
    assert_eq!(Ledger::count(&ledger.closed), 1);
    assert_eq!(Ledger::count(&ledger.broken), 1);
    assert_eq!(artifacts.count(), 0);
}

#[test]
fn i8_net_g10_capture_persist_to_writes_the_same_bytes_through_the_daemons_own_write_primitive() {
    let directory = std::env::temp_dir().join("i8-net-daemon-persist");
    std::fs::create_dir_all(&directory).expect("temp directory");
    let destination = directory.join("capture.pcap");
    let ledger = Arc::new(Ledger::default());
    let script = Script::new(vec![packet(3, 4, 40, 0x81), Step::End]);
    let artifacts = RecordingArtifacts::default();
    let observed = port(
        ScriptedSource::new(),
        backend(Arc::clone(&script), &ledger),
        artifacts.clone(),
    );
    let claims = LocalExecutionClaims::default();
    let claim = claim_for(&claims, 1);
    let target = FileTarget {
        target_type: FileTargetType::Path,
        value: destination.to_string_lossy().into_owned(),
    };
    let settlement = settled(
        run(
            &observed,
            capture_request(&uuid(40), generous(), Some(target.clone())),
            &claim,
        )
        .expect("capture"),
    );
    let published = artifacts.published()[0].1.clone();
    assert_eq!(
        settlement.capture_ref.as_deref(),
        Some("dbref:capture:00000001")
    );
    // R-NET-003: the destination holds the same complete capture the artifact holds.
    assert_eq!(settlement.destination, Some(target.clone()));
    assert_eq!(
        std::fs::read(&destination).expect("persisted capture"),
        published
    );
    // No partially written capture is left beside it.
    assert!(
        !directory
            .join(format!(".{}.pcap.tmp", uuid(40).as_str()))
            .exists()
    );
    let _ = std::fs::remove_file(&destination);

    // A capture whose device reports no packet persists the same empty stream under the requested
    // destination, so the caller's own file exists and is a legal classic-PCAP file even then.
    let ledger = Arc::new(Ledger::default());
    let script = Script::new(vec![Step::Timeout, Step::Timeout]);
    let artifacts = RecordingArtifacts::default();
    let observed = port(
        ScriptedSource::new(),
        backend(Arc::clone(&script), &ledger),
        artifacts.clone(),
    );
    let empty_limits = CaptureLimits {
        max_packets: 10,
        max_bytes: 4_096,
        max_duration_ms: 0,
    };
    let settlement = settled(
        run(
            &observed,
            capture_request(&uuid(42), empty_limits, Some(target.clone())),
            &claim,
        )
        .expect("capture"),
    );
    assert!(!settlement.cancelled);
    assert_eq!(settlement.packets_captured, 0);
    assert_eq!(
        settlement.capture_ref.as_deref(),
        Some("dbref:capture:00000001")
    );
    assert_eq!(settlement.destination, Some(target.clone()));
    let empty = std::fs::read(&destination).expect("persisted capture");
    assert_eq!(empty, expected_stream(&[]));
    assert_eq!(artifacts.published()[0].1, empty);
    let _ = std::fs::remove_file(&destination);

    // A destination this host cannot resolve is a structured failure, not a silent skip, and
    // the capture has already settled by the time it is reported.
    let ledger = Arc::new(Ledger::default());
    let script = Script::new(vec![packet(5, 6, 40, 0x82), Step::End]);
    let observed = port(
        ScriptedSource::new(),
        backend(Arc::clone(&script), &ledger),
        RecordingArtifacts::default(),
    );
    let failure = run(
        &observed,
        capture_request(
            &uuid(41),
            generous(),
            Some(FileTarget {
                target_type: FileTargetType::ContentUri,
                value: "content://captures/1".to_owned(),
            }),
        ),
        &claim,
    )
    .expect_err("this host has no content resolver");
    assert_eq!(failure.error.code, ErrorCode::InvalidArgument);
}

// ---------------------------------------------------------------------------------------
// S-NET-004 and the registered event.
// ---------------------------------------------------------------------------------------

#[test]
fn i8_net_g11_injection_calls_the_device_exactly_count_times_and_reports_local_acceptance() {
    let ledger = Arc::new(Ledger::default());
    let script = Script::new(Vec::new());
    let observed = port(
        ScriptedSource::new(),
        backend(Arc::clone(&script), &ledger),
        RecordingArtifacts::default(),
    );
    let claims = LocalExecutionClaims::default();
    let claim = claim_for(&claims, 1);
    let packet = vec![0xab; 64];
    let outcome = run(
        &observed,
        NetworkPrimitiveRequest::PacketInject {
            interface: "wlan0".to_owned(),
            packet: packet.clone(),
            count: 3,
            interval_ms: 0,
        },
        &claim,
    )
    .expect("inject");
    let NetworkPrimitiveOutcome::PacketInjected(result) = outcome else {
        panic!("injection settled with another outcome: {outcome:?}");
    };
    assert_eq!(result.requested_packets, 3);
    assert_eq!(result.accepted_packets, 3);
    assert_eq!(result.bytes_accepted, 192);
    // Exactly one send per requested repetition, of exactly the admitted bytes.
    assert_eq!(ledger.injected(), vec![packet.clone(); 3]);
    assert_eq!(Ledger::count(&ledger.opens), 1);
    assert_eq!(Ledger::count(&ledger.activated), 1);
    assert_eq!(Ledger::count(&ledger.closed), 1);
    assert_eq!(ledger.interfaces.lock().expect("ledger lock")[0], "wlan0");

    // A device that refuses a send is a structured error rather than a partial success.
    let ledger = Arc::new(Ledger::default());
    let script = Script::refusing_injection();
    let observed = port(
        ScriptedSource::new(),
        backend(Arc::clone(&script), &ledger),
        RecordingArtifacts::default(),
    );
    let failure = run(
        &observed,
        NetworkPrimitiveRequest::PacketInject {
            interface: "wlan0".to_owned(),
            packet: vec![0xcd; 8],
            count: 2,
            interval_ms: 0,
        },
        &claim,
    )
    .expect_err("a refused send is not an accepted packet");
    assert_eq!(failure.error.code, ErrorCode::CaptureFailed);
    assert!(failure.cleanup_verified);
    assert_eq!(ledger.injected().len(), 1);
    assert_eq!(Ledger::count(&ledger.closed), 1);
}

#[test]
fn i8_net_g12_inspect_never_publishes_default_network_data_artifacts() {
    let artifacts = RecordingArtifacts::default();
    let ledger = Arc::new(Ledger::default());
    let observed = port(
        ScriptedSource::new().links().routes(),
        backend(Script::new(Vec::new()), &ledger),
        artifacts.clone(),
    );
    let claims = LocalExecutionClaims::default();
    let claim = claim_for(&claims, 1);
    run(&observed, inspect_scope(NetworkScope::Routes), &claim).expect("inspect");
    run(&observed, inspect_scope(NetworkScope::All), &claim).expect("inspect");
    assert_eq!(artifacts.count(), 0);

    let artifacts = RecordingArtifacts::default();
    let blind = port(
        ScriptedSource::new(),
        backend(Script::new(Vec::new()), &ledger),
        artifacts.clone(),
    );
    run(&blind, inspect_scope(NetworkScope::Routes), &claim).expect("inspect");
    assert_eq!(artifacts.count(), 0);
}

#[test]
fn i8_net_g13_diagnose_consumes_only_the_daemons_own_observed_facts() {
    let ledger = Arc::new(Ledger::default());
    let observed = port(
        ScriptedSource::new().links().routes().update(|facts| {
            facts.dns = Some(vec!["192.0.2.53".to_owned()]);
        }),
        backend(Script::new(Vec::new()), &ledger),
        RecordingArtifacts::default(),
    );
    let claims = LocalExecutionClaims::default();
    let claim = claim_for(&claims, 1);

    let outcome = run(
        &observed,
        NetworkPrimitiveRequest::Diagnose(NetworkDiagnoseInput::Connectivity {}, magisk_plan()),
        &claim,
    )
    .expect("diagnose");
    let NetworkPrimitiveOutcome::Diagnose(NetworkDiagnoseResult::Connectivity {
        outcome,
        active_network_present,
        default_route_present,
        dns_configured,
        ..
    }) = outcome
    else {
        panic!("diagnose settled with another result: {outcome:?}");
    };
    assert_eq!(outcome, DiagnosticOutcome::Success);
    assert_eq!(active_network_present, Some(true));
    assert_eq!(default_route_present, Some(true));
    assert_eq!(dns_configured, Some(true));

    // An addressed interface with no default route is `no_route`, and a DNS fact this host
    // could not establish is omitted rather than reported as unconfigured.
    let no_route = port(
        ScriptedSource::new().links().update(|facts| {
            let mut routes = route_message(AF_INET, 24, 0, &[attribute(4, &2u32.to_ne_bytes())]);
            routes.extend_from_slice(&netlink_done());
            facts.route = Some(SourceDump {
                bytes: routes,
                truncated: false,
            });
        }),
        backend(Script::new(Vec::new()), &ledger),
        RecordingArtifacts::default(),
    );
    let outcome = run(
        &no_route,
        NetworkPrimitiveRequest::Diagnose(NetworkDiagnoseInput::Connectivity {}, magisk_plan()),
        &claim,
    )
    .expect("diagnose");
    let NetworkPrimitiveOutcome::Diagnose(NetworkDiagnoseResult::Connectivity {
        outcome,
        active_network_present,
        default_route_present,
        dns_configured,
        ..
    }) = outcome
    else {
        panic!("diagnose settled with another result: {outcome:?}");
    };
    assert_eq!(outcome, DiagnosticOutcome::NoRoute);
    assert_eq!(active_network_present, Some(true));
    assert_eq!(default_route_present, Some(false));
    assert_eq!(dns_configured, None);

    // No interface that is up, not loopback and addressed is `unreachable`.
    let loopback_only = port(
        ScriptedSource::new().update(|facts| {
            facts.link = Some(SourceDump {
                bytes: link_dump(),
                truncated: false,
            });
            let mut addresses = address_message(AF_INET, 8, 1, &[127, 0, 0, 1], None);
            addresses.extend_from_slice(&netlink_done());
            facts.address = Some(SourceDump {
                bytes: addresses,
                truncated: false,
            });
            let mut routes = route_message(AF_INET, 24, 0, &[attribute(4, &2u32.to_ne_bytes())]);
            routes.extend_from_slice(&netlink_done());
            facts.route = Some(SourceDump {
                bytes: routes,
                truncated: false,
            });
        }),
        backend(Script::new(Vec::new()), &ledger),
        RecordingArtifacts::default(),
    );
    let outcome = run(
        &loopback_only,
        NetworkPrimitiveRequest::Diagnose(NetworkDiagnoseInput::Connectivity {}, magisk_plan()),
        &claim,
    )
    .expect("diagnose");
    let NetworkPrimitiveOutcome::Diagnose(NetworkDiagnoseResult::Connectivity {
        outcome,
        active_network_present,
        default_route_present,
        ..
    }) = outcome
    else {
        panic!("diagnose settled with another result: {outcome:?}");
    };
    assert_eq!(outcome, DiagnosticOutcome::Unreachable);
    assert_eq!(active_network_present, Some(false));
    assert_eq!(default_route_present, Some(false));

    // A route test resolves against this host's own routing table by longest prefix, so a
    // destination the default route covers reports the default route and its gateway.
    let outcome = run(
        &observed,
        NetworkPrimitiveRequest::Diagnose(
            NetworkDiagnoseInput::Route {
                destination_ip: "198.51.100.7".to_owned(),
            },
            magisk_plan(),
        ),
        &claim,
    )
    .expect("diagnose");
    let NetworkPrimitiveOutcome::Diagnose(NetworkDiagnoseResult::Route {
        outcome,
        destination_ip,
        interface,
        gateway,
        ..
    }) = outcome
    else {
        panic!("diagnose settled with another result: {outcome:?}");
    };
    assert_eq!(outcome, DiagnosticOutcome::Success);
    assert_eq!(destination_ip, "198.51.100.7");
    assert_eq!(interface.as_deref(), Some("wlan0"));
    assert_eq!(gateway.as_deref(), Some("192.0.2.1"));

    // A destination the more specific route covers takes that route instead, and a route
    // without a gateway reports none rather than the default route's.
    let outcome = run(
        &observed,
        NetworkPrimitiveRequest::Diagnose(
            NetworkDiagnoseInput::Route {
                destination_ip: "192.0.2.55".to_owned(),
            },
            magisk_plan(),
        ),
        &claim,
    )
    .expect("diagnose");
    let NetworkPrimitiveOutcome::Diagnose(NetworkDiagnoseResult::Route {
        outcome,
        destination_ip,
        interface,
        gateway,
        ..
    }) = outcome
    else {
        panic!("diagnose settled with another result: {outcome:?}");
    };
    assert_eq!(outcome, DiagnosticOutcome::Success);
    assert_eq!(destination_ip, "192.0.2.55");
    assert_eq!(interface.as_deref(), Some("wlan0"));
    assert_eq!(gateway, None);

    // A destination no route in the table covers is `no_route`, which is an outcome rather
    // than an error.
    let outcome = run(
        &no_route,
        NetworkPrimitiveRequest::Diagnose(
            NetworkDiagnoseInput::Route {
                destination_ip: "203.0.113.9".to_owned(),
            },
            magisk_plan(),
        ),
        &claim,
    )
    .expect("diagnose");
    let NetworkPrimitiveOutcome::Diagnose(NetworkDiagnoseResult::Route { outcome, .. }) = outcome
    else {
        panic!("diagnose settled with another result: {outcome:?}");
    };
    assert_eq!(outcome, DiagnosticOutcome::NoRoute);

    // A destination that is not an address is malformed input, and a host with no routing
    // fact at all cannot resolve one rather than guessing.
    let failure = run(
        &observed,
        NetworkPrimitiveRequest::Diagnose(
            NetworkDiagnoseInput::Route {
                destination_ip: "example.invalid".to_owned(),
            },
            magisk_plan(),
        ),
        &claim,
    )
    .expect_err("a destination that is not an address is malformed input");
    assert_eq!(failure.error.code, ErrorCode::InvalidArgument);

    let blind = port(
        ScriptedSource::new(),
        backend(Script::new(Vec::new()), &ledger),
        RecordingArtifacts::default(),
    );
    let failure = run(
        &blind,
        NetworkPrimitiveRequest::Diagnose(
            NetworkDiagnoseInput::Route {
                destination_ip: "192.0.2.55".to_owned(),
            },
            magisk_plan(),
        ),
        &claim,
    )
    .expect_err("a host with no routing fact cannot resolve a destination");
    assert_eq!(failure.error.code, ErrorCode::InternalError);
}

/// R-NET-005 `capture.read`: this host supplies the bounded bytes of a caller-named file and
/// the Runtime owns the PCAP format, so the port never decodes a second time.
#[test]
fn i8_net_g14_capture_read_supplies_bounded_file_bytes_from_the_daemons_own_read_primitive() {
    let ledger = Arc::new(Ledger::default());
    let observed = port(
        ScriptedSource::new(),
        backend(Script::new(Vec::new()), &ledger),
        RecordingArtifacts::default(),
    );
    let claims = LocalExecutionClaims::default();
    let claim = claim_for(&claims, 1);

    let directory = std::env::temp_dir().join("i8-net-daemon-capture-read");
    std::fs::create_dir_all(&directory).expect("temp directory");
    let path = directory.join("stream.pcap");
    let stream = expected_stream(&[(&[0x91u8; 24], 7, 8, 24)]);
    std::fs::write(&path, &stream).expect("write capture");
    let outcome = run(
        &observed,
        NetworkPrimitiveRequest::CaptureFileBytes {
            target: FileTarget {
                target_type: FileTargetType::Path,
                value: path.to_string_lossy().into_owned(),
            },
        },
        &claim,
    )
    .expect("capture read");
    assert_eq!(read_pcap(&stream).expect("pcap").len(), 1);
    assert_eq!(outcome, NetworkPrimitiveOutcome::CaptureFileBytes(stream));

    // A capture file that does not exist, and a content URI this daemon has no resolver for,
    // are each a structured refusal rather than an empty read.
    let failure = run(
        &observed,
        NetworkPrimitiveRequest::CaptureFileBytes {
            target: FileTarget {
                target_type: FileTargetType::Path,
                value: directory.join("absent.pcap").to_string_lossy().into_owned(),
            },
        },
        &claim,
    )
    .expect_err("a capture file that does not exist cannot be read");
    assert_eq!(failure.error.code, ErrorCode::IoError);
    let failure = run(
        &observed,
        NetworkPrimitiveRequest::CaptureFileBytes {
            target: FileTarget {
                target_type: FileTargetType::ContentUri,
                value: "content://captures/1".to_owned(),
            },
        },
        &claim,
    )
    .expect_err("this host has no content resolver");
    assert_eq!(failure.error.code, ErrorCode::CapabilityUnavailable);
    let _ = std::fs::remove_file(&path);
}

/// The registry, the device seam and the port are one mechanism: the traits a test implements
/// here are the same ones the Magisk host installs over the bundled static libpcap.
#[test]
fn i8_net_g15_the_port_owns_one_device_seam_and_closes_it_on_every_path() {
    let ledger = Arc::new(Ledger::default());
    let script = Script::new(vec![packet(1, 1, 16, 0x01), Step::End]);
    let artifacts = RecordingArtifacts::default();
    let observed = port(
        ScriptedSource::new(),
        ScriptBackend::new(Arc::clone(&script), Arc::clone(&ledger)),
        artifacts.clone(),
    );
    let claims = LocalExecutionClaims::default();
    let claim = claim_for(&claims, 1);
    let settlement = settled(
        run(
            &observed,
            capture_request(&uuid(50), generous(), None),
            &claim,
        )
        .expect("capture"),
    );
    assert_eq!(settlement.packets_captured, 1);
    assert!(settlement.cleanup_verified);
    assert_eq!(Ledger::count(&ledger.opens), 1);
    assert_eq!(Ledger::count(&ledger.activated), 1);
    assert_eq!(Ledger::count(&ledger.closed), 1);
    assert_eq!(Ledger::count(&ledger.broken), 0);
    assert_eq!(
        ledger.filter(),
        None,
        "no BPF filter was requested for this capture"
    );
    // The one publication carries a stream the Runtime itself can read back.
    assert_eq!(
        read_pcap(&artifacts.published()[0].1).expect("pcap").len(),
        1
    );

    // A device that fails while it reads settles as a structured capture failure with the
    // same verified cleanup.
    let ledger = Arc::new(Ledger::default());
    let script = Script::new(vec![packet(1, 1, 16, 0x02), Step::Failed]);
    let observed = port(
        ScriptedSource::new(),
        ScriptBackend::new(Arc::clone(&script), Arc::clone(&ledger)),
        RecordingArtifacts::default(),
    );
    let failure = run(
        &observed,
        capture_request(&uuid(51), generous(), None),
        &claim,
    )
    .expect_err("a device failure is not a settled capture");
    assert_eq!(failure.error.code, ErrorCode::CaptureFailed);
    assert!(failure.cleanup_verified);
    assert_eq!(Ledger::count(&ledger.closed), 1);
    assert_eq!(Ledger::count(&ledger.broken), 0);
}

#[test]
fn i8_net_g16_native_default_source_uses_only_direct_route_identity_facts() {
    let routes = vec![
        RouteEntry {
            destination: "192.0.2.0/24".to_owned(),
            gateway: None,
            interface: Some("rmnet0".to_owned()),
            metric: Some(1),
        },
        RouteEntry {
            destination: "0.0.0.0/0".to_owned(),
            gateway: Some("192.0.2.1".to_owned()),
            interface: Some("wlan0".to_owned()),
            metric: Some(20),
        },
        RouteEntry {
            destination: "::/0".to_owned(),
            gateway: None,
            interface: Some("wlan0".to_owned()),
            metric: Some(30),
        },
    ];
    let observed = native_default_event_from_routes(&routes).expect("default route");
    assert_eq!(observed.network_id.as_deref(), Some("wlan0"));
    assert_eq!(
        observed.transport, None,
        "netlink does not establish Android transport"
    );

    let lost = native_default_event_from_routes(&[]).expect("established route loss");
    assert_eq!((lost.network_id, lost.transport), (None, None));

    let unresolved = vec![RouteEntry {
        destination: "0.0.0.0/0".to_owned(),
        gateway: None,
        interface: None,
        metric: None,
    }];
    assert_eq!(
        native_default_event_from_routes(&unresolved)
            .expect_err("an unresolved interface is not an established loss")
            .code,
        ErrorCode::IoError,
    );
}

#[test]
fn i8_net_g17_a_capture_declares_the_link_type_its_records_carry_and_a_nameless_one_is_refused() {
    let claims = LocalExecutionClaims::default();
    let claim = claim_for(&claims, 1);

    // A TUN interface reports raw IP: one bare IP packet per record. Its stream must declare
    // that link type rather than the Ethernet one the format defaults to.
    let ledger = Arc::new(Ledger::default());
    let script = Script::raw_ip(vec![packet(1_789_171_200, 500_000, 40, 0x45), Step::End]);
    let artifacts = RecordingArtifacts::default();
    let observed = port(
        ScriptedSource::new(),
        backend(Arc::clone(&script), &ledger),
        artifacts.clone(),
    );
    let settlement = settled(
        run(
            &observed,
            capture_request(&uuid(60), generous(), None),
            &claim,
        )
        .expect("capture"),
    );
    assert_eq!(settlement.packets_captured, 1);
    assert!(settlement.cleanup_verified);
    let [(kind, bytes)] = artifacts.published().try_into().expect("one publication");
    assert_eq!(kind, "capture");
    assert_eq!(
        bytes,
        expected_stream_of(
            runtime::PCAP_LINKTYPE_RAW,
            &[(&[0x45u8; 40], 1_789_171_200, 500_000, 40)]
        )
    );
    assert_eq!(
        u32::from_le_bytes(bytes[20..24].try_into().unwrap()),
        runtime::PCAP_LINKTYPE_RAW,
        "the stream declares the link type its own records carry"
    );
    assert_eq!(read_pcap(&bytes).expect("pcap").len(), 1);
    assert_eq!(Ledger::count(&ledger.closed), 1);

    // A link type the classic-PCAP format cannot express is refused as its own named fact, and
    // that fact is not the device fault.
    let ledger = Arc::new(Ledger::default());
    let script = Script::unrepresentable(vec![packet(1, 1, 16, 0x01), Step::End]);
    let observed = port(
        ScriptedSource::new(),
        backend(Arc::clone(&script), &ledger),
        RecordingArtifacts::default(),
    );
    let refusal = run(
        &observed,
        capture_request(&uuid(61), generous(), None),
        &claim,
    )
    .expect_err("an unrepresentable link type is not a capture");
    assert_eq!(
        refusal.error.code,
        ErrorCode::Unsupported,
        "refused with: {}",
        refusal.error.reason
    );
    assert!(refusal.cleanup_verified);
    assert_eq!(Ledger::count(&ledger.closed), 1);
    assert_eq!(Ledger::count(&ledger.broken), 0);

    // A device that cannot be activated is a device fault, and a caller can tell the two apart.
    let ledger = Arc::new(Ledger::default());
    let script = Script::failing_activation(vec![Step::End]);
    let observed = port(
        ScriptedSource::new(),
        backend(Arc::clone(&script), &ledger),
        RecordingArtifacts::default(),
    );
    let fault = run(
        &observed,
        capture_request(&uuid(62), generous(), None),
        &claim,
    )
    .expect_err("an unactivatable device is not a capture");
    assert_eq!(
        fault.error.code,
        ErrorCode::CaptureFailed,
        "faulted with: {}",
        fault.error.reason
    );
    assert!(fault.cleanup_verified);
    assert_eq!(Ledger::count(&ledger.closed), 1);
    assert_ne!(
        refusal.error.reason, fault.error.reason,
        "a named refusal is not the device fault"
    );
}
