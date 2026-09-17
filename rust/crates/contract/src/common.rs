use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use std::{borrow::Cow, collections::BTreeMap, fmt};

macro_rules! string_enum {
    ($name:ident { $($variant:ident => $value:literal),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, JsonSchema, PartialEq, Serialize, Deserialize)]
        pub enum $name { $(#[serde(rename = $value)] $variant),+ }
    };
}
pub(crate) use string_enum;

string_enum!(MotherTool { Context=>"context", Filesystem=>"filesystem", Command=>"command", Network=>"network", Visual=>"visual", Android=>"android", Automation=>"automation", TaskControl=>"task_control" });
string_enum!(CapabilityState { Available=>"available", Unavailable=>"unavailable", Unknown=>"unknown" });
string_enum!(RuntimeReadiness { Initializing=>"initializing", Ready=>"ready", Unavailable=>"unavailable" });
string_enum!(RuntimeHost { ApkRuntime=>"apk_runtime", MagiskBackend=>"magisk_backend" });
string_enum!(ExecutionClass { App=>"app", AndroidFramework=>"android_framework", Shizuku=>"shizuku", Magisk=>"magisk" });
string_enum!(TaskState { Created=>"created", Queued=>"queued", Running=>"running", Completed=>"completed", Failed=>"failed", Cancelled=>"cancelled", Interrupted=>"interrupted" });
string_enum!(AutomationExecutionState { Queued=>"queued", Running=>"running", Completed=>"completed", Failed=>"failed", Cancelled=>"cancelled", Interrupted=>"interrupted" });
string_enum!(CompatibilityState { Compatible=>"compatible", Incompatible=>"incompatible", Unknown=>"unknown" });
string_enum!(ErrorCode {
    InvalidArgument=>"INVALID_ARGUMENT", NotFound=>"NOT_FOUND", AlreadyExists=>"ALREADY_EXISTS", PermissionDenied=>"PERMISSION_DENIED",
    CapabilityUnavailable=>"CAPABILITY_UNAVAILABLE", Unsupported=>"UNSUPPORTED", StaleAuthority=>"STALE_AUTHORITY", StaleReference=>"STALE_REFERENCE",
    RevisionConflict=>"REVISION_CONFLICT", Timeout=>"TIMEOUT", Cancelled=>"CANCELLED", IoError=>"IO_ERROR", ProtocolIncompatible=>"PROTOCOL_INCOMPATIBLE",
    ResourceLimit=>"RESOURCE_LIMIT", InternalError=>"INTERNAL_ERROR", NotEmpty=>"NOT_EMPTY", ArchiveCorrupt=>"ARCHIVE_CORRUPT", ArchiveEncrypted=>"ARCHIVE_ENCRYPTED",
    RunAsUnavailable=>"RUN_AS_UNAVAILABLE", ExecutionFailed=>"EXECUTION_FAILED", CancelFailed=>"CANCEL_FAILED", CaptureFailed=>"CAPTURE_FAILED", HostTransitionPending=>"HOST_TRANSITION_PENDING"
});

pub const ERROR_CODE_TOKENS: &[&str] = &[
    "INVALID_ARGUMENT",
    "NOT_FOUND",
    "ALREADY_EXISTS",
    "PERMISSION_DENIED",
    "CAPABILITY_UNAVAILABLE",
    "UNSUPPORTED",
    "STALE_AUTHORITY",
    "STALE_REFERENCE",
    "REVISION_CONFLICT",
    "TIMEOUT",
    "CANCELLED",
    "IO_ERROR",
    "PROTOCOL_INCOMPATIBLE",
    "RESOURCE_LIMIT",
    "INTERNAL_ERROR",
    "NOT_EMPTY",
    "ARCHIVE_CORRUPT",
    "ARCHIVE_ENCRYPTED",
    "RUN_AS_UNAVAILABLE",
    "EXECUTION_FAILED",
    "CANCEL_FAILED",
    "CAPTURE_FAILED",
    "HOST_TRANSITION_PENDING",
];

pub const GRANT_KEYS: &[&str] = &[
    "android.local_network",
    "android.notifications",
    "android.notification_listener",
    "automation.exact_alarm",
    "visual.accessibility",
    "visual.media_projection_session",
    "shizuku.shell",
    "magisk.module",
    "magisk.root",
    "magisk.framework",
    "magisk.launch",
    "magisk.clipboard",
    "magisk.notifications",
    "magisk.wake_alarm",
    "execution.app_guard",
    "execution.shell_guard",
    "execution.root_guard",
];

pub const CAPABILITY_KEYS: &[&str] = &[
    "filesystem.privileged_path",
    "command.app",
    "command.shell",
    "command.root",
    "network.inspect",
    "network.internet",
    "network.local",
    "network.capture",
    "network.inject",
    "visual.image",
    "visual.hierarchy",
    "visual.coordinate_input",
    "visual.key_input",
    "visual.text_input",
    "automation.persistent_time",
    "android.notification_access",
];

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantFacts {
    #[serde(rename = "android.local_network")]
    pub android_local_network: Availability,
    #[serde(rename = "android.notifications")]
    pub android_notifications: Availability,
    #[serde(rename = "android.notification_listener")]
    pub android_notification_listener: Availability,
    #[serde(rename = "automation.exact_alarm")]
    pub automation_exact_alarm: Availability,
    #[serde(rename = "visual.accessibility")]
    pub visual_accessibility: Availability,
    #[serde(rename = "visual.media_projection_session")]
    pub visual_media_projection_session: Availability,
    #[serde(rename = "shizuku.shell")]
    pub shizuku_shell: Availability,
    #[serde(rename = "magisk.module")]
    pub magisk_module: Availability,
    #[serde(rename = "magisk.root")]
    pub magisk_root: Availability,
    #[serde(rename = "magisk.framework")]
    pub magisk_framework: Availability,
    #[serde(rename = "magisk.launch")]
    pub magisk_launch: Availability,
    #[serde(rename = "magisk.clipboard")]
    pub magisk_clipboard: Availability,
    #[serde(rename = "magisk.notifications")]
    pub magisk_notifications: Availability,
    #[serde(rename = "magisk.wake_alarm")]
    pub magisk_wake_alarm: Availability,
    #[serde(rename = "execution.app_guard")]
    pub execution_app_guard: Availability,
    #[serde(rename = "execution.shell_guard")]
    pub execution_shell_guard: Availability,
    #[serde(rename = "execution.root_guard")]
    pub execution_root_guard: Availability,
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectiveCapabilities {
    #[serde(rename = "filesystem.privileged_path")]
    pub filesystem_privileged_path: Availability,
    #[serde(rename = "command.app")]
    pub command_app: Availability,
    #[serde(rename = "command.shell")]
    pub command_shell: Availability,
    #[serde(rename = "command.root")]
    pub command_root: Availability,
    #[serde(rename = "network.inspect")]
    pub network_inspect: Availability,
    #[serde(rename = "network.internet")]
    pub network_internet: Availability,
    #[serde(rename = "network.local")]
    pub network_local: Availability,
    #[serde(rename = "network.capture")]
    pub network_capture: Availability,
    #[serde(rename = "network.inject")]
    pub network_inject: Availability,
    #[serde(rename = "visual.image")]
    pub visual_image: Availability,
    #[serde(rename = "visual.hierarchy")]
    pub visual_hierarchy: Availability,
    #[serde(rename = "visual.coordinate_input")]
    pub visual_coordinate_input: Availability,
    #[serde(rename = "visual.key_input")]
    pub visual_key_input: Availability,
    #[serde(rename = "visual.text_input")]
    pub visual_text_input: Availability,
    #[serde(rename = "automation.persistent_time")]
    pub automation_persistent_time: Availability,
    #[serde(rename = "android.notification_access")]
    pub android_notification_access: Availability,
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct ProtocolVersion;
impl Serialize for ProtocolVersion {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u32(1)
    }
}
impl<'de> Deserialize<'de> for ProtocolVersion {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = u32::deserialize(d)?;
        if v == 1 {
            Ok(Self)
        } else {
            Err(de::Error::custom("protocol_version must be 1"))
        }
    }
}
impl JsonSchema for ProtocolVersion {
    fn schema_name() -> Cow<'static, str> {
        "ProtocolVersion1".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({"type":"integer","const":1})
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct True;
impl Serialize for True {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bool(true)
    }
}
impl<'de> Deserialize<'de> for True {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if bool::deserialize(deserializer)? {
            Ok(Self)
        } else {
            Err(de::Error::custom("value must be true"))
        }
    }
}
impl JsonSchema for True {
    fn schema_name() -> Cow<'static, str> {
        "True".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({"type":"boolean","const":true})
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct UuidV4(String);
impl UuidV4 {
    pub fn parse(value: impl Into<String>) -> Result<Self, &'static str> {
        let value = value.into();
        let parsed = uuid::Uuid::parse_str(&value).map_err(|_| "invalid UUID")?;
        if parsed.get_version_num() != 4 || parsed.hyphenated().to_string() != value {
            return Err("UUID must be lowercase hyphenated RFC-4122 UUIDv4");
        };
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Display for UuidV4 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl Serialize for UuidV4 {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}
impl<'de> Deserialize<'de> for UuidV4 {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::parse(String::deserialize(d)?).map_err(de::Error::custom)
    }
}
impl JsonSchema for UuidV4 {
    fn schema_name() -> Cow<'static, str> {
        "UuidV4".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({"type":"string","pattern":"^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"})
    }
}

pub type RequestId = UuidV4;
pub type TaskId = UuidV4;
pub type AutomationId = UuidV4;
pub type ExecutionId = UuidV4;
pub type CaptureId = UuidV4;

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ScalarValue {
    Null,
    Boolean(bool),
    Integer(i64),
    String(String),
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ErrorDetailScalar {
    Null,
    Boolean(bool),
    Signed(i64),
    Unsigned(u64),
    String(String),
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ErrorDetailValue {
    Null,
    Boolean(bool),
    Signed(i64),
    Unsigned(u64),
    String(String),
    Array(Vec<ErrorDetailScalar>),
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicError {
    pub code: ErrorCode,
    #[schemars(length(min = 1, max = 128))]
    pub operation: String,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capability: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 32))]
    pub details: Option<BTreeMap<String, ErrorDetailValue>>,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Availability {
    pub state: CapabilityState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
string_enum!(FileTargetType{Path=>"path",ContentUri=>"content_uri"});
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileTarget {
    #[serde(rename = "type")]
    pub target_type: FileTargetType,
    pub value: String,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Region {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentName {
    pub package_name: String,
    pub class_name: String,
}
