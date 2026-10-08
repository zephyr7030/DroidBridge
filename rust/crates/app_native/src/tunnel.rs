//! The JNI surface of the APK Runtime's ChatGPT tunnel; the client itself is the shared Runtime one.

use crate::mcp_listener::{initialize_host_bridge, kotlin_facade};
use jni::{
    EnvUnowned, Outcome,
    objects::{JClass, JString},
    sys::{JNI_FALSE, JNI_TRUE, jboolean, jint, jlong, jstring},
};
use runtime::{TunnelClient, TunnelError, TunnelRuntime, validate_tunnel_credentials};
use std::{ptr, sync::Mutex};

static TUNNEL: Mutex<Option<TunnelRuntime>> = Mutex::new(None);

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
    // A start is idempotent for a live tunnel; a refused one is replaced, so a start after the
    // network changed tries again at once instead of waiting out its retry interval.
    if slot
        .as_ref()
        .is_some_and(|tunnel| tunnel.state() != "failed")
    {
        return Ok(());
    }
    if let Some(refused) = slot.take() {
        refused.stop();
    }
    let facade = kotlin_facade(port, product_version.clone())
        .map_err(|_| TunnelError::InvalidConfig("MCP port or product version is invalid"))?;
    let client = TunnelClient::new(facade, &tunnel_id, &api_key, &product_version)?;
    *slot = Some(TunnelRuntime::start(client)?);
    Ok(())
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_droidbridge_standalone_runtimehost_NativeRuntime_nativeTunnelValidate(
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
pub extern "system" fn Java_com_droidbridge_standalone_runtimehost_NativeRuntime_nativeTunnelStart(
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
pub extern "system" fn Java_com_droidbridge_standalone_runtimehost_NativeRuntime_nativeTunnelStop(
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
pub extern "system" fn Java_com_droidbridge_standalone_runtimehost_NativeRuntime_nativeTunnelState(
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
pub extern "system" fn Java_com_droidbridge_standalone_runtimehost_NativeRuntime_nativeTunnelLastError(
    mut env: EnvUnowned,
    _class: JClass,
) -> jstring {
    match env
        .with_env(|owned| -> jni::errors::Result<jstring> {
            let last = tunnel_slot()
                .ok()
                .and_then(|slot| slot.as_ref().and_then(TunnelRuntime::last_error));
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
pub extern "system" fn Java_com_droidbridge_standalone_runtimehost_NativeRuntime_nativeTunnelLastCall(
    _env: EnvUnowned,
    _class: JClass,
) -> jlong {
    tunnel_slot()
        .ok()
        .and_then(|slot| slot.as_ref().map(TunnelRuntime::last_call_epoch_ms))
        .unwrap_or(0)
}
