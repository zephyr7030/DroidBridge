//! The Magisk execution surface's Network primitive port (S-AUTH-NET-001, S-NET-001..005).
//!
//! This host owns every field family: interfaces, addresses and routes come from this
//! daemon's own netlink queries, sockets from its own `/proc/net/{tcp,tcp6,udp,udp6}` reads
//! and DNS from the authenticated APK companion's `LinkProperties` facts. Raw capture and
//! injection run over the bundled static libpcap through one private FFI module, and the
//! capture bytes stay in the classic-PCAP little-endian microsecond format the Runtime owns,
//! so no second capture abstraction and no second PCAP owner exists.
//!
//! Everything above the syscall/FFI seam is portable and is driven by the two seams a test
//! supplies: `NetworkHostSource` for the observation bytes and `CaptureBackend` for a device.

use contract::{
    CaptureId, DiagnosticOutcome, DnsEntry, ErrorCode, FileTarget, FileTargetType,
    InterfaceAddress, InterfaceEntry, NetworkDiagnoseInput, NetworkDiagnoseResult, NetworkScope,
    PacketInjectResult, RouteEntry, SocketEntry, SocketProtocol,
};
use domain::DomainError;
#[cfg(unix)]
use runtime::AndroidExecutionDispatch;
use runtime::{
    AdmittedExecution, ArtifactPort, CaptureSettlement, Established, ExecutionFailure,
    LocalExecutionClaim, NETWORK_MAX_CAPTURE_BYTES, NetworkDefaultChangedEvent,
    NetworkDefaultEventIngress, NetworkDefaultEventSource, NetworkDefaultSourceRegistration,
    NetworkFamily, NetworkFamilyPlan, NetworkInspectSettlement, NetworkPrimitiveOutcome,
    NetworkPrimitivePort, NetworkPrimitiveRequest, NetworkPrimitiveSettlement, NetworkSourcePlan,
    PCAP_FILE_HEADER_BYTES, PCAP_LINKTYPE_ETHERNET, PCAP_RECORD_HEADER_BYTES, PCAP_SNAPLEN,
    network_dns_probe, network_tcp_probe, network_tls_probe, pcap_file_header, pcap_record_header,
    scope_families,
};

use std::{
    collections::HashMap,
    fs,
    io::{Read as _, Write as _},
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
#[cfg(unix)]
use std::{
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    os::unix::net::UnixStream,
    thread::JoinHandle,
};

// ---------------------------------------------------------------------------------------
// Netlink reply parsing (S-NET-001): portable over the reply bytes, so a test drives it with
// scripted vectors and the syscall layer only has to produce those bytes.
// ---------------------------------------------------------------------------------------

const NLMSG_HEADER_BYTES: usize = 16;
const NLMSG_ERROR: u16 = 2;
const NLMSG_DONE: u16 = 3;
const RTM_NEWLINK: u16 = 16;
const RTM_NEWADDR: u16 = 20;
const RTM_NEWROUTE: u16 = 24;
/// The dump requests, which the kernel answers with the `RTM_NEW*` messages above.
#[cfg(unix)]
const RTM_GETLINK: u16 = 18;
#[cfg(unix)]
const RTM_GETADDR: u16 = 22;
#[cfg(unix)]
const RTM_GETROUTE: u16 = 26;
const IFINFOMSG_BYTES: usize = 16;
const IFADDRMSG_BYTES: usize = 8;
const RTMSG_BYTES: usize = 12;
const IFLA_IFNAME: u16 = 3;
const IFLA_MTU: u16 = 4;
const IFA_ADDRESS: u16 = 1;
const IFA_LOCAL: u16 = 2;
const RTA_DST: u16 = 1;
const RTA_OIF: u16 = 4;
const RTA_GATEWAY: u16 = 5;
const RTA_PRIORITY: u16 = 6;
const AF_INET: u8 = 2;
const AF_INET6: u8 = 10;
const IFF_UP: u32 = 0x1;
const IFF_LOOPBACK: u32 = 0x8;
const RTM_F_CLONED: u32 = 0x200;

/// One interface as the kernel reported it, before the R-NET-002 projection drops the facts
/// the Contract has no field for.
#[derive(Clone, Debug, PartialEq)]
pub struct LinkFacts {
    pub index: u32,
    pub name: String,
    pub up: bool,
    pub loopback: bool,
    pub mtu: Option<u32>,
    pub addresses: Vec<InterfaceAddress>,
}

/// One raw provider reply plus the source's own completeness fact. A dump the source could not
/// read to its end is reported as truncated rather than as a complete one.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SourceDump {
    pub bytes: Vec<u8>,
    pub truncated: bool,
}

/// The Magisk host's observation surface. The real implementation performs this daemon's own
/// netlink queries, `/proc/net` reads and the authenticated companion's `LinkProperties` query;
/// a test supplies scripted byte vectors. A method that cannot establish its fact says so here,
/// and the inspect settlement turns that into the R-NET-002 `unknown` state with the family's
/// data omitted.
pub trait NetworkHostSource: Send + Sync {
    fn link_dump(&self) -> Result<SourceDump, DomainError>;
    fn address_dump(&self) -> Result<SourceDump, DomainError>;
    fn route_dump(&self) -> Result<SourceDump, DomainError>;
    /// `/proc/net/<table>` text, or `None` when this host cannot see that table at all.
    fn socket_text(&self, table: &str) -> Result<Option<String>, DomainError>;
    /// The companion's `LinkProperties` DNS servers, or `None` while no companion answers.
    /// The query is an S-ANDROID-001 primitive of `execution`, so it carries that execution's
    /// own fence rather than an identity this host invents.
    fn dns_servers(
        &self,
        execution: &AdmittedExecution,
    ) -> Result<Option<Vec<String>>, DomainError>;
}

fn malformed(reason: &'static str) -> DomainError {
    DomainError::new(ErrorCode::InternalError, reason)
}

fn io_error(reason: &'static str) -> DomainError {
    DomainError::new(ErrorCode::IoError, reason)
}

fn pre_start(error: DomainError) -> ExecutionFailure {
    ExecutionFailure {
        error,
        cleanup_verified: true,
    }
}

/// R-NET-010's `CAPTURE_FAILED`, with the stable reason this host established it from. A
/// device's own error text is dynamic, so it stays on the device seam where it can be read.
fn device_failure(reason: &'static str) -> ExecutionFailure {
    ExecutionFailure {
        error: DomainError::new(ErrorCode::CaptureFailed, reason),
        cleanup_verified: true,
    }
}

/// One failure settled from the claim's own cleanup fact, so a caller never reports verified
/// cleanup it has not established.
fn failure(code: ErrorCode, reason: &'static str, claim: &LocalExecutionClaim) -> ExecutionFailure {
    ExecutionFailure {
        error: DomainError::new(code, reason),
        cleanup_verified: claim.cleanup_is_verified(),
    }
}

/// The reason one `NLMSG_ERROR` reply carries. A code this table does not name still reports
/// that the kernel refused the request rather than being read as an empty reply.
fn netlink_error_reason(body: &[u8]) -> &'static str {
    if body.len() < 4 {
        return "netlink reply reported an error";
    }
    match i32::from_ne_bytes([body[0], body[1], body[2], body[3]]) {
        0 => "netlink reply was an unexpected acknowledgement",
        -1 => "netlink request is not permitted",
        -2 => "netlink request found no such target",
        -22 => "netlink request is not supported",
        _ => "netlink reply reported an error",
    }
}

/// Walks one netlink reply. Every message must be complete inside `bytes`: a message whose
/// declared length runs past the buffer is a short reply rather than a short read, so it is
/// rejected instead of being parsed as a guessed prefix.
fn walk_netlink<'a>(
    bytes: &'a [u8],
    mut visit: impl FnMut(u16, &'a [u8]) -> Result<(), DomainError>,
) -> Result<(), DomainError> {
    let mut offset = 0usize;
    while offset < bytes.len() {
        if bytes.len() - offset < NLMSG_HEADER_BYTES {
            return Err(malformed("netlink reply has an incomplete message header"));
        }
        let header = &bytes[offset..offset + NLMSG_HEADER_BYTES];
        let length = u32::from_ne_bytes([header[0], header[1], header[2], header[3]]) as usize;
        let kind = u16::from_ne_bytes([header[4], header[5]]);
        if length < NLMSG_HEADER_BYTES || offset + length > bytes.len() {
            return Err(malformed("netlink reply has an incomplete message"));
        }
        let body = &bytes[offset + NLMSG_HEADER_BYTES..offset + length];
        if kind == NLMSG_DONE {
            return Ok(());
        }
        if kind == NLMSG_ERROR {
            return Err(DomainError::new(
                ErrorCode::InternalError,
                netlink_error_reason(body),
            ));
        }
        visit(kind, body)?;
        offset += (length + 3) & !3;
    }
    Ok(())
}

/// Whether one netlink reply already carries its own end-of-dump message. This is how the dump
/// loop knows the kernel has finished answering.
pub fn reply_is_complete(bytes: &[u8]) -> bool {
    let mut offset = 0usize;
    while bytes.len() - offset >= NLMSG_HEADER_BYTES {
        let header = &bytes[offset..offset + NLMSG_HEADER_BYTES];
        let length = u32::from_ne_bytes([header[0], header[1], header[2], header[3]]) as usize;
        let kind = u16::from_ne_bytes([header[4], header[5]]);
        if length < NLMSG_HEADER_BYTES || offset + length > bytes.len() {
            return false;
        }
        if kind == NLMSG_DONE || kind == NLMSG_ERROR {
            return true;
        }
        offset += (length + 3) & !3;
    }
    false
}

/// The prefix of one netlink reply that ends on a message boundary. A dump cut short by the
/// reader keeps every complete message and drops the partial tail, so the truncation stays the
/// source's fact to report instead of becoming a parse failure.
pub fn complete_prefix(bytes: &[u8]) -> &[u8] {
    let mut offset = 0usize;
    while bytes.len() - offset >= NLMSG_HEADER_BYTES {
        let header = &bytes[offset..offset + NLMSG_HEADER_BYTES];
        let length = u32::from_ne_bytes([header[0], header[1], header[2], header[3]]) as usize;
        if length < NLMSG_HEADER_BYTES || offset + length > bytes.len() {
            break;
        }
        offset += (length + 3) & !3;
    }
    &bytes[..offset.min(bytes.len())]
}

fn for_each_attribute<'a>(
    mut body: &'a [u8],
    mut visit: impl FnMut(u16, &'a [u8]),
) -> Result<(), DomainError> {
    while body.len() >= 4 {
        let length = u16::from_ne_bytes([body[0], body[1]]) as usize;
        let kind = u16::from_ne_bytes([body[2], body[3]]);
        if length < 4 || length > body.len() {
            return Err(malformed("netlink attribute is truncated"));
        }
        visit(kind, &body[4..length]);
        let aligned = (length + 3) & !3;
        if aligned > body.len() {
            return Ok(());
        }
        body = &body[aligned..];
    }
    Ok(())
}

fn native_u32(value: &[u8]) -> Option<u32> {
    (value.len() == 4).then(|| u32::from_ne_bytes([value[0], value[1], value[2], value[3]]))
}

/// `IFLA_IFNAME` arrives NUL-terminated. A name that is not usable text leaves the link
/// unreportable rather than being invented.
fn interface_name(value: &[u8]) -> Option<String> {
    let value = value.strip_suffix(&[0]).unwrap_or(value);
    if value.is_empty() || value.contains(&0) {
        return None;
    }
    std::str::from_utf8(value).ok().map(str::to_owned)
}

fn address_text(family: u8, value: Option<&[u8]>) -> Option<String> {
    match (family, value) {
        (AF_INET, Some(value)) if value.len() == 4 => {
            Some(Ipv4Addr::new(value[0], value[1], value[2], value[3]).to_string())
        }
        (AF_INET6, Some(value)) if value.len() == 16 => {
            let mut octets = [0u8; 16];
            octets.copy_from_slice(value);
            Some(Ipv6Addr::from(octets).to_string())
        }
        _ => None,
    }
}

/// The address and prefix one route message names. An absent `RTA_DST` is the default route,
/// whose canonical textual form is `0.0.0.0/0` or `::/0`.
fn route_destination(family: u8, prefix_length: u8, destination: Option<&[u8]>) -> Option<String> {
    let address = match (family, prefix_length) {
        (AF_INET, prefix) if prefix <= 32 => match destination {
            Some(value) if value.len() == 4 => {
                Ipv4Addr::new(value[0], value[1], value[2], value[3]).to_string()
            }
            Some(_) => return None,
            None => "0.0.0.0".to_owned(),
        },
        (AF_INET6, prefix) if prefix <= 128 => match destination {
            Some(value) if value.len() == 16 => {
                let mut octets = [0u8; 16];
                octets.copy_from_slice(value);
                Ipv6Addr::from(octets).to_string()
            }
            Some(_) => return None,
            None => "::".to_owned(),
        },
        _ => return None,
    };
    Some(format!("{address}/{prefix_length}"))
}

/// The interfaces, addresses and MTU facts of one `RTM_GETLINK` and one `RTM_GETADDR` reply.
pub fn link_facts(link_reply: &[u8], address_reply: &[u8]) -> Result<Vec<LinkFacts>, DomainError> {
    let mut links = Vec::new();
    walk_netlink(link_reply, |kind, body| {
        if kind != RTM_NEWLINK {
            return Ok(());
        }
        if body.len() < IFINFOMSG_BYTES {
            return Err(malformed("netlink link reply is truncated"));
        }
        let index = u32::from_ne_bytes([body[4], body[5], body[6], body[7]]);
        let flags = u32::from_ne_bytes([body[8], body[9], body[10], body[11]]);
        let mut name = None;
        let mut mtu = None;
        for_each_attribute(&body[IFINFOMSG_BYTES..], |kind, value| match kind {
            IFLA_IFNAME if name.is_none() => name = interface_name(value),
            IFLA_MTU if mtu.is_none() => mtu = native_u32(value),
            _ => {}
        })?;
        if let Some(name) = name {
            links.push(LinkFacts {
                index,
                name,
                up: flags & IFF_UP != 0,
                loopback: flags & IFF_LOOPBACK != 0,
                mtu,
                addresses: Vec::new(),
            });
        }
        Ok(())
    })?;
    walk_netlink(address_reply, |kind, body| {
        if kind != RTM_NEWADDR {
            return Ok(());
        }
        if body.len() < IFADDRMSG_BYTES {
            return Err(malformed("netlink address reply is truncated"));
        }
        let family = body[0];
        let prefix_length = body[1];
        let index = u32::from_ne_bytes([body[4], body[5], body[6], body[7]]);
        let mut local = None;
        let mut peer = None;
        for_each_attribute(&body[IFADDRMSG_BYTES..], |kind, value| match kind {
            IFA_LOCAL => local = Some(value),
            IFA_ADDRESS => peer = Some(value),
            _ => {}
        })?;
        // A point-to-point link reports the peer address in `IFA_ADDRESS` and its own in
        // `IFA_LOCAL`, so the local address is preferred and only falls back to the peer.
        let Some(text) = address_text(family, local.or(peer)) else {
            return Ok(());
        };
        if let Some(link) = links.iter_mut().find(|link| link.index == index) {
            link.addresses.push(InterfaceAddress {
                address: text,
                prefix_length: Some(prefix_length),
            });
        }
        Ok(())
    })?;
    Ok(links)
}

/// The R-NET-002 `interfaces` array of one link/address observation.
pub fn interface_entries(links: &[LinkFacts]) -> Vec<InterfaceEntry> {
    links
        .iter()
        .map(|link| InterfaceEntry {
            name: link.name.clone(),
            index: Some(link.index),
            up: Some(link.up),
            mtu: link.mtu,
            addresses: link.addresses.clone(),
        })
        .collect()
}

/// The R-NET-002 `routes` array of one `RTM_GETROUTE` reply. The interface name is resolved
/// against the link facts, so a route whose output interface this host did not enumerate omits
/// the field instead of naming a guessed one.
pub fn route_entries(
    route_reply: &[u8],
    links: &[LinkFacts],
) -> Result<Vec<RouteEntry>, DomainError> {
    let mut routes = Vec::new();
    walk_netlink(route_reply, |kind, body| {
        if kind != RTM_NEWROUTE {
            return Ok(());
        }
        if body.len() < RTMSG_BYTES {
            return Err(malformed("netlink route reply is truncated"));
        }
        let family = body[0];
        let prefix_length = body[1];
        let flags = u32::from_ne_bytes([body[8], body[9], body[10], body[11]]);
        // A cloned entry is a routing-cache result rather than a routing-table entry.
        if flags & RTM_F_CLONED != 0 {
            return Ok(());
        }
        let mut destination = None;
        let mut gateway = None;
        let mut output_index = None;
        let mut metric = None;
        for_each_attribute(&body[RTMSG_BYTES..], |kind, value| match kind {
            RTA_DST => destination = Some(value),
            RTA_GATEWAY => gateway = Some(value),
            RTA_OIF => output_index = native_u32(value).or(output_index),
            RTA_PRIORITY => metric = native_u32(value).or(metric),
            _ => {}
        })?;
        let Some(destination) = route_destination(family, prefix_length, destination) else {
            return Ok(());
        };
        let interface = output_index.and_then(|index| {
            links
                .iter()
                .find(|link| link.index == index)
                .map(|link| link.name.clone())
        });
        routes.push(RouteEntry {
            destination,
            gateway: address_text(family, gateway),
            interface,
            metric: metric.map(u64::from),
        });
        Ok(())
    })?;
    Ok(routes)
}

/// The prefix length one route entry's destination carries.
pub fn route_prefix(destination: &str) -> Option<u8> {
    destination.split_once('/')?.1.parse::<u8>().ok()
}

fn route_contains(destination: &str, target: IpAddr) -> bool {
    let Some((address, prefix)) = destination.split_once('/') else {
        return false;
    };
    let (Ok(network), Ok(prefix)) = (address.parse::<IpAddr>(), prefix.parse::<u8>()) else {
        return false;
    };
    match (network, target) {
        (IpAddr::V4(network), IpAddr::V4(target)) if prefix <= 32 => {
            let mask = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - u32::from(prefix))
            };
            u32::from(network) & mask == u32::from(target) & mask
        }
        (IpAddr::V6(network), IpAddr::V6(target)) if prefix <= 128 => {
            let mask = if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - u32::from(prefix))
            };
            u128::from(network) & mask == u128::from(target) & mask
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------------------
// `/proc/net` socket tables (S-NET-001): portable text parsing.
// ---------------------------------------------------------------------------------------

/// The four socket tables this host reads, in the order it reads them.
pub const SOCKET_TABLES: [&str; 4] = ["tcp", "tcp6", "udp", "udp6"];

pub fn socket_protocol_for_table(table: &str) -> SocketProtocol {
    match table {
        "tcp" | "tcp6" => SocketProtocol::Tcp,
        "udp" | "udp6" => SocketProtocol::Udp,
        _ => SocketProtocol::Other,
    }
}

/// The kernel's own socket-state names (`include/net/tcp_states.h`), which is the token a
/// `/proc/net` row reports a state in. A value this table does not name is omitted rather than
/// invented.
fn socket_state_name(state: u8) -> Option<&'static str> {
    match state {
        1 => Some("ESTABLISHED"),
        2 => Some("SYN_SENT"),
        3 => Some("SYN_RECV"),
        4 => Some("FIN_WAIT1"),
        5 => Some("FIN_WAIT2"),
        6 => Some("TIME_WAIT"),
        7 => Some("CLOSE"),
        8 => Some("CLOSE_WAIT"),
        9 => Some("LAST_ACK"),
        10 => Some("LISTEN"),
        11 => Some("CLOSING"),
        12 => Some("NEW_SYN_RECV"),
        _ => None,
    }
}

/// One `/proc/net` address field. The address is host-endian hex in the kernel's own width and
/// the port is big-endian hex; a field that is neither width is refused rather than guessed.
fn proc_address(field: &str) -> Option<(String, u16)> {
    let (address, port) = field.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    let text = match address.len() {
        8 => {
            let value = u32::from_str_radix(address, 16).ok()?;
            Ipv4Addr::from(value.to_le_bytes()).to_string()
        }
        32 => {
            let mut octets = [0u8; 16];
            for (index, chunk) in address.as_bytes().chunks(8).enumerate() {
                let value = u32::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
                octets[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
            }
            Ipv6Addr::from(octets).to_string()
        }
        _ => return None,
    };
    Some((text, port))
}

fn socket_entry(protocol: SocketProtocol, line: &str) -> Option<SocketEntry> {
    let fields = line.split_whitespace().collect::<Vec<_>>();
    if fields.len() < 8 {
        return None;
    }
    let (local_address, local_port) = proc_address(fields[1])?;
    let remote = match proc_address(fields[2]) {
        // An all-zero peer with port zero is the kernel's "not connected" row.
        Some((address, 0)) if address == "0.0.0.0" || address == "::" => None,
        other => other,
    };
    let state = u8::from_str_radix(fields[3], 16)
        .ok()
        .and_then(socket_state_name)
        .map(str::to_owned);
    Some(SocketEntry {
        protocol,
        local_address,
        local_port: Some(local_port),
        remote_address: remote.as_ref().map(|(address, _)| address.clone()),
        remote_port: remote.map(|(_, port)| port),
        state,
        uid: fields[7].parse::<u32>().ok(),
    })
}

/// One `/proc/net` table as R-NET-002 socket entries. A row this parser cannot decode is
/// skipped, so a malformed table never becomes an invented entry.
pub fn parse_sockets(table: &str, text: &str) -> Vec<SocketEntry> {
    let protocol = socket_protocol_for_table(table);
    text.lines()
        .filter_map(|line| socket_entry(protocol, line))
        .collect()
}

// ---------------------------------------------------------------------------------------
// Capture bounds, registry and the PCAP stream.
// ---------------------------------------------------------------------------------------

/// One capture's public bounds (R-NET-003). `max_bytes` bounds the complete classic-PCAP
/// stream, which is exactly the artifact S-ART-002 caps at the same number.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CaptureLimits {
    pub max_packets: u64,
    pub max_bytes: u64,
    pub max_duration_ms: u64,
}

/// One capture's progress. `bytes` counts the captured packet bytes and `stream_bytes` counts
/// the record bytes written, so `admits` bounds the artifact by the file header plus that.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CaptureProgress {
    pub packets: u64,
    pub bytes: u64,
    pub stream_bytes: u64,
}

impl CaptureProgress {
    /// Whether one more record of `packet_bytes` still fits inside the capture's bounds. The
    /// file header is part of the artifact, so it is accounted here and `max_bytes` never
    /// admits a stream whose artifact would exceed it.
    pub fn admits(&self, limits: CaptureLimits, packet_bytes: usize) -> bool {
        self.packets < limits.max_packets
            && self
                .stream_bytes
                .saturating_add(PCAP_FILE_HEADER_BYTES as u64)
                .saturating_add(PCAP_RECORD_HEADER_BYTES as u64)
                .saturating_add(packet_bytes as u64)
                <= limits.max_bytes
    }
}

/// One running capture. The stop request is the only signal a `capture.stop` may send it: the
/// R-NET-004 stop asks the capture to settle, while an execution cancellation reaches it
/// through its own execution claim instead.
pub struct CaptureSlot {
    limits: CaptureLimits,
    started: Instant,
    stop_requested: AtomicBool,
    progress: Mutex<CaptureProgress>,
}

impl CaptureSlot {
    fn new(limits: CaptureLimits) -> Self {
        Self {
            limits,
            started: Instant::now(),
            stop_requested: AtomicBool::new(false),
            progress: Mutex::new(CaptureProgress::default()),
        }
    }

    pub fn limits(&self) -> CaptureLimits {
        self.limits
    }

    pub fn elapsed_ms(&self) -> u64 {
        elapsed_ms(self.started)
    }

    pub fn progress(&self) -> CaptureProgress {
        self.progress.lock().map(|state| *state).unwrap_or_default()
    }

    pub fn stop_requested(&self) -> bool {
        self.stop_requested.load(Ordering::Acquire)
    }

    /// R-NET-004 makes stop a request for termination, and the settle that follows publishes
    /// what the capture holds before it stops, so a requested stop is not a cancellation.
    pub fn request_stop(&self) {
        self.stop_requested.store(true, Ordering::Release);
    }

    fn record(&self, packet_bytes: usize) {
        if let Ok(mut progress) = self.progress.lock() {
            progress.packets = progress.packets.saturating_add(1);
            progress.bytes = progress.bytes.saturating_add(packet_bytes as u64);
            progress.stream_bytes = progress
                .stream_bytes
                .saturating_add(PCAP_RECORD_HEADER_BYTES as u64)
                .saturating_add(packet_bytes as u64);
        }
    }
}

/// The daemon's only owner of its running captures (S-NET-003). One `capture_id` binds to one
/// capture, so a stop request and the capture it addresses meet through exactly one identity.
#[derive(Default)]
pub struct CaptureRegistry {
    running: Mutex<HashMap<String, Arc<CaptureSlot>>>,
}

impl CaptureRegistry {
    /// One capture identity, one running capture. A second registration is refused rather than
    /// aliasing two captures onto one identity.
    pub fn register(
        &self,
        capture_id: &CaptureId,
        limits: CaptureLimits,
    ) -> Result<Arc<CaptureSlot>, DomainError> {
        let mut running = self
            .running
            .lock()
            .map_err(|_| malformed("capture registry state is unavailable"))?;
        if running.contains_key(capture_id.as_str()) {
            return Err(DomainError::new(
                ErrorCode::CaptureFailed,
                "capture identity already owns a running capture",
            ));
        }
        let slot = Arc::new(CaptureSlot::new(limits));
        running.insert(capture_id.as_str().to_owned(), Arc::clone(&slot));
        Ok(slot)
    }

    /// The running capture one stop request addresses. An identity this daemon does not hold is
    /// `NOT_FOUND` (R-NET-004).
    pub fn running(&self, capture_id: &CaptureId) -> Result<Arc<CaptureSlot>, DomainError> {
        self.running
            .lock()
            .map_err(|_| malformed("capture registry state is unavailable"))?
            .get(capture_id.as_str())
            .cloned()
            .ok_or_else(|| {
                DomainError::new(
                    ErrorCode::NotFound,
                    "capture identity does not name a running capture",
                )
            })
    }

    fn retire(&self, capture_id: &CaptureId) {
        if let Ok(mut running) = self.running.lock() {
            running.remove(capture_id.as_str());
        }
    }
}

/// One classic-PCAP little-endian microsecond stream. The Runtime owns that format (S-NET-005),
/// so this only concatenates the headers the Runtime supplies.
#[derive(Default)]
struct PcapStream {
    bytes: Vec<u8>,
    records: u64,
}

impl PcapStream {
    fn new(link_type: u32) -> Self {
        Self {
            bytes: pcap_file_header(link_type).to_vec(),
            records: 0,
        }
    }

    fn push(&mut self, seconds: u32, microseconds: u32, original_len: u32, packet: &[u8]) {
        let captured = &packet[..packet.len().min(PCAP_SNAPLEN as usize)];
        self.bytes.extend_from_slice(&pcap_record_header(
            seconds,
            microseconds,
            captured.len() as u32,
            original_len,
        ));
        self.bytes.extend_from_slice(captured);
        self.records += 1;
    }
}

// ---------------------------------------------------------------------------------------
// The device seam under the capture registry.
// ---------------------------------------------------------------------------------------

/// One capture device step. `Timeout` is the device's own read window expiring with nothing to
/// report, which is what lets the capture loop re-check its bounds while it waits.
#[derive(Debug)]
pub enum CaptureStep {
    Packet {
        seconds: u32,
        microseconds: u32,
        original_len: u32,
        bytes: Vec<u8>,
    },
    Timeout,
    End,
    Failed,
}

/// The outcome of activating one capture device. A device whose link type the classic-PCAP
/// format cannot express is refused as its own fact: the caller must not report a device fault
/// for a link type it can name, and it must not write a file that mislabels its records.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureActivation {
    /// The device is active; its records carry this PCAP link type.
    Active(u32),
    /// The device's link type has no classic-PCAP representation.
    NoPcapLinkType,
}

/// One open capture device. The real implementation is the bundled static libpcap adapter and a
/// test supplies a scripted one, so every bound and settlement above the seam is exercised
/// without a device. This is not a fallback path: the Magisk host always installs the libpcap
/// adapter.
pub trait CaptureDevice: Send {
    fn activate(&mut self) -> Result<CaptureActivation, String>;
    fn next_packet(&mut self) -> CaptureStep;
    fn break_loop(&mut self);
    /// Sends one already-admitted packet and reports local send-path acceptance only.
    fn inject(&mut self, packet: &[u8]) -> Result<usize, String>;
    /// Closes the device. It is called exactly once per opened device, on every path.
    fn close(&mut self) -> Result<(), String>;
    /// The device's last error text, as libpcap's `pcap_geterr` reports it.
    fn error(&self) -> Option<String>;
}

pub trait CaptureBackend: Send + Sync {
    /// Opens one capture device without activating it, so a failed open has no side effect to
    /// clean up and activation stays a step the capture owner performs and can verify.
    fn open(&self, interface: &str, filter: Option<&str>)
    -> Result<Box<dyn CaptureDevice>, String>;
}

/// The capture device's own read window, in the units libpcap takes.
#[cfg(unix)]
const CAPTURE_READ_SLICE_MS: libc::c_int = 200;

// ---------------------------------------------------------------------------------------
// The libpcap adapter (S-NET-005). Every raw pointer and length of that C ABI lives inside this
// module and becomes bounded Rust-owned facts before it leaves.
// ---------------------------------------------------------------------------------------

/// The daemon's only capture backend: the one private adapter over the static libpcap it links
/// directly. It is an Android artifact, so it exists exactly where the daemon runs.
#[cfg(target_os = "android")]
pub use pcap_ffi::LibpcapBackend as MagiskCaptureBackend;

#[cfg(target_os = "android")]
mod pcap_ffi {
    use super::{
        CAPTURE_READ_SLICE_MS, CaptureActivation, CaptureBackend, CaptureDevice, CaptureStep,
        PCAP_SNAPLEN,
    };
    use runtime::{PCAP_LINKTYPE_ETHERNET, PCAP_LINKTYPE_RAW};
    use std::{
        ffi::{CStr, CString, c_char, c_int, c_uchar, c_uint, c_void},
        os::fd::RawFd,
        ptr,
    };

    /// `DLT_EN10MB` and `DLT_RAW`: the two link types the Runtime's PCAP file header can declare.
    /// `DLT_RAW` is one bare IPv4/IPv6 packet per record, which is what a TUN interface (a device
    /// running a VPN) reports; libpcap names that file link type `LINKTYPE_RAW`.
    const DLT_EN10MB: i32 = 1;
    const DLT_RAW: i32 = 12;
    const PCAP_ERRBUF_SIZE: usize = 256;

    #[repr(C)]
    struct PcapHandleOpaque {
        _private: [u8; 0],
    }

    #[repr(C)]
    struct BpfProgram {
        length: c_uint,
        instructions: *mut c_void,
    }

    #[repr(C)]
    struct PcapPacketHeader {
        timestamp: libc::timeval,
        captured_length: u32,
        original_length: u32,
    }

    unsafe extern "C" {
        fn pcap_create(source: *const c_char, errbuf: *mut c_char) -> *mut PcapHandleOpaque;
        fn pcap_set_snaplen(handle: *mut PcapHandleOpaque, snaplen: c_int) -> c_int;
        fn pcap_set_promisc(handle: *mut PcapHandleOpaque, promiscuous: c_int) -> c_int;
        fn pcap_set_timeout(handle: *mut PcapHandleOpaque, milliseconds: c_int) -> c_int;
        fn pcap_set_immediate_mode(handle: *mut PcapHandleOpaque, immediate: c_int) -> c_int;
        fn pcap_activate(handle: *mut PcapHandleOpaque) -> c_int;
        fn pcap_setnonblock(
            handle: *mut PcapHandleOpaque,
            nonblock: c_int,
            errbuf: *mut c_char,
        ) -> c_int;
        fn pcap_get_selectable_fd(handle: *mut PcapHandleOpaque) -> RawFd;
        fn pcap_datalink(handle: *mut PcapHandleOpaque) -> c_int;
        fn pcap_compile(
            handle: *mut PcapHandleOpaque,
            program: *mut BpfProgram,
            expression: *const c_char,
            optimize: c_int,
            netmask: u32,
        ) -> c_int;
        fn pcap_setfilter(handle: *mut PcapHandleOpaque, program: *mut BpfProgram) -> c_int;
        fn pcap_freecode(program: *mut BpfProgram);
        fn pcap_next_ex(
            handle: *mut PcapHandleOpaque,
            header: *mut *mut PcapPacketHeader,
            data: *mut *const c_uchar,
        ) -> c_int;
        fn pcap_breakloop(handle: *mut PcapHandleOpaque);
        fn pcap_inject(handle: *mut PcapHandleOpaque, bytes: *const c_void, length: usize)
        -> c_int;
        fn pcap_geterr(handle: *mut PcapHandleOpaque) -> *mut c_char;
        fn pcap_close(handle: *mut PcapHandleOpaque);
    }

    #[derive(Clone)]
    pub struct LibpcapBackend;

    impl CaptureBackend for LibpcapBackend {
        fn open(
            &self,
            interface: &str,
            filter: Option<&str>,
        ) -> Result<Box<dyn CaptureDevice>, String> {
            Ok(Box::new(PcapDevice::open(interface, filter)?))
        }
    }

    /// The one owner of one native handle. `close` and `drop` both take the handle, so the
    /// native device is released exactly once on every path.
    struct PcapDevice {
        handle: Option<*mut PcapHandleOpaque>,
        /// The activated handle's own descriptor, on which one read window is waited out.
        /// libpcap reports it as a negative value until the handle is activated.
        readable: RawFd,
        filter: Option<CString>,
        error: Option<String>,
    }

    // SAFETY: the handle belongs to the one thread that drives this device. `open` creates it
    // inside that thread, every method here is called from it, and the device moves to another
    // thread only as the boxed `CaptureDevice` the capture task owns, never shared between two.
    unsafe impl Send for PcapDevice {}

    impl PcapDevice {
        fn open(interface: &str, filter: Option<&str>) -> Result<Self, String> {
            let filter = filter
                .map(|filter| {
                    CString::new(filter)
                        .map_err(|_| "capture filter is not a valid C string".to_owned())
                })
                .transpose()?;
            let source = CString::new(interface)
                .map_err(|_| "capture interface is not a valid C string".to_owned())?;
            let mut errbuf = [0 as c_char; PCAP_ERRBUF_SIZE];
            let handle = unsafe { pcap_create(source.as_ptr(), errbuf.as_mut_ptr()) };
            if handle.is_null() {
                return Err(errbuf_text(&errbuf));
            }
            let device = Self {
                handle: Some(handle),
                readable: -1,
                filter,
                error: None,
            };
            // S-NET-003: non-promiscuous, snaplen 65,535. Immediate mode hands a packet over as
            // soon as the device delivers it, and the packet-buffer timeout is what the handle
            // states as its read window; on Linux that timeout does not time a read out while
            // immediate mode is set, which is why the wait itself happens on the descriptor
            // `activate` reports rather than inside a read.
            let settings = [
                ("snaplen", unsafe {
                    pcap_set_snaplen(handle, PCAP_SNAPLEN as c_int)
                }),
                ("promiscuous mode", unsafe { pcap_set_promisc(handle, 0) }),
                ("read window", unsafe {
                    pcap_set_timeout(handle, CAPTURE_READ_SLICE_MS)
                }),
                ("immediate mode", unsafe {
                    pcap_set_immediate_mode(handle, 1)
                }),
            ];
            for (name, result) in settings {
                if result != 0 {
                    // Returning here drops `device`, whose `Drop` releases the handle.
                    return Err(format!("capture device cannot set its {name}"));
                }
            }
            Ok(device)
        }

        fn handle(&self) -> Result<*mut PcapHandleOpaque, String> {
            self.handle
                .ok_or_else(|| "capture device is already closed".to_owned())
        }

        /// Records the device's own error text and returns it, so a failing step and the reason
        /// it failed are reported together.
        fn record_error(&mut self) -> String {
            let reason = self
                .handle
                .map(device_error)
                .unwrap_or_else(|| "capture device reported an error".to_owned());
            self.error = Some(reason.clone());
            reason
        }
    }

    impl CaptureDevice for PcapDevice {
        fn activate(&mut self) -> Result<CaptureActivation, String> {
            let handle = self.handle()?;
            if unsafe { pcap_activate(handle) } < 0 {
                return Err(self.record_error());
            }
            // Activation is what makes the handle readable within its own window, so a device
            // whose descriptor cannot bound a read is never handed to the capture loop: it
            // would hold that loop past its stop request and its time bound.
            let mut errbuf = [0 as c_char; PCAP_ERRBUF_SIZE];
            if unsafe { pcap_setnonblock(handle, 1, errbuf.as_mut_ptr()) } != 0 {
                return Err(self.record_error());
            }
            self.readable = unsafe { pcap_get_selectable_fd(handle) };
            if self.readable < 0 {
                return Err(self.record_error());
            }
            let link_type = match unsafe { pcap_datalink(handle) } {
                DLT_EN10MB => PCAP_LINKTYPE_ETHERNET,
                DLT_RAW => PCAP_LINKTYPE_RAW,
                other => return Err(format!("capture device reports link type {other}")),
            };
            let Some(filter) = self.filter.as_ref() else {
                return Ok(CaptureActivation::Active(link_type));
            };
            let mut program = BpfProgram {
                length: 0,
                instructions: ptr::null_mut(),
            };
            if unsafe { pcap_compile(handle, &mut program, filter.as_ptr(), 1, 0) } != 0 {
                return Err(self.record_error());
            }
            let applied = unsafe { pcap_setfilter(handle, &mut program) };
            unsafe { pcap_freecode(&mut program) };
            if applied != 0 {
                return Err(self.record_error());
            }
            Ok(CaptureActivation::Active(link_type))
        }

        fn next_packet(&mut self) -> CaptureStep {
            let Ok(handle) = self.handle() else {
                self.error = Some("capture device is already closed".to_owned());
                return CaptureStep::Failed;
            };
            // One read window is waited out here, where it can end: the read below takes what
            // the device has already delivered and never waits for a packet that may not come,
            // so every step of the capture loop returns within its window and the loop can
            // honour a stop request, its time bound and its limits between steps.
            let mut readable = libc::pollfd {
                fd: self.readable,
                events: libc::POLLIN,
                revents: 0,
            };
            let waited = unsafe { libc::poll(&mut readable, 1, CAPTURE_READ_SLICE_MS) };
            if waited == 0 {
                return CaptureStep::Timeout;
            }
            if waited < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    // An interrupted wait leaves the device untouched, so the next step retries.
                    return CaptureStep::Timeout;
                }
                self.error = Some("capture device read window cannot be waited on".to_owned());
                return CaptureStep::Failed;
            }
            if readable.revents & libc::POLLIN == 0 {
                self.error = Some(format!(
                    "capture device reported readiness {:#x} without readable data",
                    readable.revents
                ));
                return CaptureStep::Failed;
            }
            let mut header: *mut PcapPacketHeader = ptr::null_mut();
            let mut data: *const c_uchar = ptr::null();
            match unsafe { pcap_next_ex(handle, &mut header, &mut data) } {
                1 => {}
                0 => return CaptureStep::Timeout,
                // `PCAP_ERROR_BREAK`: the loop was broken, which is the end of the capture.
                -2 => return CaptureStep::End,
                _ => {
                    self.record_error();
                    return CaptureStep::Failed;
                }
            }
            if header.is_null() || data.is_null() {
                // libpcap reported a packet without a header, so no length can be trusted.
                self.error = Some("capture device reported a packet without a header".to_owned());
                return CaptureStep::Failed;
            }
            let header = unsafe { &*header };
            let length = header.captured_length as usize;
            let bytes = unsafe { std::slice::from_raw_parts(data, length) }.to_vec();
            CaptureStep::Packet {
                seconds: u32::try_from(header.timestamp.tv_sec).unwrap_or(u32::MAX),
                microseconds: u32::try_from(header.timestamp.tv_usec).unwrap_or_default(),
                original_len: header.original_length,
                bytes,
            }
        }

        fn break_loop(&mut self) {
            if let Some(handle) = self.handle {
                unsafe { pcap_breakloop(handle) };
            }
        }

        fn inject(&mut self, packet: &[u8]) -> Result<usize, String> {
            let handle = self.handle()?;
            let written =
                unsafe { pcap_inject(handle, packet.as_ptr().cast::<c_void>(), packet.len()) };
            if written < 0 {
                return Err(self.record_error());
            }
            Ok(written as usize)
        }

        fn close(&mut self) -> Result<(), String> {
            match self.handle.take() {
                Some(handle) => {
                    self.readable = -1;
                    unsafe { pcap_close(handle) };
                    Ok(())
                }
                None => Err("capture device is already closed".to_owned()),
            }
        }

        fn error(&self) -> Option<String> {
            self.error.clone()
        }
    }

    impl Drop for PcapDevice {
        fn drop(&mut self) {
            if let Some(handle) = self.handle.take() {
                unsafe { pcap_close(handle) };
            }
        }
    }

    fn errbuf_text(buffer: &[c_char; PCAP_ERRBUF_SIZE]) -> String {
        unsafe { CStr::from_ptr(buffer.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    }

    /// `pcap_geterr` returns a pointer into the handle's own error buffer, so the text is copied
    /// out before any further call.
    fn device_error(handle: *mut PcapHandleOpaque) -> String {
        let text = unsafe { pcap_geterr(handle) };
        if text.is_null() {
            return "capture device reported an error".to_owned();
        }
        unsafe { CStr::from_ptr(text) }
            .to_string_lossy()
            .into_owned()
    }
}

// ---------------------------------------------------------------------------------------
// The Magisk host's own observation source.
// ---------------------------------------------------------------------------------------

/// How long one `recv` on the bounded netlink socket waits before the dump reports itself
/// truncated.
#[cfg(unix)]
const NETLINK_TIMEOUT_SECONDS: libc::time_t = 5;
/// The bounded netlink reply buffer. A dump larger than this is reported as truncated.
#[cfg(unix)]
const NETLINK_REPLY_BYTES: usize = 262_144;
/// The bounded `/proc/net/<table>` read.
#[cfg(unix)]
const SOCKET_TABLE_BYTES: u64 = 8 * 1024 * 1024;

/// The App primitive that answers with the Android network facts this daemon cannot observe
/// itself, and the empty request it carries.
#[cfg(unix)]
const ANDROID_NETWORK_SNAPSHOT: &str = "AndroidNetworkSnapshot";
#[cfg(unix)]
const NETWORK_SNAPSHOT_REQUEST: &[u8] = b"{}";

/// The Magisk host's own observation source. Interfaces, addresses and routes are this daemon's
/// own netlink queries as S-NET-001 fixes them, sockets are its own `/proc/net` reads, and DNS
/// is the authenticated companion's `LinkProperties` answer. Shizuku is never consulted for a
/// family already assigned to Magisk.
#[cfg(unix)]
#[derive(Clone)]
pub struct MagiskNetworkSource {
    companion: crate::companion::CompanionPort,
}

#[cfg(unix)]
impl MagiskNetworkSource {
    pub fn new(companion: crate::companion::CompanionPort) -> Self {
        Self { companion }
    }

    /// One `RTM_GETLINK`/`RTM_GETADDR`/`RTM_GETROUTE` dump. Every raw syscall pointer stays
    /// inside this function; its result is bounded Rust-owned bytes plus the completeness fact
    /// the caller reports.
    fn dump(kind: u16) -> Result<SourceDump, DomainError> {
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

        let descriptor = unsafe {
            libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_RAW | libc::SOCK_CLOEXEC,
                libc::NETLINK_ROUTE,
            )
        };
        if descriptor < 0 {
            return Err(io_error("daemon netlink socket is unavailable"));
        }
        let socket = unsafe { OwnedFd::from_raw_fd(descriptor) };
        let raw = socket.as_raw_fd();
        let window = libc::timeval {
            tv_sec: NETLINK_TIMEOUT_SECONDS,
            tv_usec: 0,
        };
        if unsafe {
            libc::setsockopt(
                raw,
                libc::SOL_SOCKET,
                libc::SO_RCVTIMEO,
                std::ptr::from_ref(&window).cast(),
                std::mem::size_of::<libc::timeval>() as libc::socklen_t,
            )
        } != 0
        {
            return Err(io_error("daemon netlink socket cannot be bounded"));
        }
        // The kernel only reads the family of a netlink bind address; its padding is not a field
        // this daemon sets, so the address starts zeroed.
        let mut address: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        address.nl_family = libc::AF_NETLINK as u16;
        if unsafe {
            libc::bind(
                raw,
                std::ptr::from_mut(&mut address).cast(),
                std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
            )
        } != 0
        {
            return Err(io_error("daemon netlink socket cannot be bound"));
        }
        let request = netlink_dump_request(kind);
        if unsafe { libc::send(raw, request.as_ptr().cast(), request.len(), 0) } < 0 {
            return Err(io_error("daemon netlink request cannot be sent"));
        }
        let mut bytes = vec![0u8; NETLINK_REPLY_BYTES];
        let mut filled = 0usize;
        let mut complete = false;
        while filled < bytes.len() && !complete {
            let count = unsafe {
                libc::recv(
                    raw,
                    bytes[filled..].as_mut_ptr().cast(),
                    bytes.len() - filled,
                    0,
                )
            };
            if count < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) {
                    break;
                }
                return Err(io_error("daemon netlink reply cannot be read"));
            }
            if count == 0 {
                break;
            }
            filled += count as usize;
            complete = reply_is_complete(&bytes[..filled]);
        }
        if filled == 0 {
            // No reply at all establishes nothing, which is not an empty truncated dump.
            return Err(io_error("daemon netlink dump was not answered"));
        }
        bytes.truncate(filled);
        Ok(SourceDump {
            bytes,
            truncated: !complete,
        })
    }

    /// `/proc/net/<table>`. A table this host cannot see leaves its family unestablished rather
    /// than reporting an empty one.
    fn table(name: &str) -> Result<Option<String>, DomainError> {
        let mut text = String::new();
        match fs::File::open(format!("/proc/net/{name}")) {
            Ok(file) => {
                file.take(SOCKET_TABLE_BYTES)
                    .read_to_string(&mut text)
                    .map_err(|_| io_error("daemon socket table cannot be read"))?;
                Ok(Some(text))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(io_error("daemon socket table cannot be read")),
        }
    }
}

/// Projects the daemon's directly observed preferred default route into the event tuple.
/// The route's interface name is the native source identity; transport is omitted because
/// netlink does not establish Android's transport classification.
pub fn native_default_event_from_routes(
    routes: &[RouteEntry],
) -> Result<NetworkDefaultChangedEvent, DomainError> {
    let defaults = routes
        .iter()
        .filter(|route| route_prefix(&route.destination) == Some(0))
        .collect::<Vec<_>>();
    if defaults.is_empty() {
        return Ok(NetworkDefaultChangedEvent::new(None, None));
    }
    let selected = defaults
        .into_iter()
        .filter_map(|route| {
            route
                .interface
                .as_ref()
                .map(|interface| (route.metric.unwrap_or(u64::MAX), interface))
        })
        .min_by(|left, right| left.cmp(right))
        .ok_or_else(|| io_error("default route interface is unavailable"))?;
    Ok(NetworkDefaultChangedEvent::new(
        Some(selected.1.clone()),
        None,
    ))
}

#[cfg(unix)]
fn observe_native_default() -> Result<NetworkDefaultChangedEvent, DomainError> {
    let links = MagiskNetworkSource::dump(RTM_GETLINK)?;
    let addresses = MagiskNetworkSource::dump(RTM_GETADDR)?;
    let routes = MagiskNetworkSource::dump(RTM_GETROUTE)?;
    if links.truncated || addresses.truncated || routes.truncated {
        return Err(io_error("native default route observation is incomplete"));
    }
    let links = link_facts(&links.bytes, &addresses.bytes)?;
    native_default_event_from_routes(&route_entries(&routes.bytes, &links)?)
}

#[cfg(unix)]
fn open_route_observer() -> Result<OwnedFd, DomainError> {
    let descriptor = unsafe {
        libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_RAW | libc::SOCK_CLOEXEC,
            libc::NETLINK_ROUTE,
        )
    };
    if descriptor < 0 {
        return Err(io_error("daemon route observer socket is unavailable"));
    }
    let socket = unsafe { OwnedFd::from_raw_fd(descriptor) };
    let mut address: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
    address.nl_family = libc::AF_NETLINK as u16;
    address.nl_groups = RTMGRP_LINK | RTMGRP_IPV4_ROUTE | RTMGRP_IPV6_ROUTE;
    if unsafe {
        libc::bind(
            socket.as_raw_fd(),
            std::ptr::from_mut(&mut address).cast(),
            std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
        )
    } != 0
    {
        return Err(io_error("daemon route observer cannot be bound"));
    }
    Ok(socket)
}

#[cfg(unix)]
fn run_route_observer(socket: OwnedFd, mut stop: UnixStream, ingress: NetworkDefaultEventIngress) {
    let mut notification = [0u8; 8192];
    let mut descriptors = [
        libc::pollfd {
            fd: socket.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: stop.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    loop {
        let ready = unsafe { libc::poll(descriptors.as_mut_ptr(), descriptors.len() as _, -1) };
        if ready < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return;
        }
        if descriptors[1].revents != 0 {
            let mut byte = [0u8; 1];
            let _ = stop.read(&mut byte);
            return;
        }
        if descriptors[0].revents == 0 {
            continue;
        }
        let count = unsafe {
            libc::recv(
                socket.as_raw_fd(),
                notification.as_mut_ptr().cast(),
                notification.len(),
                0,
            )
        };
        if count < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        if count <= 0 {
            return;
        }
        if let Ok(observed) = observe_native_default() {
            let _ = ingress.observe(observed);
        }
    }
}

#[cfg(unix)]
struct NativeRouteObserver {
    stop: UnixStream,
    thread: JoinHandle<()>,
}

/// The Magisk Runtime fallback when no authenticated APK companion owns the Android callback.
/// One netlink subscription watches link and v4/v6 route groups; each notification triggers a
/// bounded fresh dump, while the shared event plane owns baseline, equality and saturation.
#[derive(Default)]
pub struct NativeNetworkDefaultEventSource {
    #[cfg(unix)]
    observer: Mutex<Option<(NetworkDefaultSourceRegistration, NativeRouteObserver)>>,
}

impl NetworkDefaultEventSource for NativeNetworkDefaultEventSource {
    fn start(
        &self,
        registration: &NetworkDefaultSourceRegistration,
        ingress: NetworkDefaultEventIngress,
    ) -> Result<(), DomainError> {
        #[cfg(not(unix))]
        {
            let _ = (registration, ingress);
            Err(DomainError::new(
                ErrorCode::CapabilityUnavailable,
                "native route observer is unavailable",
            ))
        }
        #[cfg(unix)]
        {
            let mut observer = self
                .observer
                .lock()
                .map_err(|_| io_error("native route observer state is unavailable"))?;
            if observer.is_some() {
                return Err(DomainError::new(
                    ErrorCode::AlreadyExists,
                    "native route observer already exists",
                ));
            }
            let socket = open_route_observer()?;
            ingress.observe(observe_native_default()?)?;
            let (stop_owner, stop_thread) = UnixStream::pair()
                .map_err(|_| io_error("native route observer stop channel is unavailable"))?;
            let thread = std::thread::Builder::new()
                .name("droidbridge-network-default".to_owned())
                .spawn(move || run_route_observer(socket, stop_thread, ingress))
                .map_err(|_| io_error("native route observer thread is unavailable"))?;
            *observer = Some((
                registration.clone(),
                NativeRouteObserver {
                    stop: stop_owner,
                    thread,
                },
            ));
            Ok(())
        }
    }

    fn stop(&self, registration: &NetworkDefaultSourceRegistration) -> Result<(), DomainError> {
        #[cfg(not(unix))]
        {
            let _ = registration;
            Ok(())
        }
        #[cfg(unix)]
        {
            let owned = {
                let mut observer = self
                    .observer
                    .lock()
                    .map_err(|_| io_error("native route observer state is unavailable"))?;
                match observer.as_ref() {
                    None => return Ok(()),
                    Some((active, _)) if active != registration => {
                        return Err(DomainError::new(
                            ErrorCode::StaleAuthority,
                            "native route observer generation is stale",
                        ));
                    }
                    Some(_) => observer.take().expect("checked native route observer"),
                }
            };
            let (_, mut owned) = owned;
            let _ = owned.stop.write_all(&[1]);
            owned
                .thread
                .join()
                .map_err(|_| io_error("native route observer cleanup failed"))?;
            Ok(())
        }
    }
}

#[cfg(unix)]
const RTMGRP_LINK: u32 = 0x1;
#[cfg(unix)]
const RTMGRP_IPV4_ROUTE: u32 = 0x40;
#[cfg(unix)]
const RTMGRP_IPV6_ROUTE: u32 = 0x400;

#[cfg(unix)]
impl NetworkHostSource for MagiskNetworkSource {
    fn link_dump(&self) -> Result<SourceDump, DomainError> {
        Self::dump(RTM_GETLINK)
    }

    fn address_dump(&self) -> Result<SourceDump, DomainError> {
        Self::dump(RTM_GETADDR)
    }

    fn route_dump(&self) -> Result<SourceDump, DomainError> {
        Self::dump(RTM_GETROUTE)
    }

    fn socket_text(&self, table: &str) -> Result<Option<String>, DomainError> {
        Self::table(table)
    }

    /// S-NET-001 assigns `dns` to Android's `LinkProperties.getDnsServers()` while the APK
    /// companion exists, so the daemon asks the companion it is already authenticated with,
    /// through the same S-ANDROID-001 primitive the APK surface serves and under the fence of
    /// the execution this observe belongs to. A companion that cannot answer, or an answer this
    /// host cannot read, leaves the family unestablished.
    fn dns_servers(
        &self,
        execution: &AdmittedExecution,
    ) -> Result<Option<Vec<String>>, DomainError> {
        let Ok(answer) = self.companion.dispatch(
            ANDROID_NETWORK_SNAPSHOT,
            NETWORK_SNAPSHOT_REQUEST,
            execution,
        ) else {
            return Ok(None);
        };
        let Ok(reply) = serde_json::from_slice::<serde_json::Value>(&answer.payload) else {
            return Ok(None);
        };
        Ok(dns_servers_from_snapshot(&reply))
    }
}

/// The companion's `LinkProperties` DNS servers, in the `{"dns":[{"server":"..."}]}` reply shape
/// the APK surface encodes. A snapshot without a usable `dns` array, or with an entry that
/// carries no server, leaves the family unestablished rather than reporting a shortened one.
pub fn dns_servers_from_snapshot(payload: &serde_json::Value) -> Option<Vec<String>> {
    payload
        .get("dns")?
        .as_array()?
        .iter()
        .map(|entry| entry.get("server")?.as_str().map(str::to_owned))
        .collect()
}

/// One dump request: the netlink header followed by the zeroed family header its kind reads.
/// rtnetlink silently drops a message shorter than that family header, so a header-only
/// request would never be answered. A zeroed family header asks for every address family.
#[cfg(unix)]
fn netlink_dump_request(kind: u16) -> Vec<u8> {
    let family_header = match kind {
        RTM_GETLINK => IFINFOMSG_BYTES,
        RTM_GETADDR => IFADDRMSG_BYTES,
        _ => RTMSG_BYTES,
    };
    let length = NLMSG_HEADER_BYTES + family_header;
    let mut request = Vec::with_capacity(length);
    request.extend_from_slice(&(length as u32).to_ne_bytes());
    request.extend_from_slice(&kind.to_ne_bytes());
    request.extend_from_slice(&(NLM_F_REQUEST | NLM_F_DUMP).to_ne_bytes());
    request.extend_from_slice(&0_u32.to_ne_bytes());
    request.extend_from_slice(&0_u32.to_ne_bytes());
    request.resize(length, 0);
    request
}

#[cfg(unix)]
const NLM_F_REQUEST: u16 = 0x1;
#[cfg(unix)]
const NLM_F_DUMP: u16 = 0x300;

// ---------------------------------------------------------------------------------------
// The port.
// ---------------------------------------------------------------------------------------

/// The Magisk execution surface's Network primitive port. It owns every field family's
/// provider and the capture registry. Default-network events use the separate S-NET-006
/// source and event plane rather than an inspect side effect.
#[derive(Clone)]
pub struct NativeNetworkPort<S, B, A> {
    source: S,
    backend: B,
    artifacts: A,
    captures: Arc<CaptureRegistry>,
}

impl<S, B, A> NativeNetworkPort<S, B, A> {
    pub fn new(source: S, backend: B, artifacts: A) -> Self {
        Self {
            source,
            backend,
            artifacts,
            captures: Arc::new(CaptureRegistry::default()),
        }
    }
}

impl<S, B, A> NetworkPrimitivePort for NativeNetworkPort<S, B, A>
where
    S: NetworkHostSource,
    B: CaptureBackend,
    A: ArtifactPort,
{
    fn run(
        &self,
        execution: &AdmittedExecution,
        request: NetworkPrimitiveRequest,
        claim: &LocalExecutionClaim,
    ) -> Result<NetworkPrimitiveSettlement, ExecutionFailure> {
        let _ = execution;
        let outcome = match request {
            NetworkPrimitiveRequest::Inspect { plan, scope, .. } => {
                NetworkPrimitiveOutcome::Inspect(self.inspect(execution, plan, scope))
            }
            NetworkPrimitiveRequest::Diagnose(input, _) => {
                NetworkPrimitiveOutcome::Diagnose(self.diagnose(execution, input)?)
            }
            NetworkPrimitiveRequest::CaptureStart {
                capture_id,
                interface,
                filter,
                max_packets,
                max_bytes,
                max_duration_ms,
                persist_to,
            } => NetworkPrimitiveOutcome::CaptureSettled(self.capture(
                &capture_id,
                &interface,
                filter.as_deref(),
                CaptureLimits {
                    max_packets,
                    max_bytes,
                    max_duration_ms,
                },
                persist_to.as_ref(),
                claim,
            )?),
            NetworkPrimitiveRequest::CaptureStop { capture_id } => {
                self.captures
                    .running(&capture_id)
                    .map_err(pre_start)?
                    .request_stop();
                NetworkPrimitiveOutcome::CaptureStopRequested
            }
            NetworkPrimitiveRequest::CaptureFileBytes { target } => {
                NetworkPrimitiveOutcome::CaptureFileBytes(capture_file_bytes(&target)?)
            }
            NetworkPrimitiveRequest::PacketInject {
                interface,
                packet,
                count,
                interval_ms,
            } => NetworkPrimitiveOutcome::PacketInjected(self.inject(
                &interface,
                &packet,
                count,
                interval_ms,
                claim,
            )?),
        };
        Ok(NetworkPrimitiveSettlement {
            outcome,
            cleanup_verified: claim.cleanup_is_verified(),
        })
    }
}

impl<S: NetworkHostSource, B, A: ArtifactPort> NativeNetworkPort<S, B, A> {
    /// R-NET-002. Every requested family whose plan assigns this host a provider is observed; a
    /// family the plan leaves unavailable, or a provider that could not establish its fact,
    /// contributes `None`, which the Runtime reports as `unavailable` or `unknown` with the data
    /// omitted.
    fn inspect(
        &self,
        execution: &AdmittedExecution,
        plan: NetworkSourcePlan,
        scope: NetworkScope,
    ) -> NetworkInspectSettlement {
        let mut settlement = NetworkInspectSettlement::default();
        for family in scope_families(scope) {
            if !matches!(plan.family(*family), NetworkFamilyPlan::Source(_)) {
                continue;
            }
            match family {
                NetworkFamily::Interfaces => settlement.interfaces = self.interfaces(),
                NetworkFamily::Routes => settlement.routes = self.routes(),
                NetworkFamily::Dns => settlement.dns = self.dns(execution),
                NetworkFamily::Sockets => settlement.sockets = self.sockets(),
            }
        }
        settlement
    }

    fn interfaces(&self) -> Option<Established<InterfaceEntry>> {
        let links = self.source.link_dump().ok()?;
        let addresses = self.source.address_dump().ok()?;
        let facts = link_facts(
            complete_prefix(&links.bytes),
            complete_prefix(&addresses.bytes),
        )
        .ok()?;
        Some(Established {
            entries: interface_entries(&facts),
            truncated: links.truncated || addresses.truncated,
        })
    }

    fn routes(&self) -> Option<Established<RouteEntry>> {
        let reply = self.source.route_dump().ok()?;
        let links = self.source.link_dump().ok()?;
        let facts = link_facts(complete_prefix(&links.bytes), &[]).ok()?;
        let entries = route_entries(complete_prefix(&reply.bytes), &facts).ok()?;
        Some(Established {
            entries,
            truncated: reply.truncated,
        })
    }

    fn dns(&self, execution: &AdmittedExecution) -> Option<Established<DnsEntry>> {
        let servers = self.source.dns_servers(execution).ok()??;
        Some(Established {
            entries: servers
                .into_iter()
                .map(|server| DnsEntry { server })
                .collect(),
            truncated: false,
        })
    }

    /// S-NET-001's socket family is the union of the four tables. A table this host cannot read
    /// leaves the union incomplete rather than silently shortened, so the family is
    /// unestablished only when no table at all could be read.
    fn sockets(&self) -> Option<Established<SocketEntry>> {
        let mut entries = Vec::new();
        let mut read_any = false;
        let mut incomplete = false;
        for table in SOCKET_TABLES {
            match self.source.socket_text(table) {
                Ok(Some(text)) => {
                    read_any = true;
                    entries.extend(parse_sockets(table, &text));
                }
                Ok(None) | Err(_) => incomplete = true,
            }
        }
        read_any.then_some(Established {
            entries,
            truncated: incomplete,
        })
    }
}

impl<S, B, A> NativeNetworkPort<S, B, A>
where
    S: NetworkHostSource,
    B: CaptureBackend,
    A: ArtifactPort,
{
    /// R-NET-009. `connectivity` and `route` consume this host's own observed facts; `dns`,
    /// `tcp` and `tls` are the one shared blocking probe implementation the Runtime owns,
    /// executed here in `droidbridged`, which is what S-NET-002 fixes as this host's diagnose
    /// path.
    fn diagnose(
        &self,
        execution: &AdmittedExecution,
        input: NetworkDiagnoseInput,
    ) -> Result<NetworkDiagnoseResult, ExecutionFailure> {
        match input {
            NetworkDiagnoseInput::Connectivity {} => self.connectivity(execution),
            NetworkDiagnoseInput::Dns { name, record_type } => {
                Ok(network_dns_probe(&name, record_type))
            }
            NetworkDiagnoseInput::Tcp {
                host,
                port,
                timeout_ms,
            } => Ok(network_tcp_probe(&host, port, timeout_ms)),
            NetworkDiagnoseInput::Tls {
                host,
                port,
                server_name,
                timeout_ms,
            } => network_tls_probe(&host, port, server_name.as_deref(), timeout_ms)
                .map_err(pre_start),
            NetworkDiagnoseInput::Route { destination_ip } => self.route(&destination_ip),
        }
    }

    fn observed_links(&self) -> Result<Vec<LinkFacts>, ExecutionFailure> {
        let links = self.source.link_dump().map_err(pre_start)?;
        let addresses = self.source.address_dump().map_err(pre_start)?;
        link_facts(
            complete_prefix(&links.bytes),
            complete_prefix(&addresses.bytes),
        )
        .map_err(pre_start)
    }

    fn observed_routes(&self, links: &[LinkFacts]) -> Result<Vec<RouteEntry>, ExecutionFailure> {
        let reply = self.source.route_dump().map_err(pre_start)?;
        route_entries(complete_prefix(&reply.bytes), links).map_err(pre_start)
    }

    /// R-NET-009 `connectivity` consumes exactly this host's own observed facts: the links that
    /// are up, are not loopback and carry at least one address establish
    /// `active_network_present`; the routing table's default route establishes
    /// `default_route_present`; and the companion's `LinkProperties` answer establishes
    /// `dns_configured`, which is omitted while no companion can answer. A probe that ran is
    /// `completed`, so a negative network outcome stays an outcome rather than an error.
    fn connectivity(
        &self,
        execution: &AdmittedExecution,
    ) -> Result<NetworkDiagnoseResult, ExecutionFailure> {
        let started = Instant::now();
        let links = self.observed_links()?;
        let routes = self.observed_routes(&links)?;
        let active = links
            .iter()
            .any(|link| link.up && !link.loopback && !link.addresses.is_empty());
        let default_route = routes
            .iter()
            .any(|route| route_prefix(&route.destination) == Some(0));
        let dns_configured = self
            .source
            .dns_servers(execution)
            .ok()
            .flatten()
            .map(|servers| !servers.is_empty());
        let outcome = if default_route {
            DiagnosticOutcome::Success
        } else if active {
            // An interface is up but the routing table holds no way out.
            DiagnosticOutcome::NoRoute
        } else {
            DiagnosticOutcome::Unreachable
        };
        Ok(NetworkDiagnoseResult::Connectivity {
            outcome,
            duration_ms: elapsed_ms(started),
            active_network_present: Some(active),
            default_route_present: Some(default_route),
            dns_configured,
        })
    }

    /// R-NET-009 `route` resolves the destination against this host's own routing table by
    /// longest-prefix match, which is the match the kernel itself would make. A destination no
    /// route covers is `no_route` rather than an error.
    fn route(&self, destination_ip: &str) -> Result<NetworkDiagnoseResult, ExecutionFailure> {
        let started = Instant::now();
        let target = destination_ip.parse::<IpAddr>().map_err(|_| {
            pre_start(DomainError::invalid(
                "network.diagnose route destination_ip is not an IP address",
            ))
        })?;
        let links = self.observed_links()?;
        let routes = self.observed_routes(&links)?;
        let matched = routes
            .iter()
            .filter(|route| route_contains(&route.destination, target))
            .max_by_key(|route| route_prefix(&route.destination));
        let (outcome, interface, gateway) = match matched {
            Some(route) => (
                DiagnosticOutcome::Success,
                route.interface.clone(),
                route.gateway.clone(),
            ),
            None => (DiagnosticOutcome::NoRoute, None, None),
        };
        Ok(NetworkDiagnoseResult::Route {
            outcome,
            duration_ms: elapsed_ms(started),
            destination_ip: destination_ip.to_owned(),
            interface,
            gateway,
        })
    }

    /// S-NET-003. The capture Task owns the device for this call's whole lifetime and closes it
    /// on every path, and the terminal settlement publishes what the capture holds: a requested
    /// stop settles the capture, while a cancelled execution publishes nothing.
    fn capture(
        &self,
        capture_id: &CaptureId,
        interface: &str,
        filter: Option<&str>,
        limits: CaptureLimits,
        persist_to: Option<&FileTarget>,
        claim: &LocalExecutionClaim,
    ) -> Result<CaptureSettlement, ExecutionFailure> {
        let slot = self
            .captures
            .register(capture_id, limits)
            .map_err(pre_start)?;
        let outcome = self.run_capture(&slot, interface, filter, claim);
        self.captures.retire(capture_id);
        let (cancelled, stream) = outcome?;
        let progress = slot.progress();
        let (capture_ref, destination) = match (cancelled, stream) {
            // A capture that settled without a cancellation publishes the stream it settled with,
            // whether or not the device reported a packet: what a zero-record capture holds is
            // still a complete classic-PCAP file — its header alone, carrying the device's own
            // link type — so `capture_ref` and `destination` describe it like any other. A
            // cancelled execution publishes nothing.
            (false, Some(stream)) => {
                let bytes = stream.bytes;
                let metadata = claim
                    .publish(|| self.artifacts.publish_as("capture", &bytes))
                    .map_err(|error| ExecutionFailure {
                        error,
                        cleanup_verified: claim.cleanup_is_verified(),
                    })?;
                let destination = persist_to
                    .map(|target| persist_capture(target, capture_id, &bytes))
                    .transpose()
                    .map_err(|error| ExecutionFailure {
                        error,
                        cleanup_verified: claim.cleanup_is_verified(),
                    })?;
                (Some(metadata.artifact_ref), destination)
            }
            _ => (None, None),
        };
        Ok(CaptureSettlement {
            cancelled,
            packets_captured: progress.packets,
            bytes_captured: progress.bytes,
            capture_ref,
            destination,
            cleanup_verified: claim.cleanup_is_verified(),
        })
    }

    /// `Ok((cancelled, stream))`. A device failure is reported with the device's own error text,
    /// and the device is closed exactly once on every path.
    #[allow(clippy::type_complexity)]
    fn run_capture(
        &self,
        slot: &Arc<CaptureSlot>,
        interface: &str,
        filter: Option<&str>,
        claim: &LocalExecutionClaim,
    ) -> Result<(bool, Option<PcapStream>), ExecutionFailure> {
        let limits = slot.limits();
        let mut device = match self.backend.open(interface, filter) {
            Ok(device) => device,
            Err(_) => return Err(device_failure("capture device cannot be opened")),
        };
        let outcome = match device.activate() {
            Ok(CaptureActivation::Active(link_type)) => {
                self.read_device(slot, limits, device.as_mut(), claim, link_type)
            }
            Ok(CaptureActivation::NoPcapLinkType) => Err(failure(
                ErrorCode::Unsupported,
                "capture interface link type has no classic-PCAP representation",
                claim,
            )),
            Err(_) => Err(device_failure("capture device cannot be activated")),
        };
        if device.close().is_err() {
            claim.mark_cleanup_unverified();
        }
        // A settled failure reports the cleanup fact as it stands after the close, never the value
        // read before it.
        outcome.map_err(|mut failed| {
            failed.cleanup_verified = claim.cleanup_is_verified();
            failed
        })
    }

    #[allow(clippy::type_complexity)]
    fn read_device(
        &self,
        slot: &CaptureSlot,
        limits: CaptureLimits,
        device: &mut dyn CaptureDevice,
        claim: &LocalExecutionClaim,
        link_type: u32,
    ) -> Result<(bool, Option<PcapStream>), ExecutionFailure> {
        let mut stream = PcapStream::new(link_type);
        loop {
            // A cancellation is the execution owner's decision, and it publishes nothing.
            if claim.checkpoint().is_err() {
                device.break_loop();
                return Ok((true, None));
            }
            if slot.stop_requested()
                || slot.elapsed_ms() >= limits.max_duration_ms
                || !slot.progress().admits(limits, 0)
            {
                return Ok((false, Some(stream)));
            }
            match device.next_packet() {
                CaptureStep::Packet {
                    seconds,
                    microseconds,
                    original_len,
                    bytes,
                } => {
                    // The bound stops the capture at the first packet that would exceed it, so
                    // the stream never grows past the bound either.
                    if !slot.progress().admits(limits, bytes.len()) {
                        return Ok((false, Some(stream)));
                    }
                    stream.push(seconds, microseconds, original_len, &bytes);
                    slot.record(bytes.len());
                }
                CaptureStep::Timeout => {}
                CaptureStep::End => return Ok((false, Some(stream))),
                CaptureStep::Failed => {
                    return Err(device_failure("capture device failed to read a packet"));
                }
            }
        }
    }

    /// R-NET-008. Each requested repetition is one `pcap_inject` of the already-admitted bytes,
    /// and the result reports local send-path acceptance only. A cancellation stops the remaining
    /// repetitions and reports itself as one, and a device failure is a structured error rather
    /// than a partially accepted success.
    fn inject(
        &self,
        interface: &str,
        packet: &[u8],
        count: u32,
        interval_ms: u64,
        claim: &LocalExecutionClaim,
    ) -> Result<PacketInjectResult, ExecutionFailure> {
        let mut device = self
            .backend
            .open(interface, None)
            .map_err(|_| device_failure("capture device cannot be opened"))?;
        // A capture file describes its own link type, while injection writes the caller's bytes
        // into the device's framing, so injection stays on the one framing those bytes are
        // admitted for (S-NET-004).
        let outcome = match device.activate() {
            Ok(CaptureActivation::Active(PCAP_LINKTYPE_ETHERNET)) => {
                self.send_repetitions(device.as_mut(), packet, count, interval_ms, claim)
            }
            Ok(_) => Err(failure(
                ErrorCode::Unsupported,
                "packet injection requires an Ethernet interface",
                claim,
            )),
            Err(_) => Err(device_failure("capture device cannot be activated")),
        };
        if device.close().is_err() {
            claim.mark_cleanup_unverified();
        }
        // Same rule as a capture: the reported cleanup fact is read after the close.
        outcome.map_err(|mut failed| {
            failed.cleanup_verified = claim.cleanup_is_verified();
            failed
        })
    }

    fn send_repetitions(
        &self,
        device: &mut dyn CaptureDevice,
        packet: &[u8],
        count: u32,
        interval_ms: u64,
        claim: &LocalExecutionClaim,
    ) -> Result<PacketInjectResult, ExecutionFailure> {
        let mut accepted = 0u64;
        let mut bytes_accepted = 0u64;
        for repetition in 0..count {
            if claim.checkpoint().is_err() {
                return Err(ExecutionFailure {
                    error: DomainError::new(ErrorCode::Cancelled, "execution was cancelled"),
                    cleanup_verified: claim.cleanup_is_verified(),
                });
            }
            if repetition > 0 && interval_ms > 0 {
                std::thread::sleep(Duration::from_millis(interval_ms));
            }
            let written = device
                .inject(packet)
                .map_err(|_| device_failure("capture device cannot send a packet"))?;
            accepted += 1;
            bytes_accepted = bytes_accepted.saturating_add(written as u64);
        }
        Ok(PacketInjectResult {
            requested_packets: u64::from(count),
            accepted_packets: accepted,
            bytes_accepted,
        })
    }
}

/// R-NET-005 `capture.read` of a caller-named file. The Runtime owns the PCAP format, so the
/// host supplies only the bounded bytes. A content-URI target names a provider this daemon has
/// no resolver for, which is a structured unavailability rather than a wrong read.
fn capture_file_bytes(target: &FileTarget) -> Result<Vec<u8>, ExecutionFailure> {
    if target.target_type != FileTargetType::Path {
        return Err(pre_start(DomainError::new(
            ErrorCode::CapabilityUnavailable,
            "the daemon has no content-resolver capability for a capture file",
        )));
    }
    let mut bytes = Vec::new();
    fs::File::open(&target.value)
        .map_err(|_| pre_start(io_error("capture file cannot be opened")))?
        .take(NETWORK_MAX_CAPTURE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| pre_start(io_error("capture file cannot be read")))?;
    if bytes.len() as u64 > NETWORK_MAX_CAPTURE_BYTES {
        return Err(pre_start(DomainError::new(
            ErrorCode::ResourceLimit,
            "capture file exceeds the capture-artifact bound",
        )));
    }
    Ok(bytes)
}

/// R-NET-003's `persist_to`. The stream is written beside its destination, fsynced and renamed
/// into place, so a partially written capture never appears at the caller's path.
fn persist_capture(
    target: &FileTarget,
    capture_id: &CaptureId,
    bytes: &[u8],
) -> Result<FileTarget, DomainError> {
    if target.target_type != FileTargetType::Path {
        return Err(DomainError::invalid(
            "network.capture persist_to requires a path target",
        ));
    }
    let destination = Path::new(&target.value);
    let directory = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| DomainError::invalid("network.capture persist_to has no directory"))?;
    if !directory.is_dir() {
        return Err(io_error("capture destination directory does not exist"));
    }
    let temporary = directory.join(format!(".{}.pcap.tmp", capture_id.as_str()));
    let written = (|| -> std::io::Result<()> {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()
    })();
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
        return Err(io_error("capture destination write failed"));
    }
    if fs::rename(&temporary, destination).is_err() {
        let _ = fs::remove_file(&temporary);
        return Err(io_error("capture destination rename failed"));
    }
    sync_directory(directory)?;
    Ok(target.clone())
}

/// A directory-entry rename is only durable once the directory itself is synced.
#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), DomainError> {
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| io_error("capture destination directory cannot be synced"))
}

/// No target outside unix runs this daemon, and none of them keeps a directory-entry sync; the
/// rename itself stays the ordering point there.
#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), DomainError> {
    Ok(())
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}
