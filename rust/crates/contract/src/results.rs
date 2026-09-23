use crate::common::string_enum;
use crate::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesystemDownloadResult {
    pub destination: FileTarget,
    pub size: u64,
    pub sha256: String,
}

string_enum!(ArchiveListOperation { List=>"list" });

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesystemArchiveListResult {
    pub operation: ArchiveListOperation,
    pub entries: Vec<ArchiveEntry>,
    pub truncated: bool,
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "operation")]
pub enum FilesystemArchiveTaskResult {
    #[serde(rename = "extract")]
    Extract {
        destination: FileTarget,
        entries_extracted: u64,
    },
    #[serde(rename = "create")]
    Create {
        destination: FileTarget,
        entries_archived: u64,
        bytes_written: u64,
        sha256: String,
    },
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, untagged)]
pub enum TaskTerminalResult {
    FilesystemDownload(FilesystemDownloadResult),
    FilesystemArchive(FilesystemArchiveTaskResult),
    Command(CommandResult),
    NetworkCapture(CaptureResult),
    Automation(AutomationTaskResult),
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterfaceAddress {
    pub address: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix_length: Option<u8>,
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterfaceEntry {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub up: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtu: Option<u32>,
    pub addresses: Vec<InterfaceAddress>,
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteEntry {
    pub destination: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gateway: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interface: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metric: Option<u64>,
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DnsEntry {
    pub server: String,
}

string_enum!(SocketProtocol { Tcp=>"tcp", Udp=>"udp", Other=>"other" });

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SocketEntry {
    pub protocol: SocketProtocol,
    pub local_address: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uid: Option<u32>,
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkFamilyAvailability {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interfaces: Option<Availability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routes: Option<Availability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dns: Option<Availability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sockets: Option<Availability>,
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkFamilyTruncated {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interfaces: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routes: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dns: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sockets: Option<bool>,
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkInspectResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interfaces: Option<Vec<InterfaceEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routes: Option<Vec<RouteEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dns: Option<Vec<DnsEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sockets: Option<Vec<SocketEntry>>,
    pub availability: NetworkFamilyAvailability,
    pub truncated: NetworkFamilyTruncated,
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureStartResult {
    pub operation: CaptureStartOperation,
    pub capture_id: CaptureId,
    pub task_id: TaskId,
}
string_enum!(CaptureStartOperation { Start=>"start" });

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureResult {
    pub operation: CaptureResultOperation,
    pub capture_id: CaptureId,
    pub packets_captured: u64,
    pub bytes_captured: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination: Option<FileTarget>,
}
string_enum!(CaptureResultOperation { CaptureResult=>"capture_result" });

string_enum!(PacketProtocol { Ethernet=>"ethernet", Ipv4=>"ipv4", Ipv6=>"ipv6", Tcp=>"tcp", Udp=>"udp", Icmp=>"icmp", Other=>"other" });

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PacketSummary {
    pub index: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    pub length: u64,
    pub protocol: PacketProtocol,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub src_mac: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dst_mac: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub src_ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dst_ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub src_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dst_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload_preview_base64: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload_total_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload_truncated: Option<bool>,
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureReadResult {
    pub packets: Vec<PacketSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_offset_packet: Option<u64>,
    pub truncated: bool,
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EthernetHeader {
    pub src_mac: String,
    pub dst_mac: String,
    pub ether_type: u16,
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ipv4Header {
    pub src: String,
    pub dst: String,
    pub ttl: u8,
    pub protocol: u8,
    pub identification: u16,
    pub dont_fragment: bool,
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ipv6Header {
    pub src: String,
    pub dst: String,
    pub hop_limit: u8,
    pub next_header: u8,
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TcpHeader {
    pub src_port: u16,
    pub dst_port: u16,
    pub sequence: u32,
    pub acknowledgement: u32,
    pub flags: Vec<String>,
    pub window: u16,
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UdpHeader {
    pub src_port: u16,
    pub dst_port: u16,
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IcmpHeader {
    pub icmp_type: u8,
    pub code: u8,
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PacketDecodeResult {
    pub length: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ethernet: Option<EthernetHeader>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ipv4: Option<Ipv4Header>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ipv6: Option<Ipv6Header>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tcp: Option<TcpHeader>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub udp: Option<UdpHeader>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icmp: Option<IcmpHeader>,
    pub payload_preview_base64: String,
    pub payload_total_bytes: u64,
    pub payload_truncated: bool,
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PacketBuildResult {
    pub packet_ref: String,
    pub length: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PacketInjectResult {
    pub requested_packets: u64,
    pub accepted_packets: u64,
    pub bytes_accepted: u64,
}

string_enum!(DiagnosticOutcome { Success=>"success", NotFound=>"not_found", Refused=>"refused", Timeout=>"timeout", Unreachable=>"unreachable", DnsError=>"dns_error", TlsError=>"tls_error", NoRoute=>"no_route" });

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "test")]
pub enum NetworkDiagnoseResult {
    #[serde(rename = "connectivity")]
    Connectivity {
        outcome: DiagnosticOutcome,
        duration_ms: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        active_network_present: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        default_route_present: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        dns_configured: Option<bool>,
    },
    #[serde(rename = "dns")]
    Dns {
        outcome: DiagnosticOutcome,
        duration_ms: u64,
        name: String,
        record_type: DnsRecordType,
        addresses: Vec<String>,
    },
    #[serde(rename = "tcp")]
    Tcp {
        outcome: DiagnosticOutcome,
        duration_ms: u64,
        host: String,
        port: u16,
        #[serde(skip_serializing_if = "Option::is_none")]
        remote_ip: Option<String>,
    },
    #[serde(rename = "tls")]
    Tls {
        outcome: DiagnosticOutcome,
        duration_ms: u64,
        host: String,
        port: u16,
        server_name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        remote_ip: Option<String>,
        certificate_verified: bool,
    },
    #[serde(rename = "route")]
    Route {
        outcome: DiagnosticOutcome,
        duration_ms: u64,
        destination_ip: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        interface: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        gateway: Option<String>,
    },
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisplayGeometry {
    pub width: u32,
    pub height: u32,
    pub rotation: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub density_dpi: Option<u32>,
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForegroundFact {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activity: Option<String>,
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeBounds {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualNode {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub class_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package_name: Option<String>,
    pub bounds: NodeBounds,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clickable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focusable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focused: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scrollable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub long_clickable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub editable: Option<bool>,
}

/// What the caller may address in this observation before it expires. Coordinate and node targets are
/// both bound to the observed scene. As everywhere else in this result, an absent reason means the
/// capability is available.
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualInteractFact {
    pub coordinate: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_unavailable_reason: Option<String>,
    pub ttl_ms: u64,
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualObserveResult {
    pub observation_id: UuidV4,
    pub observed_at: String,
    pub display: DisplayGeometry,
    pub interact: VisualInteractFact,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub foreground: Option<ForegroundFact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_format: Option<ImageFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_unavailable_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nodes: Option<Vec<VisualNode>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nodes_unavailable_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nodes_truncated: Option<bool>,
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "operation")]
pub enum AndroidPackageResult {
    #[serde(rename = "inspect")]
    Inspect { package: PackageFact },
    #[serde(rename = "list")]
    List {
        packages: Vec<PackageFact>,
        truncated: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        next_after_package: Option<String>,
    },
    #[serde(rename = "force_stop")]
    ForceStop {
        package_name: String,
        completed: True,
    },
}

string_enum!(IntentOperation { View=>"view", ExplicitActivity=>"explicit_activity" });
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AndroidIntentResult {
    pub started: True,
    pub operation: IntentOperation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component: Option<ComponentName>,
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "operation")]
pub enum AndroidClipboardResult {
    #[serde(rename = "read")]
    Read {
        has_text: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
    #[serde(rename = "write")]
    Write { written: True },
    #[serde(rename = "clear")]
    Clear { cleared: True },
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotificationSummary {
    pub notification_ref: String,
    pub expires_at: String,
    pub package_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub posted_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub action_count: u32,
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotificationAction {
    pub index: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub requires_remote_input: bool,
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "operation")]
pub enum AndroidNotificationResult {
    #[serde(rename = "list")]
    List {
        notifications: Vec<NotificationSummary>,
    },
    #[serde(rename = "get")]
    Get {
        notification: NotificationSummary,
        actions: Vec<NotificationAction>,
        actions_truncated: bool,
    },
    #[serde(rename = "dismiss")]
    Dismiss {
        notification_ref: String,
        dismissed: True,
    },
    #[serde(rename = "invoke_action")]
    InvokeAction {
        notification_ref: String,
        action_index: u8,
        invoked: True,
    },
}
