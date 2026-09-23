use crate::{
    ArtifactPort, CapabilityPort, ExecutionPort, FilesystemPreflightPort, HostControlPort,
    PersistencePort, RuntimeCore,
};
use contract::{ACTION_SPECS, ErrorCode, PublicError, PublicRequest, PublicResponse, RequestId};
use domain::DomainError;
use serde_json::Value;

pub async fn submit_public<P, A, E, C, H, F, Fut>(
    core: &RuntimeCore<P, A, E, C, H>,
    encoded: &[u8],
    ended_at: String,
    now_ms: u64,
    business_admission_open: bool,
    dispatch_installed: F,
) -> Vec<u8>
where
    P: PersistencePort + 'static,
    A: ArtifactPort + Clone + 'static,
    E: ExecutionPort + FilesystemPreflightPort + 'static,
    C: CapabilityPort + 'static,
    H: HostControlPort + 'static,
    F: FnOnce(PublicRequest) -> Fut,
    Fut: std::future::Future<Output = Result<Value, DomainError>>,
{
    let (request, operation) = match decode_public(encoded) {
        Ok(request) => request,
        Err(response) => return encode_public(*response),
    };
    let request_id = request.request_id.clone();
    let payload_sha256 = contract::canonical_payload_sha256(&request.payload);
    let result = match request.payload {
        contract::PublicPayload::TaskControl { call } => {
            core.handle_task_control(call, ended_at, now_ms).await
        }
        // The static ToolCatalog is read-only and stays served behind the admission barrier.
        contract::PublicPayload::Context {
            call: contract::ContextCall::Catalog(input),
        } => crate::context_catalog(&input).and_then(|result| {
            serde_json::to_value(result).map_err(|_| {
                DomainError::new(ErrorCode::InternalError, "catalog serialization failed")
            })
        }),
        contract::PublicPayload::Filesystem { call } if business_admission_open => {
            crate::handle_filesystem_public(
                core,
                request_id.clone(),
                payload_sha256,
                call,
                ended_at,
                now_ms,
            )
            .await
        }
        contract::PublicPayload::Filesystem { .. } => Err(DomainError::new(
            ErrorCode::HostTransitionPending,
            "Runtime host transition is pending",
        )),
        contract::PublicPayload::Command { call } if business_admission_open => {
            crate::handle_command_public(
                core,
                request_id.clone(),
                payload_sha256,
                call,
                ended_at,
                now_ms,
            )
            .await
        }
        contract::PublicPayload::Command { .. } => Err(DomainError::new(
            ErrorCode::HostTransitionPending,
            "Runtime host transition is pending",
        )),
        contract::PublicPayload::Network { call } if business_admission_open => {
            crate::handle_network_public(
                core,
                request_id.clone(),
                payload_sha256,
                call,
                ended_at,
                now_ms,
            )
            .await
        }
        contract::PublicPayload::Network { .. } => Err(DomainError::new(
            ErrorCode::HostTransitionPending,
            "Runtime host transition is pending",
        )),
        contract::PublicPayload::Visual { call } if business_admission_open => {
            crate::handle_visual_public(
                core,
                request_id.clone(),
                payload_sha256,
                call,
                ended_at,
                now_ms,
            )
            .await
        }
        contract::PublicPayload::Visual { .. } => Err(DomainError::new(
            ErrorCode::HostTransitionPending,
            "Runtime host transition is pending",
        )),
        contract::PublicPayload::Android { call } if business_admission_open => {
            crate::handle_android_public(
                core,
                request_id.clone(),
                payload_sha256,
                call,
                ended_at,
                now_ms,
            )
            .await
        }
        contract::PublicPayload::Android { .. } => Err(DomainError::new(
            ErrorCode::HostTransitionPending,
            "Runtime host transition is pending",
        )),
        // Read-only list/get stay available behind the business-admission barrier; the handler
        // refuses definition mutations while it is closed (S-AUTH-001).
        contract::PublicPayload::Automation { call } => {
            crate::handle_automation_public(
                core,
                request_id.clone(),
                payload_sha256,
                call,
                ended_at,
                now_ms,
                business_admission_open,
            )
            .await
        }
        _ => dispatch_installed(request).await,
    };
    encode_public(match result {
        Ok(result) => PublicResponse::success(request_id, result),
        Err(error) => domain_failure(request_id, &error, &operation),
    })
}

pub(crate) fn decode_public(
    encoded: &[u8],
) -> Result<(PublicRequest, String), Box<PublicResponse<Value>>> {
    if encoded.len() > crate::UI_ENVELOPE_LIMIT_BYTES {
        return Err(Box::new(public_failure(
            None,
            ErrorCode::ResourceLimit,
            "contract.request",
        )));
    }
    let value: Value = serde_json::from_slice(encoded)
        .map_err(|_| public_failure(None, ErrorCode::InvalidArgument, "contract.request"))?;
    let request_id = value
        .get("request_id")
        .and_then(Value::as_str)
        .and_then(|id| RequestId::parse(id.to_owned()).ok());
    if value
        .get("protocol_version")
        .and_then(Value::as_u64)
        .is_some_and(|version| version != 1)
    {
        return Err(Box::new(public_failure(
            request_id,
            ErrorCode::ProtocolIncompatible,
            "contract.request",
        )));
    }
    let operation = ACTION_SPECS
        .iter()
        .find(|spec| {
            value.pointer("/payload/tool").and_then(Value::as_str) == Some(spec.tool)
                && value.pointer("/payload/action").and_then(Value::as_str) == Some(spec.action)
        })
        .map(|spec| format!("{}.{}", spec.tool, spec.action))
        .unwrap_or_else(|| "contract.request".to_owned());
    serde_json::from_slice(encoded)
        .map(|request| (request, operation.clone()))
        .map_err(|_| {
            Box::new(public_failure(
                request_id,
                ErrorCode::InvalidArgument,
                &operation,
            ))
        })
}

/// A request the Runtime refused carries the Runtime's own reason, so a caller can tell apart two
/// failures that share a code, and says whether the same request can simply be sent again.
fn domain_failure(
    request_id: RequestId,
    error: &DomainError,
    operation: &str,
) -> PublicResponse<Value> {
    PublicResponse::error(
        Some(request_id),
        PublicError {
            code: error.code,
            operation: operation.to_owned(),
            retryable: error.code == ErrorCode::HostTransitionPending,
            message: None,
            capability: None,
            details: Some(std::collections::BTreeMap::from([(
                "reason".to_owned(),
                contract::ErrorDetailValue::String(error.reason.to_owned()),
            )])),
        },
    )
}

pub(crate) fn public_failure(
    request_id: Option<RequestId>,
    code: ErrorCode,
    operation: &str,
) -> PublicResponse<Value> {
    PublicResponse::error(
        request_id,
        PublicError {
            code,
            operation: operation.to_owned(),
            retryable: false,
            message: None,
            capability: None,
            details: None,
        },
    )
}

pub(crate) fn encode_public(response: PublicResponse<Value>) -> Vec<u8> {
    serde_json::to_vec(&response)
        .expect("public response contains only JSON values and wire scalars")
}
