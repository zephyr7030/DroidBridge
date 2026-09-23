use contract::{CapabilityState, ErrorCode, RuntimeHost, UuidV4};
use daemon::{
    CompanionLink, ConnectionLedger, DaemonRole, ExecutionGuardState, HelperHello, HelperRegistry,
    HostCoordinator, HostPreparation, MAX_CONNECTION_MESSAGES, MagiskExecutorFence,
    MagiskExecutorHandle, MagiskFacts, MagiskPrimitiveFamily, ModuleIdentity, ModuleObservation,
    Operation, PackageListKind, PrimitiveProcessPlan, SourceGeneration, WakeAlarmProbe,
    WireEnvelope, decode_companion_capability_snapshot, decode_frame, encode_frame,
};
use domain::{DomainError, OutstandingWork};
use persistence::{
    CanonicalState, GuardFinalizationDisposition, GuardProofReader, GuardProofRecord,
    GuardRecovery, ProcessFacts, await_guard_recovery_plan,
};
use serde_json::json;
#[cfg(not(target_os = "android"))]
use std::path::PathBuf;
#[cfg(unix)]
use std::{
    io::{Read, Write},
    os::fd::AsRawFd,
    os::unix::net::UnixStream,
};

fn id(value: u8) -> UuidV4 {
    UuidV4::parse(format!("00000000-0000-4000-8000-{value:012x}")).unwrap()
}

fn sequence_id(prefix: u32, value: u64) -> UuidV4 {
    UuidV4::parse(format!("{prefix:08x}-0000-4000-8000-{value:012x}")).unwrap()
}

struct VectorProofs(Vec<GuardProofRecord>);

impl GuardProofReader for VectorProofs {
    fn read_proof(
        &self,
        boot_id: &UuidV4,
        execution_id: &UuidV4,
    ) -> Result<Option<Vec<u8>>, DomainError> {
        Ok(self
            .0
            .iter()
            .find(|record| {
                &record.containing_boot_id == boot_id && &record.execution_id == execution_id
            })
            .map(|record| record.bytes.clone()))
    }

    fn list_proofs(&self) -> Result<Vec<GuardProofRecord>, DomainError> {
        Ok(self.0.clone())
    }
}

struct NeverLive;

impl ProcessFacts for NeverLive {
    fn is_same_process(&self, _pid: u32, _start_ticks: u64) -> Result<bool, DomainError> {
        Ok(false)
    }
}

fn observation() -> ModuleObservation {
    ModuleObservation {
        stable_present: true,
        debug_present: false,
        enabled: true,
        module_version_code: 1000,
        daemon_version_code: 1000,
        protocol_version: 1,
        metadata_self_test: true,
        excluded: false,
    }
}

#[test]
fn i7_g01_module_protocol_and_readiness_truth_are_exact() {
    let stable = ModuleIdentity::stable();
    assert_eq!(stable.module_id, "droidbridge");
    assert_eq!(stable.package, "com.droidbridge.android");
    assert!(observation().readiness(&stable, 1000).is_ok());

    let mut conflict = observation();
    conflict.debug_present = true;
    assert_eq!(
        conflict.readiness(&stable, 1000).unwrap_err().code,
        ErrorCode::CapabilityUnavailable,
    );
    let mut incompatible = observation();
    incompatible.protocol_version = 2;
    assert_eq!(
        incompatible.readiness(&stable, 1000).unwrap_err().code,
        ErrorCode::ProtocolIncompatible,
    );
    assert!(DaemonRole::BackendOnly.ready(true, false));
    assert!(!DaemonRole::RuntimeHost.ready(true, false));
    assert!(DaemonRole::RuntimeHost.ready(true, true));
}

#[test]
fn i7_g02_zero_work_transition_closes_admission_only_after_preparation() {
    let mut host = HostCoordinator::new(RuntimeHost::ApkRuntime, 7, id(1));
    let busy = host.prepare(
        id(2),
        RuntimeHost::MagiskBackend,
        OutstandingWork {
            tasks: 1,
            ..OutstandingWork::default()
        },
        9,
    );
    assert_eq!(busy.unwrap_err().code, ErrorCode::HostTransitionPending);
    assert!(host.admission_open());

    let prepared = host
        .prepare(
            id(3),
            RuntimeHost::MagiskBackend,
            OutstandingWork::default(),
            9,
        )
        .unwrap();
    assert_eq!(
        prepared,
        HostPreparation {
            transition_id: id(3),
            store_revision: 9
        }
    );
    assert!(!host.admission_open());
    host.abort(&id(3)).unwrap();
    assert!(host.admission_open());
}

#[test]
fn i7_g03_daemon_owns_core_only_for_selected_host() {
    assert_eq!(
        DaemonRole::from_owner(RuntimeHost::ApkRuntime),
        DaemonRole::BackendOnly
    );
    assert_eq!(
        DaemonRole::from_owner(RuntimeHost::MagiskBackend),
        DaemonRole::RuntimeHost,
    );
    assert!(!DaemonRole::BackendOnly.may_create_core());
    assert!(DaemonRole::RuntimeHost.may_create_core());
}

#[test]
fn i7_g04_helper_sdk_authentication_and_loss_are_isolated() {
    let mut helper = HelperRegistry::new(35, 4).unwrap();
    assert_eq!(helper.jar_name(), "droidbridge-framework-api35.jar");
    assert!(
        helper
            .accept_hello(
                0,
                HelperHello {
                    protocol_version: 1,
                    sdk_int: 35,
                    helper_generation: 4
                },
            )
            .is_ok()
    );
    assert_eq!(helper.framework_state(), CapabilityState::Available);
    assert!(
        helper
            .accept_hello(
                0,
                HelperHello {
                    protocol_version: 1,
                    sdk_int: 35,
                    helper_generation: 5
                },
            )
            .is_err()
    );
    helper.disconnected();
    assert_eq!(helper.framework_state(), CapabilityState::Unavailable);
    let mut generation = SourceGeneration::initial();
    assert_eq!(generation.current(), 1);
    assert_eq!(generation.advance().unwrap(), 2);
}

#[test]
fn i7_g05_companion_loss_removes_only_companion_capabilities() {
    let connected = MagiskFacts::ready(true, true);
    let disconnected = connected.with_companion(false);
    assert_eq!(disconnected.magisk_root, CapabilityState::Available);
    assert_eq!(
        disconnected.execution_root_guard,
        CapabilityState::Available
    );
    assert_eq!(
        disconnected.app_execution_surface,
        CapabilityState::Unavailable
    );
    assert_eq!(disconnected.shizuku_shell, CapabilityState::Unavailable);
}

#[test]
fn i7_g05_companion_link_survives_runtime_host_activation_order() {
    let mut link = CompanionLink::default();
    assert_eq!(link.capability_state(), CapabilityState::Unavailable);
    link.observe_connected();
    assert_eq!(link.capability_state(), CapabilityState::Available);
    link.observe_disconnected();
    assert_eq!(link.capability_state(), CapabilityState::Unavailable);
}

#[test]
fn i7_g06_shizuku_absence_does_not_block_magisk() {
    let facts = MagiskFacts::ready(false, true);
    assert_eq!(facts.shizuku_shell, CapabilityState::Unavailable);
    assert_eq!(facts.magisk_module, CapabilityState::Available);
    assert_eq!(facts.magisk_root, CapabilityState::Available);
}

#[test]
fn i7_g07_long_work_defers_optional_promotion_without_closing_admission() {
    let mut host = HostCoordinator::new(RuntimeHost::ApkRuntime, 2, id(7));
    let outcome = host.optional_promotion(
        id(8),
        OutstandingWork {
            automation_executions: 1,
            ..OutstandingWork::default()
        },
        12,
    );
    assert!(outcome.is_deferred());
    assert!(host.admission_open());
}

#[test]
fn i7_g10_protocol_fences_messages_and_bounds_business_slots() {
    let envelope = WireEnvelope::request(
        id(10),
        id(11),
        4,
        Some(id(12)),
        Operation::RuntimeForward,
        json!({"protocol_version":1}),
        vec![],
    );
    let encoded = encode_frame(&envelope).unwrap();
    assert_eq!(decode_frame(&encoded).unwrap(), envelope);

    let mut ledger = ConnectionLedger::new();
    for value in 20..24 {
        let request = WireEnvelope::request(
            id(value),
            id(10),
            1,
            None,
            Operation::HostStatus,
            json!({}),
            Vec::new(),
        );
        ledger.reserve_control_envelope(&request).unwrap();
    }
    for value in 24..84 {
        let request = WireEnvelope::request(
            id(value),
            id(10),
            1,
            Some(id(11)),
            Operation::CompanionExecute,
            json!({}),
            Vec::new(),
        );
        ledger.reserve_business_envelope(&request).unwrap();
    }
    let overflow = WireEnvelope::request(
        id(84),
        id(10),
        1,
        Some(id(11)),
        Operation::CompanionExecute,
        json!({}),
        Vec::new(),
    );
    assert_eq!(
        ledger
            .reserve_business_envelope(&overflow)
            .unwrap_err()
            .code,
        ErrorCode::ResourceLimit,
    );
}

#[test]
fn i7_g05_companion_snapshot_accepts_only_generation_fenced_app_facts() {
    let facts = decode_companion_capability_snapshot(&json!({
        "registrations": [{
            "key": "shizuku.shell",
            "state": "unknown",
            "reason": "CONNECTING",
            "source_generation": 7,
            "has_executor": false
        }]
    }))
    .unwrap();
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].key, "shizuku.shell");
    assert_eq!(facts[0].source_generation, 7);

    for invalid in [
        json!({"registrations":[{"key":"magisk.root","state":"available","source_generation":1,"has_executor":true}]}),
        json!({"registrations":[{"key":"shizuku.shell","state":"available","reason":"STALE","source_generation":1,"has_executor":true}]}),
        json!({"registrations":[{"key":"shizuku.shell","state":"unknown","reason":"CONNECTING","source_generation":0,"has_executor":false}]}),
    ] {
        assert_eq!(
            decode_companion_capability_snapshot(&invalid)
                .unwrap_err()
                .code,
            ErrorCode::ProtocolIncompatible,
        );
    }
}

#[test]
fn i7_g10_response_operation_must_repeat_the_reserved_request() {
    let mut ledger = ConnectionLedger::new();
    let request = WireEnvelope::request(
        id(85),
        id(10),
        1,
        None,
        Operation::CapabilitySnapshot,
        json!({}),
        Vec::new(),
    );
    ledger.reserve_control_envelope(&request).unwrap();
    let mut mismatched = WireEnvelope::response(id(86), &request, None, json!({}), Vec::new());
    mismatched.operation = Operation::HostStatus;
    assert_eq!(
        ledger.complete_envelope(&mismatched).unwrap_err().code,
        ErrorCode::ProtocolIncompatible,
    );
    let valid = WireEnvelope::response(id(91), &request, None, json!({}), Vec::new());
    ledger.complete_envelope(&valid).unwrap();
}

#[test]
fn i7_g10_response_owner_fence_must_repeat_the_reserved_request() {
    let request = WireEnvelope::request(
        id(86),
        id(87),
        3,
        Some(id(88)),
        Operation::RuntimeForward,
        json!({}),
        Vec::new(),
    );
    let mut ledger = ConnectionLedger::new();
    ledger.reserve_business_envelope(&request).unwrap();

    let mut stale = WireEnvelope::response(
        id(89),
        &request,
        request.runtime_instance_id.clone(),
        json!({}),
        Vec::new(),
    );
    stale.host_generation += 1;
    assert_eq!(
        ledger.complete_envelope(&stale).unwrap_err().code,
        ErrorCode::StaleAuthority,
    );

    let valid = WireEnvelope::response(
        id(90),
        &request,
        request.runtime_instance_id.clone(),
        json!({}),
        Vec::new(),
    );
    ledger.complete_envelope(&valid).unwrap();
}

#[cfg(unix)]
#[test]
fn i7_g10_scm_rights_roles_preserve_descriptor_order_and_ownership() {
    use daemon::unix_transport::{receive_envelope, send_envelope};

    let (mut sender, mut receiver) = UnixStream::pair().unwrap();
    let (payload_reader, mut payload_writer) = UnixStream::pair().unwrap();
    let envelope = WireEnvelope::request(
        id(90),
        id(91),
        5,
        Some(id(92)),
        Operation::RuntimeForward,
        json!({"protocol_version":1}),
        vec!["content".to_owned()],
    );
    send_envelope(&mut sender, &envelope, &[payload_reader.as_raw_fd()]).unwrap();
    let mut received = receive_envelope(&mut receiver).unwrap();
    assert_eq!(received.envelope, envelope);
    assert_eq!(received.descriptors.len(), 1);

    payload_writer.write_all(b"x").unwrap();
    let mut transferred = UnixStream::from(received.descriptors.remove(0));
    let mut byte = [0_u8; 1];
    transferred.read_exact(&mut byte).unwrap();
    assert_eq!(&byte, b"x");
}

#[test]
fn i7_g09_root_guard_death_is_durable_quarantine_not_clean() {
    let mut state = ExecutionGuardState::Running;
    state.observe_guard_death();
    assert_eq!(state, ExecutionGuardState::CleanupUnverified);
    assert!(state.blocks_admission());
    state.observe_reboot();
    assert_eq!(state, ExecutionGuardState::Clean);
}

#[test]
fn i7_g03_magisk_primitive_handle_is_fenced_and_process_identities_are_fixed() {
    let fence = MagiskExecutorFence {
        runtime_epoch: id(100),
        host_generation: 7,
        runtime_instance_id: id(101),
        source_generation: 3,
    };
    let handle = MagiskExecutorHandle::new(fence.clone(), Some(4)).unwrap();
    assert!(
        handle
            .authorize(&fence, MagiskPrimitiveFamily::NetworkCapture)
            .is_ok()
    );
    let mut stale = fence.clone();
    stale.source_generation += 1;
    assert_eq!(
        handle
            .authorize(&stale, MagiskPrimitiveFamily::Filesystem)
            .unwrap_err()
            .code,
        ErrorCode::StaleAuthority,
    );
    assert_eq!(
        handle
            .without_helper()
            .authorize(&fence, MagiskPrimitiveFamily::PrivilegedAndroid)
            .unwrap_err()
            .code,
        ErrorCode::CapabilityUnavailable,
    );

    let root = PrimitiveProcessPlan::root_shell("id".to_owned()).unwrap();
    assert_eq!(root.program(), "/system/bin/sh");
    assert_eq!(root.arguments(), ["-c", "id"]);
    let capture = PrimitiveProcessPlan::screen_capture();
    assert_eq!(capture.program(), "/system/bin/screencap");
    assert!(capture.arguments().is_empty());
    let packages = PrimitiveProcessPlan::package_list(PackageListKind::ThirdParty);
    assert_eq!(packages.program(), "/system/bin/cmd");
    assert_eq!(
        packages.arguments(),
        [
            "package",
            "list",
            "packages",
            "-3",
            "--show-versioncode",
            "--user",
            "0",
        ],
    );
    let force_stop =
        PrimitiveProcessPlan::package_force_stop("com.example.app".to_owned()).unwrap();
    assert_eq!(force_stop.program(), "/system/bin/am");
    assert!(PrimitiveProcessPlan::package_force_stop("bad package".to_owned()).is_err());
}

#[test]
fn i7_g10_message_history_exhaustion_requires_a_fresh_connection() {
    let mut exhausted = ConnectionLedger::new();
    for value in 0..MAX_CONNECTION_MESSAGES as u64 {
        exhausted
            .observe_incoming(sequence_id(0x1000_0000, value))
            .unwrap();
        exhausted
            .observe_outgoing(sequence_id(0x2000_0000, value))
            .unwrap();
    }
    assert_eq!(
        exhausted
            .observe_incoming(sequence_id(0x1000_0000, MAX_CONNECTION_MESSAGES as u64))
            .unwrap_err()
            .code,
        ErrorCode::ResourceLimit,
    );
    assert_eq!(
        exhausted
            .observe_outgoing(sequence_id(0x2000_0000, MAX_CONNECTION_MESSAGES as u64))
            .unwrap_err()
            .code,
        ErrorCode::ResourceLimit,
    );

    let old_request = sequence_id(0x3000_0000, 1);
    let mut fresh = ConnectionLedger::new();
    let unknown_request = WireEnvelope::request(
        old_request.clone(),
        id(10),
        1,
        None,
        Operation::HostStatus,
        json!({}),
        Vec::new(),
    );
    let response = WireEnvelope::response(
        sequence_id(0x3000_0000, 2),
        &unknown_request,
        None,
        json!({}),
        Vec::new(),
    );
    assert_eq!(
        fresh.complete_envelope(&response).unwrap_err().code,
        ErrorCode::ProtocolIncompatible,
    );
    fresh.reserve_control_envelope(&unknown_request).unwrap();
    fresh.complete_envelope(&response).unwrap();
}

#[test]
fn i4_g04_i7_g11_shared_guard_recovery_vectors_drive_magisk_plan() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../fixtures/guard-recovery-vectors.json"
    ))
    .unwrap();
    let vector = fixture["vectors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|vector| vector["name"] == "orphan_old_boot_proof")
        .unwrap();
    let execution = vector["proofs"][0]["execution"].as_u64().unwrap();
    let plan = await_guard_recovery_plan(
        &CanonicalState::default(),
        &id(110),
        &id(3),
        &VectorProofs(vec![GuardProofRecord {
            containing_boot_id: id(4),
            execution_id: UuidV4::parse(format!("00000000-0000-4000-8000-{execution:012x}"))
                .unwrap(),
            bytes: Vec::new(),
        }]),
        &NeverLive,
    )
    .unwrap();
    assert!(plan.guards_are_clean());
    assert_eq!(plan.records().len(), 1);
    assert!(matches!(
        plan.records()[0].recovery,
        GuardRecovery::Clean { .. }
    ));
    assert_eq!(
        plan.records()[0].finalization,
        GuardFinalizationDisposition::RemoveProof
    );
}

#[test]
fn i7_g12_wake_alarm_requires_create_clock_arm_and_disarm() {
    assert!(
        WakeAlarmProbe {
            created: true,
            clock_read: true,
            armed: true,
            disarmed: true,
        }
        .available()
    );
    for missing in [
        WakeAlarmProbe {
            created: false,
            clock_read: true,
            armed: true,
            disarmed: true,
        },
        WakeAlarmProbe {
            created: true,
            clock_read: false,
            armed: true,
            disarmed: true,
        },
        WakeAlarmProbe {
            created: true,
            clock_read: true,
            armed: false,
            disarmed: true,
        },
        WakeAlarmProbe {
            created: true,
            clock_read: true,
            armed: true,
            disarmed: false,
        },
    ] {
        assert!(!missing.available());
    }
}

// Inspects the checked-out crate sources, which do not exist on a device deployment.
#[cfg(not(target_os = "android"))]
#[test]
fn i7_g11_daemon_composition_delegates_magisk_host_and_shared_recovery() {
    let source_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let process = std::fs::read_to_string(source_root.join("process.rs")).unwrap();
    let host = std::fs::read_to_string(source_root.join("magisk_host.rs")).unwrap();
    let recovery = std::fs::read_to_string(source_root.join("magisk_guard_recovery.rs")).unwrap();

    assert!(!process.contains("struct MagiskHost"));
    assert!(!process.contains("finalize_clean_guard_records"));
    assert!(host.contains("struct MagiskHost"));
    assert!(host.contains("execute_guard_recovery"));
    assert!(recovery.contains("GuardRecoveryPlan"));
    assert!(!host.contains("MagiskRecoveryPlan"));
    assert!(!host.contains("prior_runtime_instance_ids"));
    assert!(!recovery.contains("prior_runtime_instance_ids"));
}

#[cfg(unix)]
fn companion_artifact(label: &str, bytes: &[u8]) -> std::fs::File {
    let path = std::env::temp_dir().join(format!("droidbridge-i7-{label}-{}", std::process::id(),));
    std::fs::write(&path, bytes).unwrap();
    std::fs::File::open(&path).unwrap()
}

#[cfg(unix)]
fn read_artifact(descriptor: std::os::fd::OwnedFd) -> String {
    let mut file = std::fs::File::from(descriptor);
    let mut value = String::new();
    file.read_to_string(&mut value).unwrap();
    value
}

#[cfg(unix)]
#[test]
fn i7_g10_companion_execution_carries_one_typed_primitive_and_its_descriptors() {
    let (daemon_stream, peer_stream) = UnixStream::pair().unwrap();
    let channel = daemon::companion::CompanionChannel::start(&daemon_stream).unwrap();
    channel
        .set_fence(id(10), 4, Some(id(11)))
        .expect("owner fence is publishable");

    let peer = std::thread::spawn(move || {
        let mut stream = peer_stream;
        let received = daemon::unix_transport::receive_envelope(&mut stream).unwrap();
        assert_eq!(received.envelope.kind, daemon::MessageKind::Request);
        assert_eq!(received.envelope.operation, Operation::CompanionExecute);
        assert_eq!(received.envelope.runtime_epoch, id(10));
        assert_eq!(received.envelope.host_generation, 4);
        assert_eq!(received.envelope.runtime_instance_id, Some(id(11)));
        assert_eq!(
            received.envelope.fd_roles,
            vec!["execution_guard_proof".to_owned()]
        );
        assert_eq!(received.descriptors.len(), 1);
        assert_eq!(
            read_artifact(received.descriptors.into_iter().next().unwrap()),
            "guard-proof",
        );
        let payload = received.envelope.payload.as_object().unwrap();
        assert_eq!(payload.len(), 3);
        assert_eq!(payload["primitive"], json!("AppProcessStart"));
        assert_eq!(payload["execution_id"], json!(id(12).as_str()));
        assert_eq!(payload["payload"], json!({"run_as": "app"}));

        let reply = WireEnvelope::response(
            id(13),
            &received.envelope,
            Some(id(11)),
            json!({"payload": {"exit_code": 0}}),
            vec!["stdout".to_owned()],
        );
        let artifact = companion_artifact("companion-stdout", b"companion-output");
        daemon::unix_transport::send_envelope(&mut stream, &reply, &[artifact.as_raw_fd()])
            .unwrap();
    });

    let result = channel
        .execute(
            daemon::companion::CompanionPrimitiveRequest {
                primitive: "AppProcessStart".to_owned(),
                payload: json!({"run_as": "app"}),
                execution_id: id(12),
                descriptors: vec![(
                    "execution_guard_proof".to_owned(),
                    std::os::fd::OwnedFd::from(companion_artifact(
                        "companion-proof",
                        b"guard-proof",
                    )),
                )],
            },
            std::time::Duration::from_secs(5),
        )
        .unwrap();
    peer.join().unwrap();
    assert_eq!(result.payload, json!({"exit_code": 0}));
    assert_eq!(result.descriptors.len(), 1);
    assert_eq!(result.descriptors[0].0, "stdout");
    assert_eq!(
        read_artifact(result.descriptors.into_iter().next().unwrap().1),
        "companion-output",
    );
}

#[cfg(unix)]
#[test]
fn i7_g10_companion_execution_requires_a_live_instance_and_propagates_typed_failure() {
    let (daemon_stream, _peer_stream) = UnixStream::pair().unwrap();
    let unfenced = daemon::companion::CompanionChannel::start(&daemon_stream).unwrap();
    assert_eq!(
        unfenced
            .execute(
                companion_request("AppProcessStart"),
                std::time::Duration::from_millis(50)
            )
            .unwrap_err()
            .code,
        ErrorCode::CapabilityUnavailable,
    );
    unfenced
        .set_fence(id(10), 4, None)
        .expect("owner fence is publishable");
    assert_eq!(
        unfenced
            .execute(
                companion_request("AppProcessStart"),
                std::time::Duration::from_millis(50)
            )
            .unwrap_err()
            .code,
        ErrorCode::CapabilityUnavailable,
    );

    let (daemon_stream, peer_stream) = UnixStream::pair().unwrap();
    let channel = daemon::companion::CompanionChannel::start(&daemon_stream).unwrap();
    channel.set_fence(id(10), 4, Some(id(11))).unwrap();
    let peer = std::thread::spawn(move || {
        let mut stream = peer_stream;
        let received = daemon::unix_transport::receive_envelope(&mut stream).unwrap();
        let reply = WireEnvelope::response(
            id(13),
            &received.envelope,
            Some(id(11)),
            json!({"error": {"code": "RUN_AS_UNAVAILABLE", "retryable": false}}),
            Vec::new(),
        );
        daemon::unix_transport::send_envelope(&mut stream, &reply, &[]).unwrap();
    });
    assert_eq!(
        channel
            .execute(
                companion_request("AppProcessStart"),
                std::time::Duration::from_secs(5)
            )
            .unwrap_err()
            .code,
        ErrorCode::RunAsUnavailable,
    );
    peer.join().unwrap();

    let (daemon_stream, _peer_stream) = UnixStream::pair().unwrap();
    let silent = daemon::companion::CompanionChannel::start(&daemon_stream).unwrap();
    silent.set_fence(id(10), 4, Some(id(11))).unwrap();
    assert_eq!(
        silent
            .execute(
                companion_request("AppProcessStart"),
                std::time::Duration::from_millis(20)
            )
            .unwrap_err()
            .code,
        ErrorCode::Timeout,
    );
}

#[cfg(unix)]
#[test]
fn i7_g10_companion_channel_loss_fails_pending_executions_and_never_adopts_old_correlation() {
    let (daemon_stream, peer_stream) = UnixStream::pair().unwrap();
    let channel = daemon::companion::CompanionChannel::start(&daemon_stream).unwrap();
    channel.set_fence(id(10), 4, Some(id(11))).unwrap();
    let (observed_tx, observed_rx) = std::sync::mpsc::channel();
    let peer = std::thread::spawn(move || {
        let mut stream = peer_stream;
        let received = daemon::unix_transport::receive_envelope(&mut stream).unwrap();
        observed_tx.send(received.envelope.message_id).unwrap();
    });
    assert_eq!(
        channel
            .execute(
                companion_request("AppProcessStart"),
                std::time::Duration::from_secs(5)
            )
            .unwrap_err()
            .code,
        ErrorCode::IoError,
    );
    peer.join().unwrap();
    assert_eq!(channel.next_event().unwrap_err().code, ErrorCode::IoError);

    let stale_correlation = observed_rx.recv().unwrap();
    let (fresh_stream, fresh_peer) = UnixStream::pair().unwrap();
    let fresh = daemon::companion::CompanionChannel::start(&fresh_stream).unwrap();
    fresh.set_fence(id(10), 5, Some(id(20))).unwrap();
    let request = WireEnvelope::request(
        stale_correlation.clone(),
        id(10),
        5,
        Some(id(20)),
        Operation::CompanionExecute,
        json!({"primitive": "AppProcessStart", "payload": {}, "execution_id": id(21)}),
        Vec::new(),
    );
    let late_reply = WireEnvelope::response(
        id(22),
        &request,
        Some(id(20)),
        json!({"payload": {}}),
        Vec::new(),
    );
    let peer = std::thread::spawn(move || {
        let mut stream = fresh_peer;
        daemon::unix_transport::send_envelope(&mut stream, &late_reply, &[]).unwrap();
    });
    assert_eq!(
        fresh.next_event().unwrap_err().code,
        ErrorCode::ProtocolIncompatible,
    );
    peer.join().unwrap();
}

#[cfg(unix)]
#[test]
fn i7_g10_companion_channel_rejects_descriptors_on_control_responses() {
    let (daemon_stream, peer_stream) = UnixStream::pair().unwrap();
    let channel = daemon::companion::CompanionChannel::start(&daemon_stream).unwrap();
    let probe = WireEnvelope::request(
        id(30),
        id(10),
        4,
        None,
        Operation::CapabilitySnapshot,
        json!({}),
        Vec::new(),
    );
    channel
        .request_control(&probe)
        .expect("control request is reservable");
    let peer = std::thread::spawn(move || {
        let mut stream = peer_stream;
        let received = daemon::unix_transport::receive_envelope(&mut stream).unwrap();
        assert_eq!(received.envelope.operation, Operation::CapabilitySnapshot);
        let reply = WireEnvelope::response(
            id(31),
            &received.envelope,
            None,
            json!({"registrations": []}),
            vec!["content".to_owned()],
        );
        let artifact = companion_artifact("companion-control", b"unexpected");
        daemon::unix_transport::send_envelope(&mut stream, &reply, &[artifact.as_raw_fd()])
            .unwrap();
    });
    assert_eq!(
        channel.next_event().unwrap_err().code,
        ErrorCode::ProtocolIncompatible,
    );
    peer.join().unwrap();
}

#[cfg(unix)]
fn companion_request(primitive: &str) -> daemon::companion::CompanionPrimitiveRequest {
    daemon::companion::CompanionPrimitiveRequest {
        primitive: primitive.to_owned(),
        payload: json!({"run_as": "app"}),
        execution_id: id(12),
        descriptors: Vec::new(),
    }
}

#[cfg(unix)]
fn companion_execution(execution_id: u8) -> runtime::AdmittedExecution {
    runtime::AdmittedExecution {
        execution_id: id(execution_id),
        task_id: None,
        executor: runtime::ExecutorRecord {
            host: RuntimeHost::MagiskBackend,
            provider: runtime::ProviderToken::AppFramework,
            execution_class: contract::ExecutionClass::AndroidFramework,
            capability_generation: 4,
            fence: contract::Fence {
                runtime_epoch: id(10),
                host_generation: 4,
                runtime_instance_id: id(11),
            },
        },
        payload: runtime::ExecutionPayload::OpaqueOperation("filesystem.inspect".to_owned()),
    }
}

#[cfg(unix)]
fn live_companion_port() -> (daemon::companion::CompanionPort, UnixStream) {
    let (daemon_stream, peer_stream) = UnixStream::pair().unwrap();
    let channel = daemon::companion::CompanionChannel::start(&daemon_stream).unwrap();
    let port = daemon::companion::CompanionPort::default();
    port.publish(channel).unwrap();
    port.set_fence(id(10), 4, Some(id(11))).unwrap();
    (port, peer_stream)
}

#[cfg(unix)]
fn dispatch_through(
    port: &daemon::companion::CompanionPort,
    primitive: &str,
    payload: &[u8],
    execution: &runtime::AdmittedExecution,
) -> Result<runtime::AndroidPrimitiveResult, DomainError> {
    runtime::AndroidExecutionDispatch::dispatch(port, primitive, payload, execution)
}

#[cfg(unix)]
#[test]
fn i8_fs_g04_companion_port_delegates_only_while_the_authenticated_connection_is_live() {
    let execution = companion_execution(20);
    let port = daemon::companion::CompanionPort::default();
    assert_eq!(
        dispatch_through(&port, "ContentInspect", b"{}", &execution)
            .unwrap_err()
            .code,
        ErrorCode::CapabilityUnavailable,
    );

    let (port, peer_stream) = live_companion_port();
    let peer = std::thread::spawn(move || {
        let mut stream = peer_stream;
        let received = daemon::unix_transport::receive_envelope(&mut stream).unwrap();
        assert_eq!(received.envelope.kind, daemon::MessageKind::Request);
        assert_eq!(received.envelope.operation, Operation::CompanionExecute);
        assert_eq!(received.envelope.runtime_epoch, id(10));
        assert_eq!(received.envelope.host_generation, 4);
        assert_eq!(received.envelope.runtime_instance_id, Some(id(11)));
        assert!(received.envelope.fd_roles.is_empty());
        let payload = received.envelope.payload.as_object().unwrap();
        assert_eq!(payload.len(), 3);
        assert_eq!(payload["primitive"], json!("ContentOpenRead"));
        assert_eq!(payload["execution_id"], json!(id(20).as_str()));
        assert_eq!(
            payload["payload"],
            json!({"type": "content_uri", "value": "content://authority/document/1"}),
        );

        let reply = WireEnvelope::response(
            id(21),
            &received.envelope,
            Some(id(11)),
            json!({"payload": {"total_size": 6}}),
            vec!["content".to_owned()],
        );
        let artifact = companion_artifact("companion-content", b"abcdef");
        daemon::unix_transport::send_envelope(&mut stream, &reply, &[artifact.as_raw_fd()])
            .unwrap();
    });
    let result = dispatch_through(
        &port,
        "ContentOpenRead",
        br#"{"type":"content_uri","value":"content://authority/document/1"}"#,
        &execution,
    )
    .unwrap();
    peer.join().unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&result.payload).unwrap(),
        json!({"total_size": 6}),
    );
    assert_eq!(result.descriptors.len(), 1);
    assert_eq!(result.descriptors[0].0, "content");
    let mut file = result.descriptors.into_iter().next().unwrap().1;
    let mut bytes = String::new();
    file.read_to_string(&mut bytes).unwrap();
    assert_eq!(bytes, "abcdef");

    port.withdraw().unwrap();
    assert_eq!(
        dispatch_through(&port, "ContentInspect", b"{}", &execution)
            .unwrap_err()
            .code,
        ErrorCode::CapabilityUnavailable,
    );
}

#[cfg(unix)]
#[test]
fn i8_fs_g04_companion_port_fences_the_instance_of_a_host_that_activates_after_the_connection() {
    let execution = companion_execution(40);
    let (daemon_stream, peer_stream) = UnixStream::pair().unwrap();
    let channel = daemon::companion::CompanionChannel::start(&daemon_stream).unwrap();
    let port = daemon::companion::CompanionPort::default();
    port.publish(channel).unwrap();
    assert_eq!(
        dispatch_through(&port, "ContentInspect", b"{}", &execution)
            .unwrap_err()
            .code,
        ErrorCode::CapabilityUnavailable,
    );

    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let peer = std::thread::spawn(move || {
        let mut stream = peer_stream;
        let received = daemon::unix_transport::receive_envelope(&mut stream).unwrap();
        assert_eq!(received.envelope.operation, Operation::CompanionExecute);
        assert_eq!(received.envelope.runtime_epoch, id(10));
        assert_eq!(received.envelope.host_generation, 4);
        assert_eq!(received.envelope.runtime_instance_id, Some(id(11)));
        let reply = WireEnvelope::response(
            id(41),
            &received.envelope,
            Some(id(11)),
            json!({"payload": {"total_size": 3}}),
            Vec::new(),
        );
        daemon::unix_transport::send_envelope(&mut stream, &reply, &[]).unwrap();
        let _ = release_rx.recv();
    });

    port.set_fence(id(10), 4, Some(id(11))).unwrap();
    let result = dispatch_through(&port, "ContentInspect", b"{}", &execution).unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&result.payload).unwrap(),
        json!({"total_size": 3}),
    );

    port.set_fence(id(10), 4, None).unwrap();
    assert_eq!(
        dispatch_through(&port, "ContentInspect", b"{}", &execution)
            .unwrap_err()
            .code,
        ErrorCode::CapabilityUnavailable,
    );
    release_tx.send(()).unwrap();
    peer.join().unwrap();
}

#[cfg(unix)]
#[test]
fn i8_fs_g04_companion_port_propagates_typed_failure_and_rejects_invalid_payload() {
    let execution = companion_execution(30);
    let (port, _peer_stream) = live_companion_port();
    assert_eq!(
        dispatch_through(&port, "ContentInspect", b"not-json", &execution)
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument,
    );

    let (port, peer_stream) = live_companion_port();
    let peer = std::thread::spawn(move || {
        let mut stream = peer_stream;
        let received = daemon::unix_transport::receive_envelope(&mut stream).unwrap();
        let reply = WireEnvelope::response(
            id(31),
            &received.envelope,
            Some(id(11)),
            json!({"error": {"code": "STALE_AUTHORITY", "retryable": false}}),
            Vec::new(),
        );
        daemon::unix_transport::send_envelope(&mut stream, &reply, &[]).unwrap();
    });
    assert_eq!(
        dispatch_through(&port, "ContentInspect", br#"{"target":{}}"#, &execution)
            .unwrap_err()
            .code,
        ErrorCode::StaleAuthority,
    );
    peer.join().unwrap();
}

#[test]
fn daemon_version_code_is_the_product_version_code() {
    // The module readiness check requires module.prop's versionCode, which the build stamps from
    // gradle.properties, to equal the daemon's own; a daemon built with another value never
    // becomes ready.
    let properties = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../gradle.properties"),
    )
    .unwrap();
    let product = properties
        .lines()
        .find_map(|line| line.strip_prefix("droidbridgeVersionCode="))
        .unwrap()
        .trim()
        .parse::<u64>()
        .unwrap();
    assert_eq!(daemon::VERSION_CODE, product);
}
