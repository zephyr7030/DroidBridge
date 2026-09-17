use contract::{
    Automation, AutomationAction, AutomationCommandCall, AutomationCompatibleCall,
    AutomationCondition, AutomationTrigger, Availability, CapabilityState, CommandRunInput,
    ConditionOperator, ConditionSource, ErrorCode, ExecutionClass, GrantFacts, RunAs, RuntimeHost,
    RuntimeReadiness, ScalarValue, TaskState, UuidV4,
};
use domain::{
    AdmissionDecision, AdmissionFence, AndroidRoute, AutomationSlot, CapabilityContext,
    DedupDecision, DedupIndex, DeleteDisposition, ExecutorRequest, FilesystemRoute, HostEvent,
    HostState, OutstandingWork, PackageInspectFact, Preflight, Provider, ProviderGenerations,
    ResolverFacts, RuntimeIdentity, RuntimeOwner, SettlementDisposition, TaskEvent, TaskLifecycle,
    VisualRoute, all_required, any_sufficient, derive_capabilities, evaluate_condition,
    resolve_executor, validate_automation,
};
use std::collections::BTreeMap;

fn uuid(value: &str) -> UuidV4 {
    UuidV4::parse(value).unwrap()
}

fn availability(state: CapabilityState) -> Availability {
    Availability {
        state,
        reason: None,
    }
}

fn grants(state: CapabilityState) -> GrantFacts {
    GrantFacts {
        android_local_network: availability(state),
        android_notifications: availability(state),
        android_notification_listener: availability(state),
        automation_exact_alarm: availability(state),
        visual_accessibility: availability(state),
        visual_media_projection_session: availability(state),
        shizuku_shell: availability(state),
        magisk_module: availability(state),
        magisk_root: availability(state),
        magisk_framework: availability(state),
        magisk_launch: availability(state),
        magisk_clipboard: availability(state),
        magisk_notifications: availability(state),
        magisk_wake_alarm: availability(state),
        execution_app_guard: availability(state),
        execution_shell_guard: availability(state),
        execution_root_guard: availability(state),
    }
}

fn resolver_facts(state: CapabilityState) -> ResolverFacts {
    ResolverFacts {
        app_native: state,
        app_framework: state,
        shizuku: state,
        magisk_native: state,
        magisk_framework: state,
        magisk_launch: state,
        magisk_clipboard: state,
        magisk_notifications: state,
        accessibility: state,
        media_projection: state,
        notification_listener: state,
        generations: ProviderGenerations {
            app_native: 1,
            app_framework: 1,
            shizuku: 1,
            magisk_native: 1,
            magisk_framework: 1,
            accessibility: 1,
            media_projection: 1,
            notification_listener: 1,
        },
    }
}

fn fence() -> AdmissionFence {
    AdmissionFence {
        runtime_epoch: uuid("10000000-0000-4000-8000-000000000001"),
        host_generation: 3,
        runtime_instance_id: uuid("10000000-0000-4000-8000-000000000002"),
    }
}

fn automation(action: AutomationAction) -> Automation {
    Automation {
        automation_id: uuid("20000000-0000-4000-8000-000000000001"),
        name: "bounded".to_owned(),
        enabled: true,
        trigger: AutomationTrigger::Event {
            name: "runtime.ready".to_owned(),
            r#match: None,
        },
        action,
        state: BTreeMap::new(),
        revision: 1,
        created_at: "2026-09-06T00:00:00.000Z".to_owned(),
        updated_at: "2026-09-06T00:00:00.000Z".to_owned(),
    }
}

#[test]
fn i2_g01_domain_reducers_and_resolver_preserve_authority() {
    use CapabilityState::{Available, Unavailable, Unknown};
    for left in [Available, Unavailable, Unknown] {
        for right in [Available, Unavailable, Unknown] {
            let any_expected = if left == Available || right == Available {
                Available
            } else if left == Unavailable && right == Unavailable {
                Unavailable
            } else {
                Unknown
            };
            let all_expected = if left == Unavailable || right == Unavailable {
                Unavailable
            } else if left == Available && right == Available {
                Available
            } else {
                Unknown
            };
            assert_eq!(any_sufficient(&[left, right]), any_expected);
            assert_eq!(all_required(&[left, right]), all_expected);
        }
    }

    let mut observed = grants(Unavailable);
    observed.android_local_network = availability(Unknown);
    let effective = derive_capabilities(
        &observed,
        CapabilityContext {
            sdk_int: 37,
            host: RuntimeHost::ApkRuntime,
            readiness: RuntimeReadiness::Ready,
            app_execution_surface: Available,
        },
    )
    .unwrap();
    assert_eq!(effective.network_local.state, Unknown);
    assert!(effective.network_local.reason.is_none());

    let mut facts = resolver_facts(Unavailable);
    facts.shizuku = Available;
    facts.accessibility = Available;
    let admitted = resolve_executor(
        RuntimeHost::ApkRuntime,
        fence(),
        facts,
        ExecutorRequest::Visual(VisualRoute::Hierarchy),
    )
    .unwrap();
    assert_eq!(admitted.provider(), Provider::Shizuku);
    facts.shizuku = Unavailable;
    assert_eq!(admitted.provider(), Provider::Shizuku);
    assert_eq!(facts.shizuku, Unavailable);
    assert_eq!(admitted.fence().host_generation, 3);
    assert_eq!(admitted.capability_generation(), 1);

    let root_error = resolve_executor(
        RuntimeHost::ApkRuntime,
        fence(),
        resolver_facts(Available),
        ExecutorRequest::Command(RunAs::Root),
    )
    .unwrap_err();
    assert_eq!(root_error.code, ErrorCode::RunAsUnavailable);

    let identity = RuntimeIdentity {
        owner: RuntimeOwner {
            runtime_epoch: uuid("30000000-0000-4000-8000-000000000001"),
            host: RuntimeHost::ApkRuntime,
            host_generation: 7,
        },
        runtime_instance_id: uuid("30000000-0000-4000-8000-000000000002"),
    };
    let state = HostState::Active(identity.clone());
    let busy = state.clone().apply(HostEvent::BeginTransition {
        transition_id: uuid("30000000-0000-4000-8000-000000000003"),
        target_host: RuntimeHost::MagiskBackend,
        outstanding: OutstandingWork {
            tasks: 1,
            ..OutstandingWork::default()
        },
    });
    assert_eq!(busy.unwrap_err().code, ErrorCode::HostTransitionPending);
    let state = state
        .apply(HostEvent::BeginTransition {
            transition_id: uuid("30000000-0000-4000-8000-000000000003"),
            target_host: RuntimeHost::MagiskBackend,
            outstanding: OutstandingWork::default(),
        })
        .unwrap()
        .apply(HostEvent::ReleaseSource)
        .unwrap()
        .apply(HostEvent::CommitOwner)
        .unwrap()
        .apply(HostEvent::ActivateTarget {
            runtime_instance_id: uuid("30000000-0000-4000-8000-000000000004"),
        })
        .unwrap();
    assert!(state.validate_fence(&identity).is_err());
    assert!(state.validate_admission_fence(admitted.fence()).is_err());
    match state {
        HostState::Active(current) => {
            assert_eq!(current.owner.host, RuntimeHost::MagiskBackend);
            assert_eq!(current.owner.host_generation, 8);
        }
        _ => panic!("target must be active"),
    }

    let mut task = TaskLifecycle::new();
    assert_eq!(task.apply(TaskEvent::Queue).unwrap(), TaskState::Queued);
    assert_eq!(task.apply(TaskEvent::Start).unwrap(), TaskState::Running);
    assert_eq!(
        task.apply(TaskEvent::Complete {
            postcondition_verified: true,
            cleanup_verified: false,
        })
        .unwrap(),
        TaskState::Interrupted
    );
    assert!(task.apply(TaskEvent::Start).is_err());
}

#[test]
fn i2_g02_automation_overlap_snapshots_and_deletion_are_deterministic() {
    let action = AutomationAction::Sequence {
        children: vec![
            AutomationAction::Delay { duration_ms: 1 },
            AutomationAction::Repeat {
                count: 3,
                action: Box::new(AutomationAction::Delay { duration_ms: 2 }),
                delay_ms: 0,
            },
        ],
    };
    let metrics = validate_automation(&automation(action.clone())).unwrap();
    assert_eq!(metrics.depth, 3);
    assert_eq!(metrics.nodes, 4);
    assert_eq!(metrics.expanded_visits, 6);

    let mut slot = AutomationSlot::new(automation(action)).unwrap();
    let execution_id = uuid("20000000-0000-4000-8000-000000000002");
    let task_id = uuid("20000000-0000-4000-8000-000000000003");
    let snapshot = match slot.admit(execution_id.clone(), task_id.clone()).unwrap() {
        AdmissionDecision::Admitted(snapshot) => *snapshot,
        AdmissionDecision::BusyDropped => panic!("first execution must be admitted"),
    };
    assert_eq!(
        slot.admit(
            uuid("20000000-0000-4000-8000-000000000004"),
            uuid("20000000-0000-4000-8000-000000000005")
        )
        .unwrap(),
        AdmissionDecision::BusyDropped
    );

    let revision = slot
        .update_definition(
            1,
            "updated".to_owned(),
            false,
            AutomationTrigger::Interval { every_ms: 60_000 },
            AutomationAction::Delay { duration_ms: 5 },
            "2026-09-06T00:00:01.000Z".to_owned(),
        )
        .unwrap();
    assert_eq!(revision, 2);
    assert_eq!(slot.active(), Some(&snapshot));

    assert_eq!(
        slot.delete(2, "2026-09-06T00:00:02.000Z".to_owned())
            .unwrap(),
        DeleteDisposition::Tombstoned
    );
    assert!(slot.visible().is_none());
    slot.set_state(
        &snapshot.automation_id,
        &execution_id,
        "done".to_owned(),
        ScalarValue::Boolean(true),
    )
    .unwrap();
    assert_eq!(
        slot.settle(&execution_id).unwrap(),
        SettlementDisposition::PurgedTombstone
    );
    assert!(slot.active().is_none());
    assert!(slot.visible().is_none());

    let excessive = automation(AutomationAction::Repeat {
        count: 1000,
        action: Box::new(AutomationAction::Repeat {
            count: 1000,
            action: Box::new(AutomationAction::Delay { duration_ms: 1 }),
            delay_ms: 0,
        }),
        delay_ms: 0,
    });
    assert_eq!(
        validate_automation(&excessive).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
}

#[test]
fn automation_conditions_are_strict_and_missing_values_do_not_coerce() {
    let values = BTreeMap::from([
        ("count".to_owned(), ScalarValue::Integer(2)),
        ("text".to_owned(), ScalarValue::String("2".to_owned())),
        ("null".to_owned(), ScalarValue::Null),
    ]);
    let condition = |key: &str, operator, value| AutomationCondition {
        source: ConditionSource::State,
        key: key.to_owned(),
        operator,
        value,
    };
    assert!(evaluate_condition(
        &condition("null", ConditionOperator::Exists, None),
        &values
    ));
    assert!(evaluate_condition(
        &condition(
            "count",
            ConditionOperator::GreaterThan,
            Some(ScalarValue::Integer(1))
        ),
        &values
    ));
    assert!(!evaluate_condition(
        &condition(
            "text",
            ConditionOperator::Equals,
            Some(ScalarValue::Integer(2))
        ),
        &values
    ));
    assert!(!evaluate_condition(
        &condition(
            "missing",
            ConditionOperator::NotEquals,
            Some(ScalarValue::Integer(2))
        ),
        &values
    ));
}

#[test]
fn automation_calls_reuse_public_input_bounds() {
    let invalid = automation(AutomationAction::Call {
        call: AutomationCompatibleCall::Command {
            call: AutomationCommandCall::Run(CommandRunInput {
                command: "true".to_owned(),
                run_as: RunAs::App,
                cwd: None,
                stdin: None,
                timeout_ms: 150_001,
                max_output_bytes: 1024,
                as_task: false,
            }),
        },
    });
    assert_eq!(
        validate_automation(&invalid).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
}

#[test]
fn dedup_is_bounded_content_addressed_and_cancel_bypasses_retention() {
    let request = uuid("40000000-0000-4000-8000-000000000001");
    let mut index = DedupIndex::default();
    let digest = "a".repeat(64);
    assert_eq!(
        index
            .decide_and_reserve(request.clone(), digest.clone(), 1, false)
            .unwrap(),
        DedupDecision::Admit
    );
    assert_eq!(
        index
            .decide_and_reserve(request.clone(), digest.clone(), 100_000_000, false)
            .unwrap(),
        DedupDecision::Replay
    );
    assert_eq!(
        index
            .decide_and_reserve(request.clone(), "b".repeat(64), 1, false)
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(index.settle(&request, 100_000_000).unwrap(), 186_400_000);
    assert_eq!(index.settle(&request, 100_000_001).unwrap(), 186_400_000);
    assert_eq!(
        index
            .decide_and_reserve(request.clone(), digest, 186_399_999, false)
            .unwrap(),
        DedupDecision::Replay
    );
    assert_eq!(
        index
            .decide_and_reserve(request.clone(), "c".repeat(64), 186_400_000, false)
            .unwrap(),
        DedupDecision::Admit
    );
    assert_eq!(
        index
            .decide_and_reserve(request, "not-a-digest".to_owned(), 1, true)
            .unwrap(),
        DedupDecision::BypassForTaskCancel
    );
}

#[test]
fn executor_preflight_and_android_visibility_never_guess_success() {
    use CapabilityState::{Available, Unavailable, Unknown};
    let mut facts = resolver_facts(Unavailable);
    facts.app_native = Available;
    facts.shizuku = Unknown;
    let denied = resolve_executor(
        RuntimeHost::ApkRuntime,
        fence(),
        facts,
        ExecutorRequest::Filesystem {
            route: FilesystemRoute::Mutation,
            target_type: contract::FileTargetType::Path,
            app_preflight: Preflight::Unknown,
            shizuku_preflight: Preflight::Unknown,
        },
    )
    .unwrap_err();
    assert_eq!(denied.code, ErrorCode::CapabilityUnavailable);

    facts.shizuku = Available;
    let package = resolve_executor(
        RuntimeHost::ApkRuntime,
        fence(),
        facts,
        ExecutorRequest::Android(AndroidRoute::PackageInspect(
            PackageInspectFact::VisibilityOrAbsent,
        )),
    )
    .unwrap();
    assert_eq!(package.provider(), Provider::Shizuku);
    assert_eq!(package.execution_class(), ExecutionClass::Shizuku);

    let unsupported = resolve_executor(
        RuntimeHost::MagiskBackend,
        fence(),
        resolver_facts(Available),
        ExecutorRequest::Filesystem {
            route: FilesystemRoute::Mutation,
            target_type: contract::FileTargetType::ContentUri,
            app_preflight: Preflight::Positive,
            shizuku_preflight: Preflight::Positive,
        },
    )
    .unwrap_err();
    assert_eq!(unsupported.code, ErrorCode::Unsupported);
}
