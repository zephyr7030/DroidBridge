use contract::*;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

const REQUEST_ID: &str = "018f47f2-26a8-4f26-8a65-728feb0e8461";

fn request(payload: Value) -> Value {
    json!({
        "protocol_version": 1,
        "request_id": REQUEST_ID,
        "payload": payload,
    })
}

fn repository_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("contract crate is nested under rust/crates")
}

#[test]
fn i1_g01_request_and_response_envelopes_are_exclusive_and_versioned() {
    let parsed: PublicRequest = serde_json::from_value(request(json!({
        "tool": "context",
        "action": "status",
        "input": {"detail": "compact"},
    })))
    .expect("valid request");
    assert_eq!(
        serde_json::to_value(&parsed).unwrap()["protocol_version"],
        1
    );
    assert_eq!(canonical_payload_sha256(&parsed.payload).len(), 64);

    assert!(
        serde_json::from_value::<PublicRequest>(request(json!({
            "tool": "context", "action": "status", "input": {}, "extra": true
        })))
        .is_err()
    );
    assert!(
        serde_json::from_value::<PublicRequest>(json!({
            "protocol_version": 2, "request_id": REQUEST_ID,
            "payload": {"tool":"context","action":"status","input":{}}
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<PublicRequest>(json!({
            "protocol_version": 1, "request_id": REQUEST_ID.to_uppercase(),
            "payload": {"tool":"context","action":"status","input":{}}
        }))
        .is_err()
    );

    let success = json!({
        "protocol_version":1,"request_id":REQUEST_ID,"outcome":"success","result":{}
    });
    let error = json!({
        "protocol_version":1,"request_id":REQUEST_ID,"outcome":"error",
        "error":{"code":"INVALID_ARGUMENT","operation":"context.status","retryable":false}
    });
    assert!(serde_json::from_value::<PublicResponse<Value>>(success.clone()).is_ok());
    assert!(serde_json::from_value::<PublicResponse<Value>>(error.clone()).is_ok());
    let mut invalid_success = success;
    invalid_success
        .as_object_mut()
        .unwrap()
        .insert("error".into(), json!({}));
    assert!(serde_json::from_value::<PublicResponse<Value>>(invalid_success).is_err());
    let mut invalid_error = error;
    invalid_error
        .as_object_mut()
        .unwrap()
        .insert("result".into(), json!({}));
    assert!(serde_json::from_value::<PublicResponse<Value>>(invalid_error).is_err());
}

#[test]
fn i1_g02_catalog_fields_defaults_bounds_and_internal_schemas_are_serializable() {
    let metadata = contract_metadata();
    assert_eq!(metadata["schema_version"], 1);
    assert_eq!(metadata["error_codes"].as_array().unwrap().len(), 23);
    assert_eq!(metadata["grants"].as_array().unwrap().len(), 17);
    assert_eq!(
        metadata["effective_capabilities"].as_array().unwrap().len(),
        16
    );
    assert_eq!(serde_json::to_value(TaskState::Created).unwrap(), "created");
    assert_eq!(metadata["actions"].as_array().unwrap().len(), 30);
    assert_eq!(
        metadata["result_schema_bindings"]
            .as_object()
            .unwrap()
            .len(),
        ACTION_SPECS.len()
    );
    for action in metadata["actions"].as_array().unwrap() {
        let requirement = action["capability_requirement"].as_str().unwrap();
        assert!(
            matches!(requirement, "none" | "dynamic") || CAPABILITY_KEYS.contains(&requirement)
        );
    }
    assert!(metadata["utf8_byte_bounds"].as_array().unwrap().len() >= 25);
    assert_eq!(metadata["semantic_bounds"]["automation.tree_depth"], 16);

    let schema_artifact = generated_artifacts()
        .into_iter()
        .find(|artifact| artifact.relative_path == "contract-schema.v1.json")
        .unwrap();
    let schema: Value = serde_json::from_slice(&schema_artifact.bytes).unwrap();
    let encoded = serde_json::to_string(&schema).unwrap();
    assert!(encoded.contains("maximum"));
    assert!(encoded.contains("default"));
    assert!(encoded.contains("filesystem.privileged_path"));
    assert!(
        schema["schemas"]
            .get("result.network.packet.decode")
            .is_some()
    );
    assert!(schema["schemas"].get("result.visual.observe").is_some());
    for names in metadata["result_schema_bindings"]
        .as_object()
        .unwrap()
        .values()
    {
        for name in names.as_array().unwrap() {
            assert!(
                schema["schemas"].get(name.as_str().unwrap()).is_some(),
                "missing bound result schema {name}"
            );
        }
    }

    let update_id = UuidV4::parse(REQUEST_ID).unwrap();
    let operation = DaemonOperation::MaintenanceStatus(MaintenanceStatus { update_id });
    assert_eq!(
        serde_json::to_value(operation).unwrap(),
        json!({
            "operation":"MaintenanceStatus","payload":{"update_id":REQUEST_ID}
        })
    );
    assert!(serde_json::from_value::<True>(json!(false)).is_err());
    assert!(serde_json::from_value::<True>(json!(true)).is_ok());
}

#[test]
fn i1_g03_generated_schemas_are_byte_deterministic() {
    let first = generated_artifacts();
    let second = generated_artifacts();
    assert_eq!(first, second);
    assert_eq!(generated_artifact_hashes(), generated_artifact_hashes());
    assert_eq!(first.len(), 4);
    for artifact in first {
        let path = repository_root()
            .join(GENERATED_ROOT)
            .join(artifact.relative_path);
        assert_eq!(fs::read(path).unwrap(), artifact.bytes);
    }
}

#[test]
fn i1_g04_dispatch_and_nested_domain_unions_are_unambiguous() {
    let pairs: BTreeSet<_> = ACTION_SPECS
        .iter()
        .map(|spec| (spec.tool, spec.action))
        .collect();
    assert_eq!(pairs.len(), ACTION_SPECS.len());
    assert_eq!(
        ACTION_SPECS
            .iter()
            .filter(|spec| spec.automation_compatible)
            .count(),
        7
    );

    let coordinate: PublicRequest = serde_json::from_value(request(json!({
        "tool":"visual","action":"interact","input":{
            "operation":"tap","target":"coordinate","observation_id":REQUEST_ID,"x":10,"y":20
        }
    })))
    .unwrap();
    assert_eq!(
        serde_json::to_value(coordinate).unwrap()["payload"]["input"]["target"],
        "coordinate"
    );

    assert!(
        serde_json::from_value::<PublicRequest>(request(json!({
            "tool":"visual","action":"interact","input":{
                "operation":"tap","target":"node","node_ref":"node-1",
                "observation_id":REQUEST_ID,"x":10,"y":20
            }
        })))
        .is_err()
    );
    assert!(
        serde_json::from_value::<PublicRequest>(request(json!({
            "tool":"filesystem","action":"read","input":{
                "target":{"type":"path","value":"/tmp/a"},"data_ref":"data-1"
            }
        })))
        .is_err()
    );
    serde_json::from_value::<PublicRequest>(request(json!({
        "tool":"filesystem","action":"read","input":{
            "target":{"type":"content_uri","value":"content://media/external/downloads/1"},
            "offset":0,"max_bytes":64,"encoding":"utf8"
        }
    })))
    .unwrap();
    serde_json::from_value::<PublicRequest>(request(json!({
        "tool":"visual","action":"view","input":{"path":"/tmp/image.png"}
    })))
    .unwrap();
    serde_json::from_value::<PublicRequest>(request(json!({
        "tool":"network","action":"capture","input":{
            "operation":"read","capture_ref":"capture-1","offset_packet":0,
            "max_packets":200,"include_payload":false
        }
    })))
    .unwrap();
    assert!(
        serde_json::from_value::<PublicRequest>(request(json!({
            "tool":"visual","action":"view","input":{
                "path":"/tmp/image.png","image_ref":"dbref:image:1"
            }
        })))
        .is_err()
    );
    assert!(
        serde_json::from_value::<PublicRequest>(request(json!({
            "tool":"network","action":"capture","input":{
                "operation":"read","capture_ref":"capture-1",
                "file":{"type":"path","value":"/tmp/capture.pcap"}
            }
        })))
        .is_err()
    );

    let automation_call = json!({
        "type":"call","tool":"visual","action":"interact","args":{
            "operation":"tap","target":"node","node_ref":"node-1"
        }
    });
    serde_json::from_value::<AutomationAction>(automation_call).unwrap();
    assert!(
        serde_json::from_value::<AutomationAction>(json!({
            "type":"call","tool":"visual","action":"interact","args":{
                "operation":"tap","target":"node","node_ref":"node-1"
            },"extra":true
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<AutomationAction>(json!({
            "type":"call","tool":"context","action":"status","args":{}
        }))
        .is_err()
    );
}

#[test]
fn i1_g05_settlement_bounds_cover_escaped_terminal_shapes() {
    let bounds = settlement_bounds();
    let entries = bounds["entries"].as_array().unwrap();
    assert_eq!(entries.len(), ACTION_SPECS.len());
    let operations: BTreeSet<_> = entries
        .iter()
        .map(|entry| entry["operation"].as_str().unwrap())
        .collect();
    assert_eq!(operations.len(), entries.len());
    for entry in entries {
        let bound = entry["maximum_escaped_terminal_bytes"].as_u64().unwrap();
        assert!(bound >= SETTLEMENT_RESERVE_FLOOR);
        assert!(bound < bounds["store_hard_cap_bytes"].as_u64().unwrap());
        assert_eq!(bound, MAX_FRAME_BYTES);
    }

    let error = PublicResponse::<Value>::error(
        None,
        PublicError {
            code: ErrorCode::InternalError,
            operation: "contract.request".into(),
            retryable: false,
            message: Some("\0".repeat(4096)),
            capability: None,
            details: None,
        },
    );
    assert!((serde_json::to_vec(&error).unwrap().len() as u64) < MAX_FRAME_BYTES);
}
