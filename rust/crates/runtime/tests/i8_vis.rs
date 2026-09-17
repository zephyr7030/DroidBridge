//! I8-VIS shared semantic and reference gates.

use contract::{
    Availability, CapabilityState, DisplayGeometry, ErrorCode, ForegroundFact, GrantFacts,
    ImageFormat, NodeBounds, RuntimeHost, RuntimeReadiness, UuidV4, VisualNode,
};
use domain::{
    AdmissionFence, CapabilityContext, DomainError, Preflight, ProviderGenerations, ResolverFacts,
};
use runtime::{
    AdmittedExecution, ArtifactPort, CompositeExecutionSurface, ExecutionCancelOutcome,
    ExecutionFailure, ExecutionPort, FilesystemCandidate, FilesystemPreflightPort,
    LocalExecutionClaim, NativeVisualExecutionSurface, PortFuture, RecoveryProof, RuntimeCore,
    VisualDisplaySnapshot, VisualEncodedImage, VisualHierarchySnapshot, VisualInteractionRequest,
    VisualPrimitivePort, VisualSceneProof, VisualTransformSource,
    fakes::{FakeArtifacts, FakeCapabilities, FakeHostControl, FakePersistence},
    parse_privileged_hierarchy,
};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};

const TIMESTAMP: &str = "2026-09-13T00:00:00.000Z";
const NOW_MS: u64 = 1_789_257_600_000;

fn uuid(value: u64) -> UuidV4 {
    UuidV4::parse(format!("88000000-0000-4000-8000-{value:012x}")).unwrap()
}

const fn availability(state: CapabilityState) -> Availability {
    Availability {
        state,
        reason: None,
    }
}

fn capability(shizuku: CapabilityState, accessibility: CapabilityState) -> CapabilitySnapshot {
    let available = CapabilityState::Available;
    CapabilitySnapshot {
        grants: GrantFacts {
            android_local_network: availability(available),
            android_notifications: availability(available),
            android_notification_listener: availability(available),
            automation_exact_alarm: availability(available),
            visual_accessibility: availability(accessibility),
            visual_media_projection_session: availability(available),
            shizuku_shell: availability(shizuku),
            magisk_module: availability(CapabilityState::Unavailable),
            magisk_root: availability(CapabilityState::Unavailable),
            magisk_framework: availability(CapabilityState::Unavailable),
            magisk_launch: availability(CapabilityState::Unavailable),
            magisk_clipboard: availability(CapabilityState::Unavailable),
            magisk_notifications: availability(CapabilityState::Unavailable),
            magisk_wake_alarm: availability(CapabilityState::Unavailable),
            execution_app_guard: availability(available),
            execution_shell_guard: availability(available),
            execution_root_guard: availability(CapabilityState::Unavailable),
        },
        context: CapabilityContext {
            sdk_int: 37,
            host: RuntimeHost::ApkRuntime,
            readiness: RuntimeReadiness::Ready,
            app_execution_surface: available,
        },
        resolver_facts: ResolverFacts {
            app_native: available,
            app_framework: available,
            shizuku,
            magisk_native: CapabilityState::Unavailable,
            magisk_framework: CapabilityState::Unavailable,
            magisk_launch: CapabilityState::Unavailable,
            magisk_clipboard: CapabilityState::Unavailable,
            magisk_notifications: CapabilityState::Unavailable,
            accessibility,
            media_projection: available,
            notification_listener: available,
            generations: ProviderGenerations {
                app_native: 4,
                app_framework: 4,
                shizuku: 9,
                magisk_native: 0,
                magisk_framework: 0,
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

use runtime::CapabilitySnapshot;

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

#[derive(Clone)]
struct FixtureVisualPort {
    state: Arc<Mutex<PortState>>,
}

struct PortState {
    display_generation: u64,
    part_display_generation: u64,
    providers: Vec<runtime::ProviderToken>,
    interactions: Vec<VisualInteractionRequest>,
    transform_sources: Vec<&'static str>,
    stale_interaction: bool,
    stale_node_interaction: bool,
    fatal_hierarchy: bool,
}

impl Default for FixtureVisualPort {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(PortState {
                display_generation: 7,
                part_display_generation: 7,
                providers: Vec::new(),
                interactions: Vec::new(),
                transform_sources: Vec::new(),
                stale_interaction: false,
                stale_node_interaction: false,
                fatal_hierarchy: false,
            })),
        }
    }
}

impl FixtureVisualPort {
    fn providers(&self) -> Vec<runtime::ProviderToken> {
        self.state.lock().unwrap().providers.clone()
    }

    fn interactions(&self) -> Vec<VisualInteractionRequest> {
        self.state.lock().unwrap().interactions.clone()
    }

    fn change_part_display(&self) {
        self.state.lock().unwrap().part_display_generation = 8;
    }

    fn fail_scene_proof(&self) {
        self.state.lock().unwrap().stale_interaction = true;
    }

    /// The platform's scene check rejects a node target whose scene moved; a coordinate carries no scene.
    fn fail_node_scene_proof(&self) {
        self.state.lock().unwrap().stale_node_interaction = true;
    }

    fn fail_hierarchy_without_cleanup_proof(&self) {
        self.state.lock().unwrap().fatal_hierarchy = true;
    }
}

fn display(generation: u64) -> VisualDisplaySnapshot {
    VisualDisplaySnapshot {
        display: DisplayGeometry {
            width: 1080,
            height: 2400,
            rotation: 0,
            density_dpi: Some(420),
        },
        display_generation: generation,
    }
}

fn png() -> Vec<u8> {
    b"\x89PNG\r\n\x1a\nfixture".to_vec()
}

fn node(node_ref: Option<String>) -> VisualNode {
    VisualNode {
        node_ref,
        text: Some("Settings".to_owned()),
        content_description: None,
        resource_id: Some("android:id/title".to_owned()),
        class_name: Some("android.widget.TextView".to_owned()),
        package_name: Some("com.android.settings".to_owned()),
        bounds: NodeBounds {
            left: 10,
            top: 20,
            right: 300,
            bottom: 120,
        },
        checkable: Some(false),
        checked: Some(false),
        clickable: Some(true),
        enabled: Some(true),
        focusable: Some(true),
        focused: Some(false),
        scrollable: Some(false),
        long_clickable: Some(false),
        password: Some(false),
        selected: Some(false),
        editable: None,
    }
}

impl VisualPrimitivePort for FixtureVisualPort {
    fn display(
        &self,
        execution: &AdmittedExecution,
        _claim: &LocalExecutionClaim,
    ) -> Result<VisualDisplaySnapshot, ExecutionFailure> {
        let mut state = self.state.lock().unwrap();
        state.providers.push(execution.executor.provider);
        Ok(display(state.display_generation))
    }

    fn capture_image(
        &self,
        execution: &AdmittedExecution,
        _admitted: &VisualDisplaySnapshot,
        _claim: &LocalExecutionClaim,
    ) -> Result<VisualEncodedImage, ExecutionFailure> {
        let mut state = self.state.lock().unwrap();
        state.providers.push(execution.executor.provider);
        Ok(VisualEncodedImage {
            bytes: png(),
            format: ImageFormat::Png,
            width: 1080,
            height: 2400,
            captured_display: Some(display(state.part_display_generation)),
        })
    }

    fn observe_hierarchy(
        &self,
        execution: &AdmittedExecution,
        _admitted: &VisualDisplaySnapshot,
        observation_id: &UuidV4,
        _max_nodes: u32,
        _claim: &LocalExecutionClaim,
    ) -> Result<VisualHierarchySnapshot, ExecutionFailure> {
        let mut state = self.state.lock().unwrap();
        state.providers.push(execution.executor.provider);
        if state.fatal_hierarchy {
            return Err(ExecutionFailure {
                error: DomainError::new(ErrorCode::IoError, "hierarchy cleanup was not verified"),
                cleanup_verified: false,
            });
        }
        let accessibility = execution.executor.provider == runtime::ProviderToken::Accessibility;
        Ok(VisualHierarchySnapshot {
            display: display(state.part_display_generation),
            foreground: Some(ForegroundFact {
                package: Some("com.android.settings".to_owned()),
                activity: None,
            }),
            nodes: vec![node(
                accessibility.then(|| format!("dbnode:{}:1", observation_id.as_str())),
            )],
            truncated: false,
            proof: if accessibility {
                VisualSceneProof::Accessibility {
                    component_generation: 3,
                    window_id: 9,
                    scene_revision: 4,
                    hierarchy_sha256: "a".repeat(64),
                }
            } else {
                VisualSceneProof::Privileged {
                    hierarchy_sha256: "b".repeat(64),
                }
            },
        })
    }

    fn transform(
        &self,
        _execution: &AdmittedExecution,
        source: VisualTransformSource,
        region: Option<contract::Region>,
        _claim: &LocalExecutionClaim,
    ) -> Result<VisualEncodedImage, ExecutionFailure> {
        let mut state = self.state.lock().unwrap();
        state.transform_sources.push(match source {
            VisualTransformSource::ImmutableArtifact(_) => "artifact",
            VisualTransformSource::Path { .. } => "path",
            VisualTransformSource::ContentUri(_) => "content_uri",
        });
        Ok(VisualEncodedImage {
            bytes: png(),
            format: ImageFormat::Png,
            width: region.as_ref().map_or(1080, |value| value.width),
            height: region.as_ref().map_or(2400, |value| value.height),
            captured_display: None,
        })
    }

    fn interact(
        &self,
        _execution: &AdmittedExecution,
        request: VisualInteractionRequest,
        _claim: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure> {
        let mut state = self.state.lock().unwrap();
        if state.stale_interaction
            || (state.stale_node_interaction
                && matches!(request, VisualInteractionRequest::Node { .. }))
        {
            return Err(ExecutionFailure {
                error: DomainError::new(ErrorCode::StaleReference, "scene proof changed"),
                cleanup_verified: true,
            });
        }
        state.interactions.push(request);
        Ok(())
    }
}

type VisualSurface =
    NativeVisualExecutionSurface<FakeArtifacts, FakeCapabilities, FixtureVisualPort>;
type TestSurface = CompositeExecutionSurface<
    NoFilesystem,
    runtime::UnavailableExecutionDelegate,
    runtime::UnavailableExecutionDelegate,
    VisualSurface,
>;
type TestCore =
    RuntimeCore<FakePersistence, FakeArtifacts, TestSurface, FakeCapabilities, FakeHostControl>;

fn core(shizuku: CapabilityState, accessibility: CapabilityState) -> (TestCore, FixtureVisualPort) {
    let artifacts = FakeArtifacts::default();
    let capabilities = FakeCapabilities::new(capability(shizuku, accessibility));
    let primitive = FixtureVisualPort::default();
    let visual = NativeVisualExecutionSurface::new(artifacts.clone(), capabilities.clone())
        .with_primitives(primitive.clone());
    let core = RuntimeCore::new(
        FakePersistence::default(),
        artifacts,
        CompositeExecutionSurface::new(NoFilesystem).with_visual(visual),
        capabilities.clone(),
        FakeHostControl::new(RecoveryProof::Clean).with_capabilities(capabilities),
    );
    (core, primitive)
}

fn core_with_artifacts(
    shizuku: CapabilityState,
    accessibility: CapabilityState,
    artifacts: FakeArtifacts,
) -> (TestCore, FixtureVisualPort) {
    let capabilities = FakeCapabilities::new(capability(shizuku, accessibility));
    let primitive = FixtureVisualPort::default();
    let visual = NativeVisualExecutionSurface::new(artifacts.clone(), capabilities.clone())
        .with_primitives(primitive.clone());
    let core = RuntimeCore::new(
        FakePersistence::default(),
        artifacts,
        CompositeExecutionSurface::new(NoFilesystem).with_visual(visual),
        capabilities.clone(),
        FakeHostControl::new(RecoveryProof::Clean).with_capabilities(capabilities),
    );
    (core, primitive)
}

async fn submit(
    core: &TestCore,
    id: u64,
    input: serde_json::Value,
    now_ms: u64,
) -> serde_json::Value {
    let request = serde_json::json!({
        "protocol_version": 1,
        "request_id": format!("10000000-0000-4000-8000-{id:012x}"),
        "payload": input,
    });
    serde_json::from_slice(
        &runtime::submit_public(
            core,
            &serde_json::to_vec(&request).unwrap(),
            TIMESTAMP.to_owned(),
            now_ms,
            true,
            |_| async { panic!("visual escaped canonical ingress") },
        )
        .await,
    )
    .unwrap()
}

fn observe_payload() -> serde_json::Value {
    serde_json::json!({
        "tool": "visual",
        "action": "observe",
        "input": {"include_image": true, "include_nodes": true, "max_nodes": 500}
    })
}

#[tokio::test]
async fn i8_vis_g01_observe_freezes_display_and_each_selected_source_before_side_effects() {
    let (core, primitive) = core(CapabilityState::Available, CapabilityState::Available);
    let response = submit(&core, 1, observe_payload(), NOW_MS).await;
    assert_eq!(response["outcome"], "success", "{response}");
    assert_eq!(response["result"]["display"]["width"], 1080);
    assert_eq!(response["result"]["image_format"], "png");
    assert_eq!(response["result"]["nodes"][0].get("node_ref"), None);
    assert_eq!(
        primitive.providers(),
        vec![
            runtime::ProviderToken::AppFramework,
            runtime::ProviderToken::Shizuku,
            runtime::ProviderToken::Shizuku,
        ],
    );
}

#[tokio::test]
async fn i8_vis_g06_changed_display_discards_parts_without_recapture_or_mixed_publication() {
    let (core, primitive) = core(CapabilityState::Available, CapabilityState::Available);
    primitive.change_part_display();
    let response = submit(&core, 2, observe_payload(), NOW_MS).await;
    assert_eq!(response["outcome"], "success", "{response}");
    assert_eq!(
        response["result"]["image_unavailable_reason"],
        "STALE_AUTHORITY"
    );
    assert_eq!(
        response["result"]["nodes_unavailable_reason"],
        "STALE_AUTHORITY"
    );
    assert!(response["result"].get("image_ref").is_none());
    assert!(response["result"].get("nodes").is_none());
    assert_eq!(primitive.providers().len(), 3);
}

#[tokio::test]
async fn i8_vis_g08_privileged_xml_reports_nodes_without_identity_and_coordinates_need_no_proof() {
    let (core, primitive) = core(CapabilityState::Available, CapabilityState::Available);
    let observed = submit(&core, 3, observe_payload(), NOW_MS).await;
    let observation_id = observed["result"]["observation_id"].as_str().unwrap();
    assert!(observed["result"]["nodes"][0].get("node_ref").is_none());
    let interacted = submit(
        &core,
        4,
        serde_json::json!({
            "tool": "visual",
            "action": "interact",
            "input": {"operation":"tap","target":"coordinate","observation_id":observation_id,"x":10,"y":20}
        }),
        NOW_MS + 1,
    )
    .await;
    assert_eq!(interacted["outcome"], "success", "{interacted}");
    assert_eq!(interacted["result"]["target"], "coordinate");
    assert_eq!(primitive.interactions().len(), 1);
}

#[tokio::test]
async fn i8_vis_g07_accessibility_node_ref_remains_bound_to_its_live_provider_and_scene() {
    let (core, primitive) = core(CapabilityState::Unavailable, CapabilityState::Available);
    let observed = submit(&core, 5, observe_payload(), NOW_MS).await;
    let node_ref = observed["result"]["nodes"][0]["node_ref"].as_str().unwrap();
    let interacted = submit(
        &core,
        6,
        serde_json::json!({
            "tool":"visual","action":"interact",
            "input":{"operation":"tap","target":"node","node_ref":node_ref}
        }),
        NOW_MS + 1,
    )
    .await;
    assert_eq!(interacted["outcome"], "success", "{interacted}");
    primitive.fail_scene_proof();
    let stale = submit(
        &core,
        7,
        serde_json::json!({
            "tool":"visual","action":"interact",
            "input":{"operation":"tap","target":"node","node_ref":node_ref}
        }),
        NOW_MS + 2,
    )
    .await;
    assert_eq!(stale["outcome"], "error");
    assert_eq!(stale["error"]["code"], "STALE_REFERENCE");
    assert_eq!(primitive.interactions().len(), 1);
}

#[tokio::test]
async fn i8_vis_g07_coordinates_are_rejected_against_admitted_geometry_before_delivery() {
    let (core, primitive) = core(CapabilityState::Available, CapabilityState::Available);
    let observed = submit(&core, 8, observe_payload(), NOW_MS).await;
    let observation_id = observed["result"]["observation_id"].as_str().unwrap();
    let rejected = submit(
        &core,
        9,
        serde_json::json!({
            "tool":"visual","action":"interact",
            "input":{"operation":"tap","target":"coordinate","observation_id":observation_id,"x":1080,"y":20}
        }),
        NOW_MS + 1,
    )
    .await;
    assert_eq!(rejected["outcome"], "error");
    assert_eq!(rejected["error"]["code"], "INVALID_ARGUMENT");
    assert!(primitive.interactions().is_empty());
}

#[tokio::test]
async fn i8_vis_g07_expired_observation_never_retargets_coordinate_input() {
    let (core, primitive) = core(CapabilityState::Available, CapabilityState::Available);
    let observed = submit(&core, 10, observe_payload(), NOW_MS).await;
    let observation_id = observed["result"]["observation_id"].as_str().unwrap();
    let stale = submit(
        &core,
        11,
        serde_json::json!({
            "tool":"visual","action":"interact",
            "input":{"operation":"tap","target":"coordinate","observation_id":observation_id,"x":1,"y":1}
        }),
        NOW_MS + 300_000,
    )
    .await;
    assert_eq!(stale["outcome"], "error");
    assert_eq!(stale["error"]["code"], "STALE_REFERENCE");
    assert!(primitive.interactions().is_empty());
}

#[tokio::test]
async fn i8_vis_g07_changed_scene_keeps_coordinate_targets_and_rejects_node_targets() {
    // A page that repaints between observation and interaction (DroidBridge's own screens do: every call
    // rewrites their call-time rows) leaves coordinate targets usable, because a coordinate means a place
    // on the display, while a node target means a place in the scene and must be refused when it moved.
    let (core, primitive) = core(CapabilityState::Unavailable, CapabilityState::Available);
    let observed = submit(&core, 12, observe_payload(), NOW_MS).await;
    let observation_id = observed["result"]["observation_id"].as_str().unwrap();
    let node_ref = observed["result"]["nodes"][0]["node_ref"].as_str().unwrap();
    primitive.fail_node_scene_proof();
    let coordinate = submit(
        &core,
        13,
        serde_json::json!({
            "tool":"visual","action":"interact",
            "input":{"operation":"tap","target":"coordinate","observation_id":observation_id,"x":10,"y":20}
        }),
        NOW_MS + 1,
    )
    .await;
    assert_eq!(coordinate["outcome"], "success", "{coordinate}");
    let node = submit(
        &core,
        14,
        serde_json::json!({
            "tool":"visual","action":"interact",
            "input":{"operation":"tap","target":"node","node_ref":node_ref}
        }),
        NOW_MS + 2,
    )
    .await;
    assert_eq!(node["outcome"], "error");
    assert_eq!(node["error"]["code"], "STALE_REFERENCE");
    assert_eq!(primitive.interactions().len(), 1);
}

#[tokio::test]
async fn i8_vis_g09_observe_states_what_its_observation_can_address() {
    let (accessibility_core, _primitive) = core(CapabilityState::Unavailable, CapabilityState::Available);
    let observed = submit(&accessibility_core, 15, observe_payload(), NOW_MS).await;
    assert_eq!(observed["result"]["interact"]["coordinate"], true);
    assert_eq!(observed["result"]["interact"]["ttl_ms"], 300_000);
    assert!(observed["result"]["nodes"][0].get("node_ref").is_some());
    assert!(observed["result"]["interact"].get("node_unavailable_reason").is_none());

    let (privileged, _primitive) = core(CapabilityState::Available, CapabilityState::Unavailable);
    let privileged = submit(&privileged, 16, observe_payload(), NOW_MS).await;
    // The privileged XML carries no addressable identity, so node targets are refused up front.
    assert_eq!(
        privileged["result"]["interact"]["node_unavailable_reason"],
        "NODE_REFS_UNAVAILABLE"
    );
}

#[tokio::test]
async fn i8_vis_g09_image_only_observation_accepts_the_coordinate_it_announces() {
    let (core, primitive) = core(CapabilityState::Available, CapabilityState::Available);
    let observed = submit(
        &core,
        17,
        serde_json::json!({
            "tool": "visual",
            "action": "observe",
            "input": {"include_image": true, "include_nodes": false}
        }),
        NOW_MS,
    )
    .await;
    assert_eq!(observed["result"]["interact"]["coordinate"], true);
    assert_eq!(
        observed["result"]["interact"]["node_unavailable_reason"],
        "NODES_NOT_REQUESTED"
    );
    let observation_id = observed["result"]["observation_id"].as_str().unwrap();
    let interacted = submit(
        &core,
        18,
        serde_json::json!({
            "tool":"visual","action":"interact",
            "input":{"operation":"tap","target":"coordinate","observation_id":observation_id,"x":10,"y":20}
        }),
        NOW_MS + 1,
    )
    .await;
    assert_eq!(interacted["outcome"], "success", "{interacted}");
    assert_eq!(primitive.interactions().len(), 1);
}

#[tokio::test]
async fn i8_vis_g10_view_resolves_exactly_one_source_and_publishes_truthful_image_format() {
    let (core, primitive) = core(CapabilityState::Available, CapabilityState::Available);
    let response = submit(
        &core,
        12,
        serde_json::json!({
            "tool":"visual","action":"view",
            "input":{"path":"/data/local/tmp/source.png","region":{"x":0,"y":0,"width":320,"height":240}}
        }),
        NOW_MS,
    )
    .await;
    assert_eq!(response["outcome"], "success", "{response}");
    assert_eq!(response["result"]["format"], "png");
    assert_eq!(response["result"]["width"], 320);
    assert_eq!(
        primitive.state.lock().unwrap().transform_sources,
        vec!["path"]
    );
}

#[tokio::test]
async fn i8_vis_g09_failed_observe_removes_an_image_published_before_the_failure() {
    let artifacts = FakeArtifacts::default();
    let (core, primitive) = core_with_artifacts(
        CapabilityState::Available,
        CapabilityState::Available,
        artifacts.clone(),
    );
    primitive.fail_hierarchy_without_cleanup_proof();

    let response = submit(&core, 13, observe_payload(), NOW_MS).await;

    assert_eq!(response["outcome"], "error", "{response}");
    assert_eq!(response["error"]["code"], "IO_ERROR");
    assert_eq!(
        artifacts
            .open("dbref:image:00000000-0000-4000-8000-000000000001")
            .unwrap_err()
            .code,
        ErrorCode::NotFound,
    );
}

#[tokio::test]
async fn i8_vis_g05_view_rejects_and_removes_a_mismatched_published_mime() {
    let artifacts = FakeArtifacts::default().with_image_publish_mime("image/heic");
    let (core, _) = core_with_artifacts(
        CapabilityState::Available,
        CapabilityState::Available,
        artifacts.clone(),
    );

    let response = submit(
        &core,
        14,
        serde_json::json!({
            "tool":"visual","action":"view",
            "input":{"path":"/data/local/tmp/source.png"}
        }),
        NOW_MS,
    )
    .await;

    assert_eq!(response["outcome"], "error", "{response}");
    assert_eq!(response["error"]["code"], "IO_ERROR");
    assert_eq!(
        artifacts
            .open("dbref:image:00000000-0000-4000-8000-000000000001")
            .unwrap_err()
            .code,
        ErrorCode::NotFound,
    );
}

#[test]
fn i8_vis_g08_privileged_xml_is_bounded_complete_and_never_mints_node_identity() {
    let xml = br#"<?xml version='1.0' encoding='UTF-8' standalone='yes' ?>
        <hierarchy rotation="0">
          <node index="0" text="A &amp; B" resource-id="pkg:id/title" class="android.widget.TextView" package="pkg" content-desc="Title" checkable="false" checked="false" clickable="true" enabled="true" focusable="true" focused="false" scrollable="false" long-clickable="false" password="false" selected="false" bounds="[1,2][30,40]" />
          <node index="1" text="Second" class="android.view.View" package="pkg" bounds="[0,0][1,1]" />
        </hierarchy>"#;
    let parsed = parse_privileged_hierarchy(xml, 1, display(19)).unwrap();

    assert_eq!(parsed.display, display(19));
    assert_eq!(parsed.nodes.len(), 1);
    assert!(parsed.truncated);
    assert_eq!(parsed.nodes[0].node_ref, None);
    assert_eq!(parsed.nodes[0].editable, None);
    assert_eq!(parsed.nodes[0].text.as_deref(), Some("A & B"));
    assert_eq!(parsed.nodes[0].bounds.left, 1);
    assert_eq!(parsed.nodes[0].bounds.bottom, 40);
    let expected_hash = Sha256::digest(xml)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    assert_eq!(
        parsed.proof,
        VisualSceneProof::Privileged {
            hierarchy_sha256: expected_hash,
        }
    );
}

#[test]
fn i8_vis_g08_empty_node_rect_is_reported_verbatim_and_never_voids_the_page() {
    // uiautomator reports a clipped row of the device's Settings hierarchy as an empty rect
    // (bottom < top); the page around it must still be observed.
    let xml = r#"<?xml version='1.0' encoding='UTF-8' standalone='yes' ?><hierarchy rotation="0">
        <node index="0" text="设置" resource-id="com.android.settings:id/homepage_title" class="android.widget.TextView" package="com.android.settings" content-desc="" checkable="false" checked="false" clickable="false" enabled="true" focusable="false" focused="false" scrollable="false" long-clickable="false" password="false" selected="false" bounds="[42,1209][189,1314]" drawing-order="1" hint="" />
        <node index="1" text="历史通知、对话" resource-id="android:id/summary" class="android.widget.TextView" package="com.android.settings" content-desc="" checkable="false" checked="false" clickable="false" enabled="true" focusable="false" focused="false" scrollable="false" long-clickable="false" password="false" selected="false" bounds="[210,2706][469,2654]" drawing-order="2" hint="" />
      </hierarchy>"#;
    let parsed = parse_privileged_hierarchy(xml.as_bytes(), 5_000, display(19)).unwrap();

    assert_eq!(parsed.nodes.len(), 2);
    assert!(!parsed.truncated);
    let clipped = parsed
        .nodes
        .iter()
        .find(|node| node.text.as_deref() == Some("历史通知、对话"))
        .expect("the clipped node is reported");
    assert_eq!(
        clipped.bounds,
        NodeBounds {
            left: 210,
            top: 2_706,
            right: 469,
            bottom: 2_654,
        }
    );
}

#[test]
fn i8_vis_g08_privileged_xml_rejects_external_entities_and_malformed_attributes() {
    let external = br#"<!DOCTYPE hierarchy [<!ENTITY xxe SYSTEM 'file:///data/local/tmp/secret'>]><hierarchy><node bounds="[0,0][1,1]" /></hierarchy>"#;
    assert_eq!(
        parse_privileged_hierarchy(external, 1, display(1))
            .unwrap_err()
            .code,
        ErrorCode::IoError,
    );

    let duplicate = br#"<hierarchy><node bounds="[0,0][1,1]" bounds="[0,0][2,2]" /></hierarchy>"#;
    assert_eq!(
        parse_privileged_hierarchy(duplicate, 1, display(1))
            .unwrap_err()
            .code,
        ErrorCode::IoError,
    );

    let non_numeric = br#"<hierarchy><node bounds="[a,0][1,1]" /></hierarchy>"#;
    assert_eq!(
        parse_privileged_hierarchy(non_numeric, 1, display(1))
            .unwrap_err()
            .code,
        ErrorCode::IoError,
    );

    let unclosed = br#"<hierarchy><node bounds="[0,0][1,1]"></hierarchy>"#;
    assert_eq!(
        parse_privileged_hierarchy(unclosed, 1, display(1))
            .unwrap_err()
            .code,
        ErrorCode::IoError,
    );

    let raw_markup = br#"<hierarchy><node text="a<b" bounds="[0,0][1,1]" /></hierarchy>"#;
    assert_eq!(
        parse_privileged_hierarchy(raw_markup, 1, display(1))
            .unwrap_err()
            .code,
        ErrorCode::IoError,
    );

    let malformed_after_limit =
        br#"<hierarchy><node bounds="[0,0][1,1]" /><node text="missing bounds" /></hierarchy>"#;
    assert_eq!(
        parse_privileged_hierarchy(malformed_after_limit, 1, display(1))
            .unwrap_err()
            .code,
        ErrorCode::IoError,
    );

    let malformed_root =
        br#"<hierarchy rotation="0" rotation="1"><node bounds="[0,0][1,1]" /></hierarchy>"#;
    assert_eq!(
        parse_privileged_hierarchy(malformed_root, 1, display(1))
            .unwrap_err()
            .code,
        ErrorCode::IoError,
    );
}
