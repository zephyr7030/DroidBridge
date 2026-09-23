use crate::*;
use schemars::{Schema, schema_for};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const GENERATED_ROOT: &str = "tools/fixtures/contract";
pub const KOTLIN_FIXTURE_PATH: &str =
    "app/src/test/resources/contract/kotlin-envelope-fixtures.v1.json";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GeneratedArtifact {
    pub relative_path: &'static str,
    pub bytes: Vec<u8>,
}

#[derive(Serialize)]
struct SchemaBundle {
    schema_version: u32,
    schemas: BTreeMap<&'static str, Schema>,
}

fn pretty<T: Serialize>(value: &T) -> Vec<u8> {
    let mut bytes = serde_json::to_vec_pretty(value).expect("contract artifact is serializable");
    bytes.push(b'\n');
    bytes
}

fn schema_bundle() -> SchemaBundle {
    let mut schemas = BTreeMap::new();
    schemas.insert("automation", schema_for!(Automation));
    schemas.insert("daemon_operation", schema_for!(DaemonOperation));
    schemas.insert(
        "daemon_result.maintenance_install",
        schema_for!(MaintenanceInstallResult),
    );
    schemas.insert(
        "daemon_result.maintenance_status",
        schema_for!(MaintenanceStatusResult),
    );
    schemas.insert(
        "internal_execution_envelope",
        schema_for!(InternalExecutionEnvelope<Value>),
    );
    schemas.insert("public_error", schema_for!(PublicError));
    schemas.insert("public_request", schema_for!(PublicRequest));
    schemas.insert("public_response", schema_for!(PublicResponse<Value>));
    schemas.insert(
        "result.android.clipboard",
        schema_for!(AndroidClipboardResult),
    );
    schemas.insert("result.android.intent", schema_for!(AndroidIntentResult));
    schemas.insert("result.android.launch", schema_for!(LaunchResult));
    schemas.insert(
        "result.android.notification",
        schema_for!(AndroidNotificationResult),
    );
    schemas.insert("result.android.package", schema_for!(AndroidPackageResult));
    schemas.insert(
        "result.automation.delete",
        schema_for!(AutomationDeleteResult),
    );
    schemas.insert("result.automation.get", schema_for!(AutomationGetResult));
    schemas.insert("result.automation.list", schema_for!(AutomationListResult));
    schemas.insert("result.automation.save", schema_for!(Automation));
    schemas.insert("result.automation.set_enabled", schema_for!(Automation));
    schemas.insert("result.automation.run", schema_for!(AutomationRunResult));
    schemas.insert("result.command.run", schema_for!(CommandResult));
    schemas.insert("result.command.run.accepted", schema_for!(TaskAccepted));
    schemas.insert("result.context.catalog", schema_for!(ContextCatalogResult));
    schemas.insert(
        "result.context.status.compact",
        schema_for!(ContextStatusCompact),
    );
    schemas.insert("result.context.status.full", schema_for!(ContextStatusFull));
    schemas.insert(
        "result.filesystem.archive.list",
        schema_for!(FilesystemArchiveListResult),
    );
    schemas.insert(
        "result.filesystem.archive.accepted",
        schema_for!(TaskAccepted),
    );
    schemas.insert(
        "result.filesystem.archive.terminal",
        schema_for!(FilesystemArchiveTaskResult),
    );
    schemas.insert(
        "result.filesystem.download.accepted",
        schema_for!(TaskAccepted),
    );
    schemas.insert(
        "result.filesystem.download.terminal",
        schema_for!(FilesystemDownloadResult),
    );
    schemas.insert(
        "result.filesystem.inspect",
        schema_for!(FilesystemInspectResult),
    );
    schemas.insert(
        "result.filesystem.manage",
        schema_for!(FilesystemManageResult),
    );
    schemas.insert("result.filesystem.read", schema_for!(FilesystemReadResult));
    schemas.insert(
        "result.filesystem.write",
        schema_for!(FilesystemWriteResult),
    );
    schemas.insert(
        "result.network.capture.read",
        schema_for!(CaptureReadResult),
    );
    schemas.insert(
        "result.network.capture.start",
        schema_for!(CaptureStartResult),
    );
    schemas.insert(
        "result.network.capture.terminal",
        schema_for!(CaptureResult),
    );
    schemas.insert(
        "result.network.diagnose",
        schema_for!(NetworkDiagnoseResult),
    );
    schemas.insert("result.network.inspect", schema_for!(NetworkInspectResult));
    schemas.insert(
        "result.network.packet.build",
        schema_for!(PacketBuildResult),
    );
    schemas.insert(
        "result.network.packet.decode",
        schema_for!(PacketDecodeResult),
    );
    schemas.insert(
        "result.network.packet.inject",
        schema_for!(PacketInjectResult),
    );
    schemas.insert("result.task_control.list", schema_for!(TaskListResult));
    schemas.insert("result.task_control.snapshot", schema_for!(TaskSnapshot));
    schemas.insert("result.visual.interact", schema_for!(VisualInteractResult));
    schemas.insert("result.visual.observe", schema_for!(VisualObserveResult));
    schemas.insert("result.visual.view", schema_for!(VisualViewResult));
    SchemaBundle {
        schema_version: 1,
        schemas,
    }
}

pub fn action_registry() -> Value {
    Value::Array(
        ACTION_SPECS
            .iter()
            .map(|spec| {
                json!({
                    "tool": spec.tool,
                    "action": spec.action,
                    "automation_compatible": spec.automation_compatible,
                    "capability_requirement": spec.capability_requirement,
                })
            })
            .collect(),
    )
}

pub fn result_schema_bindings() -> Value {
    let bindings: BTreeMap<&str, Vec<&str>> = [
        (
            "context.status",
            vec![
                "result.context.status.compact",
                "result.context.status.full",
            ],
        ),
        ("context.catalog", vec!["result.context.catalog"]),
        ("filesystem.inspect", vec!["result.filesystem.inspect"]),
        ("filesystem.read", vec!["result.filesystem.read"]),
        ("filesystem.write", vec!["result.filesystem.write"]),
        ("filesystem.manage", vec!["result.filesystem.manage"]),
        (
            "filesystem.download",
            vec![
                "result.filesystem.download.accepted",
                "result.filesystem.download.terminal",
            ],
        ),
        (
            "filesystem.archive",
            vec![
                "result.filesystem.archive.list",
                "result.filesystem.archive.accepted",
                "result.filesystem.archive.terminal",
            ],
        ),
        (
            "command.run",
            vec!["result.command.run", "result.command.run.accepted"],
        ),
        ("network.inspect", vec!["result.network.inspect"]),
        (
            "network.capture",
            vec![
                "result.network.capture.start",
                "result.network.capture.read",
                "result.network.capture.terminal",
            ],
        ),
        (
            "network.packet",
            vec![
                "result.network.packet.decode",
                "result.network.packet.build",
                "result.network.packet.inject",
            ],
        ),
        ("network.diagnose", vec!["result.network.diagnose"]),
        ("visual.observe", vec!["result.visual.observe"]),
        ("visual.view", vec!["result.visual.view"]),
        ("visual.interact", vec!["result.visual.interact"]),
        ("android.package", vec!["result.android.package"]),
        ("android.launch", vec!["result.android.launch"]),
        ("android.intent", vec!["result.android.intent"]),
        ("android.clipboard", vec!["result.android.clipboard"]),
        ("android.notification", vec!["result.android.notification"]),
        ("automation.list", vec!["result.automation.list"]),
        ("automation.get", vec!["result.automation.get"]),
        ("automation.save", vec!["result.automation.save"]),
        (
            "automation.set_enabled",
            vec!["result.automation.set_enabled"],
        ),
        ("automation.delete", vec!["result.automation.delete"]),
        ("automation.run", vec!["result.automation.run"]),
        ("task_control.list", vec!["result.task_control.list"]),
        ("task_control.get", vec!["result.task_control.snapshot"]),
        ("task_control.cancel", vec!["result.task_control.snapshot"]),
    ]
    .into_iter()
    .collect();
    serde_json::to_value(bindings).expect("result schema bindings are serializable")
}

pub fn contract_metadata() -> Value {
    json!({
        "schema_version": 1,
        "protocol_version": PROTOCOL_VERSION,
        "store_schema_version": STORE_SCHEMA_VERSION,
        "wire": {
            "max_frame_bytes": MAX_FRAME_BYTES,
            "default_inline_bytes": 65_536,
            "maximum_inline_bytes": 1_048_576,
            "default_list_items": 200,
            "maximum_list_items": 5_000,
            "error_message_max_utf8_bytes": 4_096,
            "error_details_max_properties": 32,
            "error_details_max_encoded_utf8_bytes": 8_192,
            "error_detail_key_max_utf8_bytes": 64,
            "error_detail_string_max_utf8_bytes": 1_024,
            "error_detail_array_max_items": 32,
        },
        "utf8_byte_bounds": [
            {"path":"error.operation","min":1,"max":128,"charset":"[a-z0-9_.-]"},
            {"path":"error.message","max":4096},
            {"path":"error.details.*.key","min":1,"max":64},
            {"path":"error.details.*.string","max":1024},
            {"path":"context.status.device.timezone","min":1,"max":255},
            {"path":"context.status.device.manufacturer","max":256},
            {"path":"context.status.device.model","max":256},
            {"path":"context.status.device.device","max":256},
            {"path":"context.status.device.build_fingerprint","max":256},
            {"path":"context.status.components.*.version","max":128},
            {"path":"command.run.command","min":1,"max":32768,"forbid_nul":true},
            {"path":"command.run.cwd","min":1,"max":4096,"forbid_nul":true},
            {"path":"command.run.stdin","max":65536},
            {"path":"network.inspect.sockets[].state","max":32,"charset":"ascii"},
            {"path":"visual.observe.foreground.*","max":512},
            {"path":"visual.observe.nodes[].textual_fields","max":4096},
            {"path":"visual.interact.text","max":65536},
            {"path":"android.*.package_name","min":1,"max":255,"forbid_nul":true},
            {"path":"android.*.class_name","min":1,"max":512,"forbid_nul":true},
            {"path":"android.intent.*_uri","max":4096,"forbid_nul":true},
            {"path":"android.intent.extras.*.key","min":1,"max":128},
            {"path":"android.intent.extras.*.string","max":4096},
            {"path":"android.clipboard.write.text","max":65536},
            {"path":"android.notification.summary.title","max":256},
            {"path":"android.notification.summary.text","max":512},
            {"path":"android.notification.action.title","max":512},
            {"path":"automation.name","min":1,"max":128},
            {"path":"automation.state.*.key","min":1,"max":64},
            {"path":"automation.state.*.string","max":4096}
        ],
        "semantic_bounds": {
            "request_dedup_retention_hours": 24,
            "request_dedup_max_records": 4096,
            "filesystem.read.selected_bytes": {"min":1,"max":1048576},
            "filesystem.write.edit.replacements": {"min":1,"max":100},
            "filesystem.archive.create.sources": {"min":1,"max":1000},
            "network.capture.stop_wait_ms": 10000,
            "network.capture.payload_preview_bytes": 4096,
            "network.packet.decoded_bytes": {"min":1,"max":131072},
            "network.packet.payload_preview_bytes": 4096,
            "network.packet.tcp_flags": ["fin","syn","rst","psh","ack","urg","ece","cwr"],
            "visual.display.rotation_degrees": [0,90,180,270],
            "android.notification.live_references": 256,
            "android.notification.reference_ttl_seconds": 300,
            "automation.tree_depth": 16,
            "automation.tree_nodes": 512,
            "automation.expanded_visits": 10000,
            "automation.execution_budget_ms": 3600000,
            "automation.state_keys": 64
        },
        "conditional_rules": [
            "filesystem.inspect.max_depth is 1 unless recursive=true",
            "command.run.timeout_ms maximum is 150000 for app|shell and 3600000 for root",
            "filesystem.read has exactly one of target|data_ref",
            "filesystem.read result has exactly one of data|data_ref",
            "command.run each stream has exactly one of inline|ref",
            "task snapshot terminal result and error are mutually exclusive",
            "visual observe image and node presence groups follow their requested availability",
            "network inspect availability, data, and truncated keys match the requested families"
        ],
        "error_codes": ERROR_CODE_TOKENS,
        "grants": GRANT_KEYS,
        "effective_capabilities": CAPABILITY_KEYS,
        "runtime_hosts": ["apk_runtime", "magisk_backend"],
        "actions": action_registry(),
        "result_schema_bindings": result_schema_bindings(),
    })
}

pub fn settlement_bounds() -> Value {
    let entries: Vec<Value> = action_registry().as_array().expect("action registry array").iter().map(|row| {
        json!({
            "operation": format!("{}.{}", row["tool"].as_str().unwrap(), row["action"].as_str().unwrap()),
            "maximum_escaped_terminal_bytes": MAX_FRAME_BYTES,
        })
    }).collect();
    json!({
        "schema_version": 1,
        "reserve_floor_bytes": SETTLEMENT_RESERVE_FLOOR,
        "store_hard_cap_bytes": 8_388_608u64,
        "entries": entries,
    })
}

pub fn kotlin_envelope_fixtures() -> Value {
    json!({
        "schema_version": 1,
        "fixtures": [
            {
                "name": "context_status_request",
                "json": {"protocol_version":1,"request_id":"018f47f2-26a8-4f26-8a65-728feb0e8461","payload":{"tool":"context","action":"status","input":{"detail":"compact"}}}
            },
            {
                "name": "success_response",
                "json": {"protocol_version":1,"request_id":"018f47f2-26a8-4f26-8a65-728feb0e8461","outcome":"success","result":{}}
            },
            {
                "name": "error_response",
                "json": {"protocol_version":1,"request_id":"018f47f2-26a8-4f26-8a65-728feb0e8461","outcome":"error","error":{"code":"INVALID_ARGUMENT","operation":"context.status","retryable":false}}
            }
        ]
    })
}

pub fn generated_artifacts() -> Vec<GeneratedArtifact> {
    [
        ("contract-schema.v1.json", pretty(&schema_bundle())),
        ("contract-metadata.v1.json", pretty(&contract_metadata())),
        ("settlement-bounds.v1.json", pretty(&settlement_bounds())),
        (
            "kotlin-envelope-fixtures.v1.json",
            pretty(&kotlin_envelope_fixtures()),
        ),
    ]
    .into_iter()
    .map(|(relative_path, bytes)| GeneratedArtifact {
        relative_path,
        bytes,
    })
    .collect()
}

pub fn generated_artifact_hashes() -> BTreeMap<&'static str, String> {
    generated_artifacts()
        .into_iter()
        .map(|artifact| {
            let digest = Sha256::digest(&artifact.bytes);
            let hex = digest.iter().map(|byte| format!("{byte:02x}")).collect();
            (artifact.relative_path, hex)
        })
        .collect()
}
