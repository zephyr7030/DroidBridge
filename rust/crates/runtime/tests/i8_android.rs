//! I8-ANDROID shared routing, privacy and notification-reference gates.

use contract::{
    AndroidCall, Availability, CapabilityState, ErrorCode, GrantFacts, PackageFact, RuntimeHost,
    RuntimeReadiness, UuidV4,
};
use domain::{
    AdmissionFence, CapabilityContext, DomainError, Preflight, ProviderGenerations, ResolverFacts,
};
use runtime::{
    AdmittedExecution, AndroidExecutionEnvelope, AndroidNotificationActionRecord,
    AndroidNotificationIdentity, AndroidNotificationRecord, AndroidPrimitivePort, CapabilityPort,
    CapabilitySnapshot, CompositeExecutionSurface, ExecutionCancelOutcome, ExecutionFailure,
    ExecutionPayload, ExecutionPort, FilesystemCandidate, FilesystemPreflightPort,
    FrameworkPackageInspection, LocalExecutionClaim, NativeAndroidExecutionSurface, PortFuture,
    PrivilegedPackageRecord, ProviderToken, RecoveryProof, RuntimeCore,
    UnavailableExecutionDelegate,
    fakes::{FakeArtifacts, FakeCapabilities, FakeHostControl, FakePersistence},
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

const TIMESTAMP: &str = "2026-09-13T00:00:00.000Z";
const NOW_MS: u64 = 1_789_257_600_000;
const OWN_PACKAGE: &str = "com.droidbridge.android";

fn uuid(value: u64) -> UuidV4 {
    UuidV4::parse(format!("89000000-0000-4000-8000-{value:012x}")).unwrap()
}

const fn availability(state: CapabilityState) -> Availability {
    Availability {
        state,
        reason: None,
    }
}

#[derive(Clone, Copy)]
struct Facts {
    host: RuntimeHost,
    app_framework: CapabilityState,
    shizuku: CapabilityState,
    listener: CapabilityState,
    magisk_framework: CapabilityState,
    magisk_launch: CapabilityState,
    magisk_clipboard: CapabilityState,
    magisk_notifications: CapabilityState,
    post_notifications: CapabilityState,
}

impl Facts {
    const fn apk() -> Self {
        Self {
            host: RuntimeHost::ApkRuntime,
            app_framework: CapabilityState::Available,
            shizuku: CapabilityState::Available,
            listener: CapabilityState::Available,
            magisk_framework: CapabilityState::Unavailable,
            magisk_launch: CapabilityState::Unavailable,
            magisk_clipboard: CapabilityState::Unavailable,
            magisk_notifications: CapabilityState::Unavailable,
            post_notifications: CapabilityState::Available,
        }
    }

    const fn magisk() -> Self {
        Self {
            host: RuntimeHost::MagiskBackend,
            app_framework: CapabilityState::Available,
            shizuku: CapabilityState::Unavailable,
            listener: CapabilityState::Unavailable,
            magisk_framework: CapabilityState::Available,
            magisk_launch: CapabilityState::Available,
            magisk_clipboard: CapabilityState::Available,
            magisk_notifications: CapabilityState::Available,
            post_notifications: CapabilityState::Available,
        }
    }
}

fn snapshot(facts: Facts) -> CapabilitySnapshot {
    let available = CapabilityState::Available;
    let root = if facts.host == RuntimeHost::MagiskBackend {
        available
    } else {
        CapabilityState::Unavailable
    };
    CapabilitySnapshot {
        grants: GrantFacts {
            android_local_network: availability(available),
            android_notifications: availability(facts.post_notifications),
            android_notification_listener: availability(facts.listener),
            automation_exact_alarm: availability(available),
            visual_accessibility: availability(CapabilityState::Unavailable),
            visual_media_projection_session: availability(CapabilityState::Unavailable),
            shizuku_shell: availability(facts.shizuku),
            magisk_module: availability(root),
            magisk_root: availability(root),
            magisk_framework: availability(facts.magisk_framework),
            magisk_launch: availability(facts.magisk_launch),
            magisk_clipboard: availability(facts.magisk_clipboard),
            magisk_notifications: availability(facts.magisk_notifications),
            magisk_wake_alarm: availability(root),
            execution_app_guard: availability(available),
            execution_shell_guard: availability(facts.shizuku),
            execution_root_guard: availability(root),
        },
        context: CapabilityContext {
            sdk_int: 37,
            host: facts.host,
            readiness: RuntimeReadiness::Ready,
            app_execution_surface: available,
        },
        resolver_facts: ResolverFacts {
            app_native: available,
            app_framework: facts.app_framework,
            shizuku: facts.shizuku,
            magisk_native: root,
            magisk_framework: facts.magisk_framework,
            magisk_launch: facts.magisk_launch,
            magisk_clipboard: facts.magisk_clipboard,
            magisk_notifications: facts.magisk_notifications,
            accessibility: CapabilityState::Unavailable,
            media_projection: CapabilityState::Unavailable,
            notification_listener: facts.listener,
            generations: ProviderGenerations {
                app_native: 4,
                app_framework: 4,
                shizuku: 9,
                magisk_native: 21,
                magisk_framework: 22,
                accessibility: 11,
                media_projection: 12,
                notification_listener: 13,
            },
        },
        fence: AdmissionFence {
            runtime_epoch: uuid(1),
            host_generation: 4,
            runtime_instance_id: uuid(2),
        },
    }
}

#[derive(Clone, Default)]
struct NoFilesystem;

impl FilesystemPreflightPort for NoFilesystem {
    fn preflight(
        &self,
        candidate: FilesystemCandidate,
        _call: &contract::FilesystemCall,
    ) -> Result<Preflight, DomainError> {
        Ok(match candidate {
            FilesystemCandidate::App => Preflight::Positive,
            FilesystemCandidate::Shizuku => Preflight::Unknown,
        })
    }
}

impl ExecutionPort for NoFilesystem {
    fn claim_and_start<'a>(
        &'a self,
        _execution: AdmittedExecution,
    ) -> PortFuture<'a, Result<runtime::ExecutionCompletion, ExecutionFailure>> {
        Box::pin(async {
            Err(ExecutionFailure {
                error: DomainError::new(ErrorCode::Unsupported, "filesystem is not installed"),
                cleanup_verified: true,
            })
        })
    }

    fn cancel<'a>(
        &'a self,
        _execution_id: &'a UuidV4,
    ) -> PortFuture<'a, Result<ExecutionCancelOutcome, DomainError>> {
        Box::pin(async { Ok(ExecutionCancelOutcome::CompletionWon) })
    }
}

#[derive(Default)]
struct PortState {
    calls: Vec<(ProviderToken, &'static str)>,
    inventory: Vec<PrivilegedPackageRecord>,
    framework_visible: Vec<String>,
    clipboard: Option<Result<Option<String>, ErrorCode>>,
    notifications: Vec<AndroidNotificationRecord>,
    dismissed: Vec<AndroidNotificationIdentity>,
    invoked: Vec<(AndroidNotificationIdentity, u8)>,
}

#[derive(Clone, Default)]
struct FixturePort {
    state: Arc<Mutex<PortState>>,
}

impl FixturePort {
    fn record(&self, execution: &AdmittedExecution, name: &'static str) {
        self.state
            .lock()
            .unwrap()
            .calls
            .push((execution.executor.provider, name));
    }

    fn calls(&self) -> Vec<(ProviderToken, &'static str)> {
        self.state.lock().unwrap().calls.clone()
    }

    fn with<T>(&self, change: impl FnOnce(&mut PortState) -> T) -> T {
        change(&mut self.state.lock().unwrap())
    }

    fn current_generation(&self, key: &str) -> Option<u64> {
        self.state
            .lock()
            .unwrap()
            .notifications
            .iter()
            .find(|record| record.key == key)
            .map(|record| record.generation)
    }
}

fn failure(code: ErrorCode) -> ExecutionFailure {
    ExecutionFailure {
        error: DomainError::new(code, "fixture primitive failure"),
        cleanup_verified: true,
    }
}

impl AndroidPrimitivePort for FixturePort {
    fn framework_package_inspect(
        &self,
        execution: &AdmittedExecution,
        package_name: &str,
        _claim: &LocalExecutionClaim,
    ) -> Result<FrameworkPackageInspection, ExecutionFailure> {
        self.record(execution, "framework_inspect");
        let visible = self.with(|state| {
            state
                .framework_visible
                .iter()
                .any(|name| name == package_name)
        });
        Ok(if visible {
            FrameworkPackageInspection::Visible(PackageFact {
                package_name: package_name.to_owned(),
                version_name: Some("1.0".to_owned()),
                version_code: Some(1),
                enabled: Some(true),
                system: None,
                launchable: None,
            })
        } else {
            FrameworkPackageInspection::VisibilityOrAbsent
        })
    }

    fn package_inventory(
        &self,
        execution: &AdmittedExecution,
        include_system: bool,
        _claim: &LocalExecutionClaim,
    ) -> Result<Vec<PrivilegedPackageRecord>, ExecutionFailure> {
        self.record(execution, "inventory");
        Ok(self.with(|state| {
            state
                .inventory
                .iter()
                .filter(|record| include_system || !record.system)
                .cloned()
                .collect()
        }))
    }

    fn force_stop(
        &self,
        execution: &AdmittedExecution,
        _package_name: &str,
        _claim: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure> {
        self.record(execution, "force_stop");
        Ok(())
    }

    fn launch(
        &self,
        execution: &AdmittedExecution,
        _input: &contract::AndroidLaunchInput,
        _claim: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure> {
        self.record(execution, "launch");
        Ok(())
    }

    fn start_intent(
        &self,
        execution: &AdmittedExecution,
        _input: &contract::AndroidIntentInput,
        _claim: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure> {
        self.record(execution, "intent");
        Ok(())
    }

    fn clipboard_read(
        &self,
        execution: &AdmittedExecution,
        _claim: &LocalExecutionClaim,
    ) -> Result<Option<String>, ExecutionFailure> {
        self.record(execution, "clipboard_read");
        self.with(|state| state.clipboard.clone())
            .unwrap_or(Ok(None))
            .map_err(failure)
    }

    fn clipboard_write(
        &self,
        execution: &AdmittedExecution,
        _text: &str,
        _claim: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure> {
        self.record(execution, "clipboard_write");
        match self.with(|state| state.clipboard.clone()) {
            Some(Err(code)) => Err(failure(code)),
            _ => Ok(()),
        }
    }

    fn clipboard_clear(
        &self,
        execution: &AdmittedExecution,
        _claim: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure> {
        self.record(execution, "clipboard_clear");
        Ok(())
    }

    fn notification_snapshot(
        &self,
        execution: &AdmittedExecution,
        _claim: &LocalExecutionClaim,
    ) -> Result<Vec<AndroidNotificationRecord>, ExecutionFailure> {
        self.record(execution, "notification_snapshot");
        Ok(self.with(|state| state.notifications.clone()))
    }

    fn notification_dismiss(
        &self,
        execution: &AdmittedExecution,
        identity: &AndroidNotificationIdentity,
        _claim: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure> {
        self.record(execution, "notification_dismiss");
        if self.current_generation(&identity.key) != Some(identity.generation) {
            return Err(failure(ErrorCode::StaleReference));
        }
        self.with(|state| state.dismissed.push(identity.clone()));
        Ok(())
    }

    fn notification_invoke(
        &self,
        execution: &AdmittedExecution,
        identity: &AndroidNotificationIdentity,
        action_index: u8,
        _claim: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure> {
        self.record(execution, "notification_invoke");
        if self.current_generation(&identity.key) != Some(identity.generation) {
            return Err(failure(ErrorCode::StaleReference));
        }
        self.with(|state| state.invoked.push((identity.clone(), action_index)));
        Ok(())
    }
}

type TestSurface = CompositeExecutionSurface<
    NoFilesystem,
    UnavailableExecutionDelegate,
    UnavailableExecutionDelegate,
    UnavailableExecutionDelegate,
    NativeAndroidExecutionSurface<FakeCapabilities, FixturePort>,
>;
type TestCore =
    RuntimeCore<FakePersistence, FakeArtifacts, TestSurface, FakeCapabilities, FakeHostControl>;

fn fixture_core(facts: Facts) -> (TestCore, FixturePort, FakeCapabilities) {
    let capabilities = FakeCapabilities::new(snapshot(facts));
    let port = FixturePort::default();
    let android = NativeAndroidExecutionSurface::new(capabilities.clone(), OWN_PACKAGE)
        .with_primitives(port.clone());
    let core = RuntimeCore::new(
        FakePersistence::default(),
        FakeArtifacts::default(),
        CompositeExecutionSurface::new(NoFilesystem).with_android(android),
        capabilities.clone(),
        FakeHostControl::new(RecoveryProof::Clean).with_capabilities(capabilities.clone()),
    );
    (core, port, capabilities)
}

async fn submit(core: &TestCore, id: u64, action: &str, input: Value, now_ms: u64) -> Value {
    let request = json!({
        "protocol_version": 1,
        "request_id": format!("10000000-0000-4000-8000-{id:012x}"),
        "payload": {"tool": "android", "action": action, "input": input},
    });
    serde_json::from_slice(
        &runtime::submit_public(
            core,
            &serde_json::to_vec(&request).unwrap(),
            TIMESTAMP.to_owned(),
            now_ms,
            true,
            |_| async { panic!("android escaped canonical ingress") },
        )
        .await,
    )
    .unwrap()
}

fn error_code(response: &Value) -> &str {
    assert_eq!(response["outcome"], "error", "{response}");
    response["error"]["code"].as_str().unwrap()
}

fn success(response: &Value) -> &Value {
    assert_eq!(response["outcome"], "success", "{response}");
    &response["result"]
}

fn package(name: &str, version_code: u64, system: bool) -> PrivilegedPackageRecord {
    PrivilegedPackageRecord {
        package_name: name.to_owned(),
        version_code,
        system,
    }
}

fn notification(key: &str, generation: u64, posted_at_ms: u64) -> AndroidNotificationRecord {
    AndroidNotificationRecord {
        key: key.to_owned(),
        generation,
        package_name: "com.example.chat".to_owned(),
        posted_at_ms: Some(posted_at_ms),
        title: Some("Alice".to_owned()),
        text: Some("See you at noon".to_owned()),
        action_count: 2,
        actions: vec![
            AndroidNotificationActionRecord {
                title: Some("Reply".to_owned()),
                requires_remote_input: true,
            },
            AndroidNotificationActionRecord {
                title: Some("Mark read".to_owned()),
                requires_remote_input: false,
            },
        ],
    }
}

#[tokio::test]
async fn i8_android_g01_package_list_pages_the_complete_privileged_inventory() {
    let unavailable = Facts {
        shizuku: CapabilityState::Unavailable,
        ..Facts::apk()
    };
    let (core, port, _) = fixture_core(unavailable);
    port.with(|state| {
        state
            .framework_visible
            .push("com.example.visible".to_owned())
    });
    let response = submit(&core, 1, "package", json!({"operation": "list"}), NOW_MS).await;
    assert_eq!(error_code(&response), "CAPABILITY_UNAVAILABLE");
    assert!(
        port.calls().is_empty(),
        "no visibility-filtered framework subset"
    );

    let (core, port, _) = fixture_core(Facts::apk());
    port.with(|state| {
        state.inventory = vec![
            package("com.example.c", 3, false),
            package("android", 37, true),
            package("com.example.a", 1, false),
            package("com.example.b", 2, false),
        ];
    });
    let first = submit(
        &core,
        2,
        "package",
        json!({"operation": "list", "limit": 2}),
        NOW_MS,
    )
    .await;
    let result = success(&first);
    assert_eq!(
        result,
        &json!({
            "operation": "list",
            "packages": [
                {"package_name": "com.example.a", "version_code": 1, "system": false},
                {"package_name": "com.example.b", "version_code": 2, "system": false},
            ],
            "truncated": true,
            "next_after_package": "com.example.b",
        })
    );
    let second = submit(
        &core,
        3,
        "package",
        json!({"operation": "list", "limit": 2, "after_package": "com.example.b"}),
        NOW_MS,
    )
    .await;
    assert_eq!(
        success(&second),
        &json!({
            "operation": "list",
            "packages": [{"package_name": "com.example.c", "version_code": 3, "system": false}],
            "truncated": false,
        })
    );
    let system = submit(
        &core,
        4,
        "package",
        json!({"operation": "list", "include_system": true}),
        NOW_MS,
    )
    .await;
    assert_eq!(success(&system)["packages"][0]["package_name"], "android");
    assert_eq!(success(&system)["packages"][0]["system"], true);
    assert!(
        port.calls()
            .iter()
            .all(|call| *call == (ProviderToken::Shizuku, "inventory"))
    );
}

#[tokio::test]
async fn i8_android_g02_force_stop_uses_only_privileged_execution() {
    let unavailable = Facts {
        shizuku: CapabilityState::Unavailable,
        ..Facts::apk()
    };
    let (core, port, _) = fixture_core(unavailable);
    let denied = submit(
        &core,
        1,
        "package",
        json!({"operation": "force_stop", "package_name": "com.example.a"}),
        NOW_MS,
    )
    .await;
    assert_eq!(error_code(&denied), "CAPABILITY_UNAVAILABLE");
    assert!(port.calls().is_empty());

    let (core, port, _) = fixture_core(Facts::apk());
    let own = submit(
        &core,
        2,
        "package",
        json!({"operation": "force_stop", "package_name": OWN_PACKAGE}),
        NOW_MS,
    )
    .await;
    assert_eq!(error_code(&own), "INVALID_ARGUMENT");
    assert!(port.calls().is_empty());
    let stopped = submit(
        &core,
        3,
        "package",
        json!({"operation": "force_stop", "package_name": "com.example.a"}),
        NOW_MS,
    )
    .await;
    assert_eq!(
        success(&stopped),
        &json!({"operation": "force_stop", "package_name": "com.example.a", "completed": true})
    );
    assert_eq!(port.calls(), vec![(ProviderToken::Shizuku, "force_stop")]);

    let (core, port, _) = fixture_core(Facts::magisk());
    let rooted = submit(
        &core,
        4,
        "package",
        json!({"operation": "force_stop", "package_name": "com.example.a"}),
        NOW_MS,
    )
    .await;
    success(&rooted);
    assert_eq!(
        port.calls(),
        vec![(ProviderToken::MagiskNative, "force_stop")]
    );
}

#[tokio::test]
async fn i8_android_g03_package_launch_works_without_broad_package_visibility() {
    let facts = Facts {
        shizuku: CapabilityState::Unavailable,
        ..Facts::apk()
    };
    let (core, port, _) = fixture_core(facts);
    let launched = submit(
        &core,
        1,
        "launch",
        json!({"operation": "package", "package_name": "com.example.hidden"}),
        NOW_MS,
    )
    .await;
    assert_eq!(
        success(&launched),
        &json!({"launched": true, "package_name": "com.example.hidden"})
    );
    assert_eq!(port.calls(), vec![(ProviderToken::AppFramework, "launch")]);

    let hidden = submit(
        &core,
        2,
        "package",
        json!({"operation": "inspect", "package_name": "com.example.hidden"}),
        NOW_MS,
    )
    .await;
    assert_eq!(
        error_code(&hidden),
        "CAPABILITY_UNAVAILABLE",
        "hidden-or-absent is never reported as NOT_FOUND by the framework"
    );

    let (core, port, _) = fixture_core(Facts::apk());
    port.with(|state| state.inventory = vec![package("com.example.hidden", 7, false)]);
    let privileged = submit(
        &core,
        3,
        "package",
        json!({"operation": "inspect", "package_name": "com.example.hidden"}),
        NOW_MS,
    )
    .await;
    assert_eq!(
        success(&privileged),
        &json!({"operation": "inspect", "package": {
            "package_name": "com.example.hidden", "version_code": 7, "system": false
        }})
    );
    let absent = submit(
        &core,
        4,
        "package",
        json!({"operation": "inspect", "package_name": "com.example.absent"}),
        NOW_MS,
    )
    .await;
    assert_eq!(error_code(&absent), "NOT_FOUND");
    assert_eq!(
        port.calls(),
        vec![
            (ProviderToken::AppFramework, "framework_inspect"),
            (ProviderToken::Shizuku, "inventory"),
            (ProviderToken::AppFramework, "framework_inspect"),
            (ProviderToken::Shizuku, "inventory"),
        ]
    );
    let component = submit(
        &core,
        5,
        "launch",
        json!({"operation": "component", "package_name": "com.example.hidden", "class_name": "com.example.Main"}),
        NOW_MS,
    )
    .await;
    assert_eq!(
        success(&component)["component"],
        json!({"package_name": "com.example.hidden", "class_name": "com.example.Main"})
    );
}

#[tokio::test]
async fn i8_android_g04_app_clipboard_collapses_observations_but_keeps_thrown_failures() {
    let (core, port, _) = fixture_core(Facts::apk());
    let mut id = 0;
    for observation in [None, Some(String::new())] {
        port.with(|state| state.clipboard = Some(Ok(observation.clone())));
        id += 1;
        let response = submit(&core, id, "clipboard", json!({"operation": "read"}), NOW_MS).await;
        assert_eq!(
            success(&response),
            &json!({"operation": "read", "has_text": false})
        );
    }
    port.with(|state| state.clipboard = Some(Ok(Some("copied".to_owned()))));
    let text = submit(&core, 3, "clipboard", json!({"operation": "read"}), NOW_MS).await;
    assert_eq!(
        success(&text),
        &json!({"operation": "read", "has_text": true, "text": "copied"})
    );
    port.with(|state| state.clipboard = Some(Err(ErrorCode::PermissionDenied)));
    let thrown = submit(&core, 4, "clipboard", json!({"operation": "read"}), NOW_MS).await;
    assert_eq!(error_code(&thrown), "PERMISSION_DENIED");
    assert!(
        port.calls()
            .iter()
            .all(|call| *call == (ProviderToken::AppFramework, "clipboard_read"))
    );
}

#[tokio::test]
async fn i8_android_g05_magisk_clipboard_serves_the_same_action_without_app_eligibility() {
    let (core, port, capabilities) = fixture_core(Facts::magisk());
    port.with(|state| state.clipboard = Some(Ok(Some("rooted".to_owned()))));
    let read = submit(&core, 1, "clipboard", json!({"operation": "read"}), NOW_MS).await;
    assert_eq!(
        success(&read),
        &json!({"operation": "read", "has_text": true, "text": "rooted"})
    );
    let write = submit(
        &core,
        2,
        "clipboard",
        json!({"operation": "write", "text": "x"}),
        NOW_MS,
    )
    .await;
    assert_eq!(
        success(&write),
        &json!({"operation": "write", "written": true})
    );
    assert_eq!(
        port.calls(),
        vec![
            (ProviderToken::MagiskFramework, "clipboard_read"),
            (ProviderToken::MagiskFramework, "clipboard_write"),
        ]
    );
    let current = capabilities.current().unwrap();
    assert_eq!(
        current.grants.shizuku_shell.state,
        CapabilityState::Unavailable
    );
    assert_eq!(
        current.grants.android_notification_listener.state,
        CapabilityState::Unavailable
    );

    let (core, port, _) = fixture_core(Facts {
        magisk_clipboard: CapabilityState::Unavailable,
        ..Facts::magisk()
    });
    let fallback = submit(&core, 3, "clipboard", json!({"operation": "clear"}), NOW_MS).await;
    assert_eq!(
        success(&fallback),
        &json!({"operation": "clear", "cleared": true})
    );
    assert_eq!(
        port.calls(),
        vec![(ProviderToken::AppFramework, "clipboard_clear")]
    );
}

#[tokio::test]
async fn i8_android_g06_notification_access_is_independent_of_post_notifications() {
    let (core, port, _) = fixture_core(Facts {
        post_notifications: CapabilityState::Unavailable,
        ..Facts::apk()
    });
    port.with(|state| state.notifications = vec![notification("0|chat|1", 1, NOW_MS - 1_000)]);
    let listed = submit(
        &core,
        1,
        "notification",
        json!({"operation": "list"}),
        NOW_MS,
    )
    .await;
    assert_eq!(success(&listed)["notifications"][0]["action_count"], 2);
    assert_eq!(
        port.calls(),
        vec![(ProviderToken::NotificationListener, "notification_snapshot")]
    );

    let (core, port, _) = fixture_core(Facts {
        listener: CapabilityState::Unavailable,
        ..Facts::apk()
    });
    let missing = submit(
        &core,
        2,
        "notification",
        json!({"operation": "list"}),
        NOW_MS,
    )
    .await;
    assert_eq!(error_code(&missing), "CAPABILITY_UNAVAILABLE");
    assert!(port.calls().is_empty());

    let (core, port, capabilities) = fixture_core(Facts {
        post_notifications: CapabilityState::Unavailable,
        ..Facts::magisk()
    });
    port.with(|state| state.notifications = vec![notification("0|chat|1", 1, NOW_MS - 1_000)]);
    let rooted = submit(
        &core,
        3,
        "notification",
        json!({"operation": "list"}),
        NOW_MS,
    )
    .await;
    success(&rooted);
    assert_eq!(
        port.calls(),
        vec![(ProviderToken::MagiskFramework, "notification_snapshot")]
    );
    assert_eq!(
        capabilities
            .current()
            .unwrap()
            .grants
            .android_notification_listener
            .state,
        CapabilityState::Unavailable,
        "Magisk notifications never fabricate the listener grant"
    );
}

#[tokio::test]
async fn i8_android_g07_stale_refs_and_actions_never_retarget() {
    let (core, port, _) = fixture_core(Facts::apk());
    port.with(|state| state.notifications = vec![notification("0|chat|1", 1, NOW_MS - 1_000)]);
    let first = submit(
        &core,
        1,
        "notification",
        json!({"operation": "list"}),
        NOW_MS,
    )
    .await;
    let summary = success(&first)["notifications"][0].clone();
    let old_ref = summary["notification_ref"].as_str().unwrap().to_owned();
    assert_eq!(summary["expires_at"], "2026-09-13T00:05:00.000Z");
    assert_eq!(summary["posted_at"], "2026-09-12T23:59:59.000Z");
    let again = submit(
        &core,
        2,
        "notification",
        json!({"operation": "list"}),
        NOW_MS,
    )
    .await;
    assert_eq!(
        success(&again)["notifications"][0]["notification_ref"],
        old_ref.as_str(),
        "the same key+generation reuses its live ref"
    );
    let get = submit(
        &core,
        3,
        "notification",
        json!({"operation": "get", "notification_ref": old_ref}),
        NOW_MS,
    )
    .await;
    assert_eq!(
        success(&get)["actions"],
        json!([
            {"index": 0, "title": "Reply", "requires_remote_input": true},
            {"index": 1, "title": "Mark read", "requires_remote_input": false},
        ])
    );
    assert_eq!(success(&get)["actions_truncated"], false);

    port.with(|state| state.notifications = vec![notification("0|chat|1", 2, NOW_MS)]);
    let replaced = submit(
        &core,
        4,
        "notification",
        json!({"operation": "get", "notification_ref": old_ref}),
        NOW_MS,
    )
    .await;
    assert_eq!(error_code(&replaced), "STALE_REFERENCE");
    let invoke_old = submit(
        &core,
        5,
        "notification",
        json!({"operation": "invoke_action", "notification_ref": old_ref, "action_index": 1}),
        NOW_MS,
    )
    .await;
    assert_eq!(error_code(&invoke_old), "STALE_REFERENCE");
    let dismiss_old = submit(
        &core,
        6,
        "notification",
        json!({"operation": "dismiss", "notification_ref": old_ref}),
        NOW_MS,
    )
    .await;
    assert_eq!(error_code(&dismiss_old), "STALE_REFERENCE");
    assert!(port.with(|state| state.invoked.is_empty() && state.dismissed.is_empty()));

    let fresh = submit(
        &core,
        7,
        "notification",
        json!({"operation": "list"}),
        NOW_MS,
    )
    .await;
    let new_ref = success(&fresh)["notifications"][0]["notification_ref"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(new_ref, old_ref);
    let invoked = submit(
        &core,
        8,
        "notification",
        json!({"operation": "invoke_action", "notification_ref": new_ref, "action_index": 1}),
        NOW_MS,
    )
    .await;
    assert_eq!(
        success(&invoked),
        &json!({"operation": "invoke_action", "notification_ref": new_ref, "action_index": 1, "invoked": true})
    );
    assert_eq!(
        port.with(|state| state.invoked.clone()),
        vec![(
            AndroidNotificationIdentity {
                key: "0|chat|1".to_owned(),
                generation: 2,
            },
            1,
        )]
    );

    let unknown = submit(
        &core,
        9,
        "notification",
        json!({"operation": "dismiss", "notification_ref": "unknown"}),
        NOW_MS,
    )
    .await;
    assert_eq!(error_code(&unknown), "NOT_FOUND");
    let expired = submit(
        &core,
        10,
        "notification",
        json!({"operation": "get", "notification_ref": new_ref}),
        NOW_MS + 300_000,
    )
    .await;
    assert_eq!(error_code(&expired), "NOT_FOUND");
}

#[tokio::test]
async fn i8_android_g08_sensitive_android_payloads_stay_out_of_logs_and_diagnostics() {
    const SECRET: &str = "s3cr3t-android-payload";
    let (core, port, _) = fixture_core(Facts::apk());
    port.with(|state| state.clipboard = Some(Err(ErrorCode::PermissionDenied)));
    let failed = submit(
        &core,
        1,
        "clipboard",
        json!({"operation": "write", "text": SECRET}),
        NOW_MS,
    )
    .await;
    assert_eq!(error_code(&failed), "PERMISSION_DENIED");
    assert!(!failed.to_string().contains(SECRET));

    for call in [
        json!({"action": "clipboard", "input": {"operation": "write", "text": SECRET}}),
        json!({"action": "intent", "input": {
            "operation": "explicit_activity",
            "package_name": "com.example.a",
            "class_name": "com.example.Main",
            "extras": {"token": SECRET},
        }}),
    ] {
        let payload = ExecutionPayload::AndroidCall(AndroidExecutionEnvelope {
            call: serde_json::from_value::<AndroidCall>(call).unwrap(),
            admitted_at: TIMESTAMP.to_owned(),
            admitted_at_ms: NOW_MS,
            privileged_inspect: None,
        });
        assert!(!format!("{payload:?}").contains(SECRET));
    }
    let mut record = notification("0|chat|1", 1, NOW_MS);
    record.title = Some(SECRET.to_owned());
    record.text = Some(SECRET.to_owned());
    record.actions[0].title = Some(SECRET.to_owned());
    assert!(!format!("{record:?}").contains(SECRET));
}

#[tokio::test]
async fn i8_android_g09_each_magisk_family_routes_independently_of_a_failed_sibling() {
    let (core, port, _) = fixture_core(Facts {
        magisk_launch: CapabilityState::Unavailable,
        ..Facts::magisk()
    });
    port.with(|state| state.notifications = vec![notification("0|chat|1", 1, NOW_MS)]);
    success(&submit(&core, 1, "clipboard", json!({"operation": "read"}), NOW_MS).await);
    success(
        &submit(
            &core,
            2,
            "notification",
            json!({"operation": "list"}),
            NOW_MS,
        )
        .await,
    );
    success(
        &submit(
            &core,
            3,
            "intent",
            json!({"operation": "view", "data_uri": "https://example.com"}),
            NOW_MS,
        )
        .await,
    );
    assert_eq!(
        port.calls(),
        vec![
            (ProviderToken::MagiskFramework, "clipboard_read"),
            (ProviderToken::MagiskFramework, "notification_snapshot"),
            (ProviderToken::AppFramework, "intent"),
        ]
    );

    let (core, port, _) = fixture_core(Facts {
        magisk_notifications: CapabilityState::Unavailable,
        ..Facts::magisk()
    });
    success(
        &submit(
            &core,
            4,
            "launch",
            json!({"operation": "package", "package_name": "com.example.a"}),
            NOW_MS,
        )
        .await,
    );
    let notifications = submit(
        &core,
        5,
        "notification",
        json!({"operation": "list"}),
        NOW_MS,
    )
    .await;
    assert_eq!(error_code(&notifications), "CAPABILITY_UNAVAILABLE");
    assert_eq!(
        port.calls(),
        vec![(ProviderToken::MagiskFramework, "launch")]
    );
}
