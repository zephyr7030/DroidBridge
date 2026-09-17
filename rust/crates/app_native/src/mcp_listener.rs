//! The APK `:runtime` loopback MCP listener (S-MCP-001, S-STACK-011). Pinned Hyper frames HTTP/1.1
//! and the shared Runtime facade owns every protocol decision. Tool calls and artifact queries go
//! to the Kotlin HostController, the single path to the authoritative Runtime host.

use bytes::Bytes;
use contract::ErrorCode;
use domain::DomainError;
use http_body_util::{BodyExt, Full};
use hyper::{
    Request, Response, StatusCode, body::Incoming, header, server::conn::http1, service::service_fn,
};
use hyper_util::rt::TokioIo;
use jni::{
    EnvUnowned, Outcome,
    objects::{JClass, JString},
    sys::{JNI_FALSE, JNI_TRUE, jboolean, jint, jstring},
};
use runtime::{
    MCP_BODY_LIMIT_BYTES, MCP_DEBUG_PORT, MCP_STABLE_PORT, McpArtifactReply, McpFacade, McpHost,
    McpRequest, PortFuture,
};
use serde_json::Value;
use std::{
    convert::Infallible,
    net::{Ipv4Addr, SocketAddr, TcpListener as StdTcpListener},
    ptr,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Semaphore, oneshot};

/// Bounds the Kotlin host calls in flight, each on its own short-lived thread.
const MAX_HOST_CALLS: usize = 16;

static LISTENER: Mutex<Option<McpListener>> = Mutex::new(None);

pub(crate) struct McpListener {
    runtime: tokio::runtime::Runtime,
    token: Arc<RwLock<String>>,
    failed: Arc<AtomicBool>,
}

impl McpListener {
    /// Binds the IPv4 loopback endpoint and returns only once it accepts connections.
    fn start<H: McpHost + 'static>(
        port: u16,
        token: String,
        facade: McpFacade<H>,
    ) -> Result<Self, DomainError> {
        let listener = StdTcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
            .map_err(|_| listener_failed())?;
        Self::serve(listener, token, facade)
    }

    fn serve<H: McpHost + 'static>(
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
        let failed = Arc::new(AtomicBool::new(false));
        runtime.spawn(accept_loop(
            listener,
            Arc::new(facade),
            Arc::clone(&token),
            Arc::clone(&failed),
        ));
        Ok(Self {
            runtime,
            token,
            failed,
        })
    }

    fn set_token(&self, token: String) -> Result<(), DomainError> {
        *self.token.write().map_err(|_| {
            DomainError::new(ErrorCode::InternalError, "MCP token slot is unavailable")
        })? = token;
        Ok(())
    }

    fn state(&self) -> &'static str {
        if self.failed.load(Ordering::SeqCst) {
            "failed"
        } else {
            "running"
        }
    }

    /// Closes the listening socket and every open exchange; admitted Runtime work is unaffected.
    fn stop(self) {
        self.runtime.shutdown_timeout(Duration::from_secs(2));
    }
}

async fn accept_loop<H: McpHost + 'static>(
    listener: tokio::net::TcpListener,
    facade: Arc<McpFacade<H>>,
    token: Arc<RwLock<String>>,
    failed: Arc<AtomicBool>,
) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let facade = Arc::clone(&facade);
                let token = Arc::clone(&token);
                tokio::spawn(async move {
                    let service = service_fn(move |request| {
                        respond(request, Arc::clone(&facade), Arc::clone(&token))
                    });
                    // A client disconnect aborts only its own exchange (S-MCP-001); an admitted
                    // Task stays Runtime-owned, so nothing is left to settle here.
                    let _ = http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                });
            }
            Err(_) => {
                failed.store(true, Ordering::SeqCst);
                return;
            }
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
    let Ok(body) = bounded_body(request.into_body()).await else {
        return Ok(empty(StatusCode::BAD_REQUEST));
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

/// The Kotlin HostController, which submits to the APK Runtime while it is authoritative and
/// otherwise forwards to the live Magisk Core.
pub(crate) struct KotlinMcpHost {
    permits: Arc<Semaphore>,
}

impl KotlinMcpHost {
    pub(crate) fn new() -> Self {
        Self {
            permits: Arc::new(Semaphore::new(MAX_HOST_CALLS)),
        }
    }

    async fn on_host_thread<T: Send + 'static>(
        &self,
        call: impl FnOnce() -> Result<T, DomainError> + Send + 'static,
    ) -> Result<T, DomainError> {
        let _permit = self.permits.acquire().await.map_err(|_| {
            DomainError::new(ErrorCode::CapabilityUnavailable, "MCP host is closed")
        })?;
        let (sender, receiver) = oneshot::channel();
        // A plain thread rather than `spawn_blocking`: the host call can block on the APK
        // Runtime's own executor, which refuses to start inside another Tokio runtime context.
        std::thread::Builder::new()
            .name("droidbridge-mcp-host".to_owned())
            .spawn(move || {
                // A departed client drops the receiver; the reply and any descriptor close here.
                let _ = sender.send(call());
            })
            .map_err(|_| DomainError::new(ErrorCode::ResourceLimit, "MCP host thread failed"))?;
        receiver.await.map_err(|_| {
            DomainError::new(
                ErrorCode::InternalError,
                "MCP host call ended without a result",
            )
        })?
    }
}

pub(crate) fn initialize_host_bridge(env: &mut jni::Env<'_>) -> jni::errors::Result<()> {
    #[cfg(target_os = "android")]
    bridge::initialize(env)?;
    #[cfg(not(target_os = "android"))]
    let _ = env;
    Ok(())
}

pub(crate) fn kotlin_facade(
    port: u16,
    product_version: String,
) -> Result<McpFacade<KotlinMcpHost>, DomainError> {
    if port != MCP_STABLE_PORT && port != MCP_DEBUG_PORT {
        return Err(DomainError::invalid("MCP port is not a build endpoint"));
    }
    McpFacade::new(KotlinMcpHost::new(), port, product_version)
}

impl McpHost for KotlinMcpHost {
    fn submit<'a>(&'a self, envelope: Vec<u8>) -> PortFuture<'a, Result<Vec<u8>, DomainError>> {
        Box::pin(self.on_host_thread(move || bridge::submit(&envelope)))
    }

    fn artifact_query<'a>(
        &'a self,
        query: Value,
    ) -> PortFuture<'a, Result<McpArtifactReply, DomainError>> {
        Box::pin(async move {
            let encoded = serde_json::to_vec(&query).map_err(|_| {
                DomainError::new(ErrorCode::InternalError, "artifact query encoding failed")
            })?;
            self.on_host_thread(move || bridge::query_artifacts(&encoded))
                .await
        })
    }
}

#[cfg(target_os = "android")]
mod bridge {
    use super::*;
    use jni::{
        Env, JValue, JavaVM, jni_sig, jni_str,
        objects::{Global, JByteArray},
    };
    use std::{fs, os::fd::FromRawFd, sync::OnceLock};

    static MCP_HOST_BRIDGE: OnceLock<Global<JClass<'static>>> = OnceLock::new();

    /// Resolves the bridge class on a Java thread, where the App class loader is visible.
    pub(super) fn initialize(env: &mut Env<'_>) -> jni::errors::Result<()> {
        if MCP_HOST_BRIDGE.get().is_some() {
            return Ok(());
        }
        let class = env.find_class(jni_str!(
            "com/droidbridge/android/runtimehost/McpHostBridge"
        ))?;
        let global = env.new_global_ref(class)?;
        let _ = MCP_HOST_BRIDGE.set(global);
        Ok(())
    }

    pub(super) fn submit(envelope: &[u8]) -> Result<Vec<u8>, DomainError> {
        let bridge = MCP_HOST_BRIDGE.get().ok_or_else(unavailable)?;
        let vm = JavaVM::singleton()
            .map_err(|_| DomainError::new(ErrorCode::InternalError, "Java VM is unavailable"))?;
        vm.attach_current_thread(|env| -> jni::errors::Result<Option<Vec<u8>>> {
            let envelope = env.byte_array_from_slice(envelope)?;
            let result = env.call_static_method(
                &**bridge,
                jni_str!("submit"),
                jni_sig!("([B)[B"),
                &[JValue::Object(envelope.as_ref())],
            );
            let result = match result {
                Ok(value) => value.into_object()?,
                Err(error) => {
                    env.exception_clear();
                    return Err(error);
                }
            };
            if result.is_null() {
                return Ok(None);
            }
            let bytes = env.cast_local::<JByteArray>(result)?;
            Ok(Some(env.convert_byte_array(&bytes)?))
        })
        .map_err(|_| DomainError::new(ErrorCode::IoError, "MCP host bridge failed"))?
        .ok_or_else(unavailable)
    }

    pub(super) fn query_artifacts(query: &[u8]) -> Result<McpArtifactReply, DomainError> {
        let bridge = MCP_HOST_BRIDGE.get().ok_or_else(unavailable)?;
        let vm = JavaVM::singleton()
            .map_err(|_| DomainError::new(ErrorCode::InternalError, "Java VM is unavailable"))?;
        let (payload, descriptor) = vm
            .attach_current_thread(
                |env| -> jni::errors::Result<(Option<Vec<u8>>, Option<fs::File>)> {
                    let query = env.byte_array_from_slice(query)?;
                    let slot = env.new_int_array(1)?;
                    slot.set_region(env, 0, &[-1])?;
                    let result = env.call_static_method(
                        &**bridge,
                        jni_str!("queryArtifacts"),
                        jni_sig!("([B[I)[B"),
                        &[
                            JValue::Object(query.as_ref()),
                            JValue::Object(slot.as_ref()),
                        ],
                    );
                    let result = match result {
                        Ok(value) => value.into_object()?,
                        Err(error) => {
                            env.exception_clear();
                            return Err(error);
                        }
                    };
                    let mut raw = [-1];
                    slot.get_region(env, 0, &mut raw)?;
                    // Owned at once, so every later failure still closes the descriptor.
                    let descriptor =
                        (raw[0] >= 0).then(|| unsafe { fs::File::from_raw_fd(raw[0]) });
                    if result.is_null() {
                        return Ok((None, descriptor));
                    }
                    let bytes = env.cast_local::<JByteArray>(result)?;
                    Ok((Some(env.convert_byte_array(&bytes)?), descriptor))
                },
            )
            .map_err(|_| DomainError::new(ErrorCode::IoError, "MCP host bridge failed"))?;
        let payload = payload.ok_or_else(unavailable)?;
        Ok(McpArtifactReply {
            payload: serde_json::from_slice(&payload).map_err(|_| {
                DomainError::new(ErrorCode::IoError, "artifact query reply is not JSON")
            })?,
            descriptor,
        })
    }

    fn unavailable() -> DomainError {
        DomainError::new(
            ErrorCode::CapabilityUnavailable,
            "authoritative Runtime host is unavailable",
        )
    }
}

#[cfg(not(target_os = "android"))]
mod bridge {
    use super::*;

    pub(super) fn submit(_envelope: &[u8]) -> Result<Vec<u8>, DomainError> {
        Err(unavailable())
    }

    pub(super) fn query_artifacts(_query: &[u8]) -> Result<McpArtifactReply, DomainError> {
        Err(unavailable())
    }

    fn unavailable() -> DomainError {
        DomainError::new(
            ErrorCode::CapabilityUnavailable,
            "the MCP host bridge requires Android",
        )
    }
}

fn listener_failed() -> DomainError {
    DomainError::new(ErrorCode::IoError, "MCP loopback listener failed")
}

fn listener_slot() -> Result<std::sync::MutexGuard<'static, Option<McpListener>>, DomainError> {
    LISTENER
        .lock()
        .map_err(|_| DomainError::new(ErrorCode::InternalError, "MCP listener slot is unavailable"))
}

fn start_listener(port: jint, token: String, product_version: String) -> Result<(), DomainError> {
    let port = u16::try_from(port)
        .ok()
        .filter(|port| *port == MCP_STABLE_PORT || *port == MCP_DEBUG_PORT)
        .ok_or_else(|| DomainError::invalid("MCP port is not a build endpoint"))?;
    let mut slot = listener_slot()?;
    if let Some(previous) = slot.take() {
        previous.stop();
    }
    let facade = kotlin_facade(port, product_version)?;
    *slot = Some(McpListener::start(port, token, facade)?);
    Ok(())
}

/// Binds the build endpoint, replacing any previous listener, and returns once it is live.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeMcpStart(
    mut env: EnvUnowned,
    _class: JClass,
    port: jint,
    token: JString,
    product_version: JString,
) -> jboolean {
    match env
        .with_env(|owned| -> jni::errors::Result<jboolean> {
            initialize_host_bridge(owned)?;
            let token = token.mutf8_chars(owned)?.to_str().into_owned();
            let product_version = product_version.mutf8_chars(owned)?.to_str().into_owned();
            Ok(if start_listener(port, token, product_version).is_ok() {
                JNI_TRUE
            } else {
                JNI_FALSE
            })
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => JNI_FALSE,
    }
}

/// Replaces the accepted bearer token; the next admitted request sees only the new token.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeMcpSetToken(
    mut env: EnvUnowned,
    _class: JClass,
    token: JString,
) -> jboolean {
    match env
        .with_env(|owned| -> jni::errors::Result<jboolean> {
            let token = token.mutf8_chars(owned)?.to_str().into_owned();
            let updated = listener_slot().and_then(|slot| match slot.as_ref() {
                Some(listener) => listener.set_token(token),
                None => Ok(()),
            });
            Ok(if updated.is_ok() { JNI_TRUE } else { JNI_FALSE })
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => JNI_FALSE,
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeMcpStop(
    _env: EnvUnowned,
    _class: JClass,
) -> jboolean {
    match listener_slot() {
        Ok(mut slot) => {
            if let Some(listener) = slot.take() {
                listener.stop();
            }
            JNI_TRUE
        }
        Err(_) => JNI_FALSE,
    }
}

/// The observed listener: `stopped`, `running`, or `failed` once serving ended by itself.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeMcpState(
    mut env: EnvUnowned,
    _class: JClass,
) -> jstring {
    match env
        .with_env(|owned| -> jni::errors::Result<jstring> {
            let state = match listener_slot() {
                Ok(slot) => slot.as_ref().map_or("stopped", McpListener::state),
                Err(_) => "failed",
            };
            Ok(owned.new_string(state)?.into_raw())
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => ptr::null_mut(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
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
        let facade = McpFacade::new(CannedHost, port, "0.1.0".to_owned()).unwrap();
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
}
