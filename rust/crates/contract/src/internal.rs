use crate::{ExecutionClass, ExecutionId, ProtocolVersion, RuntimeHost, UuidV4};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fence {
    pub runtime_epoch: UuidV4,
    pub host_generation: u64,
    pub runtime_instance_id: UuidV4,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InternalExecutionEnvelope<T> {
    pub protocol_version: ProtocolVersion,
    pub execution_id: ExecutionId,
    pub execution_class: ExecutionClass,
    pub fence: Fence,
    pub payload: T,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "operation", content = "payload")]
pub enum DaemonOperation {
    HostStatus(Empty),
    HostPrepareTransition(HostPrepareTransition),
    HostAbortTransition(TransitionId),
    HostRelease(TransitionId),
    HostActivate(HostActivate),
    RuntimeForward(serde_json::Value),
    RuntimeCancel(CancelExecution),
    CompanionExecute(serde_json::Value),
    CompanionCancel(CancelExecution),
    CapabilitySnapshot(Empty),
    DiagnosticsSnapshot(Empty),
    MaintenanceStatus(MaintenanceStatus),
    MaintenanceInstallApk(MaintenanceInstallApk),
    MaintenanceInstallModule(MaintenanceInstallModule),
}
#[derive(Clone, Debug, Default, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Empty {}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransitionId {
    pub transition_id: UuidV4,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelExecution {
    pub execution_id: ExecutionId,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostPrepareTransition {
    pub transition_id: UuidV4,
    pub runtime_epoch: UuidV4,
    pub from_host: RuntimeHost,
    pub from_generation: u64,
    pub from_instance_id: UuidV4,
    pub target_host: RuntimeHost,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostActivate {
    pub transition_id: UuidV4,
    pub runtime_epoch: UuidV4,
    pub host_generation: u64,
    pub target_host: RuntimeHost,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceStatus {
    pub update_id: UuidV4,
}
#[derive(Clone, Copy, Debug, Eq, JsonSchema, PartialEq, Serialize, Deserialize)]
pub enum CleanupState {
    #[serde(rename = "none")]
    None,
    #[serde(rename = "running")]
    Running,
    #[serde(rename = "clean")]
    Clean,
    #[serde(rename = "unverified")]
    Unverified,
}
#[derive(Clone, Copy, Debug, Eq, JsonSchema, PartialEq, Serialize, Deserialize)]
pub enum MaintenanceClean {
    #[serde(rename = "clean")]
    Clean,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceStatusResult {
    pub update_id: UuidV4,
    pub install_active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<ExecutionId>,
    pub cleanup: CleanupState,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceInstallApk {
    pub update_id: UuidV4,
    pub execution_id: ExecutionId,
    pub package: String,
    pub version_code: u64,
    pub sha256: String,
    pub signer_sha256: String,
    pub size: u64,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceInstallModule {
    pub update_id: UuidV4,
    pub execution_id: ExecutionId,
    pub module_id: String,
    pub version_code: u64,
    pub sha256: String,
    pub size: u64,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceInstallResult {
    pub update_id: UuidV4,
    pub execution_id: ExecutionId,
    pub process_exit_code: i32,
    pub cleanup: MaintenanceClean,
}
