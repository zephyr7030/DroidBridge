//! Runs one admitted AutomationExecution to its single settlement (S-AUTO-002). Calls go through
//! the ordinary public Contract path, persistent state through canonical commits, and delay and
//! budget waits through CLOCK_BOOTTIME timers that keep counting while the device is suspended.

use crate::{
    AUTOMATION_EXECUTION_ACTION, AdmittedAutomationExecution, ArtifactPort, AutomationEffects,
    AutomationExecutionOutcome, AutomationInterpreter, CapabilityPort, ExecutionPort,
    FilesystemPreflightPort, HostControlPort, PersistencePort, PortFuture, RuntimeCore,
    RuntimeState,
};
use contract::{
    AutomationCompatibleCall, AutomationExecutionSummary, AutomationId, ErrorCode, ExecutionId,
    PublicError, PublicResponse, ScalarValue, TaskAccepted, TaskId, TaskState,
};
use domain::{AUTOMATION_BUDGET_MS, DomainError};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::Notify;

/// Cadence at which an Automation Call observes the Task it is awaiting.
const CHILD_TASK_POLL_MS: u64 = 100;

/// The clocks an AutomationExecution measures its budget, delays and commits with.
pub trait AutomationClock: Send + Sync {
    /// Milliseconds of CLOCK_BOOTTIME, which keeps advancing while the device is suspended.
    fn boot_millis(&self) -> Result<u64, DomainError>;

    /// Waits `duration_ms` of CLOCK_BOOTTIME without waking a suspended device.
    fn sleep<'a>(&'a self, duration_ms: u64) -> PortFuture<'a, Result<(), DomainError>>;

    /// The current wall-clock RFC3339 millisecond instant and its Unix milliseconds.
    fn wall(&self) -> Result<(String, u64), DomainError>;
}

/// The CLOCK_BOOTTIME timerfd clock of an Android or Linux Runtime host.
#[derive(Clone, Copy, Debug, Default)]
pub struct BoottimeClock;

impl AutomationClock for BoottimeClock {
    #[cfg(any(target_os = "android", target_os = "linux"))]
    fn boot_millis(&self) -> Result<u64, DomainError> {
        let now = rustix::time::clock_gettime(rustix::time::ClockId::Boottime);
        u64::try_from(now.tv_sec)
            .ok()
            .and_then(|seconds| seconds.checked_mul(1_000))
            .zip(u64::try_from(now.tv_nsec / 1_000_000).ok())
            .and_then(|(seconds, millis)| seconds.checked_add(millis))
            .ok_or_else(|| DomainError::new(ErrorCode::InternalError, "boot clock is invalid"))
    }

    #[cfg(not(any(target_os = "android", target_os = "linux")))]
    fn boot_millis(&self) -> Result<u64, DomainError> {
        Err(unsupported_clock())
    }

    #[cfg(any(target_os = "android", target_os = "linux"))]
    fn sleep<'a>(&'a self, duration_ms: u64) -> PortFuture<'a, Result<(), DomainError>> {
        Box::pin(boottime_sleep(duration_ms))
    }

    #[cfg(not(any(target_os = "android", target_os = "linux")))]
    fn sleep<'a>(&'a self, _duration_ms: u64) -> PortFuture<'a, Result<(), DomainError>> {
        Box::pin(async { Err(unsupported_clock()) })
    }

    fn wall(&self) -> Result<(String, u64), DomainError> {
        let now = chrono::Utc::now();
        let millis = u64::try_from(now.timestamp_millis())
            .map_err(|_| DomainError::new(ErrorCode::InternalError, "wall clock is invalid"))?;
        Ok((
            now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            millis,
        ))
    }
}

#[cfg(not(any(target_os = "android", target_os = "linux")))]
fn unsupported_clock() -> DomainError {
    DomainError::new(
        ErrorCode::Unsupported,
        "CLOCK_BOOTTIME timers require an Android or Linux Runtime host",
    )
}

/// One non-waking CLOCK_BOOTTIME timerfd wait owned by this future; dropping it closes the FD.
#[cfg(any(target_os = "android", target_os = "linux"))]
async fn boottime_sleep(duration_ms: u64) -> Result<(), DomainError> {
    use rustix::time::{
        Itimerspec, TimerfdClockId, TimerfdFlags, TimerfdTimerFlags, Timespec, timerfd_create,
        timerfd_settime,
    };
    use tokio::io::{Interest, unix::AsyncFd};

    if duration_ms == 0 {
        return Ok(());
    }
    let timer_error =
        |_| DomainError::new(ErrorCode::IoError, "CLOCK_BOOTTIME timer operation failed");
    let seconds = i64::try_from(duration_ms / 1_000).map_err(|_| {
        DomainError::new(ErrorCode::ResourceLimit, "timer duration is out of range")
    })?;
    let nanos = i64::try_from((duration_ms % 1_000) * 1_000_000).map_err(|_| {
        DomainError::new(ErrorCode::ResourceLimit, "timer duration is out of range")
    })?;
    let timer = timerfd_create(
        TimerfdClockId::Boottime,
        TimerfdFlags::NONBLOCK | TimerfdFlags::CLOEXEC,
    )
    .map_err(timer_error)?;
    timerfd_settime(
        &timer,
        TimerfdTimerFlags::empty(),
        &Itimerspec {
            it_interval: Timespec {
                tv_sec: 0,
                tv_nsec: 0,
            },
            it_value: Timespec {
                tv_sec: seconds,
                tv_nsec: nanos,
            },
        },
    )
    .map_err(timer_error)?;
    let timer = AsyncFd::with_interest(timer, Interest::READABLE).map_err(|_| {
        DomainError::new(
            ErrorCode::IoError,
            "CLOCK_BOOTTIME timer registration failed",
        )
    })?;
    loop {
        let mut ready = timer.readable().await.map_err(|_| {
            DomainError::new(ErrorCode::IoError, "CLOCK_BOOTTIME timer wait failed")
        })?;
        let mut expirations = [0_u8; 8];
        match ready.try_io(|inner| {
            rustix::io::read(inner.get_ref(), &mut expirations).map_err(std::io::Error::from)
        }) {
            Ok(Ok(8)) => return Ok(()),
            Ok(Ok(_)) | Ok(Err(_)) => {
                return Err(DomainError::new(
                    ErrorCode::IoError,
                    "CLOCK_BOOTTIME timer read failed",
                ));
            }
            Err(_would_block) => {}
        }
    }
}

/// The cancellation request of one running AutomationExecution. The durable Task cancel request
/// stays the truth; this only wakes the execution that must observe it.
#[derive(Debug, Default)]
pub struct AutomationCancellation {
    requested: AtomicBool,
    notify: Notify,
}

impl AutomationCancellation {
    pub fn request(&self) {
        self.requested.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    pub fn is_requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }

    async fn requested(&self) {
        let notified = self.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if self.is_requested() {
            return;
        }
        notified.await;
    }
}

impl<P, A, E, C, H> RuntimeCore<P, A, E, C, H>
where
    P: PersistencePort + 'static,
    A: ArtifactPort + Clone + 'static,
    E: ExecutionPort + FilesystemPreflightPort + 'static,
    C: CapabilityPort + 'static,
    H: HostControlPort + 'static,
{
    /// Runs one admitted AutomationExecution to its single settlement and returns that summary.
    pub async fn run_automation_execution<K: AutomationClock>(
        &self,
        admitted: AdmittedAutomationExecution,
        clock: &K,
    ) -> Result<AutomationExecutionSummary, DomainError> {
        let execution_id = admitted.summary.execution_id.clone();
        let cancellation = self.register_automation_cancellation(&execution_id)?;
        let result = self
            .drive_automation_execution(&admitted, clock, cancellation)
            .await;
        self.release_automation_cancellation(&execution_id);
        result
    }

    async fn drive_automation_execution<K: AutomationClock>(
        &self,
        admitted: &AdmittedAutomationExecution,
        clock: &K,
        cancellation: Arc<AutomationCancellation>,
    ) -> Result<AutomationExecutionSummary, DomainError> {
        let execution_id = &admitted.summary.execution_id;
        let started_boot_ms = clock.boot_millis()?;
        match self
            .start_automation_execution(execution_id, clock.wall()?.0)
            .await
        {
            Ok(_) => {}
            // A durable cancel request that arrived while queued settles without running.
            Err(error) if error.code == ErrorCode::Cancelled => {
                return self
                    .settle_automation_execution(
                        execution_id,
                        AutomationExecutionOutcome::Cancelled(execution_error(
                            ErrorCode::Cancelled,
                        )),
                        clock.wall()?.0,
                    )
                    .await;
            }
            // A queued cancellation already settled this execution with its Task.
            Err(error) if error.code == ErrorCode::InvalidArgument => {
                return self
                    .read_state(|state| execution_summary(state, execution_id))
                    .await;
            }
            Err(error) => return Err(error),
        }
        let mut state = self
            .read_state(|state| automation_state(state, &admitted.automation_id))
            .await?;
        let mut effects = ExecutionEffects {
            core: self,
            clock,
            automation_id: admitted.automation_id.clone(),
            execution_id: execution_id.clone(),
            started_boot_ms,
            cancellation,
            cleanup_unverified: false,
        };
        let run = AutomationInterpreter
            .execute(
                &admitted.action,
                &mut state,
                &admitted.trigger_facts,
                &mut effects,
            )
            .await;
        let outcome = match run {
            _ if effects.cleanup_unverified => {
                AutomationExecutionOutcome::Interrupted(execution_error(ErrorCode::IoError))
            }
            Ok(()) => AutomationExecutionOutcome::Completed,
            Err(error) if error.code == ErrorCode::Cancelled => {
                AutomationExecutionOutcome::Cancelled(execution_error(ErrorCode::Cancelled))
            }
            Err(error) => AutomationExecutionOutcome::Failed(execution_error(error.code)),
        };
        self.settle_automation_execution(execution_id, outcome, clock.wall()?.0)
            .await
    }
}

struct ExecutionEffects<'a, P, A, E, C, H, K> {
    core: &'a RuntimeCore<P, A, E, C, H>,
    clock: &'a K,
    automation_id: AutomationId,
    execution_id: ExecutionId,
    started_boot_ms: u64,
    cancellation: Arc<AutomationCancellation>,
    cleanup_unverified: bool,
}

impl<P, A, E, C, H, K> ExecutionEffects<'_, P, A, E, C, H, K>
where
    P: PersistencePort + 'static,
    A: ArtifactPort + Clone + 'static,
    E: ExecutionPort + FilesystemPreflightPort + 'static,
    C: CapabilityPort + 'static,
    H: HostControlPort + 'static,
    K: AutomationClock,
{
    fn remaining_ms(&self) -> Result<u64, DomainError> {
        let elapsed = self
            .clock
            .boot_millis()?
            .saturating_sub(self.started_boot_ms);
        Ok(AUTOMATION_BUDGET_MS.saturating_sub(elapsed))
    }

    /// Awaits one child Task. Budget expiry or cancellation cancels the owned child first and
    /// stops only after it settles; an unverified child cleanup interrupts this execution.
    async fn await_child_task(&mut self, task_id: &TaskId) -> Result<(), DomainError> {
        loop {
            let snapshot = self.core.get_task(task_id, self.clock.wall()?.1).await?;
            match snapshot.state {
                TaskState::Completed => return Ok(()),
                TaskState::Failed | TaskState::Cancelled | TaskState::Interrupted => {
                    return Err(DomainError::new(
                        snapshot
                            .error
                            .map_or(ErrorCode::ExecutionFailed, |error| error.code),
                        "Automation child Task did not complete",
                    ));
                }
                TaskState::Created | TaskState::Queued | TaskState::Running => {}
            }
            if let Err(stop) = self.checkpoint() {
                let (timestamp, now_ms) = self.clock.wall()?;
                let settled = self.core.cancel_task(task_id, timestamp, now_ms).await?;
                if settled.state == TaskState::Interrupted {
                    self.cleanup_unverified = true;
                }
                return Err(stop);
            }
            let wait = CHILD_TASK_POLL_MS.min(self.remaining_ms()?);
            tokio::select! {
                result = self.clock.sleep(wait) => result?,
                () = self.cancellation.requested() => {}
            }
        }
    }
}

impl<P, A, E, C, H, K> AutomationEffects for ExecutionEffects<'_, P, A, E, C, H, K>
where
    P: PersistencePort + 'static,
    A: ArtifactPort + Clone + 'static,
    E: ExecutionPort + FilesystemPreflightPort + 'static,
    C: CapabilityPort + 'static,
    H: HostControlPort + 'static,
    K: AutomationClock,
{
    fn checkpoint(&mut self) -> Result<(), DomainError> {
        if self.cancellation.is_requested() {
            return Err(DomainError::new(
                ErrorCode::Cancelled,
                "AutomationExecution was cancelled",
            ));
        }
        if self.remaining_ms()? == 0 {
            return Err(DomainError::new(
                ErrorCode::Timeout,
                "Automation execution budget expired",
            ));
        }
        Ok(())
    }

    fn call<'b>(
        &'b mut self,
        call: &'b AutomationCompatibleCall,
    ) -> PortFuture<'b, Result<(), DomainError>> {
        Box::pin(async move {
            let request = public_request(call)?;
            let (timestamp, now_ms) = self.clock.wall()?;
            let response =
                crate::submit_public(self.core, &request, timestamp, now_ms, true, |_| async {
                    Err(DomainError::new(
                        ErrorCode::Unsupported,
                        "an Automation Call has no host-installed dispatch",
                    ))
                })
                .await;
            let result = call_result(&response)?;
            match serde_json::from_value::<TaskAccepted>(result) {
                Ok(accepted) => self.await_child_task(&accepted.task_id).await,
                Err(_) => Ok(()),
            }
        })
    }

    fn set_state<'b>(
        &'b mut self,
        key: &'b str,
        value: &'b ScalarValue,
    ) -> PortFuture<'b, Result<(), DomainError>> {
        Box::pin(async move {
            let automation_id = self.automation_id.clone();
            let execution_id = self.execution_id.clone();
            let key = key.to_owned();
            let value = value.clone();
            self.core
                .state_transition(move |state, _| {
                    set_automation_state_in_state(state, &automation_id, &execution_id, key, value)
                        .map(|()| ((), true))
                })
                .await
        })
    }

    fn delay<'b>(&'b mut self, duration_ms: u64) -> PortFuture<'b, Result<(), DomainError>> {
        Box::pin(async move {
            let remaining = self.remaining_ms()?;
            tokio::select! {
                result = self.clock.sleep(duration_ms.min(remaining)) => result?,
                () = self.cancellation.requested() => {
                    return Err(DomainError::new(
                        ErrorCode::Cancelled,
                        "AutomationExecution was cancelled",
                    ));
                }
            }
            if duration_ms > remaining {
                return Err(DomainError::new(
                    ErrorCode::Timeout,
                    "Automation execution budget expired",
                ));
            }
            Ok(())
        })
    }
}

/// The ordinary S-CONTRACT-003 request for one saved Call: `{tool,action,args}` becomes
/// `{tool,action,input}` under a fresh request identity.
fn public_request(call: &AutomationCompatibleCall) -> Result<Vec<u8>, DomainError> {
    let encoding = || DomainError::new(ErrorCode::InternalError, "Automation Call encoding failed");
    let mut payload = serde_json::to_value(call).map_err(|_| encoding())?;
    {
        let object = payload.as_object_mut().ok_or_else(encoding)?;
        let args = object.remove("args").ok_or_else(encoding)?;
        object.insert("input".to_owned(), args);
    }
    serde_json::to_vec(&serde_json::json!({
        "protocol_version": 1,
        "request_id": crate::command::new_uuid()?,
        "payload": payload,
    }))
    .map_err(|_| encoding())
}

fn call_result(response: &[u8]) -> Result<serde_json::Value, DomainError> {
    match serde_json::from_slice::<PublicResponse<serde_json::Value>>(response) {
        Ok(PublicResponse::Success { result, .. }) => Ok(result),
        Ok(PublicResponse::Error { error, .. }) => {
            Err(DomainError::new(error.code, "Automation Call failed"))
        }
        Err(_) => Err(DomainError::new(
            ErrorCode::InternalError,
            "Automation Call response is invalid",
        )),
    }
}

/// Creates or replaces one persistent state key of the Automation owned by this execution,
/// including a deletion tombstone's retained state (R-AUTO-010, R-AUTO-018).
pub(crate) fn set_automation_state_in_state(
    state: &mut RuntimeState,
    automation_id: &AutomationId,
    execution_id: &ExecutionId,
    key: String,
    value: ScalarValue,
) -> Result<(), DomainError> {
    let record = state
        .automations
        .iter_mut()
        .find(|record| &record.automation.automation_id == automation_id)
        .ok_or_else(|| DomainError::new(ErrorCode::NotFound, "Automation not found"))?;
    if record.active_execution_id.as_ref() != Some(execution_id) {
        return Err(DomainError::new(
            ErrorCode::StaleAuthority,
            "execution does not own this Automation state",
        ));
    }
    if key.is_empty() || key.len() > 64 || key.contains('\0') {
        return Err(DomainError::invalid(
            "Automation state key is out of bounds",
        ));
    }
    if matches!(&value, ScalarValue::String(text) if text.len() > 4_096) {
        return Err(DomainError::invalid(
            "Automation state value is out of bounds",
        ));
    }
    if !record.automation.state.contains_key(&key) && record.automation.state.len() >= 64 {
        return Err(DomainError::new(
            ErrorCode::ResourceLimit,
            "Automation state capacity is full",
        ));
    }
    record.automation.state.insert(key, value);
    Ok(())
}

fn automation_state(
    state: &RuntimeState,
    automation_id: &AutomationId,
) -> Result<BTreeMap<String, ScalarValue>, DomainError> {
    state
        .automations
        .iter()
        .find(|record| &record.automation.automation_id == automation_id)
        .map(|record| record.automation.state.clone())
        .ok_or_else(|| DomainError::new(ErrorCode::NotFound, "Automation not found"))
}

fn execution_summary(
    state: &RuntimeState,
    execution_id: &ExecutionId,
) -> Result<AutomationExecutionSummary, DomainError> {
    state
        .automation_executions
        .iter()
        .find(|execution| &execution.summary.execution_id == execution_id)
        .map(|execution| execution.summary.clone())
        .ok_or_else(|| DomainError::new(ErrorCode::NotFound, "AutomationExecution not found"))
}

fn execution_error(code: ErrorCode) -> PublicError {
    PublicError {
        code,
        operation: format!("automation.{AUTOMATION_EXECUTION_ACTION}"),
        retryable: false,
        message: None,
        capability: None,
        details: None,
    }
}
