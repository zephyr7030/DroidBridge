use crate::{ProtocolVersion, PublicError, PublicPayload, RequestId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicRequest {
    pub protocol_version: ProtocolVersion,
    pub request_id: RequestId,
    pub payload: PublicPayload,
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "outcome")]
pub enum PublicResponse<T> {
    #[serde(rename = "success")]
    Success {
        protocol_version: ProtocolVersion,
        request_id: RequestId,
        result: T,
    },
    #[serde(rename = "error")]
    Error {
        protocol_version: ProtocolVersion,
        #[serde(skip_serializing_if = "Option::is_none")]
        request_id: Option<RequestId>,
        error: PublicError,
    },
}
impl<T> PublicResponse<T> {
    pub fn success(request_id: RequestId, result: T) -> Self {
        Self::Success {
            protocol_version: ProtocolVersion,
            request_id,
            result,
        }
    }
    pub fn error(request_id: Option<RequestId>, error: PublicError) -> Self {
        Self::Error {
            protocol_version: ProtocolVersion,
            request_id,
            error,
        }
    }
}

fn canonicalize(value: Value) -> Value {
    match value {
        Value::Object(object) => {
            let sorted: BTreeMap<_, _> = object
                .into_iter()
                .map(|(key, value)| (key, canonicalize(value)))
                .collect();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(values) => Value::Array(values.into_iter().map(canonicalize).collect()),
        scalar => scalar,
    }
}

pub fn canonical_payload_bytes(payload: &PublicPayload) -> Vec<u8> {
    let value = serde_json::to_value(payload).expect("typed public payload is serializable");
    serde_json::to_vec(&canonicalize(value)).expect("canonical public payload is serializable")
}

pub fn canonical_payload_sha256(payload: &PublicPayload) -> String {
    let digest = Sha256::digest(canonical_payload_bytes(payload));
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}
