//! I10 context gates: the static ToolCatalog is served by the shared Runtime ingress.

use contract::{Availability, CapabilityState, GrantFacts, RuntimeHost, RuntimeReadiness, UuidV4};
use domain::{AdmissionFence, CapabilityContext, ProviderGenerations, ResolverFacts};
use runtime::{
    ApkRuntimeVertical, CapabilitySnapshot, RecoveryProof, RuntimeCore, VerticalEnvironment,
    fakes::{FakeArtifacts, FakeCapabilities, FakeExecutions, FakeHostControl, FakePersistence},
};
use serde_json::{Value, json};
use std::collections::BTreeSet;

type TestCore =
    RuntimeCore<FakePersistence, FakeArtifacts, FakeExecutions, FakeCapabilities, FakeHostControl>;

const NOW: &str = "2026-09-15T08:00:00.000Z";
const NOW_MS: u64 = 1_789_459_200_000;

fn uuid(value: u64) -> UuidV4 {
    UuidV4::parse(format!("99100000-0000-4000-8000-{value:012x}")).unwrap()
}

fn fact(state: CapabilityState) -> Availability {
    Availability {
        state,
        reason: (state != CapabilityState::Available).then(|| "FIXTURE".to_owned()),
    }
}

fn capability(state: CapabilityState, readiness: RuntimeReadiness) -> CapabilitySnapshot {
    CapabilitySnapshot {
        grants: GrantFacts {
            android_local_network: fact(state),
            android_notifications: fact(state),
            android_notification_listener: fact(state),
            automation_exact_alarm: fact(state),
            visual_accessibility: fact(state),
            visual_media_projection_session: fact(state),
            shizuku_shell: fact(state),
            magisk_module: fact(state),
            magisk_root: fact(state),
            magisk_framework: fact(state),
            magisk_launch: fact(state),
            magisk_clipboard: fact(state),
            magisk_notifications: fact(state),
            magisk_wake_alarm: fact(state),
            execution_app_guard: fact(state),
            execution_shell_guard: fact(state),
            execution_root_guard: fact(state),
        },
        context: CapabilityContext {
            sdk_int: 37,
            host: RuntimeHost::ApkRuntime,
            readiness,
            app_execution_surface: state,
        },
        resolver_facts: ResolverFacts {
            app_native: state,
            app_framework: state,
            shizuku: state,
            magisk_native: state,
            magisk_framework: state,
            magisk_launch: state,
            magisk_clipboard: state,
            magisk_notifications: state,
            accessibility: state,
            media_projection: state,
            notification_listener: state,
            generations: ProviderGenerations {
                app_native: 1,
                app_framework: 1,
                shizuku: 1,
                magisk_native: 1,
                magisk_framework: 1,
                accessibility: 1,
                media_projection: 1,
                notification_listener: 1,
            },
        },
        fence: AdmissionFence {
            runtime_epoch: uuid(1),
            host_generation: 1,
            runtime_instance_id: uuid(2),
        },
    }
}

fn make_core() -> (TestCore, FakeCapabilities) {
    let capabilities = FakeCapabilities::new(capability(
        CapabilityState::Available,
        RuntimeReadiness::Ready,
    ));
    let core = RuntimeCore::new(
        FakePersistence::default(),
        FakeArtifacts::default(),
        FakeExecutions::default(),
        capabilities.clone(),
        FakeHostControl::new(RecoveryProof::Clean).with_capabilities(capabilities.clone()),
    );
    (core, capabilities)
}

async fn catalog(core: &TestCore, request_id: u64, input: Value, admission_open: bool) -> Value {
    let request = json!({
        "protocol_version": 1,
        "request_id": uuid(0x1000 + request_id),
        "payload": {"tool": "context", "action": "catalog", "input": input},
    });
    serde_json::from_slice(
        &runtime::submit_public(
            core,
            &serde_json::to_vec(&request).unwrap(),
            NOW.to_owned(),
            NOW_MS,
            admission_open,
            |_| async { panic!("context.catalog escaped the shared Runtime to a host vertical") },
        )
        .await,
    )
    .unwrap()
}

const ROOT_TOOLS: [&str; 8] = [
    "context",
    "filesystem",
    "command",
    "network",
    "visual",
    "android",
    "automation",
    "task_control",
];

#[tokio::test]
async fn i10_g03_tool_catalog_is_a_static_table_independent_of_grants() {
    let (core, capabilities) = make_core();
    let inputs = [
        json!({}),
        json!({"namespace": "filesystem", "detail": "full"}),
        json!({"namespace": "automation"}),
        json!({"namespace": "android", "detail": "full"}),
    ];
    let mut available = Vec::new();
    for (index, input) in inputs.iter().enumerate() {
        available.push(catalog(&core, index as u64, input.clone(), true).await);
    }

    // Losing every grant, and closing business admission, never mutates the catalog.
    capabilities.set(capability(
        CapabilityState::Unavailable,
        RuntimeReadiness::Unavailable,
    ));
    for (index, input) in inputs.iter().enumerate() {
        let unavailable = catalog(&core, 10 + index as u64, input.clone(), false).await;
        assert_eq!(unavailable["result"], available[index]["result"], "{input}");
    }

    let root = &available[0]["result"];
    assert_eq!(
        root,
        &json!({"current_namespace": "", "root_tools": ROOT_TOOLS, "siblings": [], "actions": []})
    );

    let filesystem = &available[1]["result"];
    assert_eq!(filesystem["parent"], "");
    assert_eq!(
        filesystem["siblings"],
        json!([
            "context",
            "command",
            "network",
            "visual",
            "android",
            "automation",
            "task_control"
        ])
    );
    assert_eq!(
        filesystem["actions"],
        json!([
            {"name": "inspect", "automation_compatible": false, "capability_requirement": "dynamic"},
            {"name": "read", "automation_compatible": false, "capability_requirement": "dynamic"},
            {"name": "write", "automation_compatible": false, "capability_requirement": "dynamic"},
            {"name": "manage", "automation_compatible": true, "capability_requirement": "dynamic"},
            {"name": "download", "automation_compatible": true, "capability_requirement": "dynamic"},
            {"name": "archive", "automation_compatible": false, "capability_requirement": "dynamic"},
        ])
    );

    // Compact detail omits capability_requirement; the reserved Task provenance is absent.
    let automation = &available[2]["result"]["actions"];
    assert_eq!(
        automation,
        &json!([
            {"name": "list", "automation_compatible": false},
            {"name": "get", "automation_compatible": false},
            {"name": "save", "automation_compatible": false},
            {"name": "set_enabled", "automation_compatible": false},
            {"name": "delete", "automation_compatible": false},
            {"name": "run", "automation_compatible": false},
        ])
    );

    let android = &available[3]["result"]["actions"];
    assert_eq!(
        android
            .as_array()
            .unwrap()
            .iter()
            .map(|action| (
                action["name"].as_str().unwrap(),
                action["capability_requirement"].as_str().unwrap()
            ))
            .collect::<Vec<_>>(),
        vec![
            ("package", "dynamic"),
            ("launch", "dynamic"),
            ("intent", "dynamic"),
            ("clipboard", "dynamic"),
            ("notification", "android.notification_access"),
        ]
    );
}

fn environment() -> VerticalEnvironment {
    VerticalEnvironment {
        sdk_int: 35,
        abi: "arm64-v8a".to_owned(),
        timezone: "Asia/Shanghai".to_owned(),
        manufacturer: "OnePlus".to_owned(),
        model: "PJE110".to_owned(),
        device: "OP5D0DL1".to_owned(),
        build_fingerprint: "OnePlus/PJE110/OP5D0DL1:15/AP3A/1:user/release-keys".to_owned(),
        version_name: "0.1.0".to_owned(),
        version_code: 1000,
        runtime_epoch: uuid(1),
        host_generation: 2,
    }
}

fn status(vertical: &ApkRuntimeVertical, input: Value) -> Value {
    vertical
        .dispatch_installed(
            serde_json::from_value(json!({
                "protocol_version": 1,
                "request_id": uuid(0x2000),
                "payload": {"tool": "context", "action": "status", "input": input},
            }))
            .unwrap(),
        )
        .unwrap()
}

fn keys(value: &Value) -> BTreeSet<&str> {
    value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect()
}

#[test]
fn i10_g01_compact_status_exposes_effective_capabilities_without_grants() {
    let vertical = ApkRuntimeVertical::new(environment()).unwrap();
    vertical
        .register_capability("shizuku.shell", fact(CapabilityState::Available), 1, true)
        .unwrap();

    for input in [json!({}), json!({"detail": "compact"})] {
        let compact = status(&vertical, input);
        assert_eq!(
            keys(&compact),
            BTreeSet::from(["capabilities", "device", "runtime"])
        );
        assert_eq!(
            keys(&compact["device"]),
            BTreeSet::from(["abi", "sdk_int", "timezone"])
        );
        assert_eq!(
            compact["runtime"],
            json!({"host": "apk_runtime", "host_generation": 2, "readiness": "ready"})
        );
        // Compact capabilities are the same effective projection full status carries, and no
        // grant/backend row leaks into them.
        let full = status(&vertical, json!({"detail": "full"}));
        assert_eq!(compact["capabilities"], full["capabilities"]);
        for grant in contract::GRANT_KEYS {
            assert!(compact["capabilities"].get(*grant).is_none(), "{grant}");
        }
    }
}

#[test]
fn i10_g02_full_status_separately_exposes_grants_and_observed_components() {
    let vertical = ApkRuntimeVertical::new(environment()).unwrap();
    vertical
        .register_capability(
            "shizuku.shell",
            Availability {
                state: CapabilityState::Unavailable,
                reason: Some("SHIZUKU_NOT_RUNNING".to_owned()),
            },
            1,
            false,
        )
        .unwrap();
    let full = status(&vertical, json!({"detail": "full"}));

    assert_eq!(
        keys(&full),
        BTreeSet::from([
            "capabilities",
            "compatibility",
            "components",
            "device",
            "grants",
            "runtime"
        ])
    );
    assert_eq!(
        keys(&full["grants"]),
        contract::GRANT_KEYS
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
    );
    assert_eq!(
        full["grants"]["shizuku.shell"],
        json!({"state": "unavailable", "reason": "SHIZUKU_NOT_RUNNING"})
    );
    assert_eq!(
        full["components"]["shizuku"],
        json!({"integration_version": "13.1.5", "state": "unavailable", "reason": "SHIZUKU_NOT_RUNNING"})
    );
    let mut magisk = full["grants"]["magisk.module"].clone();
    assert_eq!(full["components"]["magisk"], magisk);
    assert_eq!(
        full["components"]["runtime_host"],
        json!({"host": "apk_runtime", "component_version": "0.1.0", "protocol_version": 1, "store_schema_version": 1})
    );
    assert_eq!(
        full["components"]["apk"],
        json!({"version_name": "0.1.0", "version_code": 1000})
    );
    assert_eq!(
        full["compatibility"],
        json!({"protocol": "compatible", "store_schema": "compatible"})
    );

    // Only the Magisk host observes its own daemon version; the module version stays omitted.
    let magisk_host =
        ApkRuntimeVertical::new_for_host(environment(), RuntimeHost::MagiskBackend).unwrap();
    magisk_host
        .register_capability("magisk.module", fact(CapabilityState::Available), 1, false)
        .unwrap();
    let observed = status(&magisk_host, json!({"detail": "full"}));
    magisk = json!({"daemon_version": "0.1.0", "state": "available"});
    assert_eq!(observed["components"]["magisk"], magisk);
    assert_eq!(
        observed["components"]["runtime_host"]["host"],
        "magisk_backend"
    );
}

#[tokio::test]
async fn i10_g03_catalog_rejects_a_namespace_outside_the_eight_mother_tools() {
    let (core, _) = make_core();
    for (index, namespace) in ["automation.execution", "Context", "contexts"]
        .into_iter()
        .enumerate()
    {
        let response = catalog(&core, index as u64, json!({"namespace": namespace}), true).await;
        assert_eq!(response["outcome"], "error", "{response}");
        assert_eq!(response["error"]["code"], "NOT_FOUND");
        assert_eq!(response["error"]["operation"], "context.catalog");
    }
}
