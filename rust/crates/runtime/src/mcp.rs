//! The loopback MCP 2026-07-28 facade semantics (S-MCP-001..007), independent of the HTTP library
//! that frames them. The facade owns no Task, Automation, capability or host truth: every tool call
//! and resource query goes to the currently authoritative Runtime through [`McpHost`].

use crate::{PortFuture, command::new_uuid};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use contract::{ErrorCode, PublicPayload, RequestId};
use domain::DomainError;
use serde_json::{Map, Value, json};
use std::{collections::BTreeSet, io::Read};

pub const MCP_PROTOCOL_VERSION: &str = "2026-07-28";
pub const MCP_BODY_LIMIT_BYTES: usize = 262_144;
pub const MCP_RESPONSE_LIMIT_BYTES: usize = 12_000_000;
pub const MCP_STABLE_PORT: u16 = 8765;
pub const MCP_DEBUG_PORT: u16 = 18765;
/// The R-TOOL-001 mother tools in their fixed `tools/list` order.
pub const MCP_TOOL_NAMES: [&str; 8] = [
    "context",
    "filesystem",
    "command",
    "network",
    "visual",
    "android",
    "automation",
    "task_control",
];
const REF_FIELDS: [&str; 4] = ["stdout_ref", "stderr_ref", "data_ref", "image_ref"];

/// What each mother tool tells an AI client before it is called: display title, one-line purpose and
/// the MCP behavior hints. A tool with any mutating action is not read-only; hints are advisory.
struct ToolPresentation {
    title: &'static str,
    description: &'static str,
    read_only: bool,
    destructive: bool,
    idempotent: bool,
    open_world: bool,
}

const fn presentation(name: &str) -> ToolPresentation {
    let (title, description, read_only, destructive, idempotent, open_world) = match name.as_bytes()
    {
        b"context" => (
            "Device context",
            "Read DroidBridge status, capabilities and the tool catalog.",
            true,
            false,
            true,
            false,
        ),
        b"filesystem" => (
            "Files",
            "Inspect, read, write, move, delete, archive and download files on the device.",
            false,
            true,
            false,
            true,
        ),
        b"command" => (
            "Shell commands",
            "Run commands on the device as the app, shell or root identity.",
            false,
            true,
            false,
            true,
        ),
        b"network" => (
            "Network",
            "Diagnose connectivity and capture or inject network traffic.",
            false,
            true,
            false,
            true,
        ),
        b"visual" => (
            "Screen",
            "Observe the screen and tap, swipe, type text or press keys.",
            false,
            true,
            false,
            false,
        ),
        b"android" => (
            "Android apps",
            "Inspect packages, launch apps and intents, and use the clipboard and notifications.",
            false,
            true,
            false,
            false,
        ),
        b"automation" => (
            "Automations",
            "List, create, update, enable, delete and run automations. Tap, long-press or type into screen elements with the visual `element` step, which finds its target by text when the automation runs.",
            false,
            true,
            false,
            false,
        ),
        _ => (
            "Tasks",
            "List, inspect and cancel background tasks.",
            false,
            true,
            false,
            false,
        ),
    };
    ToolPresentation {
        title,
        description,
        read_only,
        destructive,
        idempotent,
        open_world,
    }
}
const REF_EXPIRY_BATCH: usize = 32;
const META_PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
const META_CLIENT_INFO: &str = "io.modelcontextprotocol/clientInfo";
const META_SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";
const META_REQUEST_ID: &str = "io.droidbridge/requestId";
const META_REF_EXPIRIES: &str = "io.droidbridge/refExpiries";

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;
const HEADER_MISMATCH: i64 = -32020;
const UNSUPPORTED_PROTOCOL_VERSION: i64 = -32022;

/// One HTTP request as framed by the loopback listener.
#[derive(Clone, Debug, Default)]
pub struct McpRequest {
    pub method: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// One HTTP response; a present body is always `application/json`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpResponse {
    pub status: u16,
    pub body: Option<Vec<u8>>,
}

/// The authoritative host's answer to one S-MCP-006 internal artifact query.
#[derive(Debug)]
pub struct McpArtifactReply {
    pub payload: Value,
    /// Exactly one read-only descriptor for a successful `read`, and none otherwise.
    pub descriptor: Option<std::fs::File>,
}

/// At most this many resources are listed per `resources/list` page (S-MCP-006).
pub const MCP_RESOURCE_PAGE_SIZE: usize = 200;

/// One S-MCP-006 internal artifact query as the authoritative host decodes it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum McpArtifactQuery {
    List { cursor: Option<String> },
    Read { uri: String },
    Metadata { refs: Vec<String> },
}

impl McpArtifactQuery {
    /// Whether a RuntimeForward payload is an internal artifact query rather than a public
    /// Contract request; the two envelopes share no top-level key besides `protocol_version`.
    pub fn is_artifact_query(payload: &Value) -> bool {
        payload.get("artifact_query").is_some()
    }

    pub fn decode(payload: &Value) -> Result<Self, DomainError> {
        let invalid = || DomainError::new(ErrorCode::InvalidArgument, "artifact query is invalid");
        let envelope = payload
            .as_object()
            .filter(|envelope| {
                envelope.len() == 2 && envelope.get("protocol_version") == Some(&json!(1))
            })
            .ok_or_else(invalid)?;
        let query = envelope
            .get("artifact_query")
            .and_then(Value::as_object)
            .ok_or_else(invalid)?;
        match query.get("operation").and_then(Value::as_str) {
            Some("list") if only_keys(query, &["operation", "cursor"]) => {
                match query.get("cursor") {
                    None => Ok(Self::List { cursor: None }),
                    Some(Value::String(cursor)) => Ok(Self::List {
                        cursor: Some(cursor.clone()),
                    }),
                    Some(_) => Err(invalid()),
                }
            }
            Some("read") if query.len() == 2 => query
                .get("uri")
                .and_then(Value::as_str)
                .map(|uri| Self::Read {
                    uri: uri.to_owned(),
                })
                .ok_or_else(invalid),
            Some("metadata") if query.len() == 2 => {
                let refs = query
                    .get("refs")
                    .and_then(Value::as_array)
                    .ok_or_else(invalid)?
                    .iter()
                    .map(|artifact_ref| artifact_ref.as_str().map(str::to_owned))
                    .collect::<Option<Vec<_>>>()
                    .ok_or_else(invalid)?;
                let unique = refs.iter().collect::<BTreeSet<_>>();
                if refs.is_empty() || refs.len() > REF_EXPIRY_BATCH || unique.len() != refs.len() {
                    return Err(invalid());
                }
                Ok(Self::Metadata { refs })
            }
            _ => Err(invalid()),
        }
    }
}

/// The single host path shared by `tools/call` and resource queries: the APK Runtime while it is
/// authoritative, otherwise the live Magisk Core through RuntimeForward.
pub trait McpHost: Send + Sync {
    /// Submits one canonical S-CONTRACT-003 envelope and returns the canonical response bytes.
    fn submit<'a>(&'a self, envelope: Vec<u8>) -> PortFuture<'a, Result<Vec<u8>, DomainError>>;

    /// Answers one S-MCP-006 internal artifact query from the authoritative ArtifactStore.
    fn artifact_query<'a>(
        &'a self,
        query: Value,
    ) -> PortFuture<'a, Result<McpArtifactReply, DomainError>>;
}

pub struct McpFacade<H> {
    host: H,
    port: u16,
    product_version: String,
    tools: Value,
}

impl<H: McpHost> McpFacade<H> {
    /// Builds the facade for one build endpoint. Tool schemas are projected once from the
    /// generated Contract artifacts, never authored separately (S-CONTRACT-001).
    pub fn new(host: H, port: u16, product_version: String) -> Result<Self, DomainError> {
        if product_version.is_empty() {
            return Err(DomainError::new(
                ErrorCode::InvalidArgument,
                "MCP product version is empty",
            ));
        }
        Ok(Self {
            host,
            port,
            product_version,
            tools: tool_definitions()?,
        })
    }

    /// Handles one HTTP request against the token currently committed by the settings owner.
    pub async fn handle(&self, request: McpRequest, accepted_token: &str) -> McpResponse {
        if request.method != "POST" {
            return McpResponse {
                status: 405,
                body: None,
            };
        }
        if !self.host_is_endpoint(&request) || !origin_is_loopback(&request) {
            return McpResponse {
                status: 403,
                body: None,
            };
        }
        if !bearer_matches(&request, accepted_token) {
            return McpResponse {
                status: 401,
                body: None,
            };
        }
        self.handle_protocol(request).await
    }

    /// Handles an authenticated tunnel request after the control-plane transport has admitted it.
    pub async fn handle_tunnel(&self, request: McpRequest) -> McpResponse {
        if request.method != "POST" {
            return McpResponse {
                status: 405,
                body: None,
            };
        }
        self.handle_protocol(request).await
    }

    async fn handle_protocol(&self, request: McpRequest) -> McpResponse {
        if !content_type_is_json(&request) {
            return McpResponse {
                status: 415,
                body: None,
            };
        }
        if !accept_is_acceptable(&request) {
            return McpResponse {
                status: 406,
                body: None,
            };
        }
        if request.body.len() > MCP_BODY_LIMIT_BYTES {
            return McpResponse {
                status: 413,
                body: None,
            };
        }
        let message: Value = match serde_json::from_slice(&request.body) {
            Ok(value) => value,
            Err(_) => return rpc_error(400, Value::Null, PARSE_ERROR, "Parse error", None),
        };
        let Some(object) = message.as_object() else {
            // Batches are not part of this revision.
            return rpc_error(400, Value::Null, INVALID_REQUEST, "Invalid Request", None);
        };
        if object.contains_key("result") || object.contains_key("error") {
            return rpc_error(400, Value::Null, INVALID_REQUEST, "Invalid Request", None);
        }
        if object.get("method").is_some_and(Value::is_string) && !object.contains_key("id") {
            // The revision defines no client notification DroidBridge accepts.
            return McpResponse {
                status: 400,
                body: None,
            };
        }
        let id = match object.get("id") {
            Some(Value::String(_)) => object["id"].clone(),
            Some(Value::Number(number)) if number.is_i64() || number.is_u64() => {
                object["id"].clone()
            }
            _ => return rpc_error(200, Value::Null, INVALID_REQUEST, "Invalid Request", None),
        };
        let (Some(Value::String(version)), Some(Value::String(method))) =
            (object.get("jsonrpc"), object.get("method"))
        else {
            return rpc_error(200, id, INVALID_REQUEST, "Invalid Request", None);
        };
        if version != "2.0" {
            return rpc_error(200, id, INVALID_REQUEST, "Invalid Request", None);
        }
        let params = match object.get("params") {
            Some(Value::Object(params)) => params.clone(),
            None => Map::new(),
            Some(_) => return rpc_error(200, id, INVALID_REQUEST, "Invalid Request", None),
        };
        if let Err(response) = validate_metadata_headers(&request, &id, method, &params) {
            return response;
        }
        match method.as_str() {
            "server/discover" => self.discover(id, &params),
            "tools/list" => self.list_tools(id, &params),
            "tools/call" => self.call_tool(id, &params).await,
            "resources/list" => self.list_resources(id, &params).await,
            "resources/read" => self.read_resource(id, &params).await,
            _ => rpc_error(404, id, METHOD_NOT_FOUND, "Method not found", None),
        }
    }

    fn host_is_endpoint(&self, request: &McpRequest) -> bool {
        let Some(host) = single_header(request, "host") else {
            return false;
        };
        let Some((name, port)) = host.rsplit_once(':') else {
            return false;
        };
        port == self.port.to_string()
            && (name.eq_ignore_ascii_case("127.0.0.1") || name.eq_ignore_ascii_case("localhost"))
    }

    fn server_meta(&self) -> Value {
        json!({META_SERVER_INFO: {"name": "DroidBridge", "version": self.product_version}})
    }

    fn discover(&self, id: Value, params: &Map<String, Value>) -> McpResponse {
        if !only_keys(params, &["_meta"]) {
            return invalid_params(id, field_detail("params", "expected only _meta"));
        }
        rpc_result(
            id,
            json!({
                "resultType": "complete",
                "supportedVersions": [MCP_PROTOCOL_VERSION],
                "capabilities": {"tools": {}, "resources": {}},
                "ttlMs": 0,
                "cacheScope": "private",
                "_meta": self.server_meta(),
            }),
        )
    }

    fn list_tools(&self, id: Value, params: &Map<String, Value>) -> McpResponse {
        if !only_keys(params, &["_meta", "cursor"]) {
            return invalid_params(id, field_detail("params", "expected only _meta and cursor"));
        }
        match params.get("cursor") {
            None => {}
            Some(Value::String(cursor)) if cursor.is_empty() => {}
            // tools/list always fits one page, so no cursor was ever issued.
            Some(_) => {
                return invalid_params(id, field_detail("params.cursor", "no such page"));
            }
        }
        rpc_result(
            id,
            json!({
                "resultType": "complete",
                "tools": self.tools,
                "ttlMs": 0,
                "cacheScope": "private",
                "_meta": self.server_meta(),
            }),
        )
    }

    async fn call_tool(&self, id: Value, params: &Map<String, Value>) -> McpResponse {
        if params.len() != 3 || !only_keys(params, &["name", "arguments", "_meta"]) {
            return invalid_params(
                id,
                field_detail("params", "expected exactly name, arguments and _meta"),
            );
        }
        let Some(name) = params["name"]
            .as_str()
            .filter(|name| MCP_TOOL_NAMES.contains(name))
        else {
            return invalid_params(id, field_detail("params.name", "not an advertised tool"));
        };
        let Some(arguments) = params["arguments"]
            .as_object()
            .filter(|arguments| only_keys(arguments, &["action", "input"]))
        else {
            return invalid_params(
                id,
                field_detail(
                    "params.arguments",
                    "expected an object with only action and input",
                ),
            );
        };
        // A failure this facade answers itself is named the way the Runtime names one, so a caller
        // reads `<tool>.<action>` from either.
        let operation = match arguments["action"].as_str() {
            Some(action) => format!("{name}.{action}"),
            None => name.to_owned(),
        };
        // The requested id belongs to the caller when it supplies the private key, so the host
        // keeps its duplicate safety across redeliveries. A spec-conformant MCP client cannot know
        // that key, so an absent one is minted here rather than rejected; a malformed one is still
        // the caller's error and never silently replaced.
        let request_id = match params["_meta"].get(META_REQUEST_ID) {
            Some(value) => {
                match value
                    .as_str()
                    .and_then(|value| RequestId::parse(value.to_owned()).ok())
                {
                    Some(request_id) => request_id,
                    None => {
                        return invalid_params(
                            id,
                            field_detail(
                                &format!("params._meta.{META_REQUEST_ID}"),
                                "expected a UUIDv4",
                            ),
                        );
                    }
                }
            }
            None => match new_uuid() {
                Ok(request_id) => request_id,
                Err(_) => {
                    return self.tool_failure(
                        id,
                        &operation,
                        ErrorCode::InternalError,
                        "a request id could not be minted",
                    );
                }
            },
        };
        let mut payload = arguments.clone();
        payload.insert("tool".to_owned(), Value::String(name.to_owned()));
        // The arguments must be exactly this tool's generated `{action,input}` union; decoding
        // through the Contract type applies the same schema the tool definition advertises.
        if serde_json::from_value::<PublicPayload>(Value::Object(payload.clone())).is_err() {
            return invalid_params(
                id,
                field_detail(
                    "params.arguments",
                    "the action or input does not match this tool's advertised schema",
                ),
            );
        }
        let envelope = json!({
            "protocol_version": 1,
            "request_id": request_id,
            "payload": Value::Object(payload),
        });
        let Ok(encoded) = serde_json::to_vec(&envelope) else {
            return self.tool_failure(
                id,
                &operation,
                ErrorCode::InternalError,
                "the request envelope could not be encoded",
            );
        };
        let response = match self.host.submit(encoded).await {
            Ok(response) => response,
            // A request this facade could not hand over may still have been served, so the answer
            // names what went wrong instead of claiming the call did not happen.
            Err(error) => return self.tool_failure(id, &operation, error.code, error.reason),
        };
        let Ok(response) = serde_json::from_slice::<Value>(&response) else {
            return self.tool_failure(
                id,
                &operation,
                ErrorCode::InternalError,
                "the Runtime host answered a reply this facade could not read",
            );
        };
        let (structured, is_error) = match response.get("outcome").and_then(Value::as_str) {
            Some("success") => match response.get("result") {
                Some(result) => (result.clone(), false),
                None => {
                    return self.tool_failure(
                        id,
                        &operation,
                        ErrorCode::InternalError,
                        "the Runtime host answered a success without a result",
                    );
                }
            },
            Some("error") => match response.get("error") {
                Some(error) => (with_next_step(error.clone(), arguments), true),
                None => {
                    return self.tool_failure(
                        id,
                        &operation,
                        ErrorCode::InternalError,
                        "the Runtime host answered an error without a reason",
                    );
                }
            },
            _ => {
                return self.tool_failure(
                    id,
                    &operation,
                    ErrorCode::InternalError,
                    "the Runtime host answered an envelope with no outcome",
                );
            }
        };
        let mut meta = self.server_meta();
        if !is_error {
            match self.ref_expiries(&structured).await {
                Ok(Some(expiries)) => {
                    meta[META_REF_EXPIRIES] = expiries;
                }
                Ok(None) => {}
                // The call itself was served and its result is retained under this requestId, so a
                // repeat of it returns that result rather than serving it twice.
                Err(_) => {
                    return self.tool_failure(
                        id,
                        &operation,
                        ErrorCode::InternalError,
                        "the call was served and its result is retained under this requestId, but its artifact expiries could not be read",
                    );
                }
            }
        }
        let Ok(text) = serde_json::to_string(&structured) else {
            return self.tool_failure(
                id,
                &operation,
                ErrorCode::InternalError,
                "the Runtime result could not be encoded",
            );
        };
        let mut content = vec![json!({"type": "text", "text": text})];
        if !is_error {
            match self.inline_image(&structured).await {
                Ok(Some(image)) => content.push(image),
                Ok(None) => {}
                Err(_) => {
                    return self.tool_failure(
                        id,
                        &operation,
                        ErrorCode::InternalError,
                        "the call completed, but its image artifact could not be returned",
                    );
                }
            }
        }
        let response_id = id.clone();
        let response = rpc_result(
            id,
            json!({
                "resultType": "complete",
                "content": content,
                "structuredContent": structured,
                "isError": is_error,
                "_meta": meta,
            }),
        );
        if response
            .body
            .as_ref()
            .is_some_and(|body| body.len() > MCP_RESPONSE_LIMIT_BYTES)
        {
            return self.tool_failure(
                response_id,
                &operation,
                ErrorCode::ResourceLimit,
                "the compressed image exceeds the MCP response limit",
            );
        }
        response
    }

    /// Answers a call this facade could not serve to a Runtime result, in the shape every other
    /// failure already travels in, so a caller reads the code, the operation and the reason instead
    /// of an opaque JSON-RPC `-32603`.
    fn tool_failure(
        &self,
        id: Value,
        operation: &str,
        code: ErrorCode,
        message: &str,
    ) -> McpResponse {
        let structured = json!({
            "code": code,
            "operation": operation,
            "retryable": false,
            "message": message,
        });
        rpc_result(
            id,
            json!({
                "resultType": "complete",
                "content": [{"type": "text", "text": structured.to_string()}],
                "structuredContent": structured,
                "isError": true,
                "_meta": self.server_meta(),
            }),
        )
    }

    async fn ref_expiries(&self, result: &Value) -> Result<Option<Value>, DomainError> {
        let mut refs = BTreeSet::new();
        collect_refs(result, &mut refs);
        if refs.is_empty() {
            return Ok(None);
        }
        let refs = refs.into_iter().collect::<Vec<_>>();
        let mut expiries = Map::new();
        for batch in refs.chunks(REF_EXPIRY_BATCH) {
            let reply = self
                .host
                .artifact_query(json!({
                    "protocol_version": 1,
                    "artifact_query": {"operation": "metadata", "refs": batch},
                }))
                .await?;
            let artifacts = reply
                .payload
                .get("artifacts")
                .and_then(Value::as_array)
                .ok_or_else(invalid_host_reply)?;
            for artifact in artifacts {
                let (Some(artifact_ref), Some(expires_at)) = (
                    artifact.get("ref").and_then(Value::as_str),
                    artifact.get("expires_at").and_then(Value::as_str),
                ) else {
                    return Err(invalid_host_reply());
                };
                if !batch.iter().any(|requested| requested == artifact_ref) {
                    return Err(invalid_host_reply());
                }
                expiries.insert(
                    artifact_ref.to_owned(),
                    Value::String(expires_at.to_owned()),
                );
            }
        }
        Ok((!expiries.is_empty()).then_some(Value::Object(expiries)))
    }

    async fn inline_image(&self, result: &Value) -> Result<Option<Value>, DomainError> {
        let Some(uri) = result.get("image_ref").and_then(Value::as_str) else {
            return Ok(None);
        };
        let reply = self
            .host
            .artifact_query(json!({
                "protocol_version": 1,
                "artifact_query": {"operation": "read", "uri": uri},
            }))
            .await?;
        let (Some("image"), Some(size), Some(mime), Some(mut descriptor)) = (
            reply.payload.get("kind").and_then(Value::as_str),
            reply.payload.get("size").and_then(Value::as_u64),
            reply.payload.get("mime").and_then(Value::as_str),
            reply.descriptor,
        ) else {
            return Err(invalid_host_reply());
        };
        if reply.payload.get("uri").and_then(Value::as_str) != Some(uri)
            || !matches!(mime, "image/heic" | "image/jpeg" | "image/png")
            || size == 0
            || size > 8 * 1_024 * 1_024
        {
            return Err(invalid_host_reply());
        }
        let mut bytes = Vec::new();
        descriptor
            .by_ref()
            .take(size.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|_| invalid_host_reply())?;
        if bytes.len() as u64 != size {
            return Err(invalid_host_reply());
        }
        Ok(Some(json!({
            "type": "image",
            "data": STANDARD.encode(bytes),
            "mimeType": mime,
        })))
    }

    async fn list_resources(&self, id: Value, params: &Map<String, Value>) -> McpResponse {
        if !only_keys(params, &["_meta", "cursor"]) {
            return invalid_params(id, field_detail("params", "expected only _meta and cursor"));
        }
        let mut query = Map::new();
        query.insert("operation".to_owned(), Value::String("list".to_owned()));
        match params.get("cursor") {
            None => {}
            Some(Value::String(cursor)) => {
                query.insert("cursor".to_owned(), Value::String(cursor.clone()));
            }
            Some(_) => {
                return invalid_params(id, field_detail("params.cursor", "expected a string"));
            }
        }
        let reply = match self
            .host
            .artifact_query(json!({"protocol_version": 1, "artifact_query": query}))
            .await
        {
            Ok(reply) => reply,
            Err(_) => return internal_error(id),
        };
        if reply.payload.get("error").and_then(Value::as_str) == Some("invalid_cursor") {
            return invalid_params(id, field_detail("params.cursor", "no such page"));
        }
        let Some(listed) = reply.payload.get("resources").and_then(Value::as_array) else {
            return internal_error(id);
        };
        let mut resources = Vec::with_capacity(listed.len());
        for resource in listed {
            let (Some(uri), Some(size)) = (
                resource.get("uri").and_then(Value::as_str),
                resource.get("size").and_then(Value::as_u64),
            ) else {
                return internal_error(id);
            };
            let mut projected = json!({"uri": uri, "name": uri, "size": size});
            if let Some(mime) = resource.get("mime").and_then(Value::as_str) {
                projected["mimeType"] = Value::String(mime.to_owned());
            }
            resources.push(projected);
        }
        let mut result = json!({
            "resultType": "complete",
            "resources": resources,
            "ttlMs": 0,
            "cacheScope": "private",
            "_meta": self.server_meta(),
        });
        if let Some(next) = reply.payload.get("next_cursor").and_then(Value::as_str) {
            result["nextCursor"] = Value::String(next.to_owned());
        }
        rpc_result(id, result)
    }

    async fn read_resource(&self, id: Value, params: &Map<String, Value>) -> McpResponse {
        if params.len() != 2 || !only_keys(params, &["uri", "_meta"]) {
            return invalid_params(id, field_detail("params", "expected exactly uri and _meta"));
        }
        let Some(uri) = params["uri"].as_str() else {
            return invalid_params(id, field_detail("params.uri", "expected a string"));
        };
        let reply = match self
            .host
            .artifact_query(json!({
                "protocol_version": 1,
                "artifact_query": {"operation": "read", "uri": uri},
            }))
            .await
        {
            Ok(reply) => reply,
            Err(_) => return internal_error(id),
        };
        if reply.payload.get("error").and_then(Value::as_str) == Some("not_found") {
            return invalid_params(id, field_detail("params.uri", "no such resource"));
        }
        let (Some(kind), Some(size), Some(mut descriptor)) = (
            reply.payload.get("kind").and_then(Value::as_str),
            reply.payload.get("size").and_then(Value::as_u64),
            reply.descriptor,
        ) else {
            return internal_error(id);
        };
        if reply.payload.get("uri").and_then(Value::as_str) != Some(uri) {
            return internal_error(id);
        }
        let mut bytes = Vec::new();
        if descriptor
            .by_ref()
            .take(size.saturating_add(1))
            .read_to_end(&mut bytes)
            .is_err()
            || bytes.len() as u64 != size
        {
            return internal_error(id);
        }
        drop(descriptor);
        let mime = reply.payload.get("mime").and_then(Value::as_str);
        let contents = match kind {
            "stdout" | "stderr" | "data" => match String::from_utf8(bytes) {
                Ok(text) => {
                    let mut item = json!({"uri": uri, "text": text});
                    if let Some(mime) = mime {
                        item["mimeType"] = Value::String(mime.to_owned());
                    }
                    item
                }
                Err(error) => json!({
                    "uri": uri,
                    "mimeType": "application/octet-stream",
                    "blob": STANDARD.encode(error.into_bytes()),
                }),
            },
            "image" => match mime {
                Some(mime @ ("image/heic" | "image/jpeg" | "image/png")) => {
                    json!({"uri": uri, "mimeType": mime, "blob": STANDARD.encode(bytes)})
                }
                _ => return internal_error(id),
            },
            _ => return internal_error(id),
        };
        let response = rpc_result(
            id.clone(),
            json!({
                "resultType": "complete",
                "contents": [contents],
                "ttlMs": 0,
                "cacheScope": "private",
                "_meta": self.server_meta(),
            }),
        );
        if response
            .body
            .as_ref()
            .is_some_and(|body| body.len() > MCP_RESPONSE_LIMIT_BYTES)
        {
            return internal_error(id);
        }
        response
    }
}

/// Encodes an S-MCP-006 resource cursor for the last returned ref.
pub fn mcp_resource_cursor(last_ref: &str) -> String {
    URL_SAFE_NO_PAD.encode(last_ref.as_bytes())
}

/// Decodes a server-issued resource cursor back to its ref, rejecting any other string.
pub fn mcp_resource_cursor_ref(cursor: &str) -> Option<String> {
    let bytes = URL_SAFE_NO_PAD.decode(cursor).ok()?;
    let artifact_ref = String::from_utf8(bytes).ok()?;
    (mcp_resource_cursor(&artifact_ref) == cursor).then_some(artifact_ref)
}

fn validate_metadata_headers(
    request: &McpRequest,
    id: &Value,
    method: &str,
    params: &Map<String, Value>,
) -> Result<(), McpResponse> {
    let mismatch = || rpc_error(400, id.clone(), HEADER_MISMATCH, "Header mismatch", None);
    let Some(version) = single_header(request, "mcp-protocol-version") else {
        return Err(mismatch());
    };
    if version != MCP_PROTOCOL_VERSION {
        if is_protocol_date(version) {
            return Err(rpc_error(
                400,
                id.clone(),
                UNSUPPORTED_PROTOCOL_VERSION,
                "Unsupported protocol version",
                Some(json!({"supported": [MCP_PROTOCOL_VERSION], "requested": version})),
            ));
        }
        return Err(mismatch());
    }
    if single_header(request, "mcp-method") != Some(method) {
        return Err(mismatch());
    }
    let name_header = match header_values(request, "mcp-name").as_slice() {
        [] => None,
        [value] => Some(decode_name_header(value).ok_or_else(mismatch)?),
        _ => return Err(mismatch()),
    };
    match method {
        "tools/call" | "resources/read" => {
            let field = if method == "tools/call" {
                "name"
            } else {
                "uri"
            };
            let Some(expected) = params.get(field).and_then(Value::as_str) else {
                return Err(invalid_params(
                    id.clone(),
                    field_detail(&format!("params.{field}"), "expected a string"),
                ));
            };
            if name_header.as_deref() != Some(expected) {
                return Err(mismatch());
            }
        }
        "server/discover" | "tools/list" | "resources/list" => {
            if name_header.is_some() {
                return Err(mismatch());
            }
        }
        _ => return Ok(()),
    }
    let Some(meta) = params.get("_meta").and_then(Value::as_object) else {
        return Err(invalid_params(
            id.clone(),
            field_detail("params._meta", "expected an object"),
        ));
    };
    match meta.get(META_PROTOCOL_VERSION) {
        Some(Value::String(body_version)) if body_version == version => {}
        Some(Value::String(_)) => return Err(mismatch()),
        _ => {
            return Err(invalid_params(
                id.clone(),
                field_detail(
                    &format!("params._meta.{META_PROTOCOL_VERSION}"),
                    "expected the negotiated protocol version",
                ),
            ));
        }
    }
    if !meta
        .get(META_CLIENT_CAPABILITIES)
        .is_some_and(Value::is_object)
    {
        return Err(invalid_params(
            id.clone(),
            field_detail(
                &format!("params._meta.{META_CLIENT_CAPABILITIES}"),
                "expected an object",
            ),
        ));
    }
    if let Some(info) = meta.get(META_CLIENT_INFO) {
        let valid = info.as_object().is_some_and(|info| {
            info.get("name").is_some_and(Value::is_string)
                && info.get("version").is_some_and(Value::is_string)
        });
        if !valid {
            return Err(invalid_params(
                id.clone(),
                field_detail(
                    &format!("params._meta.{META_CLIENT_INFO}"),
                    "expected name and version strings",
                ),
            ));
        }
    }
    Ok(())
}

/// Adds what an AI client should do next to a Runtime failure that does not already say it. A
/// client that only sees a code guesses; one told the next step takes it. Only codes with a
/// concrete next step get one, and a message the Runtime wrote itself is never replaced.
fn with_next_step(mut error: Value, arguments: &Map<String, Value>) -> Value {
    let Some(object) = error.as_object_mut() else {
        return error;
    };
    if object.contains_key("message") {
        return error;
    }
    let code = object
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if let Some(step) = text_step(code, arguments).or_else(|| next_step(code)) {
        object.insert("message".to_owned(), Value::from(step));
    }
    error
}

/// Typing fails for reasons the code alone does not name: the target is not an editor, or no editor
/// has input focus. Focusing one is left to the caller, because nothing an unfocused input exposes
/// tells it apart from a control that acts when tapped.
fn text_step(code: &str, arguments: &Map<String, Value>) -> Option<&'static str> {
    let input = arguments.get("input")?;
    if arguments.get("action")?.as_str()? != "interact"
        || input.get("operation")?.as_str()? != "text"
    {
        return None;
    }
    match (code, input.get("node_ref").is_some()) {
        ("UNSUPPORTED", true) => Some(concat!(
            "That node does not accept text. If it is an input's placeholder, tap it to focus ",
            "the editor, observe again, then send text without node_ref to type into the focused ",
            "editor."
        )),
        ("CAPABILITY_UNAVAILABLE", false) => Some(concat!(
            "No editor has input focus. Observe, tap the input field, then send the text again ",
            "without node_ref. If that still fails, call context status to check text input."
        )),
        _ => None,
    }
}

fn next_step(code: &str) -> Option<&'static str> {
    Some(match code {
        "STALE_REFERENCE" => concat!(
            "The screen changed after that observation, or that observation could not address the ",
            "screen (see its interact facts). Call visual observe again and act on the new ",
            "observation's node_ref or coordinates; the old ones will not be accepted again."
        ),
        "STALE_AUTHORITY" => concat!(
            "The display or execution backend changed while this call was handled. Observe or ",
            "query again, then retry with fresh values."
        ),
        "CAPABILITY_UNAVAILABLE" => concat!(
            "That capability is not available right now. Call context status to see which ",
            "capabilities are available and why."
        ),
        "UNSUPPORTED" => concat!(
            "The current backend does not support this. Call context status to see what this ",
            "device supports, or choose another action."
        ),
        "RUN_AS_UNAVAILABLE" => concat!(
            "That run_as identity is not available. Call context status and use an identity it ",
            "lists as available."
        ),
        "PERMISSION_DENIED" => concat!(
            "The device refused this operation. Call context status to see which permission or ",
            "backend it needs."
        ),
        "NOT_FOUND" => {
            "The referenced item does not exist or has expired. Obtain a fresh reference first."
        }
        "TIMEOUT" => concat!(
            "It did not finish in time. Retry; for long-running commands use command run with ",
            "as_task and follow the task with task_control."
        ),
        "RESOURCE_LIMIT" => concat!(
            "A size or count limit was reached. Ask for less, for example fewer nodes, no image ",
            "or a smaller file."
        ),
        "HOST_TRANSITION_PENDING" => {
            "DroidBridge is switching its execution backend. Retry in a few seconds."
        }
        _ => return None,
    })
}

fn tool_definitions() -> Result<Value, DomainError> {
    let bundle: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../tools/fixtures/contract/contract-schema.v1.json"
    )))
    .map_err(|_| generated_contract_invalid())?;
    let metadata: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../tools/fixtures/contract/contract-metadata.v1.json"
    )))
    .map_err(|_| generated_contract_invalid())?;
    let schemas = bundle
        .get("schemas")
        .and_then(Value::as_object)
        .ok_or_else(generated_contract_invalid)?;
    let request = schemas
        .get("public_request")
        .and_then(Value::as_object)
        .ok_or_else(generated_contract_invalid)?;
    let dialect = request
        .get("$schema")
        .cloned()
        .ok_or_else(generated_contract_invalid)?;
    let request_defs = request
        .get("$defs")
        .and_then(Value::as_object)
        .ok_or_else(generated_contract_invalid)?;
    let variants = request_defs
        .get("PublicPayload")
        .and_then(|payload| payload.get("oneOf"))
        .and_then(Value::as_array)
        .ok_or_else(generated_contract_invalid)?;
    let bindings = metadata
        .get("result_schema_bindings")
        .and_then(Value::as_object)
        .ok_or_else(generated_contract_invalid)?;
    let mut tools = Vec::with_capacity(MCP_TOOL_NAMES.len());
    for name in MCP_TOOL_NAMES {
        let mut input = variants
            .iter()
            .find(|variant| variant.pointer("/properties/tool/const") == Some(&json!(name)))
            .and_then(Value::as_object)
            .cloned()
            .ok_or_else(generated_contract_invalid)?;
        // The caller's arguments are the payload with `tool` fixed by the Tool name.
        if let Some(Value::Object(properties)) = input.get_mut("properties") {
            properties.remove("tool");
            if properties.is_empty() {
                input.remove("properties");
            }
        }
        if let Some(Value::Array(required)) = input.get_mut("required") {
            required.retain(|field| field != "tool");
            if required.is_empty() {
                input.remove("required");
            }
        }
        input.insert("$schema".to_owned(), dialect.clone());
        input.insert("$defs".to_owned(), Value::Object(request_defs.clone()));

        let mut output_defs = Map::new();
        let mut branches = Vec::new();
        let mut schema_names = Vec::new();
        for spec in contract::ACTION_SPECS
            .iter()
            .filter(|spec| spec.tool == name)
        {
            let names = bindings
                .get(&format!("{}.{}", spec.tool, spec.action))
                .and_then(Value::as_array)
                .ok_or_else(generated_contract_invalid)?;
            for schema_name in names {
                let schema_name = schema_name
                    .as_str()
                    .ok_or_else(generated_contract_invalid)?;
                if !schema_names.contains(&schema_name) {
                    schema_names.push(schema_name);
                }
            }
        }
        schema_names.push("public_error");
        for schema_name in schema_names {
            let mut schema = schemas
                .get(schema_name)
                .and_then(Value::as_object)
                .cloned()
                .ok_or_else(generated_contract_invalid)?;
            schema.remove("$schema");
            if let Some(Value::Object(defs)) = schema.remove("$defs") {
                for (def_name, def) in defs {
                    match output_defs.get(&def_name) {
                        Some(existing) if existing != &def => {
                            return Err(generated_contract_invalid());
                        }
                        _ => {
                            output_defs.insert(def_name, def);
                        }
                    }
                }
            }
            branches.push(Value::Object(schema));
        }
        let mut output = Map::new();
        output.insert("$schema".to_owned(), dialect.clone());
        output.insert("anyOf".to_owned(), Value::Array(branches));
        if !output_defs.is_empty() {
            output.insert("$defs".to_owned(), Value::Object(output_defs));
        }
        let shown = presentation(name);
        tools.push(json!({
            "name": name,
            "title": shown.title,
            "description": shown.description,
            "inputSchema": Value::Object(input),
            "outputSchema": Value::Object(output),
            "annotations": {
                "title": shown.title,
                "readOnlyHint": shown.read_only,
                "destructiveHint": shown.destructive,
                "idempotentHint": shown.idempotent,
                "openWorldHint": shown.open_world,
            },
        }));
    }
    Ok(Value::Array(tools))
}

fn collect_refs(value: &Value, refs: &mut BTreeSet<String>) {
    match value {
        Value::Object(fields) => {
            for (key, field) in fields {
                match field {
                    Value::String(artifact_ref) if REF_FIELDS.contains(&key.as_str()) => {
                        refs.insert(artifact_ref.clone());
                    }
                    _ => collect_refs(field, refs),
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|item| collect_refs(item, refs)),
        _ => {}
    }
}

fn header_values<'a>(request: &'a McpRequest, name: &str) -> Vec<&'a str> {
    request
        .headers
        .iter()
        .filter(|(header, _)| header.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
        .collect()
}

fn single_header<'a>(request: &'a McpRequest, name: &str) -> Option<&'a str> {
    match header_values(request, name).as_slice() {
        [value] => Some(value),
        _ => None,
    }
}

fn origin_is_loopback(request: &McpRequest) -> bool {
    match header_values(request, "origin").as_slice() {
        [] => true,
        [origin] => origin
            .split_once("://")
            .and_then(|(_, authority)| {
                let host = authority.split(':').next()?;
                (!authority.contains('/')).then_some(host)
            })
            .is_some_and(|host| {
                host.eq_ignore_ascii_case("127.0.0.1") || host.eq_ignore_ascii_case("localhost")
            }),
        _ => false,
    }
}

fn bearer_matches(request: &McpRequest, accepted_token: &str) -> bool {
    let Some(token) =
        single_header(request, "authorization").and_then(|value| value.strip_prefix("Bearer "))
    else {
        return false;
    };
    if accepted_token.is_empty() || token.len() != accepted_token.len() {
        return false;
    }
    token
        .bytes()
        .zip(accepted_token.bytes())
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn content_type_is_json(request: &McpRequest) -> bool {
    single_header(request, "content-type").is_some_and(|value| {
        value
            .split(';')
            .next()
            .is_some_and(|media| media.trim().eq_ignore_ascii_case("application/json"))
    })
}

fn accept_is_acceptable(request: &McpRequest) -> bool {
    let ranges = header_values(request, "accept")
        .into_iter()
        .flat_map(|value| value.split(','))
        .filter_map(|range| range.split(';').next())
        .map(|media| media.trim().to_ascii_lowercase())
        .collect::<Vec<_>>();
    let accepts = |media: &str| ranges.iter().any(|range| range == media || range == "*/*");
    accepts("application/json") && accepts("text/event-stream")
}

fn decode_name_header(value: &str) -> Option<String> {
    match value
        .strip_prefix("=?base64?")
        .and_then(|rest| rest.strip_suffix("?="))
    {
        Some(encoded) => String::from_utf8(STANDARD.decode(encoded).ok()?).ok(),
        None => Some(value.to_owned()),
    }
}

fn is_protocol_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            4 | 7 => *byte == b'-',
            _ => byte.is_ascii_digit(),
        })
}

fn only_keys(params: &Map<String, Value>, allowed: &[&str]) -> bool {
    params.keys().all(|key| allowed.contains(&key.as_str()))
}

fn rpc_result(id: Value, result: Value) -> McpResponse {
    json_response(200, &json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

fn rpc_error(status: u16, id: Value, code: i64, message: &str, data: Option<Value>) -> McpResponse {
    let mut error = json!({"code": code, "message": message});
    if let Some(data) = data {
        error["data"] = data;
    }
    json_response(status, &json!({"jsonrpc": "2.0", "id": id, "error": error}))
}

fn invalid_params(id: Value, data: Option<Value>) -> McpResponse {
    rpc_error(200, id, INVALID_PARAMS, "Invalid params", data)
}

/// Names the field a rejected request got wrong, so a caller can repair the call from the response
/// alone. The refused value itself is never echoed: it can be arbitrarily large or sensitive.
fn field_detail(field: &str, reason: &str) -> Option<Value> {
    Some(json!({"field": field, "reason": reason}))
}

fn internal_error(id: Value) -> McpResponse {
    rpc_error(200, id, INTERNAL_ERROR, "Internal error", None)
}

fn json_response(status: u16, value: &Value) -> McpResponse {
    McpResponse {
        status,
        body: Some(serde_json::to_vec(value).expect("MCP responses contain only JSON values")),
    }
}

fn generated_contract_invalid() -> DomainError {
    DomainError::new(
        ErrorCode::InternalError,
        "generated Contract artifacts cannot project MCP tool schemas",
    )
}

fn invalid_host_reply() -> DomainError {
    DomainError::new(ErrorCode::IoError, "artifact query reply is invalid")
}
