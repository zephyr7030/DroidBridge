use crate::common::string_enum;
use crate::*;
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de};
use std::collections::BTreeMap;
fn d1() -> u32 {
    1
}
fn d50() -> u32 {
    50
}
fn d64() -> u8 {
    64
}
fn d100() -> u32 {
    100
}
fn d200() -> u32 {
    200
}
fn d300() -> u64 {
    300
}
fn d443() -> u16 {
    443
}
fn d500() -> u32 {
    500
}
fn d5000() -> u64 {
    5_000
}
fn d10k() -> u64 {
    10_000
}
fn d30s() -> u64 {
    30_000
}
fn d60s() -> u64 {
    60_000
}
fn d64k() -> u64 {
    65_536
}
fn d64m() -> u64 {
    67_108_864
}
fn d120s() -> u64 {
    120_000
}
fn d65535() -> u16 {
    65_535
}
fn yes() -> bool {
    true
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "tool")]
#[schemars(deny_unknown_fields)]
pub enum PublicPayload {
    #[serde(rename = "context")]
    Context {
        #[serde(flatten)]
        call: ContextCall,
    },
    #[serde(rename = "filesystem")]
    Filesystem {
        #[serde(flatten)]
        call: FilesystemCall,
    },
    #[serde(rename = "command")]
    Command {
        #[serde(flatten)]
        call: CommandCall,
    },
    #[serde(rename = "network")]
    Network {
        #[serde(flatten)]
        call: NetworkCall,
    },
    #[serde(rename = "visual")]
    Visual {
        #[serde(flatten)]
        call: VisualCall,
    },
    #[serde(rename = "android")]
    Android {
        #[serde(flatten)]
        call: AndroidCall,
    },
    #[serde(rename = "automation")]
    Automation {
        #[serde(flatten)]
        call: AutomationCall,
    },
    #[serde(rename = "task_control")]
    TaskControl {
        #[serde(flatten)]
        call: TaskControlCall,
    },
}

impl<'de> Deserialize<'de> for PublicPayload {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut value = serde_json::Value::deserialize(deserializer)?;
        let object = value
            .as_object_mut()
            .ok_or_else(|| de::Error::custom("payload must be an object"))?;
        if object.len() != 3
            || !object.contains_key("tool")
            || !object.contains_key("action")
            || !object.contains_key("input")
        {
            return Err(de::Error::custom(
                "payload must contain exactly tool, action, and input",
            ));
        }
        let tool = object
            .remove("tool")
            .and_then(|value| value.as_str().map(ToOwned::to_owned))
            .ok_or_else(|| de::Error::custom("tool must be a string"))?;
        let call = serde_json::Value::Object(object.clone());
        macro_rules! decode {
            ($type:ty, $variant:ident) => {
                serde_json::from_value::<$type>(call)
                    .map(|call| Self::$variant { call })
                    .map_err(de::Error::custom)
            };
        }
        match tool.as_str() {
            "context" => decode!(ContextCall, Context),
            "filesystem" => decode!(FilesystemCall, Filesystem),
            "command" => decode!(CommandCall, Command),
            "network" => decode!(NetworkCall, Network),
            "visual" => decode!(VisualCall, Visual),
            "android" => decode!(AndroidCall, Android),
            "automation" => decode!(AutomationCall, Automation),
            "task_control" => decode!(TaskControlCall, TaskControl),
            _ => Err(de::Error::custom("unknown mother tool")),
        }
    }
}

string_enum!(ContextDetail{Compact=>"compact",Full=>"full"});
#[allow(clippy::derivable_impls)]
impl Default for ContextDetail {
    fn default() -> Self {
        Self::Compact
    }
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "action", content = "input")]
pub enum ContextCall {
    #[serde(rename = "status")]
    Status(ContextStatusInput),
    #[serde(rename = "catalog")]
    Catalog(ContextCatalogInput),
}
#[derive(Clone, Debug, Default, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextStatusInput {
    #[serde(default)]
    pub detail: ContextDetail,
}
#[derive(Clone, Debug, Default, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCatalogInput {
    #[serde(default)]
    pub namespace: String,
    #[serde(default)]
    pub detail: ContextDetail,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceCompact {
    pub sdk_int: u32,
    pub abi: String,
    pub timezone: String,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceFull {
    pub sdk_int: u32,
    pub abi: String,
    pub timezone: String,
    pub manufacturer: String,
    pub model: String,
    pub device: String,
    pub build_fingerprint: String,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeStatus {
    pub host: RuntimeHost,
    pub host_generation: u64,
    pub readiness: RuntimeReadiness,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextStatusCompact {
    pub device: DeviceCompact,
    pub runtime: RuntimeStatus,
    pub capabilities: EffectiveCapabilities,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextStatusFull {
    pub device: DeviceFull,
    pub runtime: RuntimeStatus,
    pub capabilities: EffectiveCapabilities,
    pub grants: GrantFacts,
    pub components: ComponentStatus,
    pub compatibility: Compatibility,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VersionFact {
    pub version_name: String,
    pub version_code: u64,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeComponent {
    pub host: RuntimeHost,
    pub component_version: String,
    pub protocol_version: u32,
    pub store_schema_version: u32,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationFact {
    pub integration_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manager_version: Option<String>,
    pub state: CapabilityState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MagiskComponent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub daemon_version: Option<String>,
    pub state: CapabilityState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentStatus {
    pub apk: VersionFact,
    pub runtime_host: RuntimeComponent,
    pub shizuku: IntegrationFact,
    pub magisk: MagiskComponent,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Compatibility {
    pub protocol: CompatibilityState,
    pub store_schema: CompatibilityState,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogAction {
    pub name: String,
    pub automation_compatible: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capability_requirement: Option<String>,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCatalogResult {
    pub current_namespace: String,
    pub root_tools: Vec<MotherTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    pub siblings: Vec<MotherTool>,
    pub actions: Vec<CatalogAction>,
}

string_enum!(FileType{File=>"file",Directory=>"directory",Symlink=>"symlink",Other=>"other"});
string_enum!(DataEncoding{Utf8=>"utf8",Base64=>"base64"});
#[allow(clippy::derivable_impls)]
impl Default for DataEncoding {
    fn default() -> Self {
        Self::Utf8
    }
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "action", content = "input")]
pub enum FilesystemCall {
    #[serde(rename = "inspect")]
    Inspect(FilesystemInspectInput),
    #[serde(rename = "read")]
    Read(FilesystemReadInput),
    #[serde(rename = "write")]
    Write(FilesystemWriteInput),
    #[serde(rename = "manage")]
    Manage(FilesystemManageInput),
    #[serde(rename = "download")]
    Download(FilesystemDownloadInput),
    #[serde(rename = "archive")]
    Archive(FilesystemArchiveInput),
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesystemInspectInput {
    pub target: FileTarget,
    #[serde(default)]
    pub recursive: bool,
    #[serde(default = "d1")]
    #[schemars(range(min = 1, max = 16))]
    pub max_depth: u32,
    #[serde(default = "d200")]
    #[schemars(range(min = 1, max = 5000))]
    pub max_entries: u32,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileEntry {
    pub name: String,
    #[serde(rename = "type")]
    pub entry_type: FileType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<String>,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesystemInspectResult {
    pub target: FileTarget,
    #[serde(rename = "type")]
    pub target_type: FileType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entries: Option<Vec<FileEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, untagged)]
pub enum ReadSource {
    Target { target: FileTarget },
    DataRef { data_ref: String },
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FilesystemReadInput {
    #[serde(flatten)]
    pub source: ReadSource,
    #[serde(default)]
    pub offset: u64,
    #[serde(default = "d64k")]
    #[schemars(range(min = 1, max = 1048576))]
    pub max_bytes: u64,
    #[serde(default)]
    pub encoding: DataEncoding,
}

impl<'de> Deserialize<'de> for FilesystemReadInput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            target: Option<FileTarget>,
            data_ref: Option<String>,
            #[serde(default)]
            offset: u64,
            #[serde(default = "d64k")]
            max_bytes: u64,
            #[serde(default)]
            encoding: DataEncoding,
        }

        let wire = Wire::deserialize(deserializer)?;
        let source = match (wire.target, wire.data_ref) {
            (Some(target), None) => ReadSource::Target { target },
            (None, Some(data_ref)) => ReadSource::DataRef { data_ref },
            _ => {
                return Err(de::Error::custom(
                    "filesystem.read requires exactly one of target or data_ref",
                ));
            }
        };
        Ok(Self {
            source,
            offset: wire.offset,
            max_bytes: wire.max_bytes,
            encoding: wire.encoding,
        })
    }
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesystemReadResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_ref: Option<String>,
    pub returned_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_size: Option<u64>,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "mode")]
pub enum FilesystemWriteInput {
    #[serde(rename = "create")]
    Create {
        target: FileTarget,
        content: String,
        #[serde(default)]
        encoding: DataEncoding,
    },
    #[serde(rename = "replace")]
    Replace {
        target: FileTarget,
        content: String,
        #[serde(default)]
        encoding: DataEncoding,
    },
    #[serde(rename = "edit")]
    Edit {
        target: FileTarget,
        #[schemars(length(min = 1, max = 100))]
        replacements: Vec<Replacement>,
    },
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Replacement {
    pub old: String,
    pub new: String,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesystemWriteResult {
    pub bytes_written: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "operation")]
pub enum FilesystemManageInput {
    #[serde(rename = "mkdir")]
    Mkdir {
        target: FileTarget,
        #[serde(default)]
        parents: bool,
    },
    #[serde(rename = "copy")]
    Copy {
        source: FileTarget,
        destination: FileTarget,
        #[serde(default)]
        recursive: bool,
        #[serde(default)]
        overwrite: bool,
    },
    #[serde(rename = "move")]
    Move {
        source: FileTarget,
        destination: FileTarget,
        #[serde(default)]
        recursive: bool,
        #[serde(default)]
        overwrite: bool,
    },
    #[serde(rename = "delete")]
    Delete {
        target: FileTarget,
        #[serde(default)]
        recursive: bool,
    },
}
string_enum!(ManageOperation{Mkdir=>"mkdir",Copy=>"copy",Move=>"move",Delete=>"delete"});
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesystemManageResult {
    pub operation: ManageOperation,
    pub completed: True,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<FileTarget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<FileTarget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination: Option<FileTarget>,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesystemDownloadInput {
    pub url: String,
    pub destination: FileTarget,
    #[serde(default)]
    pub overwrite: bool,
    #[serde(default = "d120s")]
    #[schemars(range(min = 1000, max = 3600000))]
    pub timeout_ms: u64,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskAccepted {
    pub task_id: TaskId,
}
string_enum!(ArchiveFormat{Zip=>"zip",Tar=>"tar",TarGz=>"tar_gz"});
string_enum!(ArchiveEntryType{File=>"file",Directory=>"directory",Symlink=>"symlink",Other=>"other"});
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "operation")]
pub enum FilesystemArchiveInput {
    #[serde(rename = "list")]
    List {
        target: FileTarget,
        #[serde(default = "d200")]
        #[schemars(range(min = 1, max = 5000))]
        max_entries: u32,
    },
    #[serde(rename = "extract")]
    Extract {
        target: FileTarget,
        destination: FileTarget,
        #[serde(default)]
        overwrite: bool,
    },
    #[serde(rename = "create")]
    Create {
        #[schemars(length(min = 1, max = 1000))]
        sources: Vec<FileTarget>,
        destination: FileTarget,
        format: ArchiveFormat,
        #[serde(default)]
        overwrite: bool,
    },
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveEntry {
    pub path: String,
    #[serde(rename = "type")]
    pub entry_type: ArchiveEntryType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

string_enum!(RunAs{App=>"app",Shell=>"shell",Root=>"root"});
string_enum!(CommandTerminalState{Completed=>"completed",Failed=>"failed"});
string_enum!(CommandFailureCode{ExecutionFailed=>"EXECUTION_FAILED",Timeout=>"TIMEOUT"});
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "action", content = "input")]
pub enum CommandCall {
    #[serde(rename = "run")]
    Run(CommandRunInput),
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandRunInput {
    pub command: String,
    pub run_as: RunAs,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdin: Option<String>,
    #[serde(default = "d30s")]
    #[schemars(range(min = 1000, max = 3600000))]
    pub timeout_ms: u64,
    #[serde(default = "d64k")]
    #[schemars(range(min = 1024, max = 1048576))]
    pub max_output_bytes: u64,
    #[serde(default)]
    pub as_task: bool,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandResult {
    pub state: CommandTerminalState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<CommandFailureCode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub requested_run_as: RunAs,
    pub actual_run_as: RunAs,
    pub execution_class: ExecutionClass,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout_ref: Option<String>,
    pub stdout_truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr_ref: Option<String>,
    pub stderr_truncated: bool,
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "action", content = "input")]
pub enum TaskControlCall {
    #[serde(rename = "list")]
    List(TaskListInput),
    #[serde(rename = "get")]
    Get(TaskGetInput),
    #[serde(rename = "cancel")]
    Cancel(TaskGetInput),
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskListInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 7))]
    pub states: Option<Vec<TaskState>>,
    #[serde(default = "d100")]
    #[schemars(range(min = 1, max = 500))]
    pub limit: u32,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskGetInput {
    pub task_id: TaskId,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskSummary {
    pub task_id: TaskId,
    pub state: TaskState,
    pub tool: MotherTool,
    pub action: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waiting_reason: Option<String>,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskListResult {
    pub tasks: Vec<TaskSummary>,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskSnapshot {
    pub task_id: TaskId,
    pub state: TaskState,
    pub tool: MotherTool,
    pub action: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    pub cancel_requested: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waiting_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_class: Option<ExecutionClass>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<TaskTerminalResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<PublicError>,
}

string_enum!(NetworkScope{Interfaces=>"interfaces",Routes=>"routes",Dns=>"dns",Sockets=>"sockets",All=>"all"});
string_enum!(DnsRecordType{A=>"A",Aaaa=>"AAAA"});
#[allow(clippy::derivable_impls)]
impl Default for DnsRecordType {
    fn default() -> Self {
        Self::A
    }
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "action", content = "input")]
pub enum NetworkCall {
    #[serde(rename = "inspect")]
    Inspect(NetworkInspectInput),
    #[serde(rename = "capture")]
    Capture(NetworkCaptureInput),
    #[serde(rename = "packet")]
    Packet(NetworkPacketInput),
    #[serde(rename = "diagnose")]
    Diagnose(NetworkDiagnoseInput),
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkInspectInput {
    pub scope: NetworkScope,
    #[serde(default = "d200")]
    #[schemars(range(min = 1, max = 5000))]
    pub max_entries: u32,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, untagged)]
pub enum CaptureReadSource {
    CaptureRef { capture_ref: String },
    File { file: FileTarget },
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields, tag = "operation")]
pub enum NetworkCaptureInput {
    #[serde(rename = "start")]
    Start {
        interface: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        filter: Option<String>,
        #[serde(default = "d10k")]
        #[schemars(range(min = 1, max = 1000000))]
        max_packets: u64,
        #[serde(default = "d64m")]
        #[schemars(range(min = 1, max = 268435456))]
        max_bytes: u64,
        #[serde(default = "d60s")]
        #[schemars(range(min = 1, max = 3600000))]
        max_duration_ms: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        persist_to: Option<FileTarget>,
    },
    #[serde(rename = "stop")]
    Stop { capture_id: CaptureId },
    #[serde(rename = "read")]
    Read {
        #[serde(flatten)]
        source: CaptureReadSource,
        #[serde(default)]
        offset_packet: u64,
        #[serde(default = "d200")]
        #[schemars(range(min = 1, max = 5000))]
        max_packets: u32,
        #[serde(default)]
        include_payload: bool,
    },
}

impl<'de> Deserialize<'de> for NetworkCaptureInput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields, tag = "operation")]
        enum Wire {
            #[serde(rename = "start")]
            Start {
                interface: String,
                filter: Option<String>,
                #[serde(default = "d10k")]
                max_packets: u64,
                #[serde(default = "d64m")]
                max_bytes: u64,
                #[serde(default = "d60s")]
                max_duration_ms: u64,
                persist_to: Option<FileTarget>,
            },
            #[serde(rename = "stop")]
            Stop { capture_id: CaptureId },
            #[serde(rename = "read")]
            Read {
                capture_ref: Option<String>,
                file: Option<FileTarget>,
                #[serde(default)]
                offset_packet: u64,
                #[serde(default = "d200")]
                max_packets: u32,
                #[serde(default)]
                include_payload: bool,
            },
        }

        Ok(match Wire::deserialize(deserializer)? {
            Wire::Start {
                interface,
                filter,
                max_packets,
                max_bytes,
                max_duration_ms,
                persist_to,
            } => Self::Start {
                interface,
                filter,
                max_packets,
                max_bytes,
                max_duration_ms,
                persist_to,
            },
            Wire::Stop { capture_id } => Self::Stop { capture_id },
            Wire::Read {
                capture_ref,
                file,
                offset_packet,
                max_packets,
                include_payload,
            } => {
                let source = match (capture_ref, file) {
                    (Some(capture_ref), None) => CaptureReadSource::CaptureRef { capture_ref },
                    (None, Some(file)) => CaptureReadSource::File { file },
                    _ => {
                        return Err(de::Error::custom(
                            "network.capture read requires exactly one of capture_ref or file",
                        ));
                    }
                };
                Self::Read {
                    source,
                    offset_packet,
                    max_packets,
                    include_payload,
                }
            }
        })
    }
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, untagged)]
pub enum PacketDecodeSource {
    Raw { raw_base64: String },
    PacketRef { packet_ref: String },
    Capture { capture_ref: String, index: u64 },
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, untagged)]
pub enum PacketSource {
    Raw { raw_base64: String },
    Ref { packet_ref: String },
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EthernetBuild {
    pub src_mac: String,
    pub dst_mac: String,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "type")]
pub enum NetworkBuild {
    #[serde(rename = "ipv4")]
    Ipv4 {
        src: String,
        dst: String,
        #[serde(default = "d64")]
        ttl: u8,
        #[serde(default)]
        identification: u16,
        #[serde(default)]
        dont_fragment: bool,
    },
    #[serde(rename = "ipv6")]
    Ipv6 {
        src: String,
        dst: String,
        #[serde(default = "d64")]
        hop_limit: u8,
    },
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "type")]
pub enum TransportBuild {
    #[serde(rename = "tcp")]
    Tcp {
        src_port: u16,
        dst_port: u16,
        #[serde(default)]
        sequence: u32,
        #[serde(default)]
        acknowledgement: u32,
        #[serde(default)]
        #[schemars(length(max = 8))]
        flags: Vec<String>,
        #[serde(default = "d65535")]
        window: u16,
    },
    #[serde(rename = "udp")]
    Udp { src_port: u16, dst_port: u16 },
    #[serde(rename = "icmp")]
    Icmp { icmp_type: u8, code: u8 },
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields, tag = "operation")]
pub enum NetworkPacketInput {
    #[serde(rename = "decode")]
    Decode {
        #[serde(flatten)]
        source: PacketDecodeSource,
    },
    #[serde(rename = "build")]
    Build {
        #[serde(skip_serializing_if = "Option::is_none")]
        ethernet: Option<EthernetBuild>,
        network: NetworkBuild,
        transport: TransportBuild,
        #[serde(skip_serializing_if = "Option::is_none")]
        payload_base64: Option<String>,
    },
    #[serde(rename = "inject")]
    Inject {
        interface: String,
        packet: PacketSource,
        #[serde(default = "d1")]
        #[schemars(range(min = 1, max = 100))]
        count: u32,
        #[serde(default)]
        #[schemars(range(max = 60000))]
        interval_ms: u64,
    },
}

/// Serde refuses every field of a flattened source under `deny_unknown_fields`, so the decode
/// source is read from explicit wire fields and must name exactly one R-NET-006 source.
impl<'de> Deserialize<'de> for NetworkPacketInput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields, tag = "operation")]
        enum Wire {
            #[serde(rename = "decode")]
            Decode {
                raw_base64: Option<String>,
                packet_ref: Option<String>,
                capture_ref: Option<String>,
                index: Option<u64>,
            },
            #[serde(rename = "build")]
            Build {
                ethernet: Option<EthernetBuild>,
                network: NetworkBuild,
                transport: TransportBuild,
                payload_base64: Option<String>,
            },
            #[serde(rename = "inject")]
            Inject {
                interface: String,
                packet: PacketSource,
                #[serde(default = "d1")]
                count: u32,
                #[serde(default)]
                interval_ms: u64,
            },
        }

        Ok(match Wire::deserialize(deserializer)? {
            Wire::Decode {
                raw_base64,
                packet_ref,
                capture_ref,
                index,
            } => {
                let source = match (raw_base64, packet_ref, capture_ref, index) {
                    (Some(raw_base64), None, None, None) => PacketDecodeSource::Raw { raw_base64 },
                    (None, Some(packet_ref), None, None) => {
                        PacketDecodeSource::PacketRef { packet_ref }
                    }
                    (None, None, Some(capture_ref), Some(index)) => {
                        PacketDecodeSource::Capture { capture_ref, index }
                    }
                    _ => {
                        return Err(de::Error::custom(
                            "network.packet decode requires exactly one of raw_base64, packet_ref or capture_ref with index",
                        ));
                    }
                };
                Self::Decode { source }
            }
            Wire::Build {
                ethernet,
                network,
                transport,
                payload_base64,
            } => Self::Build {
                ethernet,
                network,
                transport,
                payload_base64,
            },
            Wire::Inject {
                interface,
                packet,
                count,
                interval_ms,
            } => Self::Inject {
                interface,
                packet,
                count,
                interval_ms,
            },
        })
    }
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "test")]
pub enum NetworkDiagnoseInput {
    #[serde(rename = "connectivity")]
    Connectivity {},
    #[serde(rename = "dns")]
    Dns {
        name: String,
        #[serde(default)]
        record_type: DnsRecordType,
    },
    #[serde(rename = "tcp")]
    Tcp {
        host: String,
        #[schemars(range(min = 1))]
        port: u16,
        #[serde(default = "d5000")]
        #[schemars(range(min = 100, max = 60000))]
        timeout_ms: u64,
    },
    #[serde(rename = "tls")]
    Tls {
        host: String,
        #[serde(default = "d443")]
        #[schemars(range(min = 1))]
        port: u16,
        #[serde(skip_serializing_if = "Option::is_none")]
        server_name: Option<String>,
        #[serde(default = "d5000")]
        #[schemars(range(min = 100, max = 60000))]
        timeout_ms: u64,
    },
    #[serde(rename = "route")]
    Route { destination_ip: String },
}

string_enum!(ImageFormat{Heic=>"heic",Jpeg=>"jpeg",Png=>"png"});
string_enum!(InteractionTarget{Node=>"node",Coordinate=>"coordinate",Focused=>"focused"});
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "action", content = "input")]
pub enum VisualCall {
    #[serde(rename = "observe")]
    Observe(VisualObserveInput),
    #[serde(rename = "view")]
    View(VisualViewInput),
    #[serde(rename = "interact")]
    Interact(VisualInteractInput),
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualObserveInput {
    #[serde(default = "yes")]
    pub include_image: bool,
    #[serde(default = "yes")]
    pub include_nodes: bool,
    #[serde(default = "d500")]
    #[schemars(range(min = 1, max = 5000))]
    pub max_nodes: u32,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, untagged)]
pub enum VisualSource {
    Path { path: String },
    ContentUri { content_uri: String },
    ImageRef { image_ref: String },
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VisualViewInput {
    #[serde(flatten)]
    pub source: VisualSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<Region>,
}

impl<'de> Deserialize<'de> for VisualViewInput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            path: Option<String>,
            content_uri: Option<String>,
            image_ref: Option<String>,
            region: Option<Region>,
        }

        let wire = Wire::deserialize(deserializer)?;
        let source = match (wire.path, wire.content_uri, wire.image_ref) {
            (Some(path), None, None) => VisualSource::Path { path },
            (None, Some(content_uri), None) => VisualSource::ContentUri { content_uri },
            (None, None, Some(image_ref)) => VisualSource::ImageRef { image_ref },
            _ => {
                return Err(de::Error::custom(
                    "visual.view requires exactly one of path, content_uri, or image_ref",
                ));
            }
        };
        Ok(Self {
            source,
            region: wire.region,
        })
    }
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "target")]
pub enum PointTarget {
    #[serde(rename = "node")]
    Node { node_ref: String },
    #[serde(rename = "coordinate")]
    Coordinate {
        observation_id: UuidV4,
        x: u32,
        y: u32,
    },
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "operation")]
#[schemars(deny_unknown_fields)]
pub enum VisualInteractInput {
    #[serde(rename = "tap")]
    Tap {
        #[serde(flatten)]
        target: PointTarget,
    },
    #[serde(rename = "long_press")]
    LongPress {
        #[serde(flatten)]
        target: PointTarget,
    },
    #[serde(rename = "swipe")]
    Swipe {
        observation_id: UuidV4,
        from_x: u32,
        from_y: u32,
        to_x: u32,
        to_y: u32,
        #[serde(default = "d300")]
        #[schemars(range(min = 1, max = 10000))]
        duration_ms: u64,
    },
    #[serde(rename = "text")]
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        node_ref: Option<String>,
    },
    #[serde(rename = "key")]
    Key {
        key_code: i32,
        #[serde(default)]
        meta_state: i32,
    },
}

#[derive(Deserialize)]
#[serde(tag = "operation")]
enum VisualInteractWire {
    #[serde(rename = "tap")]
    Tap {
        #[serde(flatten)]
        target: PointTarget,
    },
    #[serde(rename = "long_press")]
    LongPress {
        #[serde(flatten)]
        target: PointTarget,
    },
    #[serde(rename = "swipe")]
    Swipe {
        observation_id: UuidV4,
        from_x: u32,
        from_y: u32,
        to_x: u32,
        to_y: u32,
        #[serde(default = "d300")]
        duration_ms: u64,
    },
    #[serde(rename = "text")]
    Text {
        text: String,
        node_ref: Option<String>,
    },
    #[serde(rename = "key")]
    Key {
        key_code: i32,
        #[serde(default)]
        meta_state: i32,
    },
}

impl From<VisualInteractWire> for VisualInteractInput {
    fn from(value: VisualInteractWire) -> Self {
        match value {
            VisualInteractWire::Tap { target } => Self::Tap { target },
            VisualInteractWire::LongPress { target } => Self::LongPress { target },
            VisualInteractWire::Swipe {
                observation_id,
                from_x,
                from_y,
                to_x,
                to_y,
                duration_ms,
            } => Self::Swipe {
                observation_id,
                from_x,
                from_y,
                to_x,
                to_y,
                duration_ms,
            },
            VisualInteractWire::Text { text, node_ref } => Self::Text { text, node_ref },
            VisualInteractWire::Key {
                key_code,
                meta_state,
            } => Self::Key {
                key_code,
                meta_state,
            },
        }
    }
}

impl<'de> Deserialize<'de> for VisualInteractInput {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        let object = value
            .as_object()
            .ok_or_else(|| de::Error::custom("visual interaction input must be an object"))?;
        let operation = object
            .get("operation")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| de::Error::custom("visual interaction operation must be a string"))?;
        let allowed: &[&str] = match operation {
            "tap" | "long_press" => {
                match object.get("target").and_then(serde_json::Value::as_str) {
                    Some("node") => &["operation", "target", "node_ref"],
                    Some("coordinate") => &["operation", "target", "observation_id", "x", "y"],
                    _ => return Err(de::Error::custom("unknown visual interaction target")),
                }
            }
            "swipe" => &[
                "operation",
                "observation_id",
                "from_x",
                "from_y",
                "to_x",
                "to_y",
                "duration_ms",
            ],
            "text" => &["operation", "text", "node_ref"],
            "key" => &["operation", "key_code", "meta_state"],
            _ => return Err(de::Error::custom("unknown visual interaction operation")),
        };
        if object.keys().any(|key| !allowed.contains(&key.as_str())) {
            return Err(de::Error::custom("unknown visual interaction field"));
        }
        serde_json::from_value::<VisualInteractWire>(value)
            .map(Into::into)
            .map_err(de::Error::custom)
    }
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualViewResult {
    pub image_ref: String,
    pub width: u32,
    pub height: u32,
    pub format: ImageFormat,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualInteractResult {
    pub delivered: True,
    pub operation: String,
    pub target: InteractionTarget,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_class: Option<ExecutionClass>,
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "action", content = "input")]
pub enum AndroidCall {
    #[serde(rename = "package")]
    Package(AndroidPackageInput),
    #[serde(rename = "launch")]
    Launch(AndroidLaunchInput),
    #[serde(rename = "intent")]
    Intent(AndroidIntentInput),
    #[serde(rename = "clipboard")]
    Clipboard(AndroidClipboardInput),
    #[serde(rename = "notification")]
    Notification(AndroidNotificationInput),
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "operation")]
pub enum AndroidPackageInput {
    #[serde(rename = "inspect")]
    Inspect { package_name: String },
    #[serde(rename = "list")]
    List {
        #[serde(default)]
        include_system: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        after_package: Option<String>,
        #[serde(default = "d100")]
        #[schemars(range(min = 1, max = 200))]
        limit: u32,
    },
    #[serde(rename = "force_stop")]
    ForceStop { package_name: String },
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "operation")]
pub enum AndroidLaunchInput {
    #[serde(rename = "package")]
    Package { package_name: String },
    #[serde(rename = "component")]
    Component {
        package_name: String,
        class_name: String,
    },
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, untagged)]
pub enum IntentExtraValue {
    Boolean(bool),
    Integer(i64),
    String(String),
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "operation")]
pub enum AndroidIntentInput {
    #[serde(rename = "view")]
    View {
        data_uri: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        package_name: Option<String>,
    },
    #[serde(rename = "explicit_activity")]
    ExplicitActivity {
        package_name: String,
        class_name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        action: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        data_uri: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        extras: Option<BTreeMap<String, IntentExtraValue>>,
    },
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "operation")]
pub enum AndroidClipboardInput {
    #[serde(rename = "read")]
    Read {},
    #[serde(rename = "write")]
    Write { text: String },
    #[serde(rename = "clear")]
    Clear {},
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "operation")]
pub enum AndroidNotificationInput {
    #[serde(rename = "list")]
    List {
        #[serde(default = "d50")]
        #[schemars(range(min = 1, max = 100))]
        limit: u32,
    },
    #[serde(rename = "get")]
    Get { notification_ref: String },
    #[serde(rename = "dismiss")]
    Dismiss { notification_ref: String },
    #[serde(rename = "invoke_action")]
    InvokeAction {
        notification_ref: String,
        #[schemars(range(max = 31))]
        action_index: u8,
    },
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageFact {
    pub package_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_code: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launchable: Option<bool>,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchResult {
    pub launched: True,
    pub package_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component: Option<ComponentName>,
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "action", content = "input")]
pub enum AutomationCall {
    #[serde(rename = "list")]
    List(AutomationListInput),
    #[serde(rename = "get")]
    Get(AutomationGetInput),
    #[serde(rename = "save")]
    Save(AutomationSaveInput),
    #[serde(rename = "set_enabled")]
    SetEnabled(AutomationSetEnabledInput),
    #[serde(rename = "delete")]
    Delete(AutomationDeleteInput),
    #[serde(rename = "run")]
    Run(AutomationRunInput),
}
pub const ROOT_TOOL_ORDER: [MotherTool; 8] = [
    MotherTool::Context,
    MotherTool::Filesystem,
    MotherTool::Command,
    MotherTool::Network,
    MotherTool::Visual,
    MotherTool::Android,
    MotherTool::Automation,
    MotherTool::TaskControl,
];
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActionSpec {
    pub tool: &'static str,
    pub action: &'static str,
    pub automation_compatible: bool,
    pub capability_requirement: &'static str,
}

pub const ACTION_SPECS: [ActionSpec; 30] = [
    ActionSpec {
        tool: "context",
        action: "status",
        automation_compatible: false,
        capability_requirement: "none",
    },
    ActionSpec {
        tool: "context",
        action: "catalog",
        automation_compatible: false,
        capability_requirement: "none",
    },
    ActionSpec {
        tool: "filesystem",
        action: "inspect",
        automation_compatible: false,
        capability_requirement: "dynamic",
    },
    ActionSpec {
        tool: "filesystem",
        action: "read",
        automation_compatible: false,
        capability_requirement: "dynamic",
    },
    ActionSpec {
        tool: "filesystem",
        action: "write",
        automation_compatible: false,
        capability_requirement: "dynamic",
    },
    ActionSpec {
        tool: "filesystem",
        action: "manage",
        automation_compatible: true,
        capability_requirement: "dynamic",
    },
    ActionSpec {
        tool: "filesystem",
        action: "download",
        automation_compatible: true,
        capability_requirement: "dynamic",
    },
    ActionSpec {
        tool: "filesystem",
        action: "archive",
        automation_compatible: false,
        capability_requirement: "dynamic",
    },
    ActionSpec {
        tool: "command",
        action: "run",
        automation_compatible: true,
        capability_requirement: "dynamic",
    },
    ActionSpec {
        tool: "network",
        action: "inspect",
        automation_compatible: false,
        capability_requirement: "network.inspect",
    },
    ActionSpec {
        tool: "network",
        action: "capture",
        automation_compatible: false,
        capability_requirement: "network.capture",
    },
    ActionSpec {
        tool: "network",
        action: "packet",
        automation_compatible: false,
        capability_requirement: "dynamic",
    },
    ActionSpec {
        tool: "network",
        action: "diagnose",
        automation_compatible: true,
        capability_requirement: "dynamic",
    },
    ActionSpec {
        tool: "visual",
        action: "observe",
        automation_compatible: false,
        capability_requirement: "dynamic",
    },
    ActionSpec {
        tool: "visual",
        action: "view",
        automation_compatible: false,
        capability_requirement: "visual.image",
    },
    ActionSpec {
        tool: "visual",
        action: "interact",
        automation_compatible: true,
        capability_requirement: "dynamic",
    },
    ActionSpec {
        tool: "android",
        action: "package",
        automation_compatible: false,
        capability_requirement: "dynamic",
    },
    ActionSpec {
        tool: "android",
        action: "launch",
        automation_compatible: true,
        capability_requirement: "dynamic",
    },
    ActionSpec {
        tool: "android",
        action: "intent",
        automation_compatible: false,
        capability_requirement: "dynamic",
    },
    ActionSpec {
        tool: "android",
        action: "clipboard",
        automation_compatible: true,
        capability_requirement: "dynamic",
    },
    ActionSpec {
        tool: "android",
        action: "notification",
        automation_compatible: false,
        capability_requirement: "android.notification_access",
    },
    ActionSpec {
        tool: "automation",
        action: "list",
        automation_compatible: false,
        capability_requirement: "none",
    },
    ActionSpec {
        tool: "automation",
        action: "get",
        automation_compatible: false,
        capability_requirement: "none",
    },
    ActionSpec {
        tool: "automation",
        action: "save",
        automation_compatible: false,
        capability_requirement: "none",
    },
    ActionSpec {
        tool: "automation",
        action: "set_enabled",
        automation_compatible: false,
        capability_requirement: "none",
    },
    ActionSpec {
        tool: "automation",
        action: "delete",
        automation_compatible: false,
        capability_requirement: "none",
    },
    ActionSpec {
        tool: "automation",
        action: "run",
        automation_compatible: false,
        capability_requirement: "none",
    },
    ActionSpec {
        tool: "task_control",
        action: "list",
        automation_compatible: false,
        capability_requirement: "none",
    },
    ActionSpec {
        tool: "task_control",
        action: "get",
        automation_compatible: false,
        capability_requirement: "none",
    },
    ActionSpec {
        tool: "task_control",
        action: "cancel",
        automation_compatible: false,
        capability_requirement: "none",
    },
];
