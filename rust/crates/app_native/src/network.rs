//! The APK host's Network primitive (S-NET-001, S-NET-002, S-NET-004).
//!
//! S-NET-001 assigns every `network.inspect` field family to one provider before the
//! request runs, and the Runtime delivers that assignment as `NetworkSourcePlan`. This
//! port honours the delivered plan instead of re-deciding it: an `AppFramework` family
//! comes from this process's own `getifaddrs` view plus the `AndroidNetworkSnapshot`
//! companion primitive, a `ShizukuSupplement` family comes from the read-only procfs
//! supplement this host already reaches through the Shizuku filesystem primitive, and a
//! family the plan assigns to the daemon is left to its owner. A family whose assigned
//! provider cannot be consulted stays unresolved (`None`), which R-NET-002 reports as
//! `unknown` rather than as an empty array.
//!
//! `network.diagnose` runs in this process for the App/framework path, so DNS, TCP and TLS
//! call the Runtime's single shared R-NET-009 probes; `connectivity` and `route` are
//! answered from the App/framework and read-only Shizuku facts the same sources establish.
//!
//! Raw capture and injection stay refused here with the Magisk-host error, because
//! S-AUTH-NET-001 never admits them for this host and S-NET-005 allows only one capture
//! abstraction. The one capture fact this host supplies is the bytes of a caller-named
//! file, read through the filesystem primitives it already owns; the Runtime owns the PCAP
//! format.

#[cfg(target_os = "android")]
use contract::InterfaceAddress;
use contract::{
    DiagnosticOutcome, DnsEntry, ErrorCode, FileTarget, FileTargetType, InterfaceEntry,
    NetworkDiagnoseInput, NetworkDiagnoseResult, NetworkScope, RouteEntry, SocketEntry,
    SocketProtocol,
};
use domain::DomainError;
use runtime::{
    AdmittedExecution, AndroidExecutionDispatch, AndroidFrameworkFilesystemPort, Established,
    ExecutionFailure, FilesystemFrameworkPort, LocalExecutionClaim, NETWORK_MAX_CAPTURE_BYTES,
    NETWORK_MAX_SCOPE_ENTRIES, NetworkFamily, NetworkFamilyPlan, NetworkFamilySource,
    NetworkInspectSettlement, NetworkPrimitiveOutcome, NetworkPrimitivePort,
    NetworkPrimitiveRequest, NetworkPrimitiveSettlement, NetworkSourcePlan, network_dns_probe,
    network_tcp_probe, network_tls_probe, normalize_absolute_path, scope_families,
};
use std::fs::File;
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Instant;

/// The S-ANDROID-001 primitive that carries the Android framework's own network facts.
const NETWORK_SNAPSHOT_PRIMITIVE: &str = "AndroidNetworkSnapshot";

/// The snapshot request carries no input: the adapter reports the facts it owns, and the
/// Runtime decides which of them one request consumes.
const NETWORK_SNAPSHOT_REQUEST: &[u8] = b"{}";

const PROC_NET_ROUTE: &str = "/proc/net/route";
const PROC_NET_IPV6_ROUTE: &str = "/proc/net/ipv6_route";

/// The S-NET-001 socket source files, each with the protocol it reports.
const SOCKET_SOURCES: [(&str, SocketProtocol); 4] = [
    ("/proc/net/tcp", SocketProtocol::Tcp),
    ("/proc/net/tcp6", SocketProtocol::Tcp),
    ("/proc/net/udp", SocketProtocol::Udp),
    ("/proc/net/udp6", SocketProtocol::Udp),
];

const READ_CHUNK_BYTES: usize = 64 * 1024;

/// The S-NET-001 App-native interface facts: the APK process's own view of the interfaces
/// it can see. Keeping this separate from the framework link facts is what makes the
/// interfaces family independent of any framework or privileged provider.
pub trait AppInterfaceFacts: Send + Sync {
    fn interfaces(&self) -> Result<Vec<InterfaceEntry>, DomainError>;
}

/// The S-NET-001 read-only procfs supplement. The shell-UID process reads the file itself:
/// Android policy refuses these tables to the App domain, and a read through a descriptor the
/// shell process opened is still checked against the App domain.
pub trait SupplementReader: Send + Sync {
    /// At most `limit` bytes of `path`, and whether the file held more than that.
    fn read_supplement(
        &self,
        execution: &AdmittedExecution,
        path: &str,
        limit: usize,
    ) -> Result<(Vec<u8>, bool), DomainError>;
}

/// The App-native `getifaddrs` source. On a non-Android build there is no App process to
/// ask, so the family stays unresolved instead of being answered from another authority.
#[derive(Clone, Copy, Debug)]
pub struct GetifaddrsInterfaces;

/// The APK host's network primitive. `D` is the in-process App bridge, `I` the App-native
/// interface facts, and `S` the privileged filesystem primitive the Shizuku supplement
/// reads through. Default-network events use the separate S-NET-006 event plane.
pub struct ApkNetworkPort<D, I, S> {
    dispatch: D,
    interfaces: I,
    supplement: S,
}

impl<D, I, S> Clone for ApkNetworkPort<D, I, S>
where
    D: Clone,
    I: Clone,
    S: Clone,
{
    fn clone(&self) -> Self {
        Self {
            dispatch: self.dispatch.clone(),
            interfaces: self.interfaces.clone(),
            supplement: self.supplement.clone(),
        }
    }
}

impl<D, I, S> ApkNetworkPort<D, I, S> {
    pub fn new(dispatch: D, interfaces: I, supplement: S) -> Self {
        Self {
            dispatch,
            interfaces,
            supplement,
        }
    }
}

impl<D, I, S> NetworkPrimitivePort for ApkNetworkPort<D, I, S>
where
    D: AndroidExecutionDispatch + Clone + 'static,
    I: AppInterfaceFacts + Clone + 'static,
    S: SupplementReader + Clone + 'static,
{
    fn run(
        &self,
        execution: &AdmittedExecution,
        request: NetworkPrimitiveRequest,
        claim: &LocalExecutionClaim,
    ) -> Result<NetworkPrimitiveSettlement, ExecutionFailure> {
        let outcome = match request {
            NetworkPrimitiveRequest::Inspect {
                plan,
                scope,
                max_entries,
            } => NetworkPrimitiveOutcome::Inspect(self.inspect(
                execution,
                plan,
                scope,
                max_entries,
            )?),
            NetworkPrimitiveRequest::Diagnose(input, plan) => {
                NetworkPrimitiveOutcome::Diagnose(self.diagnose(execution, plan, input)?)
            }
            NetworkPrimitiveRequest::CaptureFileBytes { target } => {
                NetworkPrimitiveOutcome::CaptureFileBytes(
                    self.capture_file_bytes(execution, &target, claim)?,
                )
            }
            NetworkPrimitiveRequest::CaptureStart { .. }
            | NetworkPrimitiveRequest::CaptureStop { .. }
            | NetworkPrimitiveRequest::PacketInject { .. } => return Err(magisk_only()),
        };
        Ok(NetworkPrimitiveSettlement {
            outcome,
            cleanup_verified: true,
        })
    }
}

impl<D, I, S> ApkNetworkPort<D, I, S>
where
    D: AndroidExecutionDispatch + Clone + 'static,
    I: AppInterfaceFacts + Clone + 'static,
    S: SupplementReader + Clone + 'static,
{
    /// S-NET-001: one query per family the plan assigns to this host. Event observation is
    /// independent of inspect and belongs to the S-NET-006 subscription source.
    fn inspect(
        &self,
        execution: &AdmittedExecution,
        plan: NetworkSourcePlan,
        scope: NetworkScope,
        max_entries: u32,
    ) -> Result<NetworkInspectSettlement, ExecutionFailure> {
        let limit = max_entries as usize;
        let families = scope_families(scope);
        let needs_framework = families.iter().any(|family| {
            plan.family(*family) == NetworkFamilyPlan::Source(NetworkFamilySource::AppFramework)
        });
        let snapshot = if needs_framework {
            self.snapshot(execution)?
        } else {
            None
        };
        let supplement = supplement_execution(execution, plan);
        let mut settlement = NetworkInspectSettlement::default();
        for family in families {
            let source = match plan.family(*family) {
                NetworkFamilyPlan::Source(source) => source,
                NetworkFamilyPlan::Unavailable(_) => continue,
            };
            let read = match source {
                NetworkFamilySource::AppFramework => match (*family, snapshot.as_ref()) {
                    (NetworkFamily::Interfaces, Some(snapshot)) => self
                        .framework_interfaces(snapshot, limit)?
                        .map(FamilyRead::Interfaces),
                    (NetworkFamily::Routes, Some(snapshot)) => {
                        Some(FamilyRead::Routes(framework_routes(snapshot, limit)))
                    }
                    (NetworkFamily::Dns, Some(snapshot)) => {
                        Some(FamilyRead::Dns(framework_dns(snapshot, limit)))
                    }
                    _ => None,
                },
                NetworkFamilySource::ShizukuSupplement => match (*family, supplement.as_ref()) {
                    (NetworkFamily::Routes, Some(supplement)) => self
                        .supplement_routes(supplement, limit)?
                        .map(FamilyRead::Routes),
                    (NetworkFamily::Sockets, Some(supplement)) => self
                        .supplement_sockets(supplement, limit)?
                        .map(FamilyRead::Sockets),
                    _ => None,
                },
                NetworkFamilySource::Daemon => None,
            };
            match read {
                Some(FamilyRead::Interfaces(value)) => settlement.interfaces = Some(value),
                Some(FamilyRead::Routes(value)) => settlement.routes = Some(value),
                Some(FamilyRead::Dns(value)) => settlement.dns = Some(value),
                Some(FamilyRead::Sockets(value)) => settlement.sockets = Some(value),
                None => {}
            }
        }
        Ok(settlement)
    }

    /// R-NET-009: the shared R-NET-009 probes run in this process for the App/framework
    /// path, and `connectivity`/`route` are answered from the facts the assigned sources
    /// establish rather than from a second probe of their own.
    fn diagnose(
        &self,
        execution: &AdmittedExecution,
        plan: NetworkSourcePlan,
        input: NetworkDiagnoseInput,
    ) -> Result<NetworkDiagnoseResult, ExecutionFailure> {
        match input {
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
            } => {
                network_tls_probe(&host, port, server_name.as_deref(), timeout_ms).map_err(failure)
            }
            NetworkDiagnoseInput::Connectivity {} => self.diagnose_connectivity(execution),
            NetworkDiagnoseInput::Route { destination_ip } => {
                self.diagnose_route(execution, plan, &destination_ip)
            }
        }
    }

    /// R-NET-009 `connectivity` reports the facts the App/framework source owns. The probe
    /// was performed when the snapshot answered, so a device without an active network is
    /// `unreachable`; a snapshot that could not be taken at all is a structured error
    /// instead of a fabricated diagnostic outcome.
    fn diagnose_connectivity(
        &self,
        execution: &AdmittedExecution,
    ) -> Result<NetworkDiagnoseResult, ExecutionFailure> {
        let started = Instant::now();
        let snapshot = self.snapshot(execution)?.ok_or_else(|| {
            failure(DomainError::new(
                ErrorCode::CapabilityUnavailable,
                "network connectivity facts are unavailable",
            ))
        })?;
        let active = snapshot.default_network.is_some();
        Ok(NetworkDiagnoseResult::Connectivity {
            outcome: if active {
                DiagnosticOutcome::Success
            } else {
                DiagnosticOutcome::Unreachable
            },
            duration_ms: elapsed_ms(started),
            active_network_present: Some(active),
            default_route_present: Some(!snapshot.routes.is_empty()),
            dns_configured: Some(!snapshot.dns.is_empty()),
        })
    }

    /// R-NET-009 `route` answers from the read-only Shizuku supplement when it is reachable
    /// and otherwise from the App/framework default-route facts, so the result always names
    /// the route source that actually answered. A request no assigned source can answer is
    /// a structured error; a performed query with no match is `no_route`.
    fn diagnose_route(
        &self,
        execution: &AdmittedExecution,
        plan: NetworkSourcePlan,
        destination_ip: &str,
    ) -> Result<NetworkDiagnoseResult, ExecutionFailure> {
        let started = Instant::now();
        let destination = destination_ip.parse::<IpAddr>().map_err(|_| {
            failure(DomainError::invalid(
                "network.diagnose route destination is invalid",
            ))
        })?;
        let supplement = match supplement_execution(execution, plan) {
            Some(supplement) => {
                self.supplement_routes(&supplement, NETWORK_MAX_SCOPE_ENTRIES as usize)?
            }
            None => None,
        };
        let mut matched = supplement
            .as_ref()
            .and_then(|established| longest_prefix_match(&established.entries, destination));
        let snapshot = self.snapshot(execution)?;
        if matched.is_none()
            && let Some(snapshot) = snapshot.as_ref()
        {
            matched = framework_default_route(snapshot, destination);
        }
        let Some(route) = matched else {
            if supplement.is_none() && snapshot.is_none() {
                return Err(failure(DomainError::new(
                    ErrorCode::CapabilityUnavailable,
                    "network route facts are unavailable",
                )));
            }
            return Ok(NetworkDiagnoseResult::Route {
                outcome: DiagnosticOutcome::NoRoute,
                duration_ms: elapsed_ms(started),
                destination_ip: destination.to_string(),
                interface: None,
                gateway: None,
            });
        };
        Ok(NetworkDiagnoseResult::Route {
            outcome: DiagnosticOutcome::Success,
            duration_ms: elapsed_ms(started),
            destination_ip: destination.to_string(),
            interface: route.interface.clone(),
            gateway: route.gateway.clone(),
        })
    }

    /// The Android framework snapshot for one execution. `Ok(None)` means the assigned
    /// App/framework provider could not be consulted at all, which leaves every family it
    /// owns unresolved; a reply that is not the declared shape is a protocol failure.
    fn snapshot(
        &self,
        execution: &AdmittedExecution,
    ) -> Result<Option<NetworkSnapshotReply>, ExecutionFailure> {
        let answer = match self.dispatch.dispatch(
            NETWORK_SNAPSHOT_PRIMITIVE,
            NETWORK_SNAPSHOT_REQUEST,
            execution,
        ) {
            Ok(answer) => answer,
            Err(error) => return unresolved(error).map(|()| None),
        };
        if !answer.descriptors.is_empty() {
            return Err(failure(DomainError::new(
                ErrorCode::IoError,
                "network snapshot returned unexpected descriptors",
            )));
        }
        let reply: NetworkSnapshotReply =
            serde_json::from_slice(&answer.payload).map_err(|_| {
                failure(DomainError::new(
                    ErrorCode::IoError,
                    "network snapshot reply is invalid",
                ))
            })?;
        if let Some(identity) = reply.default_network.as_ref() {
            identity.validate().map_err(failure)?;
        }
        Ok(Some(reply))
    }

    /// S-NET-001: interfaces come from this process's own `getifaddrs` facts, and the
    /// framework link facts add what `getifaddrs` cannot report — the interface MTU and any
    /// link address it did not show. A fact only one source establishes keeps that source.
    fn framework_interfaces(
        &self,
        snapshot: &NetworkSnapshotReply,
        limit: usize,
    ) -> Result<Option<Established<InterfaceEntry>>, ExecutionFailure> {
        let mut entries = match self.interfaces.interfaces() {
            Ok(entries) => entries,
            Err(error) => return unresolved(error).map(|()| None),
        };
        for link in &snapshot.interfaces {
            match entries.iter_mut().find(|entry| entry.name == link.name) {
                Some(entry) => {
                    if entry.mtu.is_none() {
                        entry.mtu = link.mtu;
                    }
                    if entry.up.is_none() {
                        entry.up = link.up;
                    }
                    for address in &link.addresses {
                        if !entry.addresses.contains(address) {
                            entry.addresses.push(address.clone());
                        }
                    }
                }
                None => entries.push(link.clone()),
            }
        }
        Ok(Some(established(entries, limit, false)))
    }

    /// S-NET-001's read-only route supplement. `Ok(None)` means not one of its files could
    /// be read, so the family is unresolved rather than empty.
    fn supplement_routes(
        &self,
        execution: &AdmittedExecution,
        limit: usize,
    ) -> Result<Option<Established<RouteEntry>>, ExecutionFailure> {
        let mut entries = Vec::new();
        let mut truncated = false;
        let mut readable = false;
        for path in [PROC_NET_ROUTE, PROC_NET_IPV6_ROUTE] {
            let Some((text, cut)) = self.supplement_text(execution, path)? else {
                continue;
            };
            readable = true;
            truncated |= cut;
            if path == PROC_NET_ROUTE {
                append_ipv4_routes(&text, limit, &mut entries, &mut truncated)?;
            } else {
                append_ipv6_routes(&text, limit, &mut entries, &mut truncated)?;
            }
        }
        if !readable {
            return Ok(None);
        }
        Ok(Some(established(entries, limit, truncated)))
    }

    /// S-NET-001's read-only socket supplement over the four `procfs` tables.
    fn supplement_sockets(
        &self,
        execution: &AdmittedExecution,
        limit: usize,
    ) -> Result<Option<Established<SocketEntry>>, ExecutionFailure> {
        let mut entries = Vec::new();
        let mut truncated = false;
        let mut readable = false;
        for (path, protocol) in SOCKET_SOURCES {
            let Some((text, cut)) = self.supplement_text(execution, path)? else {
                continue;
            };
            readable = true;
            truncated |= cut;
            append_sockets(&text, protocol, limit, &mut entries, &mut truncated)?;
        }
        if !readable {
            return Ok(None);
        }
        Ok(Some(established(entries, limit, truncated)))
    }

    /// One read of a supplement file through the privileged filesystem primitive this host
    /// already owns. `Ok(None)` is the file this kernel does not publish, or a provider that
    /// could not be consulted; only cancellation ends the request.
    fn supplement_text(
        &self,
        execution: &AdmittedExecution,
        path: &str,
    ) -> Result<Option<(String, bool)>, ExecutionFailure> {
        let (mut bytes, truncated) =
            match self
                .supplement
                .read_supplement(execution, path, SUPPLEMENT_BYTES_LIMIT)
            {
                Ok(read) => read,
                Err(error) => return unresolved(error).map(|()| None),
            };
        if truncated {
            // A table cut at the byte bound keeps only its complete rows.
            let end = bytes
                .iter()
                .rposition(|byte| *byte == b'\n')
                .map_or(0, |index| index + 1);
            bytes.truncate(end);
        }
        String::from_utf8(bytes)
            .map_err(|_| source_format("network supplement file is not text"))
            .map(|text| Some((text, truncated)))
    }

    /// S-NET-004: the host supplies the bytes of a caller-named capture file through the
    /// filesystem primitive it already owns, and the Runtime owns the PCAP format.
    fn capture_file_bytes(
        &self,
        execution: &AdmittedExecution,
        target: &FileTarget,
        claim: &LocalExecutionClaim,
    ) -> Result<Vec<u8>, ExecutionFailure> {
        let file = match target.target_type {
            FileTargetType::Path => {
                File::open(normalize_absolute_path(&target.value).map_err(failure)?)
                    .map_err(|error| failure(crate::io_error(error)))?
            }
            FileTargetType::ContentUri => {
                let source = AndroidFrameworkFilesystemPort::new(self.dispatch.clone())
                    .open_read(execution, target);
                let source = source.map_err(failure)?;
                if source
                    .total_size
                    .is_some_and(|size| size > NETWORK_MAX_CAPTURE_BYTES)
                {
                    return Err(capture_limit());
                }
                source.file
            }
        };
        read_bounded(file, claim)
    }
}

/// The Android framework snapshot one execution read, in the R-NET-002 entry shapes.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NetworkSnapshotReply {
    interfaces: Vec<InterfaceEntry>,
    routes: Vec<RouteEntry>,
    dns: Vec<DnsEntry>,
    #[serde(default)]
    default_network: Option<SnapshotDefaultNetwork>,
}

/// The active default network's identity, which is the only fact the bounded event carries.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotDefaultNetwork {
    #[serde(default)]
    network_id: Option<String>,
    #[serde(default)]
    transport: Option<String>,
}

impl SnapshotDefaultNetwork {
    fn validate(&self) -> Result<(), DomainError> {
        for fact in [&self.network_id, &self.transport] {
            if fact
                .as_ref()
                .is_some_and(|value| value.is_empty() || value.len() > 128)
            {
                return Err(DomainError::new(
                    ErrorCode::IoError,
                    "network snapshot identity is invalid",
                ));
            }
        }
        Ok(())
    }
}

/// One family's read, so the settlement slot is assigned by family rather than by source.
enum FamilyRead {
    Interfaces(Established<InterfaceEntry>),
    Routes(Established<RouteEntry>),
    Dns(Established<DnsEntry>),
    Sockets(Established<SocketEntry>),
}

/// The bound on one supplement table read: the largest entry budget any inspect or diagnose
/// request may ask for, so a kernel table can never be read into an unbounded buffer.
/// The bounded supplement read, equal to the Shizuku UserService `read_bounded` limit.
const SUPPLEMENT_BYTES_LIMIT: usize = 262_144;

fn failure(error: DomainError) -> ExecutionFailure {
    ExecutionFailure {
        error,
        cleanup_verified: true,
    }
}

/// A provider that cannot be consulted leaves its family unresolved (S-NET-001 reports it
/// as `unknown`); a cancelled execution ends the whole request.
/// The execution a supplement read runs under: the admitted request's fence with the Shizuku
/// executor generation the plan fixed at admission. Dispatching under the App executor's own
/// generation would never reach the Shizuku session. `None` means no supplement was resolved.
fn supplement_execution(
    execution: &AdmittedExecution,
    plan: NetworkSourcePlan,
) -> Option<AdmittedExecution> {
    plan.supplement_generation().map(|generation| {
        let mut supplement = execution.clone();
        supplement.executor.capability_generation = generation;
        supplement
    })
}

fn unresolved(error: DomainError) -> Result<(), ExecutionFailure> {
    if error.code == ErrorCode::Cancelled {
        return Err(failure(error));
    }
    Ok(())
}

/// The typed refusal this host reports for the raw operations only the Magisk surface owns.
fn magisk_only() -> ExecutionFailure {
    failure(DomainError::new(
        ErrorCode::CapabilityUnavailable,
        "raw network operations require the Magisk host",
    ))
}

fn capture_limit() -> ExecutionFailure {
    failure(DomainError::new(
        ErrorCode::ResourceLimit,
        "capture file exceeds the capture artifact bound",
    ))
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

/// The family's entries under its own budget: `truncated` is the source's own truncation
/// fact, and an entry the budget cannot hold is reported as a truncation rather than
/// dropped silently.
fn established<T>(mut entries: Vec<T>, limit: usize, truncated: bool) -> Established<T> {
    let over_budget = entries.len() > limit;
    entries.truncate(limit);
    Established {
        entries,
        truncated: truncated || over_budget,
    }
}

/// S-NET-001's directly reported Android `LinkProperties` default-route facts.
fn framework_routes(snapshot: &NetworkSnapshotReply, limit: usize) -> Established<RouteEntry> {
    established(snapshot.routes.clone(), limit, false)
}

/// S-NET-001's `LinkProperties.getDnsServers()` facts.
fn framework_dns(snapshot: &NetworkSnapshotReply, limit: usize) -> Established<DnsEntry> {
    established(snapshot.dns.clone(), limit, false)
}

/// The snapshot's App/framework default routes, narrowed to the destination's family.
fn framework_default_route(
    snapshot: &NetworkSnapshotReply,
    destination: IpAddr,
) -> Option<RouteEntry> {
    snapshot
        .routes
        .iter()
        .find(|entry| route_contains(&entry.destination, destination))
        .cloned()
}

fn longest_prefix_match(entries: &[RouteEntry], destination: IpAddr) -> Option<RouteEntry> {
    let mut best: Option<(u8, RouteEntry)> = None;
    for entry in entries {
        let Some((network, prefix)) = parse_cidr(&entry.destination) else {
            continue;
        };
        if !contains(network, prefix, destination) {
            continue;
        }
        if best.as_ref().is_none_or(|(length, _)| prefix > *length) {
            best = Some((prefix, entry.clone()));
        }
    }
    best.map(|(_, entry)| entry)
}

fn route_contains(value: &str, destination: IpAddr) -> bool {
    parse_cidr(value).is_some_and(|(network, prefix)| contains(network, prefix, destination))
}

fn parse_cidr(value: &str) -> Option<(IpAddr, u8)> {
    let (address, prefix) = value.split_once('/')?;
    let address = address.parse::<IpAddr>().ok()?;
    let prefix = prefix.parse::<u8>().ok()?;
    let width = match address {
        IpAddr::V4(_) => 32,
        IpAddr::V6(_) => 128,
    };
    (prefix <= width).then_some((address, prefix))
}

fn contains(network: IpAddr, prefix: u8, destination: IpAddr) -> bool {
    match (network, destination) {
        (IpAddr::V4(network), IpAddr::V4(destination)) => {
            prefix == 0 || masked_v4(network, prefix) == masked_v4(destination, prefix)
        }
        (IpAddr::V6(network), IpAddr::V6(destination)) => {
            if prefix == 0 {
                return true;
            }
            let mask = u128::MAX << (128 - prefix);
            u128::from(network) & mask == u128::from(destination) & mask
        }
        _ => false,
    }
}

fn masked_v4(address: Ipv4Addr, prefix: u8) -> u32 {
    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    };
    u32::from(address) & mask
}

/// `/proc/net/route` prints IPv4 addresses as the host-order value of the network-order
/// address, and the mask with the same encoding.
fn append_ipv4_routes(
    text: &str,
    limit: usize,
    entries: &mut Vec<RouteEntry>,
    truncated: &mut bool,
) -> Result<(), ExecutionFailure> {
    for line in text.lines() {
        let columns = line.split_whitespace().collect::<Vec<_>>();
        match columns.first() {
            None => continue,
            Some(&"Iface") => continue,
            Some(_) => {}
        }
        if entries.len() >= limit {
            *truncated = true;
            break;
        }
        if columns.len() < 8 {
            return Err(source_format("network route source row is invalid"));
        }
        let destination = u32::from_str_radix(columns[1], 16)
            .map(host_order_ipv4)
            .map_err(|_| source_format("network route source row is invalid"))?;
        let mask = u32::from_str_radix(columns[7], 16)
            .map(host_order_ipv4)
            .map_err(|_| source_format("network route source row is invalid"))?;
        let prefix = contiguous_prefix(u32::from(mask))
            .ok_or_else(|| source_format("network route source row is invalid"))?;
        entries.push(RouteEntry {
            destination: format!("{destination}/{prefix}"),
            gateway: u32::from_str_radix(columns[2], 16)
                .map(host_order_ipv4)
                .ok()
                .filter(|gateway| !gateway.is_unspecified())
                .map(|gateway| gateway.to_string()),
            interface: Some(columns[0].to_owned()),
            metric: columns[6].parse::<u64>().ok(),
        });
    }
    Ok(())
}

/// `/proc/net/ipv6_route` prints IPv6 addresses as their network-order bytes.
fn append_ipv6_routes(
    text: &str,
    limit: usize,
    entries: &mut Vec<RouteEntry>,
    truncated: &mut bool,
) -> Result<(), ExecutionFailure> {
    for line in text.lines() {
        let columns = line.split_whitespace().collect::<Vec<_>>();
        if columns.is_empty() {
            continue;
        }
        if entries.len() >= limit {
            *truncated = true;
            break;
        }
        if columns.len() < 10 {
            return Err(source_format("network route source row is invalid"));
        }
        let destination = ipv6_address(columns[0])
            .ok_or_else(|| source_format("network route source row is invalid"))?;
        let prefix = u8::from_str_radix(columns[1], 16)
            .ok()
            .filter(|prefix| *prefix <= 128)
            .ok_or_else(|| source_format("network route source row is invalid"))?;
        entries.push(RouteEntry {
            destination: format!("{destination}/{prefix}"),
            gateway: ipv6_address(columns[4])
                .filter(|gateway| !gateway.is_unspecified())
                .map(|gateway| gateway.to_string()),
            interface: Some(columns[9].to_owned()),
            metric: u64::from_str_radix(columns[5], 16).ok(),
        });
    }
    Ok(())
}

/// One `procfs` socket table: `sl local_address rem_address st ... uid ...`, where every
/// address half is the host-order hex of one 32-bit network-order word.
fn append_sockets(
    text: &str,
    protocol: SocketProtocol,
    limit: usize,
    entries: &mut Vec<SocketEntry>,
    truncated: &mut bool,
) -> Result<(), ExecutionFailure> {
    for line in text.lines() {
        let columns = line.split_whitespace().collect::<Vec<_>>();
        let Some(first) = columns.first() else {
            continue;
        };
        if !first.ends_with(':') {
            continue;
        }
        if entries.len() >= limit {
            *truncated = true;
            break;
        }
        if columns.len() < 10 {
            return Err(source_format("network socket source row is invalid"));
        }
        let (local_address, local_port) = socket_endpoint(columns[1])?;
        let (remote_address, remote_port) = socket_endpoint(columns[2])?;
        entries.push(SocketEntry {
            protocol,
            local_address: local_address.to_string(),
            local_port: Some(local_port),
            remote_address: Some(remote_address.to_string()),
            remote_port: Some(remote_port),
            state: Some(columns[3].to_owned()),
            uid: columns[7].parse::<u32>().ok(),
        });
    }
    Ok(())
}

fn socket_endpoint(value: &str) -> Result<(IpAddr, u16), ExecutionFailure> {
    let invalid = || source_format("network socket source row is invalid");
    let (address, port) = value.split_once(':').ok_or_else(invalid)?;
    let port = u16::from_str_radix(port, 16).map_err(|_| invalid())?;
    let address = match address.len() {
        8 => u32::from_str_radix(address, 16)
            .map(host_order_ipv4)
            .map(IpAddr::V4)
            .map_err(|_| invalid())?,
        32 => {
            let mut words = [0_u32; 4];
            for (index, word) in words.iter_mut().enumerate() {
                *word = u32::from_str_radix(&address[index * 8..index * 8 + 8], 16)
                    .map_err(|_| invalid())?;
            }
            IpAddr::V6(ipv6_from_host_order(words))
        }
        _ => return Err(invalid()),
    };
    Ok((address, port))
}

/// The host-order value `procfs` prints for one network-order IPv4 address.
fn host_order_ipv4(value: u32) -> Ipv4Addr {
    Ipv4Addr::from(value.swap_bytes())
}

/// The four host-order words `procfs` prints for one network-order IPv6 address.
fn ipv6_from_host_order(words: [u32; 4]) -> Ipv6Addr {
    let mut bytes = [0_u8; 16];
    for (index, word) in words.iter().enumerate() {
        bytes[index * 4..index * 4 + 4].copy_from_slice(&word.swap_bytes().to_be_bytes());
    }
    Ipv6Addr::from(bytes)
}

/// `/proc/net/ipv6_route` prints the address as its network-order bytes.
fn ipv6_address(value: &str) -> Option<Ipv6Addr> {
    if value.len() != 32 {
        return None;
    }
    let mut bytes = [0_u8; 16];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(Ipv6Addr::from(bytes))
}

/// A prefix length is established only when the mask is one contiguous run of leading ones.
fn contiguous_prefix(mask: u32) -> Option<u8> {
    (mask.count_ones() == mask.leading_ones()).then_some(mask.leading_ones() as u8)
}

fn source_format(reason: &'static str) -> ExecutionFailure {
    failure(DomainError::new(ErrorCode::IoError, reason))
}

/// One bounded capture read. The capture artifact bound is S-ART-002's own maximum, and the
/// claim stays observable between chunks so a cancelled read stops instead of finishing.
fn read_bounded(mut file: File, claim: &LocalExecutionClaim) -> Result<Vec<u8>, ExecutionFailure> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; READ_CHUNK_BYTES];
    loop {
        claim.checkpoint().map_err(failure)?;
        let read = file
            .read(&mut buffer)
            .map_err(|error| failure(crate::io_error(error)))?;
        if read == 0 {
            return Ok(bytes);
        }
        if bytes.len() as u64 + read as u64 > NETWORK_MAX_CAPTURE_BYTES {
            return Err(capture_limit());
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
}

#[cfg(target_os = "android")]
impl AppInterfaceFacts for GetifaddrsInterfaces {
    fn interfaces(&self) -> Result<Vec<InterfaceEntry>, DomainError> {
        use std::collections::BTreeMap;
        use std::ffi::CStr;

        let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
        if unsafe { libc::getifaddrs(&mut head) } != 0 {
            return Err(DomainError::new(
                ErrorCode::IoError,
                "interface facts are unavailable",
            ));
        }
        let owned = InterfaceList(head);
        let mut entries: BTreeMap<String, InterfaceEntry> = BTreeMap::new();
        let mut cursor = head;
        while !cursor.is_null() {
            let interface = unsafe { &*cursor };
            let name = unsafe { CStr::from_ptr(interface.ifa_name) }
                .to_string_lossy()
                .into_owned();
            let index = unsafe { libc::if_nametoindex(interface.ifa_name) };
            let entry = entries
                .entry(name.clone())
                .or_insert_with(|| InterfaceEntry {
                    name,
                    index: (index != 0).then_some(index),
                    up: Some(interface.ifa_flags & libc::IFF_UP as libc::c_uint != 0),
                    mtu: None,
                    addresses: Vec::new(),
                });
            if let Some(address) = interface_address(interface)
                && !entry.addresses.contains(&address)
            {
                entry.addresses.push(address);
            }
            cursor = interface.ifa_next;
        }
        drop(owned);
        Ok(entries.into_values().collect())
    }
}

#[cfg(target_os = "android")]
struct InterfaceList(*mut libc::ifaddrs);

#[cfg(target_os = "android")]
impl Drop for InterfaceList {
    fn drop(&mut self) {
        unsafe { libc::freeifaddrs(self.0) }
    }
}

/// One address fact of a `getifaddrs` entry. The netmask is consulted only when its own
/// address family matches the address, so a mismatched pair reports the address without a
/// prefix length rather than a prefix derived from an unrelated family's mask.
#[cfg(target_os = "android")]
fn interface_address(interface: &libc::ifaddrs) -> Option<InterfaceAddress> {
    if interface.ifa_addr.is_null() {
        return None;
    }
    let mask = interface.ifa_netmask;
    match unsafe { (*interface.ifa_addr).sa_family } as libc::c_int {
        libc::AF_INET => {
            let address = unsafe { (*interface.ifa_addr.cast::<libc::sockaddr_in>()).sin_addr };
            let prefix_length = match mask_family(mask, libc::AF_INET) {
                true => {
                    let mask = unsafe { (*mask.cast::<libc::sockaddr_in>()).sin_addr };
                    contiguous_prefix(u32::from_be(mask.s_addr))
                }
                false => None,
            };
            Some(InterfaceAddress {
                address: Ipv4Addr::from(u32::from_be(address.s_addr)).to_string(),
                prefix_length,
            })
        }
        libc::AF_INET6 => {
            let address = unsafe { (*interface.ifa_addr.cast::<libc::sockaddr_in6>()).sin6_addr };
            let prefix_length = match mask_family(mask, libc::AF_INET6) {
                true => {
                    let mask = unsafe { (*mask.cast::<libc::sockaddr_in6>()).sin6_addr };
                    ipv6_prefix(u128::from_be_bytes(mask.s6_addr))
                }
                false => None,
            };
            Some(InterfaceAddress {
                address: Ipv6Addr::from(address.s6_addr).to_string(),
                prefix_length,
            })
        }
        _ => None,
    }
}

#[cfg(target_os = "android")]
fn mask_family(mask: *mut libc::sockaddr, family: libc::c_int) -> bool {
    !mask.is_null() && unsafe { (*mask).sa_family } as libc::c_int == family
}

#[cfg(target_os = "android")]
fn ipv6_prefix(mask: u128) -> Option<u8> {
    (mask.count_ones() == mask.leading_ones()).then_some(mask.leading_ones() as u8)
}

/// The App process has no `getifaddrs` off Android, so the interfaces family stays
/// unresolved rather than being answered by another authority.
#[cfg(not(target_os = "android"))]
impl AppInterfaceFacts for GetifaddrsInterfaces {
    fn interfaces(&self) -> Result<Vec<InterfaceEntry>, DomainError> {
        Err(DomainError::new(
            ErrorCode::CapabilityUnavailable,
            "App-native interface facts require Android",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use contract::{
        Availability, CapabilityState, CaptureId, ExecutionClass, Fence, GrantFacts,
        InterfaceAddress, NetworkCall, NetworkInspectInput, RuntimeHost, RuntimeReadiness, UuidV4,
    };
    use domain::{AdmissionFence, CapabilityContext, ProviderGenerations, ResolverFacts};
    use runtime::{
        AndroidPrimitiveResult, CapabilitySnapshot, ExecutionPayload, ExecutorRecord,
        LocalExecutionClaims, ProviderToken, network_source_plan,
    };
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    /// The device fixture every parsing test pins: an API 35 phone, read with
    /// `cat /proc/net/<table>`. The column layout and the byte order are the kernel's,
    /// captured rather than assumed.
    const DEVICE_ROUTE: &str =
        "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT
rmnet_data2\tC81A3D0A\t00000000\t0001\t0\t0\t0\tF8FFFFFF\t0\t0\t0
wlan0\t007E1EAC\t00000000\t0001\t0\t0\t0\t00FEFFFF\t0\t0\t0
";
    const DEVICE_ROUTE6: &str = "fe800000000000000000000000000000 40 00000000000000000000000000000000 00 00000000000000000000000000000000 00000100 00000001 00000000 00000001    wlan0
00000000000000000000000000000000 00 00000000000000000000000000000000 00 00000000000000000000000000000000 ffffffff 00000001 00000000 00200200       lo
240a42b0cc1105460000000000000000 40 00000000000000000000000000000000 00 00000000000000000000000000000000 00000400 00000001 00000000 00000001 rmnet_data1
";
    const DEVICE_TCP: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 00000000:4630 00000000:0000 0A 00000000:00000000 00:00000000 00000000 10686        0 82220 1 0000000000000000 99 0 0 10 0
   1: 987F1EAC:AA4C C8F75E7D:0050 08 00000000:00000001 00:00000000 00000000 10686        0 80873 1 0000000000000000 21 3 0 10 1400
";
    const DEVICE_TCP6: &str = "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 00000000000000000000000000000000:A22B 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  2000        0 68222 1 0000000000000000 99 0 0 10 0
   1: 0000000000000000FFFF0000987F1EAC:ACD0 0000000000000000FFFF00001AED3F3A:01BB 08 00000000:00000040 00:00000000 00000000 10686        0 88295 1 0000000000000000 23 3 30 10 1400
";
    const DEVICE_UDP: &str = "   sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode ref pointer drops
  995: 00000000:99DE 00000000:0000 07 00000000:00000000 00:00000000 00000000 10686        0 82217 2 0000000000000000 0
 2633: 987F1EAC:0044 017E1EAC:0043 01 00000000:00001100 00:00000000 00000000  1073        0 14031 2 0000000000000000 2
";

    const SNAPSHOT_PRIMITIVE: &str = "AndroidNetworkSnapshot";
    const MAGISK_ONLY: &str = "raw network operations require the Magisk host";

    fn uuid(prefix: u32, value: u64) -> UuidV4 {
        UuidV4::parse(format!("{prefix:08x}-0000-4000-8000-{value:012x}")).unwrap()
    }

    /// One scratch directory under the OS temp root, removed when the test ends.
    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("i8-net-g-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("scratch directory");
            Self(path)
        }

        fn write(&self, name: &str, bytes: &[u8]) -> std::path::PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, bytes).expect("scratch file");
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn admitted() -> AdmittedExecution {
        let execution_id = uuid(0x8500_0000, 1);
        AdmittedExecution {
            execution_id,
            task_id: None,
            executor: ExecutorRecord {
                host: RuntimeHost::ApkRuntime,
                provider: ProviderToken::AppNative,
                execution_class: ExecutionClass::App,
                capability_generation: 4,
                fence: Fence {
                    runtime_epoch: uuid(0x8100_0000, 1),
                    host_generation: 4,
                    runtime_instance_id: uuid(0x8100_0000, 0x11),
                },
            },
            payload: ExecutionPayload::NetworkCall(NetworkCall::Inspect(NetworkInspectInput {
                scope: NetworkScope::All,
                max_entries: 200,
            })),
        }
    }

    /// The claim the owning surface would hold for that execution.
    fn claim(execution: &AdmittedExecution) -> LocalExecutionClaim {
        LocalExecutionClaims::default()
            .claim(execution.execution_id.clone())
            .expect("the fixture claims its own execution")
    }

    const fn available() -> Availability {
        Availability {
            state: CapabilityState::Available,
            reason: None,
        }
    }

    /// The provider facts S-NET-001 keys its assignment on. They feed the real
    /// `network_source_plan`, so each test drives the delivered plan rather than a
    /// locally invented one.
    struct Facts {
        app_execution_surface: CapabilityState,
        shizuku: CapabilityState,
    }

    fn capability(facts: Facts) -> CapabilitySnapshot {
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
                app_execution_surface: facts.app_execution_surface,
            },
            resolver_facts: ResolverFacts {
                app_native: CapabilityState::Available,
                app_framework: facts.app_execution_surface,
                shizuku: facts.shizuku,
                magisk_native: CapabilityState::Unavailable,
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
                    shizuku: 9_001,
                    magisk_native: 9_002,
                    magisk_framework: 4,
                    accessibility: 4,
                    media_projection: 4,
                    notification_listener: 4,
                },
            },
            fence: AdmissionFence {
                runtime_epoch: uuid(0x8100_0000, 1),
                host_generation: 4,
                runtime_instance_id: uuid(0x8100_0000, 0x11),
            },
        }
    }

    /// The APK companion and the Shizuku supplement are both reachable.
    fn apk_plan() -> NetworkSourcePlan {
        network_source_plan(
            &capability(Facts {
                app_execution_surface: CapabilityState::Available,
                shizuku: CapabilityState::Available,
            }),
            ProviderToken::AppNative,
        )
    }

    /// The APK companion exists but no Shizuku shell does, so S-NET-001 keeps sockets
    /// assigned to no reachable provider and falls routes back to the framework facts.
    fn companion_only_plan() -> NetworkSourcePlan {
        network_source_plan(
            &capability(Facts {
                app_execution_surface: CapabilityState::Available,
                shizuku: CapabilityState::Unavailable,
            }),
            ProviderToken::AppNative,
        )
    }

    #[derive(Default)]
    struct FrameworkState {
        calls: Vec<String>,
        payload: Vec<u8>,
        unavailable: bool,
    }

    /// The App execution surface as one execution sees it.
    #[derive(Clone, Default)]
    struct ScriptedFramework {
        state: Arc<Mutex<FrameworkState>>,
    }

    impl ScriptedFramework {
        fn answering(payload: &[u8]) -> Self {
            let framework = Self::default();
            framework.state.lock().unwrap().payload = payload.to_vec();
            framework
        }

        fn unavailable() -> Self {
            let framework = Self::default();
            framework.state.lock().unwrap().unavailable = true;
            framework
        }

        fn calls(&self) -> Vec<String> {
            self.state.lock().unwrap().calls.clone()
        }
    }

    impl AndroidExecutionDispatch for ScriptedFramework {
        fn dispatch(
            &self,
            primitive: &str,
            _payload: &[u8],
            _execution: &AdmittedExecution,
        ) -> Result<AndroidPrimitiveResult, DomainError> {
            let mut state = self.state.lock().unwrap();
            state.calls.push(primitive.to_owned());
            if state.unavailable {
                return Err(DomainError::new(
                    ErrorCode::CapabilityUnavailable,
                    "the App execution surface is unavailable",
                ));
            }
            Ok(AndroidPrimitiveResult {
                payload: state.payload.clone(),
                descriptors: Vec::new(),
            })
        }
    }

    /// The App-native `getifaddrs` view, scripted so a test states exactly what this
    /// process observed.
    #[derive(Clone, Default)]
    struct ScriptedInterfaces {
        entries: Arc<Mutex<Option<Vec<InterfaceEntry>>>>,
    }

    impl ScriptedInterfaces {
        fn reporting(entries: Vec<InterfaceEntry>) -> Self {
            let interfaces = Self::default();
            *interfaces.entries.lock().unwrap() = Some(entries);
            interfaces
        }
    }

    impl AppInterfaceFacts for ScriptedInterfaces {
        fn interfaces(&self) -> Result<Vec<InterfaceEntry>, DomainError> {
            self.entries.lock().unwrap().clone().ok_or_else(|| {
                DomainError::new(
                    ErrorCode::CapabilityUnavailable,
                    "App-native interface facts are unavailable",
                )
            })
        }
    }

    /// The read-only privileged filesystem primitive, serving one scratch file per
    /// supplement path or refusing to be consulted at all.
    #[derive(Clone)]
    struct ScriptedSupplement {
        opened: Arc<Mutex<Vec<String>>>,
        generations: Arc<Mutex<Vec<u64>>>,
        files: BTreeMap<String, std::path::PathBuf>,
        unreachable: bool,
    }

    impl ScriptedSupplement {
        fn serving(scratch: &Scratch, files: &[(&str, &str)]) -> Self {
            let mut served = BTreeMap::new();
            for (path, text) in files {
                let name = path.trim_start_matches('/').replace('/', "-");
                served.insert((*path).to_owned(), scratch.write(&name, text.as_bytes()));
            }
            Self {
                opened: Arc::new(Mutex::new(Vec::new())),
                generations: Arc::new(Mutex::new(Vec::new())),
                files: served,
                unreachable: false,
            }
        }

        fn unreachable() -> Self {
            Self {
                opened: Arc::new(Mutex::new(Vec::new())),
                generations: Arc::new(Mutex::new(Vec::new())),
                files: BTreeMap::new(),
                unreachable: true,
            }
        }

        fn opened(&self) -> Vec<String> {
            let mut opened = self.opened.lock().unwrap().clone();
            opened.sort();
            opened
        }

        fn generations(&self) -> Vec<u64> {
            self.generations.lock().unwrap().clone()
        }
    }

    impl SupplementReader for ScriptedSupplement {
        fn read_supplement(
            &self,
            execution: &AdmittedExecution,
            path: &str,
            limit: usize,
        ) -> Result<(Vec<u8>, bool), DomainError> {
            self.opened.lock().unwrap().push(path.to_owned());
            self.generations
                .lock()
                .unwrap()
                .push(execution.executor.capability_generation);
            if self.unreachable {
                return Err(DomainError::new(
                    ErrorCode::CapabilityUnavailable,
                    "the Shizuku supplement is unavailable",
                ));
            }
            match self.files.get(path) {
                Some(file) => {
                    let mut bytes = std::fs::read(file).map_err(|_| {
                        DomainError::new(ErrorCode::IoError, "the supplement fixture is unreadable")
                    })?;
                    let truncated = bytes.len() > limit;
                    bytes.truncate(limit);
                    Ok((bytes, truncated))
                }
                None => Err(DomainError::new(
                    ErrorCode::NotFound,
                    "this kernel does not publish that table",
                )),
            }
        }
    }

    fn supplement(scratch: &Scratch) -> ScriptedSupplement {
        ScriptedSupplement::serving(
            scratch,
            &[
                ("/proc/net/route", DEVICE_ROUTE),
                ("/proc/net/ipv6_route", DEVICE_ROUTE6),
                ("/proc/net/tcp", DEVICE_TCP),
                ("/proc/net/tcp6", DEVICE_TCP6),
                ("/proc/net/udp", DEVICE_UDP),
                ("/proc/net/udp6", ""),
            ],
        )
    }

    /// One encoded Android-framework snapshot reply, in the R-NET-002 entry shapes.
    fn snapshot(
        interfaces: Vec<InterfaceEntry>,
        routes: Vec<RouteEntry>,
        dns: Vec<DnsEntry>,
        default_network: Option<(&str, &str)>,
    ) -> Vec<u8> {
        let mut value = serde_json::json!({
            "interfaces": interfaces,
            "routes": routes,
            "dns": dns,
        });
        if let Some((network_id, transport)) = default_network {
            value["default_network"] = serde_json::json!({
                "network_id": network_id,
                "transport": transport,
            });
        }
        serde_json::to_vec(&value).expect("the fixture encodes a snapshot")
    }

    fn address(address: &str, prefix_length: Option<u8>) -> InterfaceAddress {
        InterfaceAddress {
            address: address.to_owned(),
            prefix_length,
        }
    }

    fn interface(name: &str, index: Option<u32>, prefix_length: Option<u8>) -> InterfaceEntry {
        InterfaceEntry {
            name: name.to_owned(),
            index,
            up: Some(true),
            mtu: None,
            addresses: vec![address("172.30.127.152", prefix_length)],
        }
    }

    fn run<D, I, S>(
        port: &ApkNetworkPort<D, I, S>,
        execution: &AdmittedExecution,
        request: NetworkPrimitiveRequest,
    ) -> Result<NetworkPrimitiveOutcome, ExecutionFailure>
    where
        D: AndroidExecutionDispatch + Clone + 'static,
        I: AppInterfaceFacts + Clone + 'static,
        S: SupplementReader + Clone + 'static,
    {
        port.run(execution, request, &claim(execution))
            .map(|settlement| settlement.outcome)
    }

    fn inspect(
        port: &ApkNetworkPort<ScriptedFramework, ScriptedInterfaces, ScriptedSupplement>,
        execution: &AdmittedExecution,
        plan: NetworkSourcePlan,
        scope: NetworkScope,
        max_entries: u32,
    ) -> NetworkInspectSettlement {
        let outcome = run(
            port,
            execution,
            NetworkPrimitiveRequest::Inspect {
                plan,
                scope,
                max_entries,
            },
        )
        .expect("the APK path settles an inspect request");
        match outcome {
            NetworkPrimitiveOutcome::Inspect(settlement) => settlement,
            other => panic!("an inspect request settled as {other:?}"),
        }
    }

    /// S-NET-001: the delivered plan decides which providers this host consults, so a
    /// scope of `all` reads every family exactly once per assigned source.
    #[test]
    fn i8_net_g_plan_selects_the_families_this_host_queries() {
        let scratch = Scratch::new("plan");
        let framework =
            ScriptedFramework::answering(&snapshot(Vec::new(), Vec::new(), Vec::new(), None));
        let interfaces = ScriptedInterfaces::reporting(vec![interface("wlan0", Some(5), Some(23))]);
        let supplement = supplement(&scratch);
        let observed = supplement.clone();
        let port = ApkNetworkPort::new(framework.clone(), interfaces, supplement);
        let execution = admitted();
        let settlement = inspect(&port, &execution, apk_plan(), NetworkScope::All, 200);

        assert_eq!(framework.calls(), vec![SNAPSHOT_PRIMITIVE.to_owned()]);
        assert_eq!(
            observed.opened(),
            vec![
                "/proc/net/ipv6_route",
                "/proc/net/route",
                "/proc/net/tcp",
                "/proc/net/tcp6",
                "/proc/net/udp",
                "/proc/net/udp6",
            ]
        );
        assert!(settlement.interfaces.is_some());
        assert!(settlement.routes.is_some());
        assert!(settlement.dns.is_some());
        assert!(settlement.sockets.is_some());
    }

    /// S-NET-001 leaves sockets assigned to no reachable provider once `shizuku.shell` is
    /// gone, so the request must not fall back to another source for that family.
    #[test]
    fn i8_net_g_plan_leaves_an_unassigned_family_unrequested() {
        let scratch = Scratch::new("unassigned");
        let framework =
            ScriptedFramework::answering(&snapshot(Vec::new(), Vec::new(), Vec::new(), None));
        let supplement = supplement(&scratch);
        let observed = supplement.clone();
        let port = ApkNetworkPort::new(
            framework.clone(),
            ScriptedInterfaces::reporting(Vec::new()),
            supplement,
        );
        let execution = admitted();
        let settlement = inspect(
            &port,
            &execution,
            companion_only_plan(),
            NetworkScope::All,
            200,
        );

        assert_eq!(framework.calls(), vec![SNAPSHOT_PRIMITIVE.to_owned()]);
        assert!(observed.opened().is_empty());
        assert!(settlement.sockets.is_none());
        assert!(settlement.routes.is_some());
    }

    /// S-NET-001's read-only supplement reports `None` when its provider cannot be
    /// consulted, so R-NET-002 can report `unknown` instead of an authoritative emptiness.
    #[test]
    fn i8_net_g_unreachable_supplement_is_unresolved_not_empty() {
        let port = ApkNetworkPort::new(
            ScriptedFramework::answering(&snapshot(Vec::new(), Vec::new(), Vec::new(), None)),
            ScriptedInterfaces::reporting(Vec::new()),
            ScriptedSupplement::unreachable(),
        );
        let execution = admitted();
        let settlement = inspect(&port, &execution, apk_plan(), NetworkScope::All, 200);

        assert!(settlement.routes.is_none());
        assert!(settlement.sockets.is_none());
    }

    /// A reply that is not the declared snapshot shape is a protocol failure, never an
    /// authoritative empty family.
    #[test]
    fn i8_net_g_malformed_snapshot_reply_is_a_structured_error() {
        let port = ApkNetworkPort::new(
            ScriptedFramework::answering(b"{\"interfaces\":\"wlan0\"}"),
            ScriptedInterfaces::reporting(Vec::new()),
            ScriptedSupplement::unreachable(),
        );
        let execution = admitted();
        let failure = run(
            &port,
            &execution,
            NetworkPrimitiveRequest::Inspect {
                plan: apk_plan(),
                scope: NetworkScope::Interfaces,
                max_entries: 200,
            },
        )
        .expect_err("a malformed snapshot reply fails the request");

        assert_eq!(failure.error.code, ErrorCode::IoError);
        assert!(failure.cleanup_verified);
    }

    /// A snapshot the App surface cannot take at all leaves its families unresolved rather
    /// than answering them from another authority.
    #[test]
    fn i8_net_g_unavailable_framework_leaves_its_families_unresolved() {
        let port = ApkNetworkPort::new(
            ScriptedFramework::unavailable(),
            ScriptedInterfaces::reporting(Vec::new()),
            ScriptedSupplement::unreachable(),
        );
        let execution = admitted();
        let settlement = inspect(&port, &execution, apk_plan(), NetworkScope::All, 200);

        assert!(settlement.interfaces.is_none());
        assert!(settlement.dns.is_none());
        assert!(settlement.routes.is_none());
        assert!(settlement.sockets.is_none());
    }

    /// S-NET-004 keeps raw capture and injection on the Magisk host; this surface refuses
    /// them with that exact reason instead of degrading to a partial implementation.
    #[test]
    fn i8_net_g_raw_capture_and_injection_require_the_magisk_host() {
        let port = ApkNetworkPort::new(
            ScriptedFramework::answering(&snapshot(Vec::new(), Vec::new(), Vec::new(), None)),
            ScriptedInterfaces::reporting(Vec::new()),
            ScriptedSupplement::unreachable(),
        );
        let execution = admitted();
        let capture_id = CaptureId::parse("e1000000-0000-4000-8000-000000000001").unwrap();
        let requests = [
            NetworkPrimitiveRequest::CaptureStart {
                capture_id: capture_id.clone(),
                interface: "wlan0".to_owned(),
                filter: None,
                max_packets: 100,
                max_bytes: 4_096,
                max_duration_ms: 1_000,
                persist_to: None,
            },
            NetworkPrimitiveRequest::CaptureStop { capture_id },
            NetworkPrimitiveRequest::PacketInject {
                interface: "wlan0".to_owned(),
                packet: vec![0_u8; 64],
                count: 1,
                interval_ms: 0,
            },
        ];
        for request in requests {
            let failure = run(&port, &execution, request)
                .expect_err("a raw network operation is refused on the APK host");
            assert_eq!(failure.error.code, ErrorCode::CapabilityUnavailable);
            assert_eq!(failure.error.reason, MAGISK_ONLY);
            assert!(failure.cleanup_verified);
        }
    }

    /// S-NET-004: the one capture fact this host supplies is the bytes of the caller's own
    /// file, unchanged and bounded, with the Runtime owning the PCAP format.
    #[test]
    fn i8_net_g_capture_file_bytes_returns_exactly_the_named_file() {
        let scratch = Scratch::new("capture-bytes");
        let bytes = b"\xd4\xc3\xb2\xa1\x02\x00\x04\x00";
        let path = scratch.write("capture.pcap", bytes);
        let port = ApkNetworkPort::new(
            ScriptedFramework::answering(&snapshot(Vec::new(), Vec::new(), Vec::new(), None)),
            ScriptedInterfaces::reporting(Vec::new()),
            ScriptedSupplement::unreachable(),
        );
        let execution = admitted();
        let outcome = run(
            &port,
            &execution,
            NetworkPrimitiveRequest::CaptureFileBytes {
                target: FileTarget {
                    target_type: FileTargetType::Path,
                    value: path.to_string_lossy().into_owned(),
                },
            },
        )
        .expect("the APK path reads a caller-named capture file");

        assert_eq!(
            outcome,
            NetworkPrimitiveOutcome::CaptureFileBytes(bytes.to_vec())
        );
    }

    /// A capture file the caller names by a path this host must not accept is refused
    /// before any read, so no request can widen the filesystem surface.
    #[test]
    fn i8_net_g_capture_file_bytes_rejects_a_relative_path() {
        let port = ApkNetworkPort::new(
            ScriptedFramework::answering(&snapshot(Vec::new(), Vec::new(), Vec::new(), None)),
            ScriptedInterfaces::reporting(Vec::new()),
            ScriptedSupplement::unreachable(),
        );
        let execution = admitted();
        let failure = run(
            &port,
            &execution,
            NetworkPrimitiveRequest::CaptureFileBytes {
                target: FileTarget {
                    target_type: FileTargetType::Path,
                    value: "capture.pcap".to_owned(),
                },
            },
        )
        .expect_err("a relative capture path is rejected");

        assert_eq!(failure.error.code, ErrorCode::InvalidArgument);
    }

    /// The device fixture's rows decode into exactly the R-NET-002 route shape: the IPv4
    /// destination is the host-order value this kernel prints, and the IPv6 table prints
    /// network-order bytes.
    #[test]
    fn i8_net_g_supplement_routes_decode_to_the_contract_shape() {
        let scratch = Scratch::new("routes");
        let port = ApkNetworkPort::new(
            ScriptedFramework::answering(&snapshot(Vec::new(), Vec::new(), Vec::new(), None)),
            ScriptedInterfaces::reporting(Vec::new()),
            supplement(&scratch),
        );
        let execution = admitted();
        let settlement = inspect(&port, &execution, apk_plan(), NetworkScope::Routes, 200);
        let routes = settlement.routes.expect("the supplement answered").entries;

        assert_eq!(
            routes,
            vec![
                RouteEntry {
                    destination: "10.61.26.200/29".to_owned(),
                    gateway: None,
                    interface: Some("rmnet_data2".to_owned()),
                    metric: Some(0),
                },
                RouteEntry {
                    destination: "172.30.126.0/23".to_owned(),
                    gateway: None,
                    interface: Some("wlan0".to_owned()),
                    metric: Some(0),
                },
                RouteEntry {
                    destination: "fe80::/64".to_owned(),
                    gateway: None,
                    interface: Some("wlan0".to_owned()),
                    metric: Some(256),
                },
                RouteEntry {
                    destination: "::/0".to_owned(),
                    gateway: None,
                    interface: Some("lo".to_owned()),
                    metric: Some(4_294_967_295),
                },
                RouteEntry {
                    destination: "240a:42b0:cc11:546::/64".to_owned(),
                    gateway: None,
                    interface: Some("rmnet_data1".to_owned()),
                    metric: Some(1_024),
                },
            ]
        );
    }

    /// The device fixture's socket rows decode into exactly the R-NET-002 socket shape,
    /// including the IPv4-mapped IPv6 form of the same local address.
    #[test]
    fn i8_net_g_supplement_sockets_decode_to_the_contract_shape() {
        let scratch = Scratch::new("sockets");
        let port = ApkNetworkPort::new(
            ScriptedFramework::answering(&snapshot(Vec::new(), Vec::new(), Vec::new(), None)),
            ScriptedInterfaces::reporting(Vec::new()),
            supplement(&scratch),
        );
        let execution = admitted();
        let settlement = inspect(&port, &execution, apk_plan(), NetworkScope::Sockets, 200);
        let sockets = settlement.sockets.expect("the supplement answered").entries;

        assert_eq!(
            sockets,
            vec![
                SocketEntry {
                    protocol: SocketProtocol::Tcp,
                    local_address: "0.0.0.0".to_owned(),
                    local_port: Some(0x4630),
                    remote_address: Some("0.0.0.0".to_owned()),
                    remote_port: Some(0),
                    state: Some("0A".to_owned()),
                    uid: Some(10_686),
                },
                SocketEntry {
                    protocol: SocketProtocol::Tcp,
                    local_address: "172.30.127.152".to_owned(),
                    local_port: Some(0xAA4C),
                    remote_address: Some("125.94.247.200".to_owned()),
                    remote_port: Some(80),
                    state: Some("08".to_owned()),
                    uid: Some(10_686),
                },
                SocketEntry {
                    protocol: SocketProtocol::Tcp,
                    local_address: "::".to_owned(),
                    local_port: Some(0xA22B),
                    remote_address: Some("::".to_owned()),
                    remote_port: Some(0),
                    state: Some("0A".to_owned()),
                    uid: Some(2_000),
                },
                SocketEntry {
                    protocol: SocketProtocol::Tcp,
                    local_address: "::ffff:172.30.127.152".to_owned(),
                    local_port: Some(0xACD0),
                    remote_address: Some("::ffff:58.63.237.26".to_owned()),
                    remote_port: Some(443),
                    state: Some("08".to_owned()),
                    uid: Some(10_686),
                },
                SocketEntry {
                    protocol: SocketProtocol::Udp,
                    local_address: "0.0.0.0".to_owned(),
                    local_port: Some(0x99DE),
                    remote_address: Some("0.0.0.0".to_owned()),
                    remote_port: Some(0),
                    state: Some("07".to_owned()),
                    uid: Some(10_686),
                },
                SocketEntry {
                    protocol: SocketProtocol::Udp,
                    local_address: "172.30.127.152".to_owned(),
                    local_port: Some(0x0044),
                    remote_address: Some("172.30.126.1".to_owned()),
                    remote_port: Some(0x0043),
                    state: Some("01".to_owned()),
                    uid: Some(1_073),
                },
            ]
        );
    }

    /// A scope reads only its own families, so a single-family request never touches the
    /// other sources.
    #[test]
    fn i8_net_g_scope_requests_only_its_own_families() {
        let scratch = Scratch::new("scope");
        let framework =
            ScriptedFramework::answering(&snapshot(Vec::new(), Vec::new(), Vec::new(), None));
        let supplement = supplement(&scratch);
        let observed = supplement.clone();
        let port = ApkNetworkPort::new(
            framework.clone(),
            ScriptedInterfaces::reporting(Vec::new()),
            supplement,
        );
        let execution = admitted();
        let settlement = inspect(&port, &execution, apk_plan(), NetworkScope::Routes, 200);

        assert_eq!(
            observed.opened(),
            vec!["/proc/net/ipv6_route", "/proc/net/route"]
        );
        assert!(framework.calls().is_empty());
        assert!(settlement.routes.is_some());
        assert!(settlement.interfaces.is_none());
        assert!(settlement.dns.is_none());
        assert!(settlement.sockets.is_none());
    }

    /// An entry beyond the family budget is reported as a truncation rather than dropped
    /// silently, so R-NET-002 can state the family is partial.
    #[test]
    fn i8_net_g_family_budget_reports_truncation() {
        let scratch = Scratch::new("budget");
        let port = ApkNetworkPort::new(
            ScriptedFramework::answering(&snapshot(Vec::new(), Vec::new(), Vec::new(), None)),
            ScriptedInterfaces::reporting(Vec::new()),
            supplement(&scratch),
        );
        let execution = admitted();
        let settlement = inspect(&port, &execution, apk_plan(), NetworkScope::Sockets, 2);
        let sockets = settlement.sockets.expect("the supplement answered");

        assert_eq!(sockets.entries.len(), 2);
        assert!(sockets.truncated);
    }

    /// A table larger than the bounded supplement read keeps its complete rows and reports
    /// the family as truncated instead of parsing a row cut at the byte bound.
    #[test]
    fn i8_net_g_supplement_byte_bound_keeps_complete_rows_and_reports_truncation() {
        let scratch = Scratch::new("byte-bound");
        let header = DEVICE_TCP.lines().next().expect("the fixture has a header");
        let row = DEVICE_TCP
            .lines()
            .nth(2)
            .expect("the fixture has a connection row");
        let rows = SUPPLEMENT_BYTES_LIMIT / row.len() + 16;
        let mut table = format!("{header}\n");
        for _ in 0..rows {
            table.push_str(row);
            table.push('\n');
        }
        let port = ApkNetworkPort::new(
            ScriptedFramework::answering(&snapshot(Vec::new(), Vec::new(), Vec::new(), None)),
            ScriptedInterfaces::reporting(Vec::new()),
            ScriptedSupplement::serving(&scratch, &[("/proc/net/tcp", table.as_str())]),
        );
        let execution = admitted();
        let settlement = inspect(&port, &execution, apk_plan(), NetworkScope::Sockets, 5_000);
        let sockets = settlement.sockets.expect("the supplement answered");

        assert!(sockets.truncated);
        assert!(!sockets.entries.is_empty());
        assert!(sockets.entries.len() < rows);
    }

    /// S-NET-001's supplement reads run under the Shizuku executor generation the plan fixed
    /// at admission, never under the generation of the App executor that admitted the request.
    #[test]
    fn i8_net_g_supplement_reads_run_under_the_planned_shizuku_generation() {
        let scratch = Scratch::new("supplement-generation");
        let supplement = supplement(&scratch);
        let observed = supplement.clone();
        let port = ApkNetworkPort::new(
            ScriptedFramework::answering(&snapshot(Vec::new(), Vec::new(), Vec::new(), None)),
            ScriptedInterfaces::reporting(Vec::new()),
            supplement,
        );
        let execution = admitted();
        let plan = apk_plan();
        let shizuku = plan
            .supplement_generation()
            .expect("the APK plan resolved the supplement");
        assert_ne!(shizuku, execution.executor.capability_generation);

        let settlement = inspect(&port, &execution, plan, NetworkScope::All, 200);

        assert!(settlement.sockets.is_some());
        let generations = observed.generations();
        assert!(!generations.is_empty());
        assert!(generations.iter().all(|generation| *generation == shizuku));
        assert!(companion_only_plan().supplement_generation().is_none());
    }

    /// S-NET-001: interfaces come from this process's own `getifaddrs` facts, and the
    /// framework link facts add what that view cannot report.
    #[test]
    fn i8_net_g_interfaces_merge_app_native_and_framework_facts() {
        let mut link = interface("wlan0", Some(5), Some(23));
        link.mtu = Some(1_500);
        link.addresses.push(address("fe80::1", Some(64)));
        let mut framework_only = interface("rmnet_data1", Some(9), Some(64));
        framework_only.addresses = vec![address("240a:42b0:cc11:0546::1", Some(64))];
        let port = ApkNetworkPort::new(
            ScriptedFramework::answering(&snapshot(
                vec![link, framework_only],
                Vec::new(),
                Vec::new(),
                None,
            )),
            ScriptedInterfaces::reporting(vec![interface("wlan0", Some(5), Some(23))]),
            ScriptedSupplement::unreachable(),
        );
        let execution = admitted();
        let settlement = inspect(&port, &execution, apk_plan(), NetworkScope::Interfaces, 200);
        let interfaces = settlement
            .interfaces
            .expect("both sources answered")
            .entries;

        assert_eq!(
            interfaces,
            vec![
                InterfaceEntry {
                    name: "wlan0".to_owned(),
                    index: Some(5),
                    up: Some(true),
                    mtu: Some(1_500),
                    addresses: vec![
                        address("172.30.127.152", Some(23)),
                        address("fe80::1", Some(64)),
                    ],
                },
                InterfaceEntry {
                    name: "rmnet_data1".to_owned(),
                    index: Some(9),
                    up: Some(true),
                    mtu: None,
                    addresses: vec![address("240a:42b0:cc11:0546::1", Some(64))],
                },
            ]
        );
    }

    /// R-NET-009 `route` consumes the read-only Shizuku supplement and answers with the
    /// longest matching prefix, and reports `no_route` only after a query did run.
    #[test]
    fn i8_net_g_diagnose_route_matches_the_longest_prefix() {
        let scratch = Scratch::new("diagnose-route");
        let port = ApkNetworkPort::new(
            ScriptedFramework::answering(&snapshot(Vec::new(), Vec::new(), Vec::new(), None)),
            ScriptedInterfaces::reporting(Vec::new()),
            supplement(&scratch),
        );
        let execution = admitted();
        let matched = run(
            &port,
            &execution,
            NetworkPrimitiveRequest::Diagnose(
                NetworkDiagnoseInput::Route {
                    destination_ip: "172.30.127.152".to_owned(),
                },
                apk_plan(),
            ),
        )
        .expect("the supplement answers a route query");
        match matched {
            NetworkPrimitiveOutcome::Diagnose(NetworkDiagnoseResult::Route {
                outcome,
                destination_ip,
                interface,
                gateway,
                ..
            }) => {
                assert_eq!(outcome, DiagnosticOutcome::Success);
                assert_eq!(destination_ip, "172.30.127.152");
                assert_eq!(interface.as_deref(), Some("wlan0"));
                assert!(gateway.is_none());
            }
            other => panic!("a route diagnose settled as {other:?}"),
        }

        let unmatched = run(
            &port,
            &execution,
            NetworkPrimitiveRequest::Diagnose(
                NetworkDiagnoseInput::Route {
                    destination_ip: "8.8.8.8".to_owned(),
                },
                apk_plan(),
            ),
        )
        .expect("the supplement answers a route query");
        match unmatched {
            NetworkPrimitiveOutcome::Diagnose(NetworkDiagnoseResult::Route {
                outcome,
                interface,
                ..
            }) => {
                assert_eq!(outcome, DiagnosticOutcome::NoRoute);
                assert!(interface.is_none());
            }
            other => panic!("a route diagnose settled as {other:?}"),
        }
    }

    /// R-NET-009 `connectivity` reports the App/framework facts it observed, and a
    /// provider that cannot be consulted is a structured error rather than a fabricated
    /// diagnostic outcome.
    #[test]
    fn i8_net_g_diagnose_connectivity_reports_the_observed_facts() {
        let port = ApkNetworkPort::new(
            ScriptedFramework::answering(&snapshot(
                Vec::new(),
                vec![RouteEntry {
                    destination: "0.0.0.0/0".to_owned(),
                    gateway: Some("172.30.126.1".to_owned()),
                    interface: Some("wlan0".to_owned()),
                    metric: None,
                }],
                vec![DnsEntry {
                    server: "172.30.126.1".to_owned(),
                }],
                Some(("100", "wifi")),
            )),
            ScriptedInterfaces::reporting(Vec::new()),
            ScriptedSupplement::unreachable(),
        );
        let execution = admitted();
        let outcome = run(
            &port,
            &execution,
            NetworkPrimitiveRequest::Diagnose(NetworkDiagnoseInput::Connectivity {}, apk_plan()),
        )
        .expect("the App surface answers a connectivity diagnose");
        match outcome {
            NetworkPrimitiveOutcome::Diagnose(NetworkDiagnoseResult::Connectivity {
                outcome,
                active_network_present,
                default_route_present,
                dns_configured,
                ..
            }) => {
                assert_eq!(outcome, DiagnosticOutcome::Success);
                assert_eq!(active_network_present, Some(true));
                assert_eq!(default_route_present, Some(true));
                assert_eq!(dns_configured, Some(true));
            }
            other => panic!("a connectivity diagnose settled as {other:?}"),
        }

        let unavailable = ApkNetworkPort::new(
            ScriptedFramework::unavailable(),
            ScriptedInterfaces::reporting(Vec::new()),
            ScriptedSupplement::unreachable(),
        );
        let failure = run(
            &unavailable,
            &execution,
            NetworkPrimitiveRequest::Diagnose(NetworkDiagnoseInput::Connectivity {}, apk_plan()),
        )
        .expect_err("an unconsultable provider is a structured error");
        assert_eq!(failure.error.code, ErrorCode::CapabilityUnavailable);
    }
}
