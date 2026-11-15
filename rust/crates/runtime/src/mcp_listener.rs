//! The loopback MCP listener (S-MCP-001, S-STACK-011). Pinned Hyper frames HTTP/1.1 and the shared
//! facade owns every protocol decision; the host it is built with is the authoritative Runtime.

use crate::{MCP_BODY_LIMIT_BYTES, McpFacade, McpHost, McpRequest};
use bytes::Bytes;
use contract::ErrorCode;
use domain::DomainError;
use http_body_util::{BodyExt, Full};
use hyper::{
    Request, Response, StatusCode, body::Incoming, header, server::conn::http1, service::service_fn,
};
use hyper_util::rt::{TokioIo, TokioTimer};
use std::{
    convert::Infallible,
    net::{Ipv4Addr, SocketAddr, TcpListener as StdTcpListener},
    sync::{Arc, RwLock},
    time::Duration,
};
use tokio::sync::Semaphore;

/// Bounds the open connections. Any local App may connect before it proves the token, so without
/// this bound idle connections could exhaust the process's descriptors.
const MAX_CONNECTIONS: usize = 32;
/// A connection that has not sent a complete request head within this time is closed, including
/// one that sits idle between requests.
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);
/// A request body that has not fully arrived within this time is refused.
const BODY_READ_TIMEOUT: Duration = Duration::from_secs(30);
/// The pause after a failed accept, which is transient (descriptor or memory pressure, or a peer
/// that went away) and must not end the listener.
const ACCEPT_RETRY_DELAY: Duration = Duration::from_millis(100);

pub struct McpListener {
    runtime: tokio::runtime::Runtime,
    token: Arc<RwLock<String>>,
}

impl McpListener {
    /// Binds the IPv4 loopback endpoint and returns only once it accepts connections.
    pub fn start<H: McpHost + 'static>(
        port: u16,
        token: String,
        facade: McpFacade<H>,
    ) -> Result<Self, DomainError> {
        let listener = StdTcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
            .map_err(|_| listener_failed())?;
        Self::serve(listener, token, facade)
    }

    pub fn serve<H: McpHost + 'static>(
        listener: StdTcpListener,
        token: String,
        facade: McpFacade<H>,
    ) -> Result<Self, DomainError> {
        listener
            .set_nonblocking(true)
            .map_err(|_| listener_failed())?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("droidbridge-mcp")
            .enable_all()
            .build()
            .map_err(|_| listener_failed())?;
        let listener = {
            let _entered = runtime.enter();
            tokio::net::TcpListener::from_std(listener).map_err(|_| listener_failed())?
        };
        let token = Arc::new(RwLock::new(token));
        runtime.spawn(accept_loop(listener, Arc::new(facade), Arc::clone(&token)));
        Ok(Self { runtime, token })
    }

    pub fn set_token(&self, token: String) -> Result<(), DomainError> {
        *self.token.write().map_err(|_| {
            DomainError::new(ErrorCode::InternalError, "MCP token slot is unavailable")
        })? = token;
        Ok(())
    }

    pub fn state(&self) -> &'static str {
        "running"
    }

    /// Closes the listening socket and every open exchange; admitted Runtime work is unaffected.
    pub fn stop(self) {
        self.runtime.shutdown_timeout(Duration::from_secs(2));
    }
}

async fn accept_loop<H: McpHost + 'static>(
    listener: tokio::net::TcpListener,
    facade: Arc<McpFacade<H>>,
    token: Arc<RwLock<String>>,
) {
    let connections = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    loop {
        // A full set of connections leaves new ones waiting in the kernel backlog.
        let Ok(permit) = Arc::clone(&connections).acquire_owned().await else {
            return;
        };
        match listener.accept().await {
            Ok((stream, _)) => {
                let facade = Arc::clone(&facade);
                let token = Arc::clone(&token);
                tokio::spawn(async move {
                    let _permit = permit;
                    let service = service_fn(move |request| {
                        respond(request, Arc::clone(&facade), Arc::clone(&token))
                    });
                    // A client disconnect aborts only its own exchange (S-MCP-001); an admitted
                    // Task stays Runtime-owned, so nothing is left to settle here.
                    let _ = http1::Builder::new()
                        .timer(TokioTimer::new())
                        .header_read_timeout(HEADER_READ_TIMEOUT)
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                });
            }
            Err(_) => tokio::time::sleep(ACCEPT_RETRY_DELAY).await,
        }
    }
}

async fn respond<H: McpHost>(
    request: Request<Incoming>,
    facade: Arc<McpFacade<H>>,
    token: Arc<RwLock<String>>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    if request.uri().path() != "/mcp" {
        return Ok(empty(StatusCode::NOT_FOUND));
    }
    let method = request.method().as_str().to_owned();
    let headers = request
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_owned(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            )
        })
        .collect();
    let body =
        match tokio::time::timeout(BODY_READ_TIMEOUT, bounded_body(request.into_body())).await {
            Ok(Ok(body)) => body,
            Ok(Err(_)) => return Ok(empty(StatusCode::BAD_REQUEST)),
            Err(_) => return Ok(empty(StatusCode::REQUEST_TIMEOUT)),
        };
    // A poisoned token slot admits nobody rather than a stale token.
    let accepted = token.read().map(|token| token.clone()).unwrap_or_default();
    let response = facade
        .handle(
            McpRequest {
                method,
                headers,
                body,
            },
            &accepted,
        )
        .await;
    let status = StatusCode::from_u16(response.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let built = match response.body {
        Some(body) => Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Full::new(Bytes::from(body))),
        None => Response::builder()
            .status(status)
            .body(Full::new(Bytes::new())),
    };
    Ok(built.unwrap_or_else(|_| empty(StatusCode::INTERNAL_SERVER_ERROR)))
}

/// Buffers at most one byte past the S-MCP-001 cap, which is enough for the facade to answer 413
/// in its admission order without holding an oversized body.
async fn bounded_body(mut body: Incoming) -> Result<Vec<u8>, hyper::Error> {
    let mut bytes = Vec::new();
    while let Some(frame) = body.frame().await {
        if let Ok(data) = frame?.into_data() {
            let room = MCP_BODY_LIMIT_BYTES + 1 - bytes.len();
            bytes.extend_from_slice(&data[..data.len().min(room)]);
            if bytes.len() > MCP_BODY_LIMIT_BYTES {
                break;
            }
        }
    }
    Ok(bytes)
}

fn empty(status: StatusCode) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::new()));
    *response.status_mut() = status;
    response
}

fn listener_failed() -> DomainError {
    DomainError::new(ErrorCode::IoError, "MCP loopback listener failed")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{McpArtifactReply, PortFuture};
    use serde_json::{Value, json};
    use std::io::{Read, Write};

    const TOKEN: &str = "tXw1sO3n6b2WqQm9J0gKf8yVhZcR4dLpA7eNuTiYkMs";
    const ROTATED: &str = "Q2Vk5Yv0nLm8sX1rT4aZ7uB3cW6eH9jK2pF5gD8iM0o";

    struct CannedHost;

    impl McpHost for CannedHost {
        fn submit<'a>(&'a self, envelope: Vec<u8>) -> PortFuture<'a, Result<Vec<u8>, DomainError>> {
            Box::pin(async move {
                let request: Value = serde_json::from_slice(&envelope).unwrap();
                Ok(serde_json::to_vec(&json!({
                    "protocol_version": 1,
                    "request_id": request["request_id"],
                    "outcome": "success",
                    "result": {"tasks": []},
                }))
                .unwrap())
            })
        }

        fn artifact_query<'a>(
            &'a self,
            _query: Value,
        ) -> PortFuture<'a, Result<McpArtifactReply, DomainError>> {
            Box::pin(async { Err(listener_failed()) })
        }
    }

    fn exchange(port: u16, path: &str, token: &str, method: &str, body: &Value) -> String {
        let body = serde_json::to_vec(body).unwrap();
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let head = format!(
            "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\n\
             Content-Type: application/json\r\nAccept: application/json, text/event-stream\r\n\
             MCP-Protocol-Version: 2026-07-28\r\nMcp-Method: {method}\r\n{name}\
             Content-Length: {length}\r\nConnection: close\r\n\r\n",
            name = if method == "tools/call" {
                "Mcp-Name: task_control\r\n"
            } else {
                ""
            },
            length = body.len(),
        );
        stream.write_all(head.as_bytes()).unwrap();
        stream.write_all(&body).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    fn meta() -> Value {
        json!({
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {},
            "io.droidbridge/requestId": "99200000-0000-4000-8000-000000000009",
        })
    }

    #[test]
    fn i10_g06_loopback_listener_frames_http_through_the_shared_facade() {
        let listener = StdTcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let facade = McpFacade::new(CannedHost, "0.1.0".to_owned()).unwrap();
        let served = McpListener::serve(listener, TOKEN.to_owned(), facade).unwrap();
        assert_eq!(served.state(), "running");

        let call = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
            "name": "task_control", "arguments": {"action": "list", "input": {}}, "_meta": meta(),
        }});
        let answered = exchange(port, "/mcp", TOKEN, "tools/call", &call);
        assert!(answered.starts_with("HTTP/1.1 200 OK\r\n"), "{answered}");
        assert!(
            answered
                .to_ascii_lowercase()
                .contains("content-type: application/json\r\n"),
            "{answered}"
        );
        let body: Value = serde_json::from_str(answered.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body["result"]["structuredContent"], json!({"tasks": []}));
        assert_eq!(body["result"]["isError"], false);

        // Rotation replaces the accepted token for the very next request.
        served.set_token(ROTATED.to_owned()).unwrap();
        let refused = exchange(port, "/mcp", TOKEN, "tools/call", &call);
        assert!(
            refused.starts_with("HTTP/1.1 401 Unauthorized\r\n"),
            "{refused}"
        );
        let rotated = exchange(port, "/mcp", ROTATED, "tools/call", &call);
        assert!(rotated.starts_with("HTTP/1.1 200 OK\r\n"), "{rotated}");

        let elsewhere = exchange(port, "/sse", ROTATED, "tools/call", &call);
        assert!(
            elsewhere.starts_with("HTTP/1.1 404 Not Found\r\n"),
            "{elsewhere}"
        );

        served.stop();
        assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_err());
    }

    #[test]
    fn a_silent_connection_is_closed_by_the_header_timeout() {
        let listener = StdTcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let facade = McpFacade::new(CannedHost, "0.1.0".to_owned()).unwrap();
        let served = McpListener::serve(listener, TOKEN.to_owned(), facade).unwrap();

        let mut silent = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        silent
            .set_read_timeout(Some(HEADER_READ_TIMEOUT + Duration::from_secs(5)))
            .unwrap();
        let mut rest = Vec::new();
        // The server ends the exchange; a client-side read timeout would fail this read instead.
        silent.read_to_end(&mut rest).unwrap();

        served.stop();
    }
}
