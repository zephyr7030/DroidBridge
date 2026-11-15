//! The root edition's MCP ingress: the loopback listener and the ChatGPT tunnel run in this daemon
//! from the settings it owns, and serve the Runtime host while one is active. The frontend changes
//! the settings only through the operations here, each of which commits before it applies and
//! answers with the committed state.

use crate::settings::{SettingsStore, TunnelSettings, valid_api_key, valid_tunnel_id};
use runtime::{McpFacade, McpHost, McpListener, TunnelClient, TunnelRuntime};
use serde_json::{Value, json};

const LISTENER_FAILED: &str = "MCP_LISTENER_FAILED";
const RUNTIME_UNAVAILABLE: &str = "RUNTIME_UNAVAILABLE";
const TUNNEL_RUNTIME_FAILED: &str = "TUNNEL_RUNTIME_FAILED";

pub(crate) struct DaemonIngress<H> {
    settings: SettingsStore,
    port: u16,
    product_version: String,
    host: Option<H>,
    listener: Option<(McpListener, String)>,
    listener_failed: bool,
    tunnel: Option<(TunnelRuntime, TunnelSettings)>,
    tunnel_failed: bool,
}

impl<H: McpHost + Clone + 'static> DaemonIngress<H> {
    pub(crate) fn new(settings: SettingsStore, port: u16) -> Self {
        Self {
            settings,
            port,
            product_version: env!("CARGO_PKG_VERSION").to_owned(),
            host: None,
            listener: None,
            listener_failed: false,
            tunnel: None,
            tunnel_failed: false,
        }
    }

    /// Serves [host], or nothing while it is none; both stop what served the previous host.
    pub(crate) fn attach(&mut self, host: Option<H>) {
        self.stop_listener();
        self.stop_tunnel();
        self.host = host;
        self.reconcile();
    }

    /// Brings the listener and the tunnel to the committed settings.
    pub(crate) fn reconcile(&mut self) {
        let mcp = self.settings.mcp().ok().filter(|settings| settings.enabled);
        self.reconcile_listener(mcp.map(|settings| settings.token));
        let tunnel = self.settings.tunnel().ok().flatten();
        self.reconcile_tunnel(tunnel.filter(|settings| settings.enabled));
    }

    pub(crate) fn mcp_settings(&mut self) -> Value {
        match self.settings.mcp() {
            Ok(settings) => self.mcp_status(settings.enabled),
            Err(_) => failure("IO_ERROR"),
        }
    }

    pub(crate) fn mcp_set_enabled(&mut self, enabled: bool) -> Value {
        let Ok(mut settings) = self.settings.mcp() else {
            return failure("IO_ERROR");
        };
        if settings.enabled != enabled {
            settings.enabled = enabled;
            if self.settings.commit_mcp(&settings).is_err() {
                return failure("IO_ERROR");
            }
        }
        self.reconcile_listener(enabled.then(|| settings.token.clone()));
        self.mcp_status(enabled)
    }

    pub(crate) fn mcp_rotate(&mut self) -> Value {
        let Ok(mut settings) = self.settings.mcp() else {
            return failure("IO_ERROR");
        };
        let Ok(token) = crate::settings::new_token() else {
            return failure("IO_ERROR");
        };
        settings.token = token;
        if self.settings.commit_mcp(&settings).is_err() {
            return failure("IO_ERROR");
        }
        self.reconcile_listener(settings.enabled.then(|| settings.token.clone()));
        self.mcp_status(settings.enabled)
    }

    pub(crate) fn mcp_reveal(&self) -> Value {
        match self.settings.mcp() {
            Ok(settings) => json!({"schema_version": 1, "token": settings.token}),
            Err(_) => failure("IO_ERROR"),
        }
    }

    pub(crate) fn tunnel_settings(&mut self) -> Value {
        match self.settings.tunnel() {
            Ok(settings) => self.tunnel_status(settings),
            Err(_) => failure("IO_ERROR"),
        }
    }

    /// Checks the credentials against the control plane before anything is committed.
    pub(crate) fn tunnel_configure(&mut self, tunnel_id: &str, api_key: &str) -> Value {
        if !valid_tunnel_id(tunnel_id) || !valid_api_key(api_key) {
            return failure("INVALID_CONFIG");
        }
        match runtime::validate_tunnel_credentials(tunnel_id, api_key, &self.product_version) {
            "valid" => {}
            "invalid_tunnel" => return failure("TUNNEL_NOT_FOUND"),
            "invalid_key" => return failure("API_KEY_INVALID"),
            _ => return failure("OPENAI_UNAVAILABLE"),
        }
        let enabled = match self.settings.tunnel() {
            Ok(current) => current.is_some_and(|settings| settings.enabled),
            Err(_) => return failure("IO_ERROR"),
        };
        let settings = TunnelSettings::new(tunnel_id.to_owned(), api_key.to_owned(), enabled);
        if self.settings.commit_tunnel(&settings).is_err() {
            return failure("IO_ERROR");
        }
        self.reconcile_tunnel(enabled.then(|| settings.clone()));
        self.tunnel_status(Some(settings))
    }

    pub(crate) fn tunnel_set_enabled(&mut self, enabled: bool) -> Value {
        let mut settings = match self.settings.tunnel() {
            Ok(Some(settings)) => settings,
            Ok(None) => return failure("NOT_CONFIGURED"),
            Err(_) => return failure("IO_ERROR"),
        };
        if settings.enabled != enabled {
            settings.enabled = enabled;
            if self.settings.commit_tunnel(&settings).is_err() {
                return failure("IO_ERROR");
            }
        }
        self.reconcile_tunnel(enabled.then(|| settings.clone()));
        self.tunnel_status(Some(settings))
    }

    pub(crate) fn tunnel_clear(&mut self) -> Value {
        self.stop_tunnel();
        if self.settings.clear_tunnel().is_err() {
            return failure("IO_ERROR");
        }
        self.tunnel_status(None)
    }

    fn mcp_status(&self, enabled: bool) -> Value {
        let running = self
            .listener
            .as_ref()
            .is_some_and(|(listener, _)| listener.state() == "running");
        let reason = if !enabled || running {
            None
        } else if self.host.is_none() {
            Some(RUNTIME_UNAVAILABLE)
        } else if self.listener_failed || self.listener.is_some() {
            Some(LISTENER_FAILED)
        } else {
            None
        };
        let state = match (reason, running) {
            (Some(_), _) => "failed",
            (None, true) => "running",
            (None, false) => "stopped",
        };
        let mut status = json!({
            "schema_version": 1,
            "enabled": enabled,
            "listener": state,
            "endpoint": format!("http://127.0.0.1:{}/mcp", self.port),
            "protocol_version": runtime::MCP_PROTOCOL_VERSION,
        });
        if let Some(reason) = reason {
            status["reason"] = Value::from(reason);
        }
        status
    }

    fn tunnel_status(&mut self, settings: Option<TunnelSettings>) -> Value {
        let observed_call = self
            .tunnel
            .as_ref()
            .map(|(tunnel, _)| tunnel.last_call_epoch_ms())
            .filter(|value| *value > 0);
        // The running tunnel knows only calls since it started, so the first one is kept.
        let settings = match (settings, observed_call) {
            (Some(mut current), Some(call)) if current.first_call_epoch_ms.is_none() => {
                current.first_call_epoch_ms = Some(call);
                if self.settings.commit_tunnel(&current).is_err() {
                    current.first_call_epoch_ms = None;
                }
                Some(current)
            }
            (settings, _) => settings,
        };
        let enabled = settings.as_ref().is_some_and(|settings| settings.enabled);
        let native = self.tunnel.as_ref().map(|(tunnel, _)| tunnel.state());
        let reason = if !enabled {
            None
        } else if self.host.is_none() {
            Some(RUNTIME_UNAVAILABLE)
        } else if self.tunnel_failed || native == Some("failed") {
            Some(TUNNEL_RUNTIME_FAILED)
        } else {
            None
        };
        let state = match (enabled, reason, native) {
            (false, _, _) => "stopped",
            (true, Some(_), _) => "failed",
            (true, None, Some("running")) => "running",
            (true, None, _) => "connecting",
        };
        let mut status = json!({
            "schema_version": 1,
            "configured": settings.is_some(),
            "enabled": enabled,
            "state": state,
            "protocol_version": runtime::MCP_PROTOCOL_VERSION,
        });
        if let Some(settings) = &settings {
            status["tunnel_id"] = Value::from(settings.tunnel_id.clone());
        }
        if let Some(reason) = reason {
            status["reason"] = Value::from(reason);
        }
        if let Some(call) = observed_call.or(settings.and_then(|value| value.first_call_epoch_ms)) {
            status["last_call_epoch_ms"] = Value::from(call);
        }
        // Only a tunnel that is enabled but not running has a failure worth naming.
        if matches!(state, "connecting" | "failed")
            && let Some(error) = self
                .tunnel
                .as_ref()
                .and_then(|(tunnel, _)| tunnel.last_error())
        {
            status["last_error"] = Value::from(error);
        }
        status
    }

    fn reconcile_listener(&mut self, token: Option<String>) {
        let (Some(token), Some(host)) = (token, self.host.clone()) else {
            self.stop_listener();
            return;
        };
        if let Some((listener, current)) = &mut self.listener {
            if listener.state() == "running" {
                if *current != token {
                    if listener.set_token(token.clone()).is_ok() {
                        *current = token;
                    } else {
                        // A listener still holding the replaced token must not keep serving.
                        self.stop_listener();
                        self.listener_failed = true;
                    }
                }
                return;
            }
            self.stop_listener();
        }
        let started = McpFacade::new(host, self.product_version.clone())
            .and_then(|facade| McpListener::start(self.port, token.clone(), facade));
        match started {
            Ok(listener) => {
                self.listener = Some((listener, token));
                self.listener_failed = false;
            }
            Err(_) => self.listener_failed = true,
        }
    }

    fn reconcile_tunnel(&mut self, settings: Option<TunnelSettings>) {
        let (Some(settings), Some(host)) = (settings, self.host.clone()) else {
            self.stop_tunnel();
            return;
        };
        if self.tunnel.as_ref().is_some_and(|(tunnel, running)| {
            running.tunnel_id == settings.tunnel_id
                && running.api_key == settings.api_key
                && tunnel.state() != "failed"
        }) {
            return;
        }
        self.stop_tunnel();
        let started = McpFacade::new(host, self.product_version.clone())
            .map_err(|_| ())
            .and_then(|facade| {
                TunnelClient::new(
                    facade,
                    &settings.tunnel_id,
                    &settings.api_key,
                    &self.product_version,
                )
                .map_err(|_| ())
            })
            .and_then(|client| TunnelRuntime::start(client).map_err(|_| ()));
        match started {
            Ok(tunnel) => self.tunnel = Some((tunnel, settings)),
            Err(()) => self.tunnel_failed = true,
        }
    }

    fn stop_listener(&mut self) {
        if let Some((listener, _)) = self.listener.take() {
            listener.stop();
        }
        self.listener_failed = false;
    }

    fn stop_tunnel(&mut self) {
        if let Some((tunnel, _)) = self.tunnel.take() {
            tunnel.stop();
        }
        self.tunnel_failed = false;
    }
}

impl<H> Drop for DaemonIngress<H> {
    fn drop(&mut self) {
        if let Some((listener, _)) = self.listener.take() {
            listener.stop();
        }
        if let Some((tunnel, _)) = self.tunnel.take() {
            tunnel.stop();
        }
    }
}

fn failure(code: &str) -> Value {
    json!({"schema_version": 1, "error": code})
}
