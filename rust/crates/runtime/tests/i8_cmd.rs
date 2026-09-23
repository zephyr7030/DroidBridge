//! I8-CMD: the two host Command surfaces over one shared semantic handler.
//!
//! Every test here drives `NativeCommandExecutionSurface` exactly as the APK and daemon
//! hosts do, with a scripted process primitive standing in for the App guard runner, the
//! typed Shizuku process primitive and the daemon root runner. That isolates the shared
//! handler's public meaning from the three transport mechanics, which is what the node's
//! handoff gate claims: one result/error projection, exact requested/actual identity,
//! Shizuku only as the APK surface's `shell` primitive provider, and one owner for
//! timeout, cancellation, reap and output.

use contract::{
    Availability, CapabilityState, CommandCall, CommandFailureCode, CommandRunInput,
    CommandTerminalState, ErrorCode, ExecutionClass, GrantFacts, RunAs, RuntimeHost,
    RuntimeReadiness, TaskTerminalResult, UuidV4,
};
use domain::{
    AdmissionFence, CapabilityContext, DomainError, ExecutorRequest, ProviderGenerations,
    ResolverFacts,
};
use runtime::{
    AdmittedExecution, AndroidCommandSettlement, ArtifactPort, CapabilitySnapshot,
    CommandProcessCause, CommandProcessOutcome, CommandProcessPort, CommandProcessRequest,
    CommandProcessSettlement, ExecutionCancelOutcome, ExecutionCompletion, ExecutionFailure,
    ExecutionOutcome, ExecutionPayload, ExecutionPort, ExecutorRecord, LocalExecutionClaim,
    LocalExecutionClaims, NativeCommandExecutionSurface, ProviderToken, RESERVE_FLOOR_BYTES,
    RecoveryProof, RuntimeCore, SynchronousAdmission, TaskAdmission, command_executor_request,
    command_settlement_bound_bytes,
    fakes::{FakeArtifacts, FakeCapabilities, FakeExecutions, FakeHostControl, FakePersistence},
    validate_command_input, validate_command_process_request,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration as StdDuration,
};

/// The APK surface owns `app` and `shell`; the Magisk surface owns `root` and forwards the
/// other two (S-AUTH-CMD-001).
static APK_IDENTITIES: [RunAs; 2] = [RunAs::App, RunAs::Shell];
static MAGISK_IDENTITIES: [RunAs; 3] = [RunAs::Root, RunAs::App, RunAs::Shell];

const APK_INSTANCE: u64 = 0x11;
const MAGISK_INSTANCE: u64 = 0x22;

fn uuid(prefix: u32, value: u64) -> UuidV4 {
    UuidV4::parse(format!("{prefix:08x}-0000-4000-8000-{value:012x}")).unwrap()
}

const fn availability(state: CapabilityState) -> Availability {
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

/// The provider facts one host sees. `app_execution_surface` is the authenticated APK
/// surface's reachability, which is what S-AUTH-002 makes `command.app` depend on.
#[derive(Clone, Copy)]
struct Facts {
    host: RuntimeHost,
    app_native: CapabilityState,
    shizuku: CapabilityState,
    magisk_native: CapabilityState,
    app_execution_surface: CapabilityState,
    host_generation: u64,
    shizuku_generation: u64,
    magisk_generation: u64,
    instance: u64,
}

fn apk_facts(instance: u64) -> Facts {
    Facts {
        host: RuntimeHost::ApkRuntime,
        app_native: CapabilityState::Available,
        shizuku: CapabilityState::Available,
        magisk_native: CapabilityState::Available,
        app_execution_surface: CapabilityState::Available,
        host_generation: 4,
        shizuku_generation: 9_001,
        magisk_generation: 9_002,
        instance,
    }
}

fn magisk_facts(instance: u64) -> Facts {
    Facts {
        host: RuntimeHost::MagiskBackend,
        app_native: CapabilityState::Available,
        shizuku: CapabilityState::Available,
        magisk_native: CapabilityState::Available,
        app_execution_surface: CapabilityState::Available,
        host_generation: 7,
        shizuku_generation: 9_001,
        magisk_generation: 9_002,
        instance,
    }
}

fn capability(facts: Facts) -> CapabilitySnapshot {
    CapabilitySnapshot {
        grants: grants(CapabilityState::Available),
        context: CapabilityContext {
            sdk_int: 37,
            host: facts.host,
            readiness: RuntimeReadiness::Ready,
            app_execution_surface: facts.app_execution_surface,
        },
        resolver_facts: ResolverFacts {
            app_native: facts.app_native,
            app_framework: facts.app_execution_surface,
            shizuku: facts.shizuku,
            magisk_native: facts.magisk_native,
            magisk_framework: CapabilityState::Available,
            magisk_launch: CapabilityState::Available,
            magisk_clipboard: CapabilityState::Available,
            magisk_notifications: CapabilityState::Available,
            accessibility: CapabilityState::Available,
            media_projection: CapabilityState::Available,
            notification_listener: CapabilityState::Available,
            generations: ProviderGenerations {
                app_native: facts.host_generation,
                app_framework: facts.host_generation,
                shizuku: facts.shizuku_generation,
                magisk_native: facts.magisk_generation,
                magisk_framework: facts.host_generation,
                accessibility: facts.host_generation,
                media_projection: facts.host_generation,
                notification_listener: facts.host_generation,
            },
        },
        fence: AdmissionFence {
            runtime_epoch: uuid(0x8100_0000, 1),
            host_generation: facts.host_generation,
            runtime_instance_id: uuid(0x8100_0000, facts.instance),
        },
    }
}

fn provider_generation(facts: Facts, provider: ProviderToken) -> u64 {
    match provider {
        ProviderToken::AppNative => facts.host_generation,
        ProviderToken::Shizuku => facts.shizuku_generation,
        ProviderToken::MagiskNative => facts.magisk_generation,
        other => panic!("no generation fixture for {other:?}"),
    }
}

fn provider_of(run_as: RunAs) -> ProviderToken {
    match run_as {
        RunAs::App => ProviderToken::AppNative,
        RunAs::Shell => ProviderToken::Shizuku,
        RunAs::Root => ProviderToken::MagiskNative,
    }
}

fn run_call(run_as: RunAs, as_task: bool) -> CommandCall {
    CommandCall::Run(CommandRunInput {
        command: "printf 'hello'".to_owned(),
        run_as,
        cwd: Some("/data/local/tmp".to_owned()),
        stdin: None,
        timeout_ms: 30_000,
        max_output_bytes: 65_536,
        as_task,
    })
}

fn call_run_as(call: &CommandCall) -> RunAs {
    let CommandCall::Run(input) = call;
    input.run_as
}

/// Admits one Command exactly as `RuntimeCore` does, so the executor record carries the
/// provider, class and generation the handler later re-checks.
fn admitted(facts: Facts, call: &CommandCall, instance: u64) -> AdmittedExecution {
    let capability = capability(facts);
    let CommandCall::Run(input) = call;
    let mut resolver_facts = capability.resolver_facts;
    resolver_facts.app_native = capability.context.app_execution_surface;
    resolver_facts.generations.app_native = capability.fence.host_generation;
    let executor = domain::resolve_executor(
        capability.context.host,
        capability.fence.clone(),
        resolver_facts,
        ExecutorRequest::Command(input.run_as),
    )
    .expect("the fixture admits the requested identity");
    AdmittedExecution {
        execution_id: uuid(0x8500_0000, instance),
        task_id: None,
        executor: ExecutorRecord::from(&executor),
        payload: ExecutionPayload::CommandCall(call.clone()),
    }
}

/// The executor record an already-admitted Command carries, rewritten to the identity and
/// provider under test, so a mismatch after admission is observable without inventing an
/// admission the resolver would refuse.
fn mismatched(
    facts: Facts,
    call: &CommandCall,
    provider: ProviderToken,
    instance: u64,
) -> AdmittedExecution {
    let mut execution = admitted(facts, &run_call(RunAs::App, false), instance);
    execution.payload = ExecutionPayload::CommandCall(call.clone());
    execution.executor.provider = provider;
    execution
}

fn outcome(cause: CommandProcessCause, exit_code: Option<i32>) -> CommandProcessOutcome {
    CommandProcessOutcome {
        cause,
        exit_code,
        stdout: b"hello\n".to_vec(),
        stdout_truncated: false,
        stderr: Vec::new(),
        stderr_truncated: false,
    }
}

fn exited(exit_code: i32) -> CommandProcessSettlement {
    CommandProcessSettlement {
        outcome: outcome(CommandProcessCause::Exited, Some(exit_code)),
        cleanup_verified: true,
    }
}

/// One scripted process primitive. It records every request, returns the scripted
/// settlement for the admitted identity, and can hold a run open until its claim is
/// cancelled — which is what a real runner does while it polls that claim.
#[derive(Clone, Default)]
struct ScriptedPort {
    state: Arc<PortState>,
}

#[derive(Default)]
struct PortState {
    scripts: Mutex<BTreeMap<String, Result<CommandProcessSettlement, ExecutionFailure>>>,
    requests: Mutex<Vec<CommandProcessRequest>>,
    entered: Mutex<usize>,
    hold: Mutex<bool>,
}

fn key(run_as: RunAs) -> String {
    format!("{run_as:?}")
}

impl ScriptedPort {
    fn new() -> Self {
        Self::default()
    }

    fn script(&self, run_as: RunAs, result: Result<CommandProcessSettlement, ExecutionFailure>) {
        self.state
            .scripts
            .lock()
            .expect("script lock")
            .insert(key(run_as), result);
    }

    fn requests(&self) -> Vec<CommandProcessRequest> {
        self.state.requests.lock().expect("request lock").clone()
    }

    fn entered(&self) -> usize {
        *self.state.entered.lock().expect("entry lock")
    }

    fn hold(&self) {
        *self.state.hold.lock().expect("hold lock") = true;
    }
}

impl CommandProcessPort for ScriptedPort {
    fn run(
        &self,
        _execution: &AdmittedExecution,
        request: CommandProcessRequest,
        claim: &LocalExecutionClaim,
    ) -> Result<CommandProcessSettlement, ExecutionFailure> {
        self.state
            .requests
            .lock()
            .expect("request lock")
            .push(request.clone());
        *self.state.entered.lock().expect("entry lock") += 1;
        if *self.state.hold.lock().expect("hold lock") {
            // A held run settles only through the claim it was given, like every real
            // runner: cancellation is observed, never injected.
            loop {
                if claim.checkpoint().is_err() {
                    return Err(ExecutionFailure {
                        error: DomainError::new(ErrorCode::Cancelled, "execution was cancelled"),
                        cleanup_verified: true,
                    });
                }
                std::thread::sleep(StdDuration::from_millis(5));
            }
        }
        match self
            .state
            .scripts
            .lock()
            .expect("script lock")
            .get(&key(request.run_as))
        {
            Some(Ok(scripted)) => Ok(scripted.clone()),
            Some(Err(failure)) => Err(failure.clone()),
            None => Err(ExecutionFailure {
                error: DomainError::new(
                    ErrorCode::InternalError,
                    "the scripted process primitive was not given a settlement",
                ),
                cleanup_verified: true,
            }),
        }
    }
}

type Surface = NativeCommandExecutionSurface<FakeArtifacts, FakeCapabilities, ScriptedPort>;

fn surface(
    capabilities: FakeCapabilities,
    identities: &'static [RunAs],
    port: ScriptedPort,
) -> Surface {
    NativeCommandExecutionSurface::new(FakeArtifacts::default(), capabilities, identities, port)
}

async fn start(
    surface: &Surface,
    execution: AdmittedExecution,
) -> Result<ExecutionCompletion, ExecutionFailure> {
    surface.claim_and_start(execution).await
}

fn apk_capabilities() -> FakeCapabilities {
    FakeCapabilities::new(capability(apk_facts(APK_INSTANCE)))
}

fn magisk_capabilities() -> FakeCapabilities {
    FakeCapabilities::new(capability(magisk_facts(MAGISK_INSTANCE)))
}

fn capabilities_of(facts: Facts) -> FakeCapabilities {
    FakeCapabilities::new(capability(facts))
}

fn identities_of(facts: Facts) -> &'static [RunAs] {
    match facts.host {
        RuntimeHost::ApkRuntime => &APK_IDENTITIES,
        _ => &MAGISK_IDENTITIES,
    }
}

/// The synchronous public result of one admitted Command, or a failure for anything that
/// is not a terminal command result.
fn public_result(completion: &ExecutionCompletion) -> &serde_json::Value {
    match &completion.outcome {
        ExecutionOutcome::SynchronousCompleted { result, .. } => result,
        other => panic!("expected a synchronous command result, found {other:?}"),
    }
}

/// `duration_ms` is the measured wall clock of the run, so it is the one field the two
/// surfaces cannot share; every other field is public meaning.
fn comparable(value: &serde_json::Value) -> serde_json::Value {
    let mut value = value.clone();
    value
        .as_object_mut()
        .expect("command result is an object")
        .remove("duration_ms");
    value
}

/// Gate 01: both host Command surfaces project one public result and error semantics.
#[tokio::test]
async fn i8_cmd_g01_both_host_surfaces_share_one_public_result_and_error_semantics() {
    let apk = apk_facts(APK_INSTANCE);
    let magisk = magisk_facts(MAGISK_INSTANCE);

    let scripted_cases = [
        (RunAs::App, exited(0)),
        (RunAs::Shell, exited(0)),
        (RunAs::App, exited(7)),
        (
            RunAs::Shell,
            CommandProcessSettlement {
                outcome: outcome(CommandProcessCause::Timeout, None),
                cleanup_verified: true,
            },
        ),
    ];
    for (run_as, scripted) in scripted_cases {
        let apk_port = ScriptedPort::new();
        apk_port.script(run_as, Ok(scripted.clone()));
        let magisk_port = ScriptedPort::new();
        magisk_port.script(run_as, Ok(scripted.clone()));

        let call = run_call(run_as, false);
        let apk_surface = surface(apk_capabilities(), &APK_IDENTITIES, apk_port.clone());
        let magisk_surface = surface(
            magisk_capabilities(),
            &MAGISK_IDENTITIES,
            magisk_port.clone(),
        );

        let from_apk = start(&apk_surface, admitted(apk, &call, 1)).await.unwrap();
        let from_magisk = start(&magisk_surface, admitted(magisk, &call, 2))
            .await
            .unwrap();
        assert_eq!(
            comparable(public_result(&from_apk)),
            comparable(public_result(&from_magisk)),
            "the two host surfaces diverged on the public result for {run_as:?}"
        );

        let expected = match scripted.outcome.cause {
            CommandProcessCause::Exited if scripted.outcome.exit_code == Some(0) => {
                ("completed", None, Some(0))
            }
            CommandProcessCause::Exited => ("failed", Some("EXECUTION_FAILED"), Some(7)),
            CommandProcessCause::Timeout => ("failed", Some("TIMEOUT"), None),
            other => panic!("unsupported scripted cause {other:?}"),
        };
        let result = public_result(&from_apk);
        assert_eq!(result["state"], expected.0);
        assert_eq!(
            result.get("failure_code").and_then(|value| value.as_str()),
            expected.1
        );
        assert_eq!(
            result.get("exit_code").and_then(|value| value.as_i64()),
            expected.2.map(i64::from)
        );
        assert_eq!(result["stdout"], "hello\n");
        assert_eq!(result["stderr"], "");
        assert_eq!(result["stdout_truncated"], false);
        assert_eq!(result["stderr_truncated"], false);
        assert!(result.get("stdout_ref").is_none());
        assert!(result.get("stderr_ref").is_none());
        assert_eq!(
            result["requested_run_as"],
            format!("{run_as:?}").to_lowercase()
        );
        assert_eq!(result["actual_run_as"], result["requested_run_as"]);

        // The program, the argv and every remaining bound are fixed by the shared handler
        // on both surfaces, so neither host can reinterpret the shell text.
        for port in [apk_port, magisk_port] {
            let requests = port.requests();
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].run_as, run_as);
            assert_eq!(requests[0].program, "/system/bin/sh");
            assert_eq!(requests[0].arguments, vec!["-c", "printf 'hello'"]);
            assert_eq!(requests[0].cwd.as_deref(), Some("/data/local/tmp"));
            assert_eq!(requests[0].timeout_ms, 30_000);
            assert_eq!(requests[0].max_output_bytes, 65_536);
            assert_eq!(requests[0].stdin, None);
        }
    }

    // An unverified cleanup, and an owner lost before verified cleanup, are the same
    // structured failure on both surfaces and never a terminal command result.
    for run_as in [RunAs::App, RunAs::Shell] {
        let call = run_call(run_as, false);
        let unverified_causes = [CommandProcessCause::Exited, CommandProcessCause::OwnerLost];
        for cause in unverified_causes {
            let unverified = CommandProcessSettlement {
                outcome: outcome(cause, (cause == CommandProcessCause::Exited).then_some(0)),
                cleanup_verified: false,
            };
            let apk_port = ScriptedPort::new();
            apk_port.script(run_as, Ok(unverified.clone()));
            let magisk_port = ScriptedPort::new();
            magisk_port.script(run_as, Ok(unverified));
            let apk_surface = surface(apk_capabilities(), &APK_IDENTITIES, apk_port);
            let magisk_surface = surface(magisk_capabilities(), &MAGISK_IDENTITIES, magisk_port);
            let from_apk = start(&apk_surface, admitted(apk, &call, 3))
                .await
                .unwrap_err();
            let from_magisk = start(&magisk_surface, admitted(magisk, &call, 4))
                .await
                .unwrap_err();
            assert_eq!(from_apk.error.code, ErrorCode::IoError);
            assert_eq!(from_magisk.error.code, from_apk.error.code);
            assert!(!from_apk.cleanup_verified);
            assert_eq!(from_magisk.cleanup_verified, from_apk.cleanup_verified);
        }

        // A cancelled process is the one non-terminal settlement: both surfaces report the
        // same cancellation and carry the same cleanup uncertainty with it.
        let unverified = CommandProcessSettlement {
            outcome: outcome(CommandProcessCause::Cancelled, None),
            cleanup_verified: false,
        };
        let apk_port = ScriptedPort::new();
        apk_port.script(run_as, Ok(unverified.clone()));
        let magisk_port = ScriptedPort::new();
        magisk_port.script(run_as, Ok(unverified));
        let apk_surface = surface(apk_capabilities(), &APK_IDENTITIES, apk_port);
        let magisk_surface = surface(magisk_capabilities(), &MAGISK_IDENTITIES, magisk_port);
        let from_apk = start(&apk_surface, admitted(apk, &call, 7)).await.unwrap();
        let from_magisk = start(&magisk_surface, admitted(magisk, &call, 8))
            .await
            .unwrap();
        for completion in [&from_apk, &from_magisk] {
            assert!(!completion.cleanup_verified);
            match &completion.outcome {
                ExecutionOutcome::Cancelled { error, .. } => {
                    assert_eq!(error.code, ErrorCode::Cancelled);
                    assert_eq!(error.operation, "command.run");
                }
                other => panic!("expected a cancellation, found {other:?}"),
            }
        }
    }

    // An out-of-range request is INVALID_ARGUMENT before either surface reaches a process
    // primitive.
    let mut invalid = run_call(RunAs::App, false);
    let CommandCall::Run(input) = &mut invalid;
    input.timeout_ms = 1_000_000;
    assert_eq!(
        validate_command_input(&invalid).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    let probing = ScriptedPort::new();
    probing.script(RunAs::App, Ok(exited(0)));
    let apk_surface = surface(apk_capabilities(), &APK_IDENTITIES, probing.clone());
    let failure = start(&apk_surface, admitted(apk, &invalid, 5))
        .await
        .unwrap_err();
    assert_eq!(failure.error.code, ErrorCode::InvalidArgument);
    assert!(failure.cleanup_verified);
    assert!(probing.requests().is_empty());

    // `as_task=true` keeps the complete R-CMD-006 result inside the Task terminal shape.
    let port = ScriptedPort::new();
    port.script(RunAs::App, Ok(exited(3)));
    let task_call = run_call(RunAs::App, true);
    let apk_surface = surface(apk_capabilities(), &APK_IDENTITIES, port);
    let mut execution = admitted(apk, &task_call, 6);
    execution.task_id = Some(uuid(0x8600_0000, 6));
    let completion = start(&apk_surface, execution).await.unwrap();
    match &completion.outcome {
        ExecutionOutcome::Completed { result, .. } => match result {
            TaskTerminalResult::Command(result) => {
                assert_eq!(result.state, CommandTerminalState::Failed);
                assert_eq!(
                    result.failure_code,
                    Some(CommandFailureCode::ExecutionFailed)
                );
                assert_eq!(result.exit_code, Some(3));
                assert_eq!(result.requested_run_as, RunAs::App);
                assert_eq!(result.actual_run_as, RunAs::App);
                assert_eq!(result.execution_class, ExecutionClass::App);
                assert_eq!(result.stdout.as_deref(), Some("hello\n"));
            }
            other => panic!("expected a Command task result, found {other:?}"),
        },
        other => panic!("expected a completed Task, found {other:?}"),
    }
}

/// Gate 02: the reported requested and actual identity is exactly the admitted one.
#[tokio::test]
async fn i8_cmd_g02_requested_and_actual_identity_are_exact_on_each_surface() {
    let apk = apk_facts(APK_INSTANCE);
    let magisk = magisk_facts(MAGISK_INSTANCE);

    let cases: [(Facts, RunAs, &str, ProviderToken); 3] = [
        (apk, RunAs::App, "app", ProviderToken::AppNative),
        (apk, RunAs::Shell, "shell", ProviderToken::Shizuku),
        (magisk, RunAs::Root, "root", ProviderToken::MagiskNative),
    ];
    for (facts, run_as, identity, provider) in cases {
        let call = run_call(run_as, false);
        let execution = admitted(facts, &call, 10);
        assert_eq!(
            execution.executor.provider, provider,
            "{run_as:?} resolved to the wrong provider on {:?}",
            facts.host
        );
        assert_eq!(
            execution.executor.capability_generation,
            provider_generation(facts, provider)
        );
        let port = ScriptedPort::new();
        port.script(run_as, Ok(exited(0)));
        let runner = surface(capabilities_of(facts), identities_of(facts), port.clone());
        let result = public_result(&start(&runner, execution).await.unwrap()).clone();
        assert_eq!(result["requested_run_as"], identity);
        assert_eq!(result["actual_run_as"], identity);
        assert_eq!(port.requests()[0].run_as, run_as);
    }

    // The Magisk surface forwards the two APK identities with their own execution class
    // instead of reporting its own.
    for (run_as, class) in [(RunAs::App, "app"), (RunAs::Shell, "shizuku")] {
        let call = run_call(run_as, false);
        let port = ScriptedPort::new();
        port.script(run_as, Ok(exited(0)));
        let runner = surface(magisk_capabilities(), &MAGISK_IDENTITIES, port);
        let result =
            public_result(&start(&runner, admitted(magisk, &call, 11)).await.unwrap()).clone();
        assert_eq!(
            result["requested_run_as"],
            format!("{run_as:?}").to_lowercase()
        );
        assert_eq!(result["actual_run_as"], result["requested_run_as"]);
        assert_eq!(result["execution_class"], class);
    }

    // `root` is unavailable on the APK surface, and it is refused rather than falling
    // through to another identity.
    let apk_port = ScriptedPort::new();
    apk_port.script(RunAs::Root, Ok(exited(0)));
    let root_call = run_call(RunAs::Root, false);
    assert_eq!(
        command_executor_request(&capability(apk), &root_call)
            .unwrap_err()
            .code,
        ErrorCode::RunAsUnavailable
    );
    let mut root_execution = admitted(apk, &run_call(RunAs::App, false), 12);
    root_execution.payload = ExecutionPayload::CommandCall(root_call);
    let runner = surface(apk_capabilities(), &APK_IDENTITIES, apk_port.clone());
    let failure = start(&runner, root_execution).await.unwrap_err();
    assert_eq!(failure.error.code, ErrorCode::StaleAuthority);
    assert!(failure.cleanup_verified);
    assert!(apk_port.requests().is_empty());

    // An already-admitted execution whose record names a different provider than the
    // requested identity is stale authority, not an identity fallback.
    let mismatches = [
        (apk, run_call(RunAs::App, false), ProviderToken::Shizuku, 13),
        (
            magisk,
            run_call(RunAs::App, false),
            ProviderToken::MagiskNative,
            14,
        ),
        (
            magisk,
            run_call(RunAs::Shell, false),
            ProviderToken::AppNative,
            15,
        ),
        (
            apk,
            run_call(RunAs::Shell, false),
            ProviderToken::MagiskNative,
            16,
        ),
        (
            magisk,
            run_call(RunAs::Root, false),
            ProviderToken::AppNative,
            17,
        ),
    ];
    for (facts, call, provider, instance) in mismatches {
        let port = ScriptedPort::new();
        port.script(call_run_as(&call), Ok(exited(0)));
        port.script(RunAs::Root, Ok(exited(0)));
        let runner = surface(capabilities_of(facts), identities_of(facts), port.clone());
        let execution = mismatched(facts, &call, provider, instance);
        let failure = start(&runner, execution).await.unwrap_err();
        assert_eq!(failure.error.code, ErrorCode::StaleAuthority);
        assert!(failure.cleanup_verified);
        assert!(port.requests().is_empty());
    }
}

/// Gate 03: Shizuku stays the APK surface's `shell` process primitive, so it never
/// becomes a third Command implementation and no host falls back to another identity.
#[tokio::test]
async fn i8_cmd_g03_shizuku_is_only_the_apk_surfaces_shell_primitive_provider() {
    let apk = apk_facts(APK_INSTANCE);
    let magisk = magisk_facts(MAGISK_INSTANCE);

    // `run_as=shell` admits the Shizuku provider on both hosts, and nothing else.
    for facts in [apk, magisk] {
        let execution = admitted(facts, &run_call(RunAs::Shell, false), 20);
        assert_eq!(execution.executor.provider, ProviderToken::Shizuku);
        assert_eq!(execution.executor.execution_class, ExecutionClass::Shizuku);
        assert_eq!(
            execution.executor.capability_generation,
            provider_generation(facts, ProviderToken::Shizuku)
        );
    }

    // Without `shizuku.shell`, `run_as=shell` is unavailable even where `root` is
    // available: the Magisk surface never impersonates the shell identity itself.
    let without_shizuku = Facts {
        shizuku: CapabilityState::Unavailable,
        ..magisk
    };
    assert_eq!(
        command_executor_request(&capability(without_shizuku), &run_call(RunAs::Shell, false))
            .unwrap_err()
            .code,
        ErrorCode::CapabilityUnavailable
    );
    assert!(
        command_executor_request(&capability(without_shizuku), &run_call(RunAs::Root, false))
            .is_ok()
    );

    // Without `execution.app_guard` (and therefore no live App execution surface),
    // `run_as=app` is unavailable even though the root provider is available.
    let without_app = Facts {
        app_native: CapabilityState::Unavailable,
        app_execution_surface: CapabilityState::Unavailable,
        ..magisk
    };
    assert_eq!(
        command_executor_request(&capability(without_app), &run_call(RunAs::App, false))
            .unwrap_err()
            .code,
        ErrorCode::CapabilityUnavailable
    );
    assert!(
        command_executor_request(&capability(without_app), &run_call(RunAs::Root, false)).is_ok()
    );

    // Withdrawing a forwarded identity's authority after admission is stale authority
    // rather than a fallback to `root`, which is still available on that host.
    for run_as in [RunAs::App, RunAs::Shell] {
        let withdrawn = Facts {
            app_native: CapabilityState::Unavailable,
            app_execution_surface: CapabilityState::Unavailable,
            shizuku: CapabilityState::Unavailable,
            ..magisk
        };
        let port = ScriptedPort::new();
        port.script(run_as, Ok(exited(0)));
        port.script(RunAs::Root, Ok(exited(0)));
        let runner = surface(capabilities_of(withdrawn), &MAGISK_IDENTITIES, port.clone());
        let call = run_call(run_as, false);
        let execution = admitted(magisk, &call, 21);
        assert_eq!(execution.executor.provider, provider_of(run_as));
        let failure = start(&runner, execution).await.unwrap_err();
        assert_eq!(failure.error.code, ErrorCode::StaleAuthority);
        assert!(port.requests().is_empty());
    }

    // No host surface realizes `Shizuku` for a non-shell identity, and the APK surface
    // realizes neither `root` nor a Shizuku identity it was not admitted for.
    let wrong_identities = [
        (
            magisk,
            run_call(RunAs::Root, false),
            ProviderToken::Shizuku,
            22,
        ),
        (
            apk,
            run_call(RunAs::Root, false),
            ProviderToken::MagiskNative,
            23,
        ),
        (
            apk,
            run_call(RunAs::Shell, false),
            ProviderToken::AppNative,
            24,
        ),
        (
            magisk,
            run_call(RunAs::Root, false),
            ProviderToken::AppNative,
            25,
        ),
    ];
    for (facts, call, provider, instance) in wrong_identities {
        let port = ScriptedPort::new();
        port.script(RunAs::Root, Ok(exited(0)));
        port.script(RunAs::Shell, Ok(exited(0)));
        let runner = surface(capabilities_of(facts), identities_of(facts), port.clone());
        let execution = mismatched(facts, &call, provider, instance);
        let failure = start(&runner, execution).await.unwrap_err();
        assert_eq!(failure.error.code, ErrorCode::StaleAuthority);
        assert!(port.requests().is_empty());
    }

    // The Shizuku provider's generation is the one the resolver published for it, so a
    // stale `shizuku.shell` generation is stale authority rather than a fresh primitive.
    let port = ScriptedPort::new();
    port.script(RunAs::Shell, Ok(exited(0)));
    let mut stale = capability(magisk);
    stale.resolver_facts.generations.shizuku += 1;
    let runner = surface(
        FakeCapabilities::new(stale),
        &MAGISK_IDENTITIES,
        port.clone(),
    );
    let failure = start(
        &runner,
        admitted(magisk, &run_call(RunAs::Shell, false), 26),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.error.code, ErrorCode::StaleAuthority);
    assert!(port.requests().is_empty());
}

/// Gate 04: cancellation is observed through the one claim the runner already holds, and
/// the cancelled execution emits no competing command result.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn i8_cmd_g04_cancellation_reaches_the_runner_and_emits_no_competing_result() {
    let apk = apk_facts(APK_INSTANCE);
    let magisk = magisk_facts(MAGISK_INSTANCE);

    for facts in [apk, magisk] {
        let port = ScriptedPort::new();
        port.hold();
        let call = run_call(RunAs::App, false);
        let execution = admitted(facts, &call, 31);
        let execution_id = execution.execution_id.clone();
        let runner = Arc::new(surface(
            capabilities_of(facts),
            identities_of(facts),
            port.clone(),
        ));
        let owned = Arc::clone(&runner);
        let running = tokio::spawn(async move { owned.claim_and_start(execution).await });
        tokio::time::timeout(StdDuration::from_secs(5), async {
            while port.entered() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the process runner reached the claimed execution");

        let outcome = runner.cancel(&execution_id).await.unwrap();
        assert_eq!(
            outcome,
            ExecutionCancelOutcome::Cancelled {
                cleanup_verified: true
            },
            "cancellation must be the claim's outcome on {:?}",
            facts.host
        );
        let ExecutionCompletion {
            outcome,
            cleanup_verified,
            ..
        } = tokio::time::timeout(StdDuration::from_secs(5), running)
            .await
            .expect("the runner settled after cancellation")
            .unwrap()
            .unwrap();
        assert!(cleanup_verified);
        match outcome {
            ExecutionOutcome::Cancelled {
                error,
                encoded_bytes,
            } => {
                assert_eq!(error.code, ErrorCode::Cancelled);
                assert_eq!(error.operation, "command.run");
                assert!(!error.retryable);
                assert_eq!(encoded_bytes, RESERVE_FLOOR_BYTES);
            }
            other => panic!("cancellation emitted a competing result: {other:?}"),
        }
        // One settlement, one recorded process request, one claim.
        assert_eq!(port.requests().len(), 1);
        assert_eq!(port.entered(), 1);
        assert_eq!(
            runner.cancel(&execution_id).await.unwrap(),
            ExecutionCancelOutcome::CompletionWon
        );
    }
}

/// Gate 04: the synchronous cancellation entry point trips the same claim the runner
/// polls, and one claim admits one execution identity at a time.
#[tokio::test]
async fn i8_cmd_g04_request_cancel_trips_the_same_claim_the_runner_polls() {
    let claims = LocalExecutionClaims::default();
    let execution_id = uuid(0x8600_0000, 1);
    let claim = claims.claim(execution_id.clone()).unwrap();
    assert!(claims.contains(&execution_id));
    assert!(!claims.cancel_requested(&execution_id));
    assert!(claim.checkpoint().is_ok());

    // An unknown execution has nothing to cancel, which is what `cancel` reports as
    // `CompletionWon`.
    assert!(!claims.request_cancel(&uuid(0x8600_0000, 99)));
    assert!(!claims.cancel_requested(&uuid(0x8600_0000, 99)));

    // The runner's own poll observes the request made from another thread.
    assert!(claims.request_cancel(&execution_id));
    assert!(claims.cancel_requested(&execution_id));
    assert_eq!(claim.checkpoint().unwrap_err().code, ErrorCode::Cancelled);

    // Settlement releases the claim, so the identity becomes claimable again exactly once.
    claims.finish(&claim, true);
    assert!(!claims.contains(&execution_id));
    assert!(!claims.cancel_requested(&execution_id));
    assert!(!claims.request_cancel(&execution_id));
    let reclaimed = claims.claim(execution_id.clone()).unwrap();
    assert_eq!(reclaimed.execution_id(), &execution_id);
    let duplicate = claims
        .claim(execution_id)
        .err()
        .expect("a second claim on the same identity is refused");
    assert_eq!(duplicate.code, ErrorCode::InvalidArgument);
    assert!(reclaimed.cleanup_is_verified());
    assert!(reclaimed.checkpoint().is_ok());
}

/// Gate 04: a retained stream beyond the inline bound becomes exactly one artifact, and
/// the settlement the two surfaces exchange keeps every terminal cause distinct.
#[tokio::test]
async fn i8_cmd_g04_retained_output_has_one_artifact_owner_beyond_the_inline_bound() {
    let apk = apk_facts(APK_INSTANCE);
    let magisk = magisk_facts(MAGISK_INSTANCE);
    let long_stdout = vec![b'a'; 70_000];
    let long_stderr = vec![b'b'; 70_000];

    for facts in [apk, magisk] {
        let artifacts = FakeArtifacts::default();
        let port = ScriptedPort::new();
        port.script(
            RunAs::App,
            Ok(CommandProcessSettlement {
                outcome: CommandProcessOutcome {
                    cause: CommandProcessCause::Exited,
                    exit_code: Some(0),
                    stdout: long_stdout.clone(),
                    stdout_truncated: true,
                    stderr: long_stderr.clone(),
                    stderr_truncated: true,
                },
                cleanup_verified: true,
            }),
        );
        let runner = NativeCommandExecutionSurface::new(
            artifacts.clone(),
            capabilities_of(facts),
            identities_of(facts),
            port,
        );
        let call = run_call(RunAs::App, false);
        let execution = admitted(facts, &call, 40);
        let execution_id = execution.execution_id.clone();
        let completion = start(&runner, execution).await.unwrap();
        let result = public_result(&completion);
        assert!(result.get("stdout").is_none());
        assert!(result.get("stderr").is_none());
        assert_eq!(result["stdout_truncated"], true);
        assert_eq!(result["stderr_truncated"], true);
        let stdout_ref = result["stdout_ref"].as_str().expect("stdout artifact ref");
        let stderr_ref = result["stderr_ref"].as_str().expect("stderr artifact ref");
        assert!(stdout_ref.starts_with("dbref:stdout:"));
        assert!(stderr_ref.starts_with("dbref:stderr:"));
        assert_ne!(stdout_ref, stderr_ref);
        assert_eq!(
            artifacts.execution_owner(stdout_ref),
            Some(execution_id.clone())
        );
        assert_eq!(artifacts.execution_owner(stderr_ref), Some(execution_id));
        assert_eq!(artifacts.open(stdout_ref).unwrap(), long_stdout);
        assert_eq!(artifacts.open(stderr_ref).unwrap(), long_stderr);
        assert_eq!(artifacts.metadata(stdout_ref).unwrap().byte_count, 70_000);
        assert!(completion.cleanup_verified);
    }

    // The encoder the APK surface produces and the decoder the Magisk surface consumes
    // agree on every terminal cause, so a forwarded settlement cannot change meaning.
    for (cause, exit_code) in [
        (CommandProcessCause::Exited, Some(17)),
        (CommandProcessCause::Timeout, None),
        (CommandProcessCause::Cancelled, None),
        (CommandProcessCause::OwnerLost, None),
    ] {
        let settlement = CommandProcessSettlement {
            outcome: CommandProcessOutcome {
                cause,
                exit_code,
                stdout: b"out".to_vec(),
                stdout_truncated: false,
                stderr: b"err".to_vec(),
                stderr_truncated: true,
            },
            cleanup_verified: true,
        };
        let encoded = AndroidCommandSettlement::encode(&settlement).unwrap();
        let decoded = AndroidCommandSettlement::decode(&encoded).unwrap();
        assert_eq!(decoded.outcome.cause, cause);
        assert_eq!(decoded.outcome.exit_code, exit_code);
        assert_eq!(decoded.outcome.stdout, b"out");
        assert_eq!(decoded.outcome.stderr, b"err");
        assert!(decoded.outcome.stderr_truncated);
        assert!(decoded.cleanup_verified);
        // Only a reaped process reports an exit code, and a frame the decoder cannot
        // trust is refused rather than defaulted.
        assert_eq!(
            decoded.outcome.exit_code.is_some(),
            cause == CommandProcessCause::Exited
        );
    }
    assert_eq!(command_settlement_bound_bytes(), 262_144);
    assert_eq!(RESERVE_FLOOR_BYTES, 16_384);
    let exact_process_request = CommandProcessRequest {
        run_as: RunAs::Shell,
        program: "/system/bin/sh".to_owned(),
        arguments: vec!["-c".to_owned(), "id".to_owned()],
        cwd: Some("/".to_owned()),
        stdin: Some(b"input".to_vec()),
        timeout_ms: 1_000,
        max_output_bytes: 1_024,
    };
    assert!(validate_command_process_request(&exact_process_request, RunAs::Shell).is_ok());
    for invalid in [
        CommandProcessRequest {
            run_as: RunAs::App,
            ..exact_process_request.clone()
        },
        CommandProcessRequest {
            program: "/system/bin/id".to_owned(),
            ..exact_process_request.clone()
        },
        CommandProcessRequest {
            arguments: vec!["id".to_owned()],
            ..exact_process_request.clone()
        },
        CommandProcessRequest {
            stdin: Some(vec![0xff]),
            ..exact_process_request.clone()
        },
        CommandProcessRequest {
            timeout_ms: 150_001,
            ..exact_process_request.clone()
        },
        CommandProcessRequest {
            max_output_bytes: 1,
            ..exact_process_request.clone()
        },
    ] {
        assert_eq!(
            validate_command_process_request(&invalid, RunAs::Shell)
                .unwrap_err()
                .code,
            ErrorCode::InvalidArgument
        );
    }
    for malformed in [
        &b"{\"cause\":\"teleported\"}"[..],
        &b"{\"cause\":\"exited\",\"unexpected\":1}"[..],
        &b"not json"[..],
    ] {
        assert_eq!(
            AndroidCommandSettlement::decode(malformed)
                .unwrap_err()
                .code,
            ErrorCode::IoError
        );
    }
}

#[tokio::test]
async fn i8_cmd_g04_cleanup_uncertainty_is_explicit_for_sync_and_task_results() {
    let facts = apk_facts(APK_INSTANCE);
    let make_core = || {
        let capabilities = capabilities_of(facts);
        let executions = FakeExecutions::default();
        let host =
            FakeHostControl::new(RecoveryProof::Clean).with_capabilities(capabilities.clone());
        (
            RuntimeCore::new(
                FakePersistence::default(),
                FakeArtifacts::default(),
                executions.clone(),
                capabilities,
                host,
            ),
            executions,
        )
    };
    let completion = |outcome| ExecutionCompletion {
        fence: capability(facts).fence,
        capability_generation: facts.host_generation,
        outcome,
        cleanup_verified: false,
    };

    let (core, executions) = make_core();
    executions.push(Ok(completion(ExecutionOutcome::SynchronousCompleted {
        result: serde_json::json!({"state": "completed"}),
        encoded_bytes: 64,
    })));
    let sync_error = core
        .run_synchronous(
            SynchronousAdmission {
                request_id: uuid(0x8700_0000, 1),
                payload_sha256: "11".repeat(32),
                execution_id: uuid(0x8700_0000, 2),
                operation: "command.run".to_owned(),
                route: ExecutorRequest::Command(RunAs::App),
                payload: ExecutionPayload::CommandCall(run_call(RunAs::App, false)),
                settlement_bound_bytes: RESERVE_FLOOR_BYTES,
                now_ms: 1,
            },
            "2026-09-13T00:00:00.000Z".to_owned(),
            1,
        )
        .await
        .unwrap_err();
    assert_eq!(sync_error.code, ErrorCode::IoError);
    assert_eq!(
        serde_json::to_value(&sync_error).unwrap()["details"]["cleanup_unverified"],
        true
    );

    let (core, executions) = make_core();
    let task_id = uuid(0x8700_0000, 3);
    core.admit_task(TaskAdmission {
        request_id: uuid(0x8700_0000, 4),
        payload_sha256: "22".repeat(32),
        task_id: task_id.clone(),
        execution_id: uuid(0x8700_0000, 5),
        tool: contract::MotherTool::Command,
        action: "run".to_owned(),
        route: ExecutorRequest::Command(RunAs::App),
        payload: ExecutionPayload::CommandCall(run_call(RunAs::App, true)),
        created_at: "2026-09-13T00:00:00.000Z".to_owned(),
        settlement_bound_bytes: RESERVE_FLOOR_BYTES,
        now_ms: 2,
    })
    .await
    .unwrap();
    executions.push(Ok(completion(ExecutionOutcome::Failed {
        error: contract::PublicError {
            code: ErrorCode::ExecutionFailed,
            operation: "command.run".to_owned(),
            retryable: false,
            message: None,
            capability: None,
            details: None,
        },
        encoded_bytes: 64,
    })));
    let task = core
        .run_task(&task_id, "2026-09-13T00:00:01.000Z".to_owned(), 3)
        .await
        .unwrap();
    assert_eq!(task.state, contract::TaskState::Interrupted);
    let task_error = task.error.unwrap();
    assert_eq!(task_error.code, ErrorCode::IoError);
    assert_eq!(
        serde_json::to_value(task_error).unwrap()["details"]["cleanup_unverified"],
        true
    );
}
