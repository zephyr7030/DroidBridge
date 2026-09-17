use crate::mcp_listener::{KotlinMcpHost, initialize_host_bridge, kotlin_facade};
use jni::{
    EnvUnowned, Outcome,
    objects::{JClass, JString},
    sys::{JNI_FALSE, JNI_TRUE, jboolean, jint, jlong, jstring},
};
use reqwest::{
    Url,
    header::{self, HeaderMap, HeaderName, HeaderValue},
};
use runtime::{MCP_BODY_LIMIT_BYTES, McpFacade, McpHost, McpRequest};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, hash_map::DefaultHasher},
    fmt,
    hash::{Hash, Hasher},
    ptr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicI64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::{Semaphore, watch},
    task::JoinSet,
    time::Instant,
};
use uuid::Uuid;

const API_BASE_URL: &str = "https://api.openai.com/";
const CLIENT_NAME: &str = "droidbridge-android";
const WIRE_PROTOCOL_VERSION: &str = "2026-08-25";
const POLL_LIMIT: usize = 8;
const POLL_TIMEOUT: Duration = Duration::from_secs(15);
const POLL_GUARDRAIL: Duration = Duration::from_secs(10);
const RESPONSE_ATTEMPTS: usize = 3;
const MAX_POLL_BODY_BYTES: usize = MCP_BODY_LIMIT_BYTES * 25 + 64 * 1024;
const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);
const SERVER_INFO: &str =
    r#"{"version":2,"channels":[{"name":"main","stateless":true,"proc_affinity":true}]}"#;

const HEADER_CLIENT_NAME: &str = "x-tunnel-client-name";
const HEADER_CLIENT_VERSION: &str = "x-tunnel-client-version";
const HEADER_WIRE_VERSION: &str = "x-tunnel-client-wire-protocol-version";
const HEADER_INSTANCE_ID: &str = "x-tunnel-client-instance-id";
const HEADER_SERVER_INFO: &str = "x-tunnel-mcp-server-info";
const HEADER_SHARD_TOKEN: &str = "x-tunnel-shard-token";

static TUNNEL: Mutex<Option<TunnelRuntime>> = Mutex::new(None);

/// The tunnel's HTTP transport carries the Runtime's one pinned Rustls configuration (S-NET-002)
/// instead of reqwest's default policy. The default is the Android platform verifier, whose
/// revocation pass reports genuine public chains without OCSP responders as revoked and refuses
/// to connect, which is why the tunnel must not fall back to it.
fn transport() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .tls_backend_preconfigured(runtime::network_tls_client_config().as_ref().clone())
}

#[derive(Debug)]
pub(crate) enum TunnelError {
    InvalidConfig(&'static str),
    Transport(reqwest::Error),
    ControlPlaneStatus {
        status: u16,
        retry_after: Option<Duration>,
    },
    InvalidControlPlaneResponse(&'static str),
    RuntimeUnavailable,
}

impl TunnelError {
    fn poll_retryable(&self) -> bool {
        matches!(
            self,
            Self::Transport(_)
                | Self::InvalidControlPlaneResponse(_)
                | Self::ControlPlaneStatus {
                    status: 429 | 500..=599,
                    ..
                }
        )
    }

    fn needs_operator(&self) -> bool {
        matches!(
            self,
            Self::ControlPlaneStatus {
                status: 401 | 403,
                ..
            }
        )
    }

    /// A short, secret-free token for the status page: which way the control plane last failed.
    fn token(&self) -> String {
        match self {
            Self::InvalidConfig(_) => "invalid_config".to_owned(),
            // A connect timeout is both a connect and a timeout error; name the phase first.
            Self::Transport(error) if error.is_connect() && error.is_timeout() => {
                "connect_timeout".to_owned()
            }
            Self::Transport(error) if error.is_connect() => "transport_connect".to_owned(),
            Self::Transport(error) if error.is_timeout() => "transport_timeout".to_owned(),
            Self::Transport(error) if error.is_body() || error.is_decode() => {
                "transport_body".to_owned()
            }
            Self::Transport(_) => "transport".to_owned(),
            Self::ControlPlaneStatus { status, .. } => format!("http_{status}"),
            Self::InvalidControlPlaneResponse(_) => "invalid_response".to_owned(),
            Self::RuntimeUnavailable => "runtime_unavailable".to_owned(),
        }
    }

    fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::ControlPlaneStatus { retry_after, .. } => *retry_after,
            _ => None,
        }
    }
}

impl fmt::Display for TunnelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(reason) => {
                write!(formatter, "invalid tunnel configuration: {reason}")
            }
            Self::Transport(error) => write!(formatter, "tunnel transport failed: {error}"),
            Self::ControlPlaneStatus { status, .. } => {
                write!(formatter, "tunnel control plane returned HTTP {status}")
            }
            Self::InvalidControlPlaneResponse(reason) => {
                write!(formatter, "invalid tunnel control-plane response: {reason}")
            }
            Self::RuntimeUnavailable => formatter.write_str("tunnel runtime is unavailable"),
        }
    }
}

struct TunnelRuntime {
    runtime: tokio::runtime::Runtime,
    shutdown: watch::Sender<bool>,
    ready: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    last_call_epoch_ms: Arc<AtomicI64>,
    last_error: Arc<Mutex<Option<String>>>,
}

impl TunnelRuntime {
    fn start(client: TunnelClient<KotlinMcpHost>) -> Result<Self, TunnelError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("droidbridge-tunnel")
            .enable_all()
            .build()
            .map_err(|_| TunnelError::RuntimeUnavailable)?;
        let (shutdown, receiver) = watch::channel(false);
        let ready = Arc::new(AtomicBool::new(false));
        let failed = Arc::new(AtomicBool::new(false));
        let readiness = Arc::clone(&ready);
        let observed = Arc::clone(&failed);
        let last_call_epoch_ms = Arc::clone(&client.last_call_epoch_ms);
        let last_error = Arc::clone(&client.last_error);
        runtime.spawn(async move {
            if client.run(receiver, readiness).await.is_err() {
                observed.store(true, Ordering::SeqCst);
            }
        });
        Ok(Self {
            runtime,
            shutdown,
            ready,
            failed,
            last_call_epoch_ms,
            last_error,
        })
    }

    fn state(&self) -> &'static str {
        if self.failed.load(Ordering::SeqCst) {
            "failed"
        } else if self.ready.load(Ordering::SeqCst) {
            "running"
        } else {
            "connecting"
        }
    }

    fn stop(self) {
        let _ = self.shutdown.send(true);
        self.runtime.shutdown_timeout(Duration::from_secs(2));
    }
}

fn tunnel_slot() -> Result<std::sync::MutexGuard<'static, Option<TunnelRuntime>>, TunnelError> {
    TUNNEL.lock().map_err(|_| TunnelError::RuntimeUnavailable)
}

fn start_tunnel(
    port: jint,
    tunnel_id: String,
    api_key: String,
    product_version: String,
) -> Result<(), TunnelError> {
    let port =
        u16::try_from(port).map_err(|_| TunnelError::InvalidConfig("MCP port is invalid"))?;
    let mut slot = tunnel_slot()?;
    if slot.is_some() {
        return Ok(());
    }
    let facade = kotlin_facade(port, product_version.clone())
        .map_err(|_| TunnelError::InvalidConfig("MCP port or product version is invalid"))?;
    let client = TunnelClient::new(facade, &tunnel_id, &api_key, &product_version)?;
    *slot = Some(TunnelRuntime::start(client)?);
    Ok(())
}

fn validate_tunnel_credentials(
    tunnel_id: &str,
    api_key: &str,
    product_version: &str,
) -> &'static str {
    if !valid_tunnel_id(tunnel_id)
        || api_key.is_empty()
        || api_key.len() > 512
        || !api_key.bytes().all(|byte| byte.is_ascii_graphic())
        || product_version.is_empty()
        || product_version.len() > 64
        || !product_version.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return "unavailable";
    }
    let _ = rustls::crypto::ring::default_provider().install_default();
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return "unavailable";
    };
    runtime.block_on(async {
        let Ok(url) =
            Url::parse(API_BASE_URL).and_then(|base| base.join(&format!("v1/tunnels/{tunnel_id}")))
        else {
            return "unavailable";
        };
        let Ok(mut authorization) = HeaderValue::from_str(&format!("Bearer {api_key}")) else {
            return "unavailable";
        };
        authorization.set_sensitive(true);
        let Ok(user_agent) = HeaderValue::from_str(&format!("{CLIENT_NAME}/{product_version}"))
        else {
            return "unavailable";
        };
        let Ok(client) = transport().build() else {
            return "unavailable";
        };
        let response = client
            .get(url)
            .header(header::AUTHORIZATION, authorization)
            .header(header::ACCEPT, "application/json")
            .header(header::USER_AGENT, user_agent)
            .timeout(Duration::from_secs(15))
            .send()
            .await;
        match response {
            Ok(value) => match value.status().as_u16() {
                200 => "valid",
                401 | 403 => "invalid_key",
                404 => "invalid_tunnel",
                _ => "unavailable",
            },
            Err(_) => "unavailable",
        }
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeTunnelValidate(
    mut env: EnvUnowned,
    _class: JClass,
    tunnel_id: JString,
    api_key: JString,
    product_version: JString,
) -> jstring {
    match env
        .with_env(|owned| -> jni::errors::Result<jstring> {
            let tunnel_id = tunnel_id.mutf8_chars(owned)?.to_str().into_owned();
            let api_key = api_key.mutf8_chars(owned)?.to_str().into_owned();
            let product_version = product_version.mutf8_chars(owned)?.to_str().into_owned();
            let state = validate_tunnel_credentials(&tunnel_id, &api_key, &product_version);
            Ok(owned.new_string(state)?.into_raw())
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeTunnelStart(
    mut env: EnvUnowned,
    _class: JClass,
    port: jint,
    tunnel_id: JString,
    api_key: JString,
    product_version: JString,
) -> jboolean {
    match env
        .with_env(|owned| -> jni::errors::Result<jboolean> {
            initialize_host_bridge(owned)?;
            let tunnel_id = tunnel_id.mutf8_chars(owned)?.to_str().into_owned();
            let api_key = api_key.mutf8_chars(owned)?.to_str().into_owned();
            let product_version = product_version.mutf8_chars(owned)?.to_str().into_owned();
            Ok(
                if start_tunnel(port, tunnel_id, api_key, product_version).is_ok() {
                    JNI_TRUE
                } else {
                    JNI_FALSE
                },
            )
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => JNI_FALSE,
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeTunnelStop(
    _env: EnvUnowned,
    _class: JClass,
) -> jboolean {
    match tunnel_slot() {
        Ok(mut slot) => {
            if let Some(tunnel) = slot.take() {
                tunnel.stop();
            }
            JNI_TRUE
        }
        Err(_) => JNI_FALSE,
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeTunnelState(
    mut env: EnvUnowned,
    _class: JClass,
) -> jstring {
    match env
        .with_env(|owned| -> jni::errors::Result<jstring> {
            let state = tunnel_slot()
                .map(|slot| slot.as_ref().map_or("stopped", TunnelRuntime::state))
                .unwrap_or("failed");
            Ok(owned.new_string(state)?.into_raw())
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => ptr::null_mut(),
    }
}

/// The token of the control plane's last failure while the tunnel is not running, or null.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeTunnelLastError(
    mut env: EnvUnowned,
    _class: JClass,
) -> jstring {
    match env
        .with_env(|owned| -> jni::errors::Result<jstring> {
            let last = tunnel_slot().ok().and_then(|slot| {
                slot.as_ref().and_then(|runtime| {
                    runtime
                        .last_error
                        .lock()
                        .ok()
                        .and_then(|error| error.clone())
                })
            });
            Ok(match last {
                Some(token) => owned.new_string(token)?.into_raw(),
                None => ptr::null_mut(),
            })
        })
        .into_outcome()
    {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_android_runtimehost_NativeRuntime_nativeTunnelLastCall(
    _env: EnvUnowned,
    _class: JClass,
) -> jlong {
    tunnel_slot()
        .ok()
        .and_then(|slot| {
            slot.as_ref()
                .map(|runtime| runtime.last_call_epoch_ms.load(Ordering::SeqCst))
        })
        .unwrap_or(0)
}

impl std::error::Error for TunnelError {}

impl From<reqwest::Error> for TunnelError {
    fn from(error: reqwest::Error) -> Self {
        Self::Transport(error)
    }
}

pub(crate) struct TunnelClient<H> {
    http: reqwest::Client,
    facade: Arc<McpFacade<H>>,
    poll_url: Url,
    response_url: Url,
    common_headers: HeaderMap,
    instance_id: Uuid,
    last_call_epoch_ms: Arc<AtomicI64>,
    last_error: Arc<Mutex<Option<String>>>,
}

impl<H> Clone for TunnelClient<H> {
    fn clone(&self) -> Self {
        Self {
            http: self.http.clone(),
            facade: Arc::clone(&self.facade),
            poll_url: self.poll_url.clone(),
            response_url: self.response_url.clone(),
            common_headers: self.common_headers.clone(),
            instance_id: self.instance_id,
            last_call_epoch_ms: Arc::clone(&self.last_call_epoch_ms),
            last_error: Arc::clone(&self.last_error),
        }
    }
}

impl<H: McpHost + 'static> TunnelClient<H> {
    pub(crate) fn new(
        facade: McpFacade<H>,
        tunnel_id: &str,
        api_key: &str,
        product_version: &str,
    ) -> Result<Self, TunnelError> {
        Self::new_with_base_url(
            facade,
            tunnel_id,
            api_key,
            product_version,
            API_BASE_URL,
            false,
        )
    }

    fn new_with_base_url(
        facade: McpFacade<H>,
        tunnel_id: &str,
        api_key: &str,
        product_version: &str,
        base_url: &str,
        allow_loopback_http: bool,
    ) -> Result<Self, TunnelError> {
        if !valid_tunnel_id(tunnel_id) {
            return Err(TunnelError::InvalidConfig("tunnel ID is invalid"));
        }
        if api_key.is_empty() || !api_key.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err(TunnelError::InvalidConfig("API key is invalid"));
        }
        if product_version.is_empty()
            || product_version.len() > 64
            || !product_version.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(TunnelError::InvalidConfig("product version is invalid"));
        }
        let base = Url::parse(base_url)
            .map_err(|_| TunnelError::InvalidConfig("control-plane URL is invalid"))?;
        let secure = base.scheme() == "https";
        let loopback_http = allow_loopback_http
            && base.scheme() == "http"
            && base
                .host_str()
                .is_some_and(|host| host == "127.0.0.1" || host == "localhost");
        if !secure && !loopback_http {
            return Err(TunnelError::InvalidConfig(
                "control-plane URL must use HTTPS",
            ));
        }
        let poll_url = base
            .join(&format!("v1/tunnels/{tunnel_id}/poll"))
            .map_err(|_| TunnelError::InvalidConfig("poll URL is invalid"))?;
        let response_url = base
            .join(&format!("v1/tunnels/{tunnel_id}/response"))
            .map_err(|_| TunnelError::InvalidConfig("response URL is invalid"))?;

        let instance_id = Uuid::new_v4();
        let mut common_headers = HeaderMap::new();
        let mut authorization = HeaderValue::from_str(&format!("Bearer {api_key}"))
            .map_err(|_| TunnelError::InvalidConfig("API key is invalid"))?;
        authorization.set_sensitive(true);
        common_headers.insert(header::AUTHORIZATION, authorization);
        common_headers.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
        common_headers.insert(
            header::USER_AGENT,
            HeaderValue::from_str(&format!("{CLIENT_NAME}/{product_version}"))
                .map_err(|_| TunnelError::InvalidConfig("product version is invalid"))?,
        );
        insert_static_header(&mut common_headers, HEADER_CLIENT_NAME, CLIENT_NAME)?;
        insert_header(&mut common_headers, HEADER_CLIENT_VERSION, product_version)?;
        insert_static_header(
            &mut common_headers,
            HEADER_WIRE_VERSION,
            WIRE_PROTOCOL_VERSION,
        )?;
        insert_header(
            &mut common_headers,
            HEADER_INSTANCE_ID,
            &instance_id.to_string(),
        )?;
        insert_static_header(&mut common_headers, HEADER_SERVER_INFO, SERVER_INFO)?;

        let _ = rustls::crypto::ring::default_provider().install_default();
        let http = transport().build()?;
        Ok(Self {
            http,
            facade: Arc::new(facade),
            poll_url,
            response_url,
            common_headers,
            instance_id,
            last_call_epoch_ms: Arc::new(AtomicI64::new(0)),
            last_error: Arc::new(Mutex::new(None)),
        })
    }

    pub(crate) async fn run(
        &self,
        mut shutdown: watch::Receiver<bool>,
        ready: Arc<AtomicBool>,
    ) -> Result<(), TunnelError> {
        let mut failure_count = 0_u32;
        let mut commands = JoinSet::new();
        let permits = Arc::new(Semaphore::new(POLL_LIMIT));
        loop {
            if *shutdown.borrow() {
                stop_commands(&mut commands).await;
                return Ok(());
            }
            if let Some(error) = reap_commands(&mut commands) {
                stop_commands(&mut commands).await;
                return Err(error);
            }
            // The poll loop owns the control plane. One poll loop submits its commands to workers
            // and keeps polling, so a command in flight never delays the next poll; only a full
            // worker pool closes the gate.
            if commands.len() >= POLL_LIMIT {
                let finished = tokio::select! {
                    changed = shutdown.changed() => {
                        if changed.is_err() || *shutdown.borrow() {
                            stop_commands(&mut commands).await;
                            return Ok(());
                        }
                        continue;
                    }
                    joined = commands.join_next() => joined,
                };
                if let Some(Ok(Err(error))) = finished
                    && error.needs_operator()
                {
                    stop_commands(&mut commands).await;
                    return Err(error);
                }
                continue;
            }
            let poll_started = Instant::now();
            let poll = tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        stop_commands(&mut commands).await;
                        return Ok(());
                    }
                    continue;
                }
                result = self.poll_once() => result,
            };
            let (received_at, batch) = match poll {
                Ok(result) => {
                    failure_count = 0;
                    ready.store(true, Ordering::SeqCst);
                    self.record_error(None, poll_started.elapsed());
                    result
                }
                Err(error) if error.poll_retryable() => {
                    ready.store(false, Ordering::SeqCst);
                    self.record_error(Some(&error), poll_started.elapsed());
                    failure_count = failure_count.saturating_add(1);
                    let delay = retry_delay(failure_count, self.instance_id, error.retry_after());
                    tokio::select! {
                        changed = shutdown.changed() => {
                            if changed.is_err() || *shutdown.borrow() {
                                stop_commands(&mut commands).await;
                                return Ok(());
                            }
                        }
                        _ = tokio::time::sleep(delay) => {}
                    }
                    continue;
                }
                Err(error) => {
                    self.record_error(Some(&error), poll_started.elapsed());
                    stop_commands(&mut commands).await;
                    return Err(error);
                }
            };
            for command in batch {
                let client = self.clone();
                let permits = Arc::clone(&permits);
                commands.spawn(async move {
                    let Ok(_permit) = permits.acquire_owned().await else {
                        return Ok(());
                    };
                    client.process_command(command, received_at).await
                });
            }
        }
    }

    /// Records the last poll failure with how long the attempt took, e.g. `transport_timeout_45s`.
    fn record_error(&self, error: Option<&TunnelError>, elapsed: Duration) {
        if let Ok(mut last) = self.last_error.lock() {
            *last = error.map(|error| format!("{}_{}s", error.token(), elapsed.as_secs()));
        }
    }

    async fn poll_once(&self) -> Result<(Instant, Vec<Value>), TunnelError> {
        let mut poll_url = self.poll_url.clone();
        poll_url
            .query_pairs_mut()
            .append_pair("limit", &POLL_LIMIT.to_string())
            .append_pair("timeout_ms", &POLL_TIMEOUT.as_millis().to_string());
        let mut response = self
            .http
            .get(poll_url)
            .headers(self.common_headers.clone())
            .timeout(POLL_TIMEOUT + POLL_GUARDRAIL)
            .send()
            .await?;
        let received_at = Instant::now();
        let status = response.status().as_u16();
        if status == 204 {
            return Ok((received_at, Vec::new()));
        }
        if status != 200 {
            return Err(status_error(&response));
        }
        let bytes = read_bounded(&mut response, MAX_POLL_BODY_BYTES).await?;
        let envelope: PollEnvelope = serde_json::from_slice(&bytes).map_err(|_| {
            TunnelError::InvalidControlPlaneResponse("poll body is not a command envelope")
        })?;
        Ok((received_at, envelope.commands))
    }

    async fn process_command(&self, raw: Value, received_at: Instant) -> Result<(), TunnelError> {
        let Some(command_type) = raw.get("command_type").and_then(Value::as_str) else {
            return Ok(());
        };
        match command_type {
            "jsonrpc" => {
                let Ok(command) = serde_json::from_value::<JsonRpcCommand>(raw) else {
                    return Ok(());
                };
                self.process_jsonrpc(command, received_at).await
            }
            "session_termination" => {
                let Ok(command) = serde_json::from_value::<SessionTerminationCommand>(raw) else {
                    return Ok(());
                };
                self.process_session_termination(command, received_at).await
            }
            _ => Ok(()),
        }
    }

    async fn process_jsonrpc(
        &self,
        command: JsonRpcCommand,
        received_at: Instant,
    ) -> Result<(), TunnelError> {
        let Some(base) = command.base(received_at) else {
            return Ok(());
        };
        let Some(message) = command.jsonrpc.as_object() else {
            return Ok(());
        };
        if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
            || !message.get("method").is_some_and(Value::is_string)
            || message
                .get("id")
                .is_some_and(|id| !id.is_string() && !id.is_i64() && !id.is_u64())
        {
            return Ok(());
        }
        let received_epoch_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| i64::try_from(duration.as_millis()).ok())
            .unwrap_or(0);
        self.last_call_epoch_ms
            .store(received_epoch_ms, Ordering::SeqCst);
        let rpc_id = message.get("id").cloned();
        let notification = rpc_id.is_none();
        let deadline = base.deadline;
        let client = self.clone();
        let execute = async move {
            if base.channel != "main" {
                let response = if notification {
                    TunnelResponse::ack(&base, 400, "notify_ack")
                } else {
                    TunnelResponse::jsonrpc(
                        &base,
                        400,
                        jsonrpc_failure(
                            rpc_id.clone().unwrap_or(Value::Null),
                            "Unsupported tunnel channel",
                            false,
                        ),
                    )
                };
                return client.post_response(&base, response, notification).await;
            }

            let mut headers = flatten_headers(command.headers);
            set_default_header(&mut headers, "Content-Type", "application/json");
            set_default_header(
                &mut headers,
                "Accept",
                "application/json, text/event-stream",
            );
            let response = client
                .facade
                .handle_tunnel(McpRequest {
                    method: "POST".to_owned(),
                    headers,
                    body: serde_json::to_vec(&command.jsonrpc).map_err(|_| {
                        TunnelError::InvalidControlPlaneResponse(
                            "JSON-RPC command cannot be encoded",
                        )
                    })?,
                })
                .await;
            let payload = if notification {
                TunnelResponse::ack(&base, response.status, "notify_ack")
            } else {
                let body = response
                    .body
                    .and_then(|body| serde_json::from_slice(&body).ok())
                    .unwrap_or_else(|| {
                        jsonrpc_failure(rpc_id.unwrap_or(Value::Null), "MCP request failed", true)
                    });
                TunnelResponse::jsonrpc(&base, response.status, body)
            };
            client.post_response(&base, payload, notification).await
        };
        run_before_deadline(deadline, execute).await
    }

    async fn process_session_termination(
        &self,
        command: SessionTerminationCommand,
        received_at: Instant,
    ) -> Result<(), TunnelError> {
        let Some(base) = command.base(received_at) else {
            return Ok(());
        };
        if !has_nonempty_header(&command.headers, "Mcp-Session-Id") {
            return Ok(());
        }
        let status = if base.channel == "main" { 405 } else { 400 };
        let response = TunnelResponse::ack(&base, status, "session_termination_response");
        let client = self.clone();
        run_before_deadline(base.deadline, async move {
            client.post_response(&base, response, false).await
        })
        .await
    }

    async fn post_response(
        &self,
        base: &CommandBase,
        payload: TunnelResponse,
        notification: bool,
    ) -> Result<(), TunnelError> {
        let body = serde_json::to_vec(&payload).map_err(|_| {
            TunnelError::InvalidControlPlaneResponse("tunnel response cannot be encoded")
        })?;
        let shard_token = HeaderValue::from_str(&base.shard_token).map_err(|_| {
            TunnelError::InvalidControlPlaneResponse("shard token is not a valid header value")
        })?;
        let mut failure_count = 0_u32;
        for attempt in 1..=RESPONSE_ATTEMPTS {
            let sent = self
                .http
                .post(self.response_url.clone())
                .headers(self.common_headers.clone())
                .header(header::CONTENT_TYPE, "application/json")
                .header(HEADER_SHARD_TOKEN, shard_token.clone())
                .body(body.clone())
                .send()
                .await;
            let response = match sent {
                Ok(response) => response,
                Err(error) => {
                    if attempt == RESPONSE_ATTEMPTS || notification {
                        return Err(TunnelError::Transport(error));
                    }
                    failure_count += 1;
                    tokio::time::sleep(retry_delay(failure_count, self.instance_id, None)).await;
                    continue;
                }
            };
            let status = response.status().as_u16();
            if status == 200 || status == 404 {
                return Ok(());
            }
            let error = status_error(&response);
            let retryable = if notification {
                status == 429
            } else {
                matches!(status, 408 | 429 | 502 | 503 | 504)
            };
            if !retryable || attempt == RESPONSE_ATTEMPTS {
                return Err(error);
            }
            failure_count += 1;
            tokio::time::sleep(retry_delay(
                failure_count,
                self.instance_id,
                error.retry_after(),
            ))
            .await;
        }
        Err(TunnelError::InvalidControlPlaneResponse(
            "response retries were exhausted",
        ))
    }
}

/// Reaps the commands that finished without waiting for one, and reports the first failure that
/// needs an operator: a refused credential ends the tunnel instead of repeating per command.
fn reap_commands(commands: &mut JoinSet<Result<(), TunnelError>>) -> Option<TunnelError> {
    while let Some(finished) = commands.try_join_next() {
        if let Ok(Err(error)) = finished
            && error.needs_operator()
        {
            return Some(error);
        }
    }
    None
}

/// Ends every command still in flight, so no command outlives the tunnel that owned it.
async fn stop_commands(commands: &mut JoinSet<Result<(), TunnelError>>) {
    commands.abort_all();
    while commands.join_next().await.is_some() {}
}

#[derive(Deserialize)]
struct PollEnvelope {
    commands: Vec<Value>,
}

#[derive(Deserialize)]
struct JsonRpcCommand {
    request_id: String,
    shard_token: String,
    #[serde(default)]
    channel: Option<String>,
    created_at: String,
    #[serde(default)]
    response_timeout: Option<Value>,
    #[serde(default)]
    headers: BTreeMap<String, Vec<String>>,
    jsonrpc: Value,
}

impl JsonRpcCommand {
    fn base(&self, received_at: Instant) -> Option<CommandBase> {
        CommandBase::new(
            &self.request_id,
            &self.shard_token,
            self.channel.as_deref(),
            &self.created_at,
            self.response_timeout.as_ref(),
            received_at,
        )
    }
}

#[derive(Deserialize)]
struct SessionTerminationCommand {
    request_id: String,
    shard_token: String,
    #[serde(default)]
    channel: Option<String>,
    created_at: String,
    #[serde(default)]
    response_timeout: Option<Value>,
    #[serde(default)]
    headers: BTreeMap<String, Vec<String>>,
}

impl SessionTerminationCommand {
    fn base(&self, received_at: Instant) -> Option<CommandBase> {
        CommandBase::new(
            &self.request_id,
            &self.shard_token,
            self.channel.as_deref(),
            &self.created_at,
            self.response_timeout.as_ref(),
            received_at,
        )
    }
}

struct CommandBase {
    request_id: String,
    shard_token: String,
    channel: String,
    deadline: Option<Instant>,
}

impl CommandBase {
    fn new(
        request_id: &str,
        shard_token: &str,
        channel: Option<&str>,
        created_at: &str,
        response_timeout: Option<&Value>,
        received_at: Instant,
    ) -> Option<Self> {
        let channel = channel.unwrap_or("main");
        if request_id.is_empty()
            || shard_token.is_empty()
            || !valid_channel(channel)
            || chrono::DateTime::parse_from_rfc3339(created_at).is_err()
        {
            return None;
        }
        let deadline = response_timeout
            .and_then(parse_response_timeout)
            .and_then(|duration| received_at.checked_add(duration));
        Some(Self {
            request_id: request_id.to_owned(),
            shard_token: shard_token.to_owned(),
            channel: channel.to_owned(),
            deadline,
        })
    }
}

#[derive(Serialize)]
struct TunnelResponse {
    request_id: String,
    channel: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    resp_json: Option<Value>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    resp_headers: BTreeMap<String, Vec<String>>,
    resp_code: u16,
    resp_type: &'static str,
}

impl TunnelResponse {
    fn jsonrpc(base: &CommandBase, status: u16, body: Value) -> Self {
        Self {
            request_id: base.request_id.clone(),
            channel: base.channel.clone(),
            resp_json: Some(body),
            resp_headers: BTreeMap::from([(
                "Content-Type".to_owned(),
                vec!["application/json".to_owned()],
            )]),
            resp_code: status,
            resp_type: "jsonrpc_response",
        }
    }

    fn ack(base: &CommandBase, status: u16, response_type: &'static str) -> Self {
        Self {
            request_id: base.request_id.clone(),
            channel: base.channel.clone(),
            resp_json: None,
            resp_headers: BTreeMap::new(),
            resp_code: status,
            resp_type: response_type,
        }
    }
}

fn valid_tunnel_id(value: &str) -> bool {
    value.len() == 39
        && value.starts_with("tunnel_")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

fn valid_channel(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_-".contains(&byte))
}

fn parse_response_timeout(value: &Value) -> Option<Duration> {
    let value = value.as_str()?;
    let (digits, unit) = if let Some(digits) = value.strip_suffix("ns") {
        (digits, Duration::from_nanos(1))
    } else if let Some(digits) = value.strip_suffix("us") {
        (digits, Duration::from_micros(1))
    } else if let Some(digits) = value.strip_suffix("ms") {
        (digits, Duration::from_millis(1))
    } else if let Some(digits) = value.strip_suffix('s') {
        (digits, Duration::from_secs(1))
    } else if let Some(digits) = value.strip_suffix('m') {
        (digits, Duration::from_secs(60))
    } else {
        (value.strip_suffix('h')?, Duration::from_secs(60 * 60))
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    unit.checked_mul(digits.parse().ok()?)
}

fn flatten_headers(headers: BTreeMap<String, Vec<String>>) -> Vec<(String, String)> {
    headers
        .into_iter()
        .flat_map(|(name, values)| values.into_iter().map(move |value| (name.clone(), value)))
        .collect()
}

fn set_default_header(headers: &mut Vec<(String, String)>, name: &str, value: &str) {
    if !headers
        .iter()
        .any(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
    {
        headers.push((name.to_owned(), value.to_owned()));
    }
}

fn has_nonempty_header(headers: &BTreeMap<String, Vec<String>>, name: &str) -> bool {
    headers.iter().any(|(candidate, values)| {
        candidate.eq_ignore_ascii_case(name) && values.iter().any(|value| !value.is_empty())
    })
}

fn jsonrpc_failure(id: Value, message: &str, with_provenance: bool) -> Value {
    let mut error = json!({"code": -32603, "message": message});
    if with_provenance {
        error["data"] = json!({
            "tunnel_failure": {
                "version": 1,
                "source": "client_internal",
                "upstream_response_received": false,
            }
        });
    }
    json!({"jsonrpc": "2.0", "id": id, "error": error})
}

async fn run_before_deadline(
    deadline: Option<Instant>,
    future: impl Future<Output = Result<(), TunnelError>>,
) -> Result<(), TunnelError> {
    match deadline {
        Some(deadline) if deadline <= Instant::now() => Ok(()),
        Some(deadline) => match tokio::time::timeout_at(deadline, future).await {
            Ok(result) => result,
            Err(_) => Ok(()),
        },
        None => future.await,
    }
}

fn status_error(response: &reqwest::Response) -> TunnelError {
    TunnelError::ControlPlaneStatus {
        status: response.status().as_u16(),
        retry_after: response
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(parse_retry_after),
    }
}

fn parse_retry_after(value: &str) -> Option<Duration> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if value.bytes().all(|byte| byte.is_ascii_digit()) {
        let seconds = value.parse::<u64>().ok()?;
        return Some(Duration::from_secs(seconds).min(MAX_RETRY_AFTER));
    }
    let retry_at = chrono::DateTime::parse_from_rfc2822(value)
        .ok()?
        .with_timezone(&chrono::Utc);
    let delay = (retry_at - chrono::Utc::now()).to_std().ok()?;
    (!delay.is_zero()).then_some(delay.min(MAX_RETRY_AFTER))
}

fn retry_delay(failure_count: u32, instance_id: Uuid, retry_after: Option<Duration>) -> Duration {
    let exponent = failure_count.saturating_sub(1).min(6);
    let base_ms = 250_u64.saturating_mul(1_u64 << exponent).min(10_000);
    let mut hasher = DefaultHasher::new();
    instance_id.hash(&mut hasher);
    failure_count.hash(&mut hasher);
    let jitter_percent = 75 + hasher.finish() % 51;
    let local = Duration::from_millis(base_ms * jitter_percent / 100);
    retry_after.map_or(local, |server| server.max(local))
}

fn insert_static_header(
    headers: &mut HeaderMap,
    name: &'static str,
    value: &'static str,
) -> Result<(), TunnelError> {
    headers.insert(
        HeaderName::from_static(name),
        HeaderValue::from_static(value),
    );
    Ok(())
}

fn insert_header(
    headers: &mut HeaderMap,
    name: &'static str,
    value: &str,
) -> Result<(), TunnelError> {
    headers.insert(
        HeaderName::from_static(name),
        HeaderValue::from_str(value)
            .map_err(|_| TunnelError::InvalidConfig("header value is invalid"))?,
    );
    Ok(())
}

async fn read_bounded(
    response: &mut reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, TunnelError> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(TunnelError::InvalidControlPlaneResponse(
            "poll body exceeds size limit",
        ));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(TunnelError::InvalidControlPlaneResponse(
                "poll body exceeds size limit",
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use contract::ErrorCode;
    use domain::DomainError;
    use runtime::{MCP_DEBUG_PORT, McpArtifactReply, PortFuture};
    use std::{
        io::{Read, Write},
        net::{SocketAddr, TcpListener, TcpStream},
        sync::{Arc, Mutex, atomic::AtomicUsize},
        thread,
        time::Duration,
    };
    use tokio::sync::Notify;

    const TUNNEL_ID: &str = "tunnel_0123456789abcdefghijklmnopqrstuv";

    #[derive(Clone, Default)]
    struct NoopHost;

    impl McpHost for NoopHost {
        fn submit<'a>(
            &'a self,
            _envelope: Vec<u8>,
        ) -> PortFuture<'a, Result<Vec<u8>, DomainError>> {
            unreachable!()
        }

        fn artifact_query<'a>(
            &'a self,
            _query: Value,
        ) -> PortFuture<'a, Result<McpArtifactReply, DomainError>> {
            unreachable!()
        }
    }

    #[derive(Clone, Debug)]
    struct CapturedRequest {
        path: String,
        headers: String,
        body: Vec<u8>,
    }

    fn client(base_url: &str) -> TunnelClient<NoopHost> {
        TunnelClient::new_with_base_url(
            McpFacade::new(NoopHost, MCP_DEBUG_PORT, "0.1.0".to_owned()).unwrap(),
            TUNNEL_ID,
            "test-api-key",
            "0.1.0",
            base_url,
            true,
        )
        .unwrap()
    }

    fn command(jsonrpc: Value) -> Value {
        json!({
            "request_id": "req_1",
            "shard_token": "shard-secret",
            "command_type": "jsonrpc",
            "channel": "main",
            "created_at": "2026-09-15T00:00:00Z",
            "headers": {
                "MCP-Protocol-Version": ["2026-07-28"],
                "Mcp-Method": [jsonrpc["method"].as_str().unwrap()],
            },
            "jsonrpc": jsonrpc,
        })
    }

    fn tools_list() -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": "rpc_1",
            "method": "tools/list",
            "params": {"_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {},
            }}
        })
    }

    fn spawn_server(
        statuses: Vec<u16>,
    ) -> (
        String,
        Arc<Mutex<Vec<CapturedRequest>>>,
        thread::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let captured = Arc::new(Mutex::new(Vec::new()));
        let output = Arc::clone(&captured);
        let handle = thread::spawn(move || {
            for status in statuses {
                let (mut stream, _) = listener.accept().unwrap();
                output.lock().unwrap().push(read_request(&mut stream));
                let reason = if status == 200 {
                    "OK"
                } else {
                    "Service Unavailable"
                };
                let extra = if status == 503 {
                    "Retry-After: 0\r\n"
                } else {
                    ""
                };
                let response_body = if status == 200 {
                    "{\"status\":\"ok\"}"
                } else {
                    ""
                };
                write!(
                    stream,
                    "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nContent-Type: application/json\r\n{extra}Connection: close\r\n\r\n{response_body}",
                    response_body.len(),
                )
                .unwrap();
            }
        });
        (format!("http://{address}/"), captured, handle)
    }

    #[tokio::test]
    async fn tools_list_is_dispatched_directly_and_correlated() {
        let (base_url, captured, server) = spawn_server(vec![200]);
        let client = client(&base_url);
        client
            .process_command(command(tools_list()), Instant::now())
            .await
            .unwrap();
        assert!(client.last_call_epoch_ms.load(Ordering::SeqCst) > 0);
        server.join().unwrap();

        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert!(request.headers.contains("POST /v1/tunnels/"));
        assert!(
            request
                .headers
                .contains("authorization: Bearer test-api-key")
        );
        assert!(
            request
                .headers
                .contains("x-tunnel-shard-token: shard-secret")
        );
        assert!(request.headers.contains(WIRE_PROTOCOL_VERSION));
        assert!(request.headers.contains(SERVER_INFO));
        let payload: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(payload["request_id"], "req_1");
        assert_eq!(payload["channel"], "main");
        assert_eq!(payload["resp_code"], 200);
        assert_eq!(payload["resp_type"], "jsonrpc_response");
        assert_eq!(payload["resp_json"]["id"], "rpc_1");
        assert_eq!(
            payload["resp_json"]["result"]["tools"]
                .as_array()
                .unwrap()
                .len(),
            8
        );
        assert!(payload.get("shard_token").is_none());
    }

    #[tokio::test]
    async fn terminal_response_retry_reuses_the_exact_body_and_correlation() {
        let (base_url, captured, server) = spawn_server(vec![503, 200]);
        let client = client(&base_url);
        client
            .process_command(
                json!({
                    "request_id": "req_terminate",
                    "shard_token": "route-1",
                    "command_type": "session_termination",
                    "created_at": "2026-09-15T00:00:00Z",
                    "headers": {"Mcp-Session-Id": ["session_1"]},
                }),
                Instant::now(),
            )
            .await
            .unwrap();
        server.join().unwrap();

        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].body, requests[1].body);
        assert!(
            requests
                .iter()
                .all(|request| request.headers.contains("x-tunnel-shard-token: route-1"))
        );
        let payload: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(payload["resp_code"], 405);
        assert_eq!(payload["resp_type"], "session_termination_response");
        assert!(payload.get("resp_json").is_none());
    }

    #[tokio::test]
    async fn notification_is_forwarded_then_acknowledged_without_a_json_body() {
        let (base_url, captured, server) = spawn_server(vec![200]);
        let client = client(&base_url);
        client
            .process_command(
                command(json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/cancelled",
                    "params": {},
                })),
                Instant::now(),
            )
            .await
            .unwrap();
        server.join().unwrap();

        let requests = captured.lock().unwrap();
        let payload: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(payload["resp_code"], 400);
        assert_eq!(payload["resp_type"], "notify_ack");
        assert!(payload.get("resp_json").is_none());
    }

    #[tokio::test]
    async fn expired_command_never_contacts_the_control_plane() {
        let client = client("http://127.0.0.1:9/");
        let mut expired = command(tools_list());
        expired["response_timeout"] = json!("0s");
        client
            .process_command(expired, Instant::now())
            .await
            .unwrap();
    }

    #[test]
    fn timeout_and_identifier_validation_match_the_wire_contract() {
        assert_eq!(
            parse_response_timeout(&json!("4500ms")),
            Some(Duration::from_millis(4500))
        );
        assert_eq!(parse_response_timeout(&json!("0s")), Some(Duration::ZERO));
        for invalid in [
            json!(30),
            json!(" 1s"),
            json!("1.5s"),
            json!("1m30s"),
            json!("-1s"),
            json!("1d"),
        ] {
            assert_eq!(parse_response_timeout(&invalid), None);
        }
        assert!(valid_tunnel_id(TUNNEL_ID));
        assert!(!valid_tunnel_id("tunnel_ABC"));
        assert!(valid_channel("main_2"));
        assert!(!valid_channel("Main"));
        assert_eq!(parse_retry_after("5"), Some(Duration::from_secs(5)));
        assert_eq!(parse_retry_after("invalid"), None);
        assert!(TunnelError::InvalidControlPlaneResponse("malformed poll body").poll_retryable());
    }

    /// Reads one HTTP request frame, head and body, as the tunnel's transport writes it.
    fn read_request(stream: &mut TcpStream) -> CapturedRequest {
        let mut raw = Vec::new();
        let mut block = [0_u8; 4096];
        let header_end = loop {
            let read = stream.read(&mut block).unwrap();
            assert!(read > 0);
            raw.extend_from_slice(&block[..read]);
            if let Some(index) = raw.windows(4).position(|window| window == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let headers = String::from_utf8(raw[..header_end].to_vec()).unwrap();
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.split_once(':').and_then(|(name, value)| {
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
            })
            .unwrap_or(0);
        while raw.len() - header_end < content_length {
            let read = stream.read(&mut block).unwrap();
            assert!(read > 0);
            raw.extend_from_slice(&block[..read]);
        }
        let path = headers
            .lines()
            .next()
            .and_then(|line| line.split(' ').nth(1))
            .unwrap_or_default()
            .to_owned();
        CapturedRequest {
            path,
            headers,
            body: raw[header_end..header_end + content_length].to_vec(),
        }
    }

    /// The script's answer. A client that shut itself down while this answer was in flight has
    /// closed the connection its own shutdown ended, so a refused write is where this script's
    /// part in that connection ends; every assertion in this module is on the requests it
    /// captured, never on an answer a gone client refused.
    fn write_response(stream: &mut TcpStream, status: u16, body: &str) -> std::io::Result<()> {
        let reason = match status {
            200 => "OK",
            204 => "No Content",
            401 => "Unauthorized",
            403 => "Forbidden",
            _ => "Service Unavailable",
        };
        write!(
            stream,
            "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}",
            body.len(),
        )
    }

    /// A host whose submissions wait for the test, so a command can be held in flight.
    #[derive(Clone, Default)]
    struct GateHost {
        started: Arc<AtomicUsize>,
        gate: Arc<Notify>,
    }

    impl GateHost {
        fn started(&self) -> usize {
            self.started.load(Ordering::SeqCst)
        }

        fn release_one(&self) {
            self.gate.notify_one();
        }
    }

    impl McpHost for GateHost {
        fn submit<'a>(&'a self, envelope: Vec<u8>) -> PortFuture<'a, Result<Vec<u8>, DomainError>> {
            Box::pin(async move {
                self.started.fetch_add(1, Ordering::SeqCst);
                let gate = Arc::clone(&self.gate);
                gate.notified().await;
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
            Box::pin(async { Err(DomainError::new(ErrorCode::IoError, "no artifacts here")) })
        }
    }

    /// Serves the tunnel's control plane from one script: the first poll answers with `commands`,
    /// every later poll waits briefly and answers 204 like a real long poll, and each response POST
    /// answers with the next status from `response_statuses` (the last one repeats).
    struct ScriptedControlPlane {
        base_url: String,
        address: SocketAddr,
        captured: Arc<Mutex<Vec<CapturedRequest>>>,
        stop: Arc<AtomicBool>,
        handle: Option<thread::JoinHandle<()>>,
    }

    impl ScriptedControlPlane {
        fn new(commands: Vec<Value>, response_statuses: Vec<u16>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let captured = Arc::new(Mutex::new(Vec::new()));
            let output = Arc::clone(&captured);
            let stop = Arc::new(AtomicBool::new(false));
            let halted = Arc::clone(&stop);
            let handle = thread::spawn(move || {
                let mut polls = 0_usize;
                let mut responses = 0_usize;
                loop {
                    let (mut stream, _) = listener.accept().unwrap();
                    if halted.load(Ordering::SeqCst) {
                        return;
                    }
                    let request = read_request(&mut stream);
                    let poll = request.path.contains("/poll");
                    output.lock().unwrap().push(request);
                    let answered = if poll {
                        let answered = if polls == 0 {
                            write_response(
                                &mut stream,
                                200,
                                &json!({"commands": commands}).to_string(),
                            )
                        } else {
                            thread::sleep(Duration::from_millis(10));
                            write_response(&mut stream, 204, "")
                        };
                        polls += 1;
                        answered
                    } else {
                        let status = response_statuses
                            .get(responses)
                            .or_else(|| response_statuses.last())
                            .copied()
                            .unwrap_or(200);
                        responses += 1;
                        write_response(&mut stream, status, "{}")
                    };
                    if answered.is_err() {
                        continue;
                    }
                }
            });
            Self {
                base_url: format!("http://{address}/"),
                address,
                captured,
                stop,
                handle: Some(handle),
            }
        }

        fn request_count(&self, fragment: &str) -> usize {
            self.captured
                .lock()
                .unwrap()
                .iter()
                .filter(|request| request.path.contains(fragment))
                .count()
        }

        fn stop(mut self) {
            self.stop.store(true, Ordering::SeqCst);
            let _ = TcpStream::connect(self.address);
            if let Some(handle) = self.handle.take() {
                handle.join().unwrap();
            }
        }
    }

    fn gated_client(base_url: &str, host: GateHost) -> TunnelClient<GateHost> {
        TunnelClient::new_with_base_url(
            McpFacade::new(host, MCP_DEBUG_PORT, "0.1.0".to_owned()).unwrap(),
            TUNNEL_ID,
            "test-api-key",
            "0.1.0",
            base_url,
            true,
        )
        .unwrap()
    }

    fn tools_call(request_id: &str, rpc_id: &str) -> Value {
        let mut built = command(json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "method": "tools/call",
            "params": {
                "name": "task_control",
                "arguments": {"action": "list", "input": {}},
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities": {},
                },
            },
        }));
        built["request_id"] = json!(request_id);
        built["headers"]["Mcp-Name"] = json!(["task_control"]);
        built
    }

    async fn wait_for(condition: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !condition() {
            assert!(
                Instant::now() < deadline,
                "the expected state was never reached"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    #[tokio::test]
    async fn polling_continues_while_a_command_is_running() {
        let host = GateHost::default();
        let control_plane =
            ScriptedControlPlane::new(vec![tools_call("req_gate", "rpc_gate")], vec![200]);
        let client = gated_client(&control_plane.base_url, host.clone());
        let (shutdown, receiver) = watch::channel(false);
        let running =
            tokio::spawn(
                async move { client.run(receiver, Arc::new(AtomicBool::new(false))).await },
            );

        wait_for(|| host.started() == 1).await;
        wait_for(|| control_plane.request_count("/poll") >= 2).await;
        assert_eq!(control_plane.request_count("/response"), 0);

        host.release_one();
        wait_for(|| control_plane.request_count("/response") == 1).await;
        let _ = shutdown.send(true);
        assert!(running.await.unwrap().is_ok());
        control_plane.stop();
    }

    #[tokio::test]
    async fn shutdown_ends_a_command_still_in_flight() {
        let host = GateHost::default();
        let control_plane =
            ScriptedControlPlane::new(vec![tools_call("req_gate", "rpc_gate")], vec![200]);
        let client = gated_client(&control_plane.base_url, host.clone());
        let (shutdown, receiver) = watch::channel(false);
        let running =
            tokio::spawn(
                async move { client.run(receiver, Arc::new(AtomicBool::new(false))).await },
            );

        wait_for(|| host.started() == 1).await;
        let _ = shutdown.send(true);
        let outcome = tokio::time::timeout(Duration::from_secs(5), running)
            .await
            .unwrap()
            .unwrap();
        assert!(outcome.is_ok());
        assert_eq!(control_plane.request_count("/response"), 0);
        control_plane.stop();
    }

    #[tokio::test]
    async fn a_refused_credential_from_a_running_command_ends_the_tunnel() {
        let host = GateHost::default();
        let control_plane = ScriptedControlPlane::new(
            vec![
                tools_call("req_one", "rpc_one"),
                tools_call("req_two", "rpc_two"),
            ],
            vec![401],
        );
        let client = gated_client(&control_plane.base_url, host.clone());
        let (shutdown, receiver) = watch::channel(false);
        let running =
            tokio::spawn(
                async move { client.run(receiver, Arc::new(AtomicBool::new(false))).await },
            );

        wait_for(|| host.started() == 2).await;
        host.release_one();
        let outcome = tokio::time::timeout(Duration::from_secs(10), running)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            outcome,
            Err(TunnelError::ControlPlaneStatus { status: 401, .. })
        ));
        assert_eq!(control_plane.request_count("/response"), 1);
        let _ = shutdown.send(true);
        control_plane.stop();
    }
}
