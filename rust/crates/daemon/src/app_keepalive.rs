//! Keeps the App's Runtime alive while a persistent agent connection is enabled (the owner's
//! choice A: the tunnel stays in the App and the Magisk module keeps that process alive).
//!
//! It follows what root keep-alive modules do, limited to what a platform service offers: the App
//! is put on the device-idle allowlist and allowed to run in the background once per daemon
//! process, and a Runtime that stops answering the companion socket is started again through the
//! Activity Manager. Locking `oom_score_adj` and freezing vendor background managers are left
//! out: the first fights the framework on a timer, the second changes every app on the device.

#[cfg(unix)]
use crate::magisk_host::fixed_property;
use serde_json::Value;
use std::{
    fs,
    path::Path,
    time::{Duration, Instant},
};

/// The action the App's Runtime service answers by entering the foreground and restoring its
/// enabled connections.
pub(crate) const WAKE_ACTION: &str = "com.droidbridge.android.action.KEEPALIVE_WAKE";
const SERVICE_CLASS: &str = "com.droidbridge.android.runtimehost.DroidBridgeService";
/// The App's committed ChatGPT tunnel preference; only its `enabled` flag is read here.
const TUNNEL_SETTINGS: &str = "tunnel.json";
/// Minimum spacing after the n-th consecutive wake; a Runtime that answers resets the sequence.
const WAKE_SPACING_SECONDS: [u64; 5] = [0, 15, 60, 180, 300];
#[cfg(unix)]
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

/// Whether the App has a connection enabled that must survive its process: the committed tunnel
/// preference. A missing or unreadable file never wakes the App.
pub(crate) fn connection_wanted(canonical_base: &Path) -> bool {
    let Ok(bytes) = fs::read(canonical_base.join(TUNNEL_SETTINGS)) else {
        return false;
    };
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return false;
    };
    value.get("schema_version").and_then(Value::as_u64) == Some(1)
        && value.get("enabled").and_then(Value::as_bool) == Some(true)
}

/// Whether a wake is due, given the previous wake and how many consecutive wakes went unanswered.
pub(crate) fn wake_due(last_wake: Option<Instant>, unanswered: usize, now: Instant) -> bool {
    let Some(last) = last_wake else {
        return true;
    };
    let spacing = WAKE_SPACING_SECONDS[unanswered.min(WAKE_SPACING_SECONDS.len() - 1)];
    now.saturating_duration_since(last) >= Duration::from_secs(spacing)
}

pub(crate) struct AppKeepAlive {
    package: &'static str,
    exempted: bool,
    unanswered: usize,
    last_wake: Option<Instant>,
}

impl AppKeepAlive {
    pub(crate) const fn new(package: &'static str) -> Self {
        Self {
            package,
            exempted: false,
            unanswered: 0,
            last_wake: None,
        }
    }

    /// The App's Runtime answered the companion socket.
    pub(crate) fn observe_present(&mut self) {
        self.unanswered = 0;
        self.last_wake = None;
    }

    /// No Runtime answered the companion socket: start it again when a connection wants it.
    #[cfg(unix)]
    pub(crate) fn observe_absent(&mut self, canonical_base: &Path) {
        if !connection_wanted(canonical_base) || !user_unlocked() {
            return;
        }
        let now = Instant::now();
        if !wake_due(self.last_wake, self.unanswered, now) {
            return;
        }
        if !self.exempted {
            let allowlisted = format!("+{}", self.package);
            self.exempted = run_logged(
                "/system/bin/cmd",
                &["deviceidle", "whitelist", &allowlisted],
            ) && run_logged(
                "/system/bin/cmd",
                &[
                    "appops",
                    "set",
                    self.package,
                    "RUN_ANY_IN_BACKGROUND",
                    "allow",
                ],
            );
        }
        self.last_wake = Some(now);
        self.unanswered = self.unanswered.saturating_add(1);
        let component = format!("{}/{SERVICE_CLASS}", self.package);
        run_logged(
            "/system/bin/am",
            &[
                "start-foreground-service",
                "--user",
                "0",
                "-a",
                WAKE_ACTION,
                "-n",
                &component,
            ],
        );
    }
}

/// The App is not direct-boot aware, so its service cannot start before the user unlocks.
#[cfg(unix)]
fn user_unlocked() -> bool {
    fixed_property("sys.boot_completed").is_ok_and(|value| value == "1")
        && fixed_property("sys.user.0.ce_available").is_ok_and(|value| value == "true")
}

/// Runs one fixed platform command and reports a failure on the daemon's stderr log.
#[cfg(unix)]
fn run_logged(program: &str, arguments: &[&str]) -> bool {
    use std::process::{Command, Stdio};
    let outcome = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "cannot spawn".to_owned())
        .and_then(|mut child| {
            let deadline = Instant::now() + COMMAND_TIMEOUT;
            loop {
                match child.try_wait() {
                    Ok(Some(status)) if status.success() => return Ok(()),
                    Ok(Some(status)) => return Err(format!("exit {status}")),
                    Ok(None) if Instant::now() >= deadline => {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err("timed out".to_owned());
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                    Err(_) => return Err("cannot observe".to_owned()),
                }
            }
        });
    match outcome {
        Ok(()) => true,
        Err(reason) => {
            let operation = arguments.first().copied().unwrap_or_default();
            eprintln!("droidbridged: keep-alive {program} {operation} failed: {reason}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{connection_wanted, wake_due};
    use std::{
        fs,
        time::{Duration, Instant},
    };

    #[test]
    fn keepalive_follows_the_committed_tunnel_preference_only() {
        let base = std::env::temp_dir().join(format!("keepalive-{}", std::process::id()));
        fs::create_dir_all(&base).unwrap();
        let settings = base.join("tunnel.json");
        let _ = fs::remove_file(&settings);
        assert!(!connection_wanted(&base), "no preference never wakes");
        for (contents, wanted) in [
            (
                r#"{"schema_version":1,"enabled":true,"tunnel_id":"t"}"#,
                true,
            ),
            (r#"{"schema_version":1,"enabled":false}"#, false),
            (r#"{"schema_version":2,"enabled":true}"#, false),
            (r#"{"schema_version":1,"enabled":"true"}"#, false),
            ("not json", false),
        ] {
            fs::write(&settings, contents).unwrap();
            assert_eq!(connection_wanted(&base), wanted, "{contents}");
        }
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn unanswered_wakes_are_spaced_further_apart() {
        let start = Instant::now();
        assert!(wake_due(None, 0, start));
        let later = |seconds| start + Duration::from_secs(seconds);
        assert!(wake_due(Some(start), 0, start));
        assert!(!wake_due(Some(start), 1, later(14)));
        assert!(wake_due(Some(start), 1, later(15)));
        assert!(!wake_due(Some(start), 2, later(59)));
        assert!(!wake_due(Some(start), 9, later(299)));
        assert!(wake_due(Some(start), 9, later(300)));
    }
}
