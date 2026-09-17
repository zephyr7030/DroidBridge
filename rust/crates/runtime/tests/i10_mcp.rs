//! I10 MCP facade gates: method set, header/envelope validation, bearer authentication,
//! tool/resource mapping and routing without owned business truth.

use domain::DomainError;
use runtime::{
    MCP_DEBUG_PORT, McpArtifactReply, McpFacade, McpHost, McpRequest, McpResponse, PortFuture,
    mcp_resource_cursor, mcp_resource_cursor_ref,
};
use serde_json::{Value, json};
use std::{
    io::{Seek, SeekFrom, Write},
    sync::{Arc, Mutex},
};

const TOKEN: &str = "tXw1sO3n6b2WqQm9J0gKf8yVhZcR4dLpA7eNuTiYkMs";
const REQUEST_ID: &str = "99200000-0000-4000-8000-000000000001";

#[derive(Default)]
struct HostState {
    submitted: Vec<Value>,
    queries: Vec<Value>,
    submit_reply: Option<Result<Value, ()>>,
    query_replies: Vec<(Value, Option<Vec<u8>>)>,
}

#[derive(Clone, Default)]
struct RecordingHost(Arc<Mutex<HostState>>);

impl RecordingHost {
    fn reply_submit(&self, response: Value) {
        self.0.lock().unwrap().submit_reply = Some(Ok(response));
    }

    fn fail_submit(&self) {
        self.0.lock().unwrap().submit_reply = Some(Err(()));
    }

    fn queue_query(&self, payload: Value, bytes: Option<&[u8]>) {
        self.0
            .lock()
            .unwrap()
            .query_replies
            .push((payload, bytes.map(<[u8]>::to_vec)));
    }

    fn submitted(&self) -> Vec<Value> {
        self.0.lock().unwrap().submitted.clone()
    }

    fn queries(&self) -> Vec<Value> {
        self.0.lock().unwrap().queries.clone()
    }
}

impl McpHost for RecordingHost {
    fn submit<'a>(&'a self, envelope: Vec<u8>) -> PortFuture<'a, Result<Vec<u8>, DomainError>> {
        Box::pin(async move {
            let mut state = self.0.lock().unwrap();
            state
                .submitted
                .push(serde_json::from_slice(&envelope).unwrap());
            match state.submit_reply.clone() {
                Some(Ok(response)) => Ok(serde_json::to_vec(&response).unwrap()),
                _ => Err(DomainError::new(
                    contract::ErrorCode::CapabilityUnavailable,
                    "Runtime unavailable",
                )),
            }
        })
    }

    fn artifact_query<'a>(
        &'a self,
        query: Value,
    ) -> PortFuture<'a, Result<McpArtifactReply, DomainError>> {
        Box::pin(async move {
            let mut state = self.0.lock().unwrap();
            state.queries.push(query);
            if state.query_replies.is_empty() {
                return Err(DomainError::new(
                    contract::ErrorCode::IoError,
                    "companion unavailable",
                ));
            }
            let (payload, bytes) = state.query_replies.remove(0);
            let descriptor = bytes.map(|bytes| {
                let mut file = tempfile();
                file.write_all(&bytes).unwrap();
                file.seek(SeekFrom::Start(0)).unwrap();
                file
            });
            Ok(McpArtifactReply {
                payload,
                descriptor,
            })
        })
    }
}

fn tempfile() -> std::fs::File {
    let path = std::env::temp_dir().join(format!(
        "droidbridge-i10-mcp-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    std::fs::remove_file(path).unwrap();
    file
}

fn facade() -> (McpFacade<RecordingHost>, RecordingHost) {
    let host = RecordingHost::default();
    (
        McpFacade::new(host.clone(), MCP_DEBUG_PORT, "0.1.0".to_owned()).unwrap(),
        host,
    )
}

fn meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": {},
    })
}

fn body(id: Value, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

fn headers(method: &str, name: Option<&str>) -> Vec<(String, String)> {
    let mut headers = vec![
        ("Host".to_owned(), "127.0.0.1:18765".to_owned()),
        ("Authorization".to_owned(), format!("Bearer {TOKEN}")),
        ("Content-Type".to_owned(), "application/json".to_owned()),
        (
            "Accept".to_owned(),
            "application/json, text/event-stream".to_owned(),
        ),
        ("MCP-Protocol-Version".to_owned(), "2026-07-28".to_owned()),
        ("Mcp-Method".to_owned(), method.to_owned()),
    ];
    if let Some(name) = name {
        headers.push(("Mcp-Name".to_owned(), name.to_owned()));
    }
    headers
}

fn request(method: &str, name: Option<&str>, body: &Value) -> McpRequest {
    McpRequest {
        method: "POST".to_owned(),
        headers: headers(method, name),
        body: serde_json::to_vec(body).unwrap(),
    }
}

fn with_header(mut request: McpRequest, name: &str, value: Option<&str>) -> McpRequest {
    request
        .headers
        .retain(|(header, _)| !header.eq_ignore_ascii_case(name));
    if let Some(value) = value {
        request.headers.push((name.to_owned(), value.to_owned()));
    }
    request
}

fn json_body(response: &McpResponse) -> Value {
    serde_json::from_slice(response.body.as_ref().expect("JSON body")).unwrap()
}

fn tool_call(name: &str, arguments: Value) -> McpRequest {
    let mut meta = meta();
    meta["io.droidbridge/requestId"] = json!(REQUEST_ID);
    request(
        "tools/call",
        Some(name),
        &body(
            json!(7),
            "tools/call",
            json!({"name": name, "arguments": arguments, "_meta": meta}),
        ),
    )
}

#[tokio::test]
async fn i10_g04_supported_method_set_is_exactly_the_five_2026_methods() {
    let (facade, _) = facade();
    let discover = facade
        .handle(
            request(
                "server/discover",
                None,
                &body(json!(1), "server/discover", json!({"_meta": meta()})),
            ),
            TOKEN,
        )
        .await;
    assert_eq!(discover.status, 200);
    assert_eq!(
        json_body(&discover),
        json!({"jsonrpc": "2.0", "id": 1, "result": {
            "resultType": "complete",
            "supportedVersions": ["2026-07-28"],
            "capabilities": {"tools": {}, "resources": {}},
            "ttlMs": 0,
            "cacheScope": "private",
            "_meta": {"io.modelcontextprotocol/serverInfo": {"name": "DroidBridge", "version": "0.1.0"}},
        }})
    );

    let listed = facade
        .handle(
            request(
                "tools/list",
                None,
                &body(json!("list"), "tools/list", json!({"_meta": meta()})),
            ),
            TOKEN,
        )
        .await;
    let listed = json_body(&listed);
    let tools = listed["result"]["tools"].as_array().unwrap();
    assert_eq!(
        tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        runtime::MCP_TOOL_NAMES.to_vec()
    );
    for tool in tools {
        let mut keys = tool
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "annotations",
                "description",
                "inputSchema",
                "name",
                "outputSchema",
                "title"
            ]
        );
        let hints = tool["annotations"].as_object().unwrap();
        for hint in [
            "readOnlyHint",
            "destructiveHint",
            "idempotentHint",
            "openWorldHint",
        ] {
            assert!(hints[hint].is_boolean(), "{hint} is declared");
        }
        // Only the status/catalog tool is read-only; every tool with a mutating action says so.
        assert_eq!(hints["readOnlyHint"], tool["name"] == "context");
        assert_eq!(hints["destructiveHint"], tool["name"] != "context");
        // The caller never supplies `tool`; the root is the generated `{action,input}` union.
        assert!(tool["inputSchema"].pointer("/properties/tool").is_none());
        assert_eq!(
            tool["inputSchema"]["oneOf"].as_array().unwrap().len(),
            contract::ACTION_SPECS
                .iter()
                .filter(|spec| spec.tool == tool["name"])
                .count()
        );
        assert_eq!(tool["inputSchema"]["unevaluatedProperties"], false);
        assert_eq!(
            tool["outputSchema"]["anyOf"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()["title"],
            "PublicError"
        );
    }
    assert!(listed["result"].get("nextCursor").is_none());

    for (method, status, code) in [
        ("prompts/list", 404, -32601),
        ("initialize", 404, -32601),
        ("subscriptions/listen", 404, -32601),
    ] {
        let response = facade
            .handle(
                request(
                    method,
                    None,
                    &body(json!(2), method, json!({"_meta": meta()})),
                ),
                TOKEN,
            )
            .await;
        assert_eq!(response.status, status, "{method}");
        assert_eq!(json_body(&response)["error"]["code"], code, "{method}");
    }
}

#[tokio::test]
async fn i10_g05_header_and_envelope_validation_is_deterministic() {
    let (facade, host) = facade();
    let list = || {
        request(
            "tools/list",
            None,
            &body(json!(3), "tools/list", json!({"_meta": meta()})),
        )
    };

    let cases: Vec<(McpRequest, u16, Option<i64>)> = vec![
        (
            McpRequest {
                method: "GET".to_owned(),
                ..list()
            },
            405,
            None,
        ),
        (
            with_header(list(), "Host", Some("192.168.1.2:18765")),
            403,
            None,
        ),
        (
            with_header(list(), "Host", Some("localhost:8765")),
            403,
            None,
        ),
        (
            with_header(list(), "Origin", Some("https://evil.example")),
            403,
            None,
        ),
        (
            with_header(list(), "Content-Type", Some("text/plain")),
            415,
            None,
        ),
        (
            with_header(list(), "Accept", Some("application/json")),
            406,
            None,
        ),
        (
            McpRequest {
                body: vec![b' '; runtime::MCP_BODY_LIMIT_BYTES + 1],
                ..list()
            },
            413,
            None,
        ),
        (
            McpRequest {
                body: b"{not json".to_vec(),
                ..list()
            },
            400,
            Some(-32700),
        ),
        (
            McpRequest {
                body: serde_json::to_vec(&json!([body(json!(1), "tools/list", json!({}))]))
                    .unwrap(),
                ..list()
            },
            400,
            Some(-32600),
        ),
        (
            with_header(list(), "MCP-Protocol-Version", None),
            400,
            Some(-32020),
        ),
        (
            with_header(list(), "MCP-Protocol-Version", Some("2025-11-25")),
            400,
            Some(-32022),
        ),
        (
            with_header(list(), "Mcp-Method", Some("tools/call")),
            400,
            Some(-32020),
        ),
        (
            with_header(list(), "Mcp-Name", Some("context")),
            400,
            Some(-32020),
        ),
        (
            request(
                "tools/list",
                None,
                &body(
                    json!(4),
                    "tools/list",
                    json!({"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28"}}),
                ),
            ),
            200,
            Some(-32602),
        ),
        (
            request(
                "tools/list",
                None,
                &body(
                    json!(5),
                    "tools/list",
                    json!({"_meta": {
                        "io.modelcontextprotocol/protocolVersion": "2026-07-01",
                        "io.modelcontextprotocol/clientCapabilities": {},
                    }}),
                ),
            ),
            400,
            Some(-32020),
        ),
        (
            request(
                "tools/list",
                None,
                &json!({"jsonrpc": "1.0", "id": 6, "method": "tools/list", "params": {"_meta": meta()}}),
            ),
            200,
            Some(-32600),
        ),
    ];
    for (index, (candidate, status, code)) in cases.into_iter().enumerate() {
        let response = facade.handle(candidate, TOKEN).await;
        assert_eq!(response.status, status, "case {index}");
        match code {
            Some(code) => assert_eq!(json_body(&response)["error"]["code"], code, "case {index}"),
            None => assert!(response.body.is_none(), "case {index}"),
        }
    }

    let unsupported = facade
        .handle(
            with_header(list(), "MCP-Protocol-Version", Some("2025-11-25")),
            TOKEN,
        )
        .await;
    assert_eq!(
        json_body(&unsupported)["error"]["data"],
        json!({"supported": ["2026-07-28"], "requested": "2025-11-25"})
    );

    // A notification is refused with no body and no business side effect.
    let notification = facade
        .handle(
            McpRequest {
                body: serde_json::to_vec(&json!({"jsonrpc": "2.0", "method": "tools/list"}))
                    .unwrap(),
                ..list()
            },
            TOKEN,
        )
        .await;
    assert_eq!(
        notification,
        McpResponse {
            status: 400,
            body: None
        }
    );

    // tools/call requires Mcp-Name equal to params.name before any business admission.
    let unnamed = with_header(
        tool_call("context", json!({"action": "status", "input": {}})),
        "Mcp-Name",
        None,
    );
    assert_eq!(
        json_body(&facade.handle(unnamed, TOKEN).await)["error"]["code"],
        -32020
    );
    let misnamed = with_header(
        tool_call("context", json!({"action": "status", "input": {}})),
        "Mcp-Name",
        Some("filesystem"),
    );
    assert_eq!(
        json_body(&facade.handle(misnamed, TOKEN).await)["error"]["code"],
        -32020
    );
    assert!(host.submitted().is_empty());
}

#[tokio::test]
async fn i10_g06_bearer_authentication_admits_only_the_committed_token() {
    let (facade, host) = facade();
    host.reply_submit(json!({
        "protocol_version": 1, "request_id": REQUEST_ID, "outcome": "success",
        "result": {"current_namespace": "", "root_tools": [], "siblings": [], "actions": []},
    }));
    let call = || tool_call("context", json!({"action": "catalog", "input": {}}));

    for candidate in [
        with_header(call(), "Authorization", None),
        with_header(call(), "Authorization", Some("Bearer wrong")),
        with_header(call(), "Authorization", Some(&format!("bearer {TOKEN}"))),
        with_header(call(), "Authorization", Some(TOKEN)),
    ] {
        assert_eq!(
            facade.handle(candidate, TOKEN).await,
            McpResponse {
                status: 401,
                body: None
            }
        );
    }
    // After rotation the previously committed token is refused on the very next request.
    assert_eq!(
        facade
            .handle(call(), "Q2Vk5Yv0nLm8sX1rT4aZ7uB3cW6eH9jK2pF5gD8iM0o")
            .await
            .status,
        401
    );
    assert_eq!(facade.handle(call(), "").await.status, 401);
    assert!(host.submitted().is_empty());

    assert_eq!(facade.handle(call(), TOKEN).await.status, 200);
    assert_eq!(host.submitted().len(), 1);
}

#[tokio::test]
async fn i10_g07_tool_and_resource_references_map_deterministically() {
    let (facade, host) = facade();
    host.reply_submit(json!({
        "protocol_version": 1, "request_id": REQUEST_ID, "outcome": "success",
        "result": {"exit_code": 0, "stdout_ref": "dbref:stdout:99300000-0000-4000-8000-000000000001",
                   "nested": {"image_ref": "dbref:image:99300000-0000-4000-8000-000000000002"}},
    }));
    host.queue_query(
        json!({"artifacts": [
            {"ref": "dbref:image:99300000-0000-4000-8000-000000000002", "expires_at": "2026-09-16T08:00:00.000Z"},
            {"ref": "dbref:stdout:99300000-0000-4000-8000-000000000001", "expires_at": "2026-09-16T08:00:01.000Z"},
        ]}),
        None,
    );
    let called = json_body(
        &facade
            .handle(
                tool_call(
                    "command",
                    json!({"action": "run", "input": {"command": "id", "run_as": "app"}}),
                ),
                TOKEN,
            )
            .await,
    );
    assert_eq!(
        host.submitted(),
        vec![json!({
            "protocol_version": 1,
            "request_id": REQUEST_ID,
            "payload": {"tool": "command", "action": "run", "input": {"command": "id", "run_as": "app"}},
        })]
    );
    let result = &called["result"];
    assert_eq!(result["resultType"], "complete");
    assert_eq!(result["isError"], false);
    assert_eq!(
        result["content"][0]["text"],
        serde_json::to_string(&result["structuredContent"]).unwrap()
    );
    assert_eq!(
        result["_meta"]["io.droidbridge/refExpiries"],
        json!({
            "dbref:image:99300000-0000-4000-8000-000000000002": "2026-09-16T08:00:00.000Z",
            "dbref:stdout:99300000-0000-4000-8000-000000000001": "2026-09-16T08:00:01.000Z",
        })
    );
    assert_eq!(
        host.queries(),
        vec![
            json!({"protocol_version": 1, "artifact_query": {"operation": "metadata", "refs": [
                "dbref:image:99300000-0000-4000-8000-000000000002",
                "dbref:stdout:99300000-0000-4000-8000-000000000001",
            ]}})
        ]
    );

    // A business failure is the exact R-CONTRACT-002 error with isError=true, never a JSON-RPC code.
    let error =
        json!({"code": "CAPABILITY_UNAVAILABLE", "operation": "command.run", "retryable": false});
    host.reply_submit(json!({"protocol_version": 1, "request_id": REQUEST_ID, "outcome": "error", "error": error}));
    let failed = json_body(
        &facade
            .handle(
                tool_call(
                    "command",
                    json!({"action": "run", "input": {"command": "id", "run_as": "root"}}),
                ),
                TOKEN,
            )
            .await,
    );
    assert_eq!(failed["result"]["structuredContent"], error);
    assert_eq!(failed["result"]["isError"], true);
    assert!(
        failed["result"]["_meta"]
            .get("io.droidbridge/refExpiries")
            .is_none()
    );

    // Arguments outside the tool's generated union are -32602, and the rejection names the field
    // that has to change.
    for arguments in [
        json!({"action": "run"}),
        json!({"action": "status", "input": {}}),
        json!({"action": "run", "input": {"command": "id", "run_as": "app"}, "extra": true}),
    ] {
        let response = json_body(&facade.handle(tool_call("command", arguments), TOKEN).await);
        assert_eq!(response["error"]["code"], -32602);
        assert_eq!(response["error"]["data"]["field"], "params.arguments");
    }
    let unknown_tool = tool_call(
        "automation.execution",
        json!({"action": "run", "input": {}}),
    );
    let unknown = json_body(&facade.handle(unknown_tool, TOKEN).await);
    assert_eq!(unknown["error"]["code"], -32602);
    assert_eq!(unknown["error"]["data"]["field"], "params.name");

    // resources/list projects `{uri,name,mimeType?,size}` and passes the server-issued cursor.
    let cursor = mcp_resource_cursor("dbref:data:99300000-0000-4000-8000-000000000003");
    assert_eq!(
        mcp_resource_cursor_ref(&cursor).as_deref(),
        Some("dbref:data:99300000-0000-4000-8000-000000000003")
    );
    assert_eq!(mcp_resource_cursor_ref("not a cursor!"), None);
    host.queue_query(
        json!({"resources": [
            {"uri": "dbref:image:99300000-0000-4000-8000-000000000002", "mime": "image/png", "size": 4},
            {"uri": "dbref:data:99300000-0000-4000-8000-000000000003", "size": 3},
        ], "next_cursor": cursor}),
        None,
    );
    let listed = json_body(
        &facade
            .handle(
                request(
                    "resources/list",
                    None,
                    &body(json!(8), "resources/list", json!({"_meta": meta()})),
                ),
                TOKEN,
            )
            .await,
    );
    assert_eq!(
        listed["result"]["resources"],
        json!([
            {"uri": "dbref:image:99300000-0000-4000-8000-000000000002", "name": "dbref:image:99300000-0000-4000-8000-000000000002", "mimeType": "image/png", "size": 4},
            {"uri": "dbref:data:99300000-0000-4000-8000-000000000003", "name": "dbref:data:99300000-0000-4000-8000-000000000003", "size": 3},
        ])
    );
    assert_eq!(listed["result"]["nextCursor"], cursor);
    host.queue_query(json!({"error": "invalid_cursor"}), None);
    let invalid = facade
        .handle(
            request(
                "resources/list",
                None,
                &body(
                    json!(9),
                    "resources/list",
                    json!({"cursor": "stale", "_meta": meta()}),
                ),
            ),
            TOKEN,
        )
        .await;
    assert_eq!(json_body(&invalid)["error"]["code"], -32602);

    // resources/read: UTF-8 text, arbitrary bytes and images use their exact content shapes.
    let read = |uri: &str| {
        request(
            "resources/read",
            Some(uri),
            &body(
                json!(10),
                "resources/read",
                json!({"uri": uri, "_meta": meta()}),
            ),
        )
    };
    let text_uri = "dbref:stdout:99300000-0000-4000-8000-000000000001";
    host.queue_query(
        json!({"uri": text_uri, "kind": "stdout", "size": 6}),
        Some(b"uid=0\n"),
    );
    assert_eq!(
        json_body(&facade.handle(read(text_uri), TOKEN).await)["result"]["contents"],
        json!([{"uri": text_uri, "text": "uid=0\n"}])
    );
    let data_uri = "dbref:data:99300000-0000-4000-8000-000000000003";
    host.queue_query(
        json!({"uri": data_uri, "kind": "data", "size": 3}),
        Some(&[0xff, 0x00, 0xfe]),
    );
    assert_eq!(
        json_body(&facade.handle(read(data_uri), TOKEN).await)["result"]["contents"],
        json!([{"uri": data_uri, "mimeType": "application/octet-stream", "blob": "/wD+"}])
    );
    let image_uri = "dbref:image:99300000-0000-4000-8000-000000000002";
    host.queue_query(
        json!({"uri": image_uri, "kind": "image", "mime": "image/png", "size": 4}),
        Some(b"\x89PNG"),
    );
    assert_eq!(
        json_body(&facade.handle(read(image_uri), TOKEN).await)["result"]["contents"],
        json!([{"uri": image_uri, "mimeType": "image/png", "blob": "iVBORw=="}])
    );
    host.queue_query(json!({"error": "not_found"}), None);
    let missing = json_body(&facade.handle(read("dbref:stdout:gone"), TOKEN).await);
    assert_eq!(missing["error"]["code"], -32602);
    assert_eq!(missing["error"]["data"]["field"], "params.uri");

    // A short descriptor never yields a partial result.
    host.queue_query(
        json!({"uri": text_uri, "kind": "stdout", "size": 99}),
        Some(b"uid=0\n"),
    );
    assert_eq!(
        json_body(&facade.handle(read(text_uri), TOKEN).await)["error"]["code"],
        -32603
    );
}

#[tokio::test]
async fn i10_g08_facade_routes_every_call_without_owning_business_truth() {
    let (facade, host) = facade();
    let call = || tool_call("task_control", json!({"action": "list", "input": {}}));

    // No facade-side replay: both identical calls reach the authoritative host, whose retained
    // request_id owns duplicate safety.
    host.reply_submit(json!({
        "protocol_version": 1, "request_id": REQUEST_ID, "outcome": "success", "result": {"tasks": []},
    }));
    facade.handle(call(), TOKEN).await;
    facade.handle(call(), TOKEN).await;
    assert_eq!(host.submitted().len(), 2);
    assert_eq!(host.submitted()[0], host.submitted()[1]);

    // An unavailable host is reported as the structured failure of the call that asked for it, so a
    // client reads a code and a reason instead of an opaque internal JSON-RPC error.
    host.fail_submit();
    let unavailable = json_body(&facade.handle(call(), TOKEN).await);
    assert_eq!(unavailable["result"]["isError"], true);
    assert_eq!(
        unavailable["result"]["structuredContent"],
        json!({
            "code": "CAPABILITY_UNAVAILABLE",
            "operation": "task_control.list",
            "retryable": false,
            "message": "Runtime unavailable",
        })
    );
    assert_eq!(
        unavailable["result"]["content"][0]["text"],
        unavailable["result"]["structuredContent"].to_string()
    );
    assert!(unavailable.get("error").is_none());

    // Resource listings are requeried from the host every time; nothing is cached.
    for _ in 0..2 {
        host.queue_query(json!({"resources": []}), None);
        let listed = facade
            .handle(
                request(
                    "resources/list",
                    None,
                    &body(json!(11), "resources/list", json!({"_meta": meta()})),
                ),
                TOKEN,
            )
            .await;
        assert_eq!(json_body(&listed)["result"]["resources"], json!([]));
    }
    assert_eq!(host.queries().len(), 2);
    let lost = facade
        .handle(
            request(
                "resources/list",
                None,
                &body(json!(12), "resources/list", json!({"_meta": meta()})),
            ),
            TOKEN,
        )
        .await;
    assert_eq!(json_body(&lost)["error"]["code"], -32603);
}

#[tokio::test]
async fn i10_g09_tool_calls_are_admitted_without_the_private_request_id_key() {
    let (facade, host) = facade();
    host.reply_submit(json!({
        "protocol_version": 1, "request_id": REQUEST_ID, "outcome": "success",
        "result": {"operation": "read"},
    }));
    let arguments = json!({"action": "clipboard", "input": {"operation": "read"}});

    // The private key is not part of 2026-07-28 `_meta` and `tools/list` never advertises it, so a
    // spec-conformant client cannot supply it: the call is admitted with an id minted here.
    let conformant = || {
        request(
            "tools/call",
            Some("android"),
            &body(
                json!(7),
                "tools/call",
                json!({"name": "android", "arguments": arguments, "_meta": meta()}),
            ),
        )
    };
    let admitted = json_body(&facade.handle(conformant(), TOKEN).await);
    assert_eq!(admitted["result"]["isError"], false);
    facade.handle(conformant(), TOKEN).await;
    let submitted = host.submitted();
    assert_eq!(submitted.len(), 2);
    for envelope in &submitted {
        assert_eq!(envelope["payload"]["tool"], "android");
        let request_id = envelope["request_id"].as_str().expect("request_id");
        assert_eq!(
            uuid::Uuid::parse_str(request_id).unwrap().get_version_num(),
            4
        );
    }
    assert_ne!(submitted[0]["request_id"], submitted[1]["request_id"]);

    // A caller that does know the key keeps owning its id, and the host its duplicate safety.
    facade
        .handle(tool_call("android", arguments.clone()), TOKEN)
        .await;
    assert_eq!(host.submitted()[2]["request_id"], REQUEST_ID);

    // A malformed id stays the caller's error instead of being silently replaced.
    let mut malformed_meta = meta();
    malformed_meta["io.droidbridge/requestId"] = json!("not-a-uuid");
    let malformed = json_body(
        &facade
            .handle(
                request(
                    "tools/call",
                    Some("android"),
                    &body(
                        json!(8),
                        "tools/call",
                        json!({"name": "android", "arguments": arguments, "_meta": malformed_meta}),
                    ),
                ),
                TOKEN,
            )
            .await,
    );
    assert_eq!(malformed["error"]["code"], -32602);
    assert_eq!(
        malformed["error"]["data"]["field"],
        "params._meta.io.droidbridge/requestId"
    );
    assert_eq!(host.submitted().len(), 3);
}
