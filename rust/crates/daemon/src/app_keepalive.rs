//! Keeps the App's Runtime alive while something needs it: an enabled agent connection (the
//! owner's choice A — the tunnel stays in the App and the root module keeps that process alive),
//! a local MCP listener the App was already running, or a Task this daemon itself still owns.
//!
//! It follows what root keep-alive modules do, limited to what a platform service offers: the App
//! is put on the device-idle allowlist and allowed to run in the background once per daemon
//! process, and a Runtime that stops answering the companion socket is started again through the
//! Activity Manager. Locking `oom_score_adj` and freezing vendor background managers are left
//! out: the first fights the framework on a timer, the second changes every app on the device.

#[cfg(unix)]
use crate::magisk_host::fixed_property;
use serde_json::Value;
#[cfg(unix)]
use std::thread;
use std::{
    fs,
    path::Path,
    sync::{Arc, Condvar, Mutex, Once},
    time::{Duration, Instant},
};

/// The action the App's Runtime service answers by entering the foreground and restoring its
/// enabled connections.
pub(crate) const WAKE_ACTION: &str = "com.droidbridge.android.action.KEEPALIVE_WAKE";
/// The action the App's Runtime service answers by holding itself in the foreground for exactly
/// the Tasks this daemon still owns; both sides spell it and its extras identically.
#[cfg(unix)]
const TASK_ACTIVITY_ACTION: &str = "com.droidbridge.android.action.TASK_ACTIVITY";
/// The shortest spacing between two Task-count wakes.
#[cfg(unix)]
const PUBLISH_SPACING: Duration = Duration::from_secs(1);
const SERVICE_CLASS: &str = "com.droidbridge.android.runtimehost.DroidBridgeService";
/// The App's committed agent connections; only their `enabled` flags are read here.
const TUNNEL_SETTINGS: &str = "tunnel.json";
const MCP_SETTINGS: &str = "mcp.json";
/// Minimum spacing after the n-th consecutive wake; a Runtime that answers resets the sequence.
const WAKE_SPACING_SECONDS: [u64; 5] = [0, 15, 60, 180, 300];
#[cfg(unix)]
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

/// Whether the committed ChatGPT tunnel preference is enabled. The tunnel dials out and is
/// restored on its own, so it wakes the App even before this device has ever run it.
pub(crate) fn connection_wanted(canonical_base: &Path) -> bool {
    enabled(&canonical_base.join(TUNNEL_SETTINGS))
}

/// Whether the committed local MCP preference is enabled. It opens a loopback listener, so a
/// boot alone never starts it: it is only ever restored to an App that was already running it.
fn mcp_wanted(canonical_base: &Path) -> bool {
    enabled(&canonical_base.join(MCP_SETTINGS))
}

/// One committed preference's `enabled` flag. A missing or unreadable file never wakes the App,
/// and nothing else in the file is read.
fn enabled(path: &Path) -> bool {
    let Ok(bytes) = fs::read(path) else {
        return false;
    };
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return false;
    };
    value.get("schema_version").and_then(Value::as_u64) == Some(1)
        && value.get("enabled").and_then(Value::as_bool) == Some(true)
}

/// Whether an absent App must be started again. The tunnel and this daemon's own Tasks answer for
/// themselves; the local MCP listener is restored only to an App this daemon already saw run it.
const fn start_wanted(tasks_want_app: bool, tunnel: bool, mcp: bool, ran_this_boot: bool) -> bool {
    tasks_want_app || tunnel || (mcp && ran_this_boot)
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
    ran_this_boot: bool,
}

impl AppKeepAlive {
    pub(crate) const fn new(package: &'static str) -> Self {
        Self {
            package,
            exempted: false,
            unanswered: 0,
            last_wake: None,
            ran_this_boot: false,
        }
    }

    /// The App's Runtime answered the companion socket.
    pub(crate) fn observe_present(&mut self) {
        self.unanswered = 0;
        self.last_wake = None;
        self.ran_this_boot = true;
    }

    /// No Runtime answered the companion socket: start it again when a connection, or a Task this
    /// daemon still owns, wants it. An enabled local MCP listener counts only once this daemon has
    /// seen the App run it, so a boot still never opens that listener by itself.
    #[cfg(unix)]
    pub(crate) fn observe_absent(&mut self, canonical_base: &Path, tasks_want_app: bool) {
        let wanted = start_wanted(
            tasks_want_app,
            connection_wanted(canonical_base),
            mcp_wanted(canonical_base),
            self.ran_this_boot,
        );
        if !wanted || !user_unlocked() {
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

/// One count of non-terminal Tasks, as the App was last told it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TaskActivityPublication {
    pub(crate) active: u64,
    pub(crate) revision: u64,
}

#[derive(Default)]
struct TaskActivityState {
    epoch: String,
    active: u64,
    revision: u64,
    connected: bool,
    published: Option<TaskActivityPublication>,
}

impl TaskActivityState {
    /// What the App has not been told yet. Only a live companion connection is told anything: a
    /// wake sent to an App that is not there would promise a foreground service no process can
    /// enter, and a release is only ever owed to an App that was told to hold in the first place.
    fn due(&self) -> Option<(String, TaskActivityPublication)> {
        if !self.connected || self.revision == 0 {
            return None;
        }
        let publication = TaskActivityPublication {
            active: self.active,
            revision: self.revision,
        };
        match self.published {
            Some(published) if published.revision >= publication.revision => None,
            Some(published) if published.active == 0 && publication.active == 0 => None,
            None if publication.active == 0 => None,
            _ => Some((self.epoch.clone(), publication)),
        }
    }
}

/// The non-terminal Task count of a Runtime this daemon hosts.
///
/// The App serves the Android primitives those Tasks execute, so while they run its Runtime
/// process must not be a process the platform may reclaim. The count is carried to the App's
/// Runtime service, which holds itself in the foreground for exactly as long as it lasts, and it
/// also keeps the wake loop reviving an App that died with Tasks outstanding.
pub(crate) struct TaskActivityBeacon {
    #[cfg_attr(not(unix), expect(dead_code, reason = "the wake is Android-only"))]
    package: &'static str,
    state: Mutex<TaskActivityState>,
    changed: Condvar,
    #[cfg_attr(not(unix), expect(dead_code, reason = "the publisher is Android-only"))]
    publisher: Once,
}

impl TaskActivityBeacon {
    pub(crate) fn new(package: &'static str) -> Arc<Self> {
        Arc::new(Self {
            package,
            state: Mutex::new(TaskActivityState::default()),
            changed: Condvar::new(),
            publisher: Once::new(),
        })
    }

    /// The canonical Runtime committed this count. Revisions order one Runtime epoch; a new epoch
    /// owns its own sequence and starts by telling the App what it now holds.
    pub(crate) fn publish(&self, epoch: &str, active: u64, revision: u64) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.epoch != epoch {
            state.epoch = epoch.to_owned();
            state.revision = 0;
            state.published = None;
        }
        if revision <= state.revision {
            return;
        }
        state.revision = revision;
        state.active = active;
        drop(state);
        self.changed.notify_all();
    }

    /// Whether a dead App must be started for Tasks alone.
    pub(crate) fn wake_wanted(&self) -> bool {
        self.state.lock().is_ok_and(|state| state.active > 0)
    }

    /// A companion connection was accepted or lost. A new connection is told the current count
    /// even when the count itself did not change, since the App that answers it may be a new
    /// process that holds nothing yet.
    pub(crate) fn set_connected(self: &Arc<Self>, connected: bool) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.connected = connected;
        state.published = None;
        drop(state);
        #[cfg(unix)]
        if connected {
            let beacon = Arc::clone(self);
            self.publisher.call_once(move || {
                let _ = thread::Builder::new()
                    .name("droidbridge-task-activity".to_owned())
                    .spawn(move || publish_loop(&beacon));
            });
        }
        self.changed.notify_all();
    }
}

/// Carries every count the App still owes a hold for, one wake at a time. Counts that change
/// while a wake is in flight are coalesced, and consecutive wakes are spaced, so a Task-heavy
/// run costs the device one Activity Manager call per second at most.
#[cfg(unix)]
fn publish_loop(beacon: &Arc<TaskActivityBeacon>) {
    loop {
        let Ok(mut state) = beacon.state.lock() else {
            return;
        };
        let due = loop {
            if let Some(due) = state.due() {
                break due;
            }
            let Ok(next) = beacon.changed.wait(state) else {
                return;
            };
            state = next;
        };
        drop(state);
        let (epoch, publication) = due;
        wake_task_activity(beacon.package, &epoch, publication);
        let Ok(mut state) = beacon.state.lock() else {
            return;
        };
        state.published = Some(publication);
        drop(state);
        thread::sleep(PUBLISH_SPACING);
    }
}

#[cfg(unix)]
fn wake_task_activity(package: &str, epoch: &str, publication: TaskActivityPublication) {
    let component = format!("{package}/{SERVICE_CLASS}");
    let active = publication.active.to_string();
    let revision = publication.revision.to_string();
    run_logged(
        "/system/bin/am",
        &[
            "start-foreground-service",
            "--user",
            "0",
            "-a",
            TASK_ACTIVITY_ACTION,
            "-n",
            &component,
            "--es",
            "runtime_epoch",
            epoch,
            "--ei",
            "active_tasks",
            &active,
            "--el",
            "canonical_revision",
            &revision,
        ],
    );
}

#[cfg(test)]
mod tests {
    use super::{
        TaskActivityBeacon, TaskActivityPublication, connection_wanted, mcp_wanted, start_wanted,
        wake_due,
    };
    use std::{
        fs,
        time::{Duration, Instant},
    };

    fn due(beacon: &TaskActivityBeacon) -> Option<(String, TaskActivityPublication)> {
        beacon.state.lock().unwrap().due()
    }

    fn mark_published(beacon: &TaskActivityBeacon, publication: TaskActivityPublication) {
        beacon.state.lock().unwrap().published = Some(publication);
    }

    #[test]
    fn only_a_connected_app_is_told_a_count_it_does_not_already_hold() {
        let beacon = TaskActivityBeacon::new("com.droidbridge.android");
        beacon.publish("epoch-a", 1, 7);
        assert_eq!(due(&beacon), None, "a disconnected App is never woken");
        assert!(beacon.wake_wanted(), "but its Tasks still want it started");

        beacon.set_connected(true);
        let first = due(&beacon).expect("the live App is told the count");
        assert_eq!(first.0, "epoch-a");
        assert_eq!(
            first.1,
            TaskActivityPublication {
                active: 1,
                revision: 7
            }
        );
        mark_published(&beacon, first.1);
        assert_eq!(due(&beacon), None, "the same count is not repeated");

        beacon.publish("epoch-a", 2, 6);
        assert_eq!(due(&beacon), None, "an older revision never overtakes");
        beacon.publish("epoch-a", 0, 9);
        let release = due(&beacon).expect("the release is owed").1;
        assert_eq!(
            release,
            TaskActivityPublication {
                active: 0,
                revision: 9
            }
        );
        mark_published(&beacon, release);
        beacon.publish("epoch-a", 0, 11);
        assert_eq!(due(&beacon), None, "idle is never announced twice");
        assert!(!beacon.wake_wanted(), "an idle daemon never wakes the App");
    }

    #[test]
    fn a_reconnected_app_is_told_again_and_an_idle_one_is_left_alone() {
        let beacon = TaskActivityBeacon::new("com.droidbridge.android");
        beacon.set_connected(true);
        beacon.publish("epoch-a", 0, 3);
        assert_eq!(
            due(&beacon),
            None,
            "a fresh connection with no Tasks is quiet"
        );
        beacon.publish("epoch-a", 4, 4);
        let held = due(&beacon).expect("Tasks arrived").1;
        mark_published(&beacon, held);

        beacon.set_connected(false);
        assert_eq!(due(&beacon), None);
        beacon.set_connected(true);
        assert_eq!(
            due(&beacon).expect("a new App process holds nothing yet").1,
            held,
        );

        beacon.publish("epoch-b", 1, 1);
        assert_eq!(
            due(&beacon).expect("a new epoch owns its own sequence").1,
            TaskActivityPublication {
                active: 1,
                revision: 1
            },
        );
    }

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
    fn a_boot_starts_the_tunnel_but_never_opens_the_local_listener_by_itself() {
        let base = std::env::temp_dir().join(format!("keepalive-mcp-{}", std::process::id()));
        fs::create_dir_all(&base).unwrap();
        fs::write(
            base.join("mcp.json"),
            r#"{"schema_version":1,"enabled":true,"token":"t"}"#,
        )
        .unwrap();
        assert!(mcp_wanted(&base));
        fs::write(
            base.join("mcp.json"),
            r#"{"schema_version":1,"enabled":false}"#,
        )
        .unwrap();
        assert!(!mcp_wanted(&base));
        fs::remove_dir_all(&base).unwrap();

        assert!(!start_wanted(false, false, true, false), "boot alone");
        assert!(
            start_wanted(false, false, true, true),
            "restored after a kill"
        );
        assert!(
            start_wanted(false, true, false, false),
            "the tunnel needs no history"
        );
        assert!(
            start_wanted(true, false, false, false),
            "nor does an outstanding Task"
        );
        assert!(!start_wanted(false, false, false, true));
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
