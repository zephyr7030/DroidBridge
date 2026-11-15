//! The APK `:runtime` side of the loopback MCP listener: tool calls and artifact queries go to the
//! Kotlin HostController, the single path to the authoritative Runtime host.

use contract::ErrorCode;
use domain::DomainError;
use jni::{
    EnvUnowned, Outcome,
    objects::{JClass, JString},
    sys::{JNI_FALSE, JNI_TRUE, jboolean, jint, jstring},
};
use runtime::{
    MCP_DEBUG_PORT, MCP_STABLE_PORT, McpArtifactReply, McpFacade, McpHost, McpListener, PortFuture,
};
use serde_json::Value;
use std::{
    ptr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{Semaphore, oneshot};

/// Bounds the Kotlin host calls in flight, each on its own short-lived thread.
const MAX_HOST_CALLS: usize = 16;
/// How long a call waits for a slot before it is answered as busy. A call whose client left keeps
/// its slot until it finishes, so an unbounded wait would hang every client behind it.
const HOST_SLOT_WAIT: Duration = Duration::from_secs(10);

static LISTENER: Mutex<Option<McpListener>> = Mutex::new(None);

/// The Kotlin HostController, which submits to the APK Runtime.
pub(crate) struct KotlinMcpHost {
    permits: Arc<Semaphore>,
    slot_wait: Duration,
}

impl KotlinMcpHost {
    pub(crate) fn new() -> Self {
        Self {
            permits: Arc::new(Semaphore::new(MAX_HOST_CALLS)),
            slot_wait: HOST_SLOT_WAIT,
        }
    }

    async fn on_host_thread<T: Send + 'static>(
        &self,
        call: impl FnOnce() -> Result<T, DomainError> + Send + 'static,
    ) -> Result<T, DomainError> {
        let permit = match tokio::time::timeout(
            self.slot_wait,
            Arc::clone(&self.permits).acquire_owned(),
        )
        .await
        {
            Ok(Ok(permit)) => permit,
            Ok(Err(_)) => {
                return Err(DomainError::new(
                    ErrorCode::CapabilityUnavailable,
                    "MCP host is closed",
                ));
            }
            Err(_) => {
                return Err(DomainError::new(
                    ErrorCode::ResourceLimit,
                    "MCP host is busy: every call slot is held by a call that is still running; retry shortly",
                ));
            }
        };
        let (sender, receiver) = oneshot::channel();
        // A plain thread rather than `spawn_blocking`: the host call can block on the APK
        // Runtime's own executor, which refuses to start inside another Tokio runtime context.
        std::thread::Builder::new()
            .name("droidbridge-mcp-host".to_owned())
            .spawn(move || {
                // Client cancellation must not free capacity while this host call is still running.
                let _permit = permit;
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
    product_version: String,
) -> Result<McpFacade<KotlinMcpHost>, DomainError> {
    McpFacade::new(KotlinMcpHost::new(), product_version)
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
            "com/droidbridge/standalone/runtimehost/McpHostBridge"
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
    let facade = kotlin_facade(product_version)?;
    *slot = Some(McpListener::start(port, token, facade)?);
    Ok(())
}

/// Binds the build endpoint, replacing any previous listener, and returns once it is live.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_standalone_runtimehost_NativeRuntime_nativeMcpStart(
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
pub extern "system" fn Java_com_droidbridge_standalone_runtimehost_NativeRuntime_nativeMcpSetToken(
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
pub extern "system" fn Java_com_droidbridge_standalone_runtimehost_NativeRuntime_nativeMcpStop(
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

/// The observed listener: `stopped`, `running`, or `failed` when its slot cannot be read.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_standalone_runtimehost_NativeRuntime_nativeMcpState(
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
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    #[derive(Debug)]
    struct DropTracked(Arc<AtomicBool>, Option<oneshot::Sender<()>>);

    impl Drop for DropTracked {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
            if let Some(finished) = self.1.take() {
                let _ = finished.send(());
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_client_keeps_slot_until_worker_and_reply_are_finished() {
        let host = Arc::new(KotlinMcpHost {
            permits: Arc::new(Semaphore::new(1)),
            slot_wait: Duration::from_secs(5),
        });
        let (started, ready) = oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let (finished, ended) = oneshot::channel();
        let reply_dropped = Arc::new(AtomicBool::new(false));
        let worker_dropped = Arc::clone(&reply_dropped);
        let first_host = Arc::clone(&host);
        let first = tokio::spawn(async move {
            first_host
                .on_host_thread(move || {
                    let _ = started.send(());
                    let result = blocked.recv_timeout(Duration::from_secs(5));
                    result.expect("test must release the host worker");
                    Ok(DropTracked(worker_dropped, Some(finished)))
                })
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), ready)
            .await
            .expect("host worker must start")
            .expect("worker must signal its start");

        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());
        let occupied_after_client_cancel = host.permits.available_permits() == 0;

        release.send(()).expect("host worker must still be running");
        tokio::time::timeout(Duration::from_secs(2), ended)
            .await
            .expect("worker must finish")
            .expect("worker must signal completion");
        let next = tokio::time::timeout(Duration::from_secs(2), host.on_host_thread(|| Ok(42)))
            .await
            .expect("a completed worker must release its slot")
            .expect("the next host call must complete");

        assert_eq!(next, 42);
        assert!(reply_dropped.load(Ordering::SeqCst));
        assert!(
            occupied_after_client_cancel,
            "client cancellation released a slot while its worker was still running"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_call_that_finds_every_slot_held_is_answered_busy_and_the_held_call_is_untouched() {
        let host = Arc::new(KotlinMcpHost {
            permits: Arc::new(Semaphore::new(1)),
            slot_wait: Duration::from_millis(150),
        });
        let (started, ready) = oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let held_host = Arc::clone(&host);
        let held = tokio::spawn(async move {
            held_host
                .on_host_thread(move || {
                    let _ = started.send(());
                    blocked
                        .recv_timeout(Duration::from_secs(5))
                        .expect("test must release the host worker");
                    Ok(7)
                })
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), ready)
            .await
            .expect("host worker must start")
            .expect("worker must signal its start");

        let waited = std::time::Instant::now();
        let busy = host.on_host_thread(|| Ok(1)).await.unwrap_err();
        assert_eq!(busy.code, ErrorCode::ResourceLimit);
        assert!(busy.reason.contains("busy"), "{}", busy.reason);
        assert!(waited.elapsed() >= Duration::from_millis(100));
        assert!(waited.elapsed() < Duration::from_secs(3));

        // The call that holds the slot is not cancelled: it finishes with its own answer, and
        // only then does the slot take another call.
        release.send(()).expect("host worker must still be running");
        assert_eq!(held.await.unwrap().unwrap(), 7);
        let next = tokio::time::timeout(Duration::from_secs(2), host.on_host_thread(|| Ok(42)))
            .await
            .expect("a finished worker must release its slot")
            .unwrap();
        assert_eq!(next, 42);
    }

    #[tokio::test]
    async fn failed_host_call_releases_its_slot() {
        let host = KotlinMcpHost {
            permits: Arc::new(Semaphore::new(1)),
            slot_wait: Duration::from_secs(5),
        };
        let result = host
            .on_host_thread(|| {
                Err::<(), _>(DomainError::new(ErrorCode::IoError, "test host failure"))
            })
            .await;
        assert_eq!(result.unwrap_err().code, ErrorCode::IoError);
        let next = tokio::time::timeout(Duration::from_secs(2), host.on_host_thread(|| Ok(42)))
            .await
            .expect("a failed host call must release its slot")
            .unwrap();
        assert_eq!(next, 42);
    }
}
