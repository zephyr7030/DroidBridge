use crate::{
    AdmittedExecution, ArtifactPort, AutomationClock, CapabilityPort, CapabilitySnapshot,
    ExecutionOutcome, ExecutionPayload, ExecutionPort, ExecutorRecord, HostControlPort,
    NetworkDefaultChangedEvent, NetworkDefaultEventPlane, NetworkDefaultEventSource,
    NetworkDefaultSourceRegistration, NetworkDefaultSubscription, NetworkEventDelivery,
    PersistencePort, RESERVE_FLOOR_BYTES, RETAINED_MUTATION_RECORD_BYTES,
    RETAINED_RESULT_LIMIT_BYTES, RecoveryProof, RetainedMutationRecord, RuntimeState,
    STORE_LIMIT_BYTES, SYNCHRONOUS_RECORD_BYTES, SynchronousExecutionRecord,
    SynchronousExecutionState, TASK_RECORD_BYTES, TaskOrigin, TaskRecord,
};
use chrono::DateTime;
use contract::{
    ErrorCode, MotherTool, PublicError, RequestId, RuntimeReadiness, TaskControlCall, TaskId,
    TaskListResult, TaskSnapshot, TaskState, TaskSummary, UuidV4,
};
use domain::{
    DedupDecision, DomainError, ExecutorRequest, MAX_QUEUED_TASKS, MAX_RUNNING_TASKS, TaskEvent,
    TaskLifecycle, derive_capabilities,
};
use std::{
    collections::{BTreeMap, HashSet},
    sync::{Arc, Mutex as StdMutex},
};
use tokio::sync::{Mutex, Notify, Semaphore, watch};

const MAX_TERMINAL_TASKS: usize = 500;
const TASK_HISTORY_RETENTION_MS: u64 = 7 * 86_400_000;

#[derive(Clone, Debug)]
pub struct TaskAdmission {
    pub request_id: RequestId,
    pub payload_sha256: String,
    pub task_id: TaskId,
    pub execution_id: UuidV4,
    pub tool: MotherTool,
    pub action: String,
    pub route: ExecutorRequest,
    pub payload: ExecutionPayload,
    pub created_at: String,
    pub settlement_bound_bytes: u64,
    pub now_ms: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TaskAdmissionResult {
    Admitted(TaskSnapshot),
    Replay(TaskSnapshot),
}

#[derive(Clone, Debug)]
pub struct SynchronousAdmission {
    pub request_id: RequestId,
    pub payload_sha256: String,
    pub execution_id: UuidV4,
    pub operation: String,
    pub route: ExecutorRequest,
    pub payload: ExecutionPayload,
    pub settlement_bound_bytes: u64,
    pub now_ms: u64,
}

pub struct RuntimeCore<P, A, E, C, H> {
    persistence: Arc<P>,
    artifacts: Arc<A>,
    executions: Arc<E>,
    capabilities: Arc<C>,
    host_control: Arc<H>,
    mutation: Arc<Mutex<()>>,
    leaf_permits: Arc<Semaphore>,
    synchronous_waiters: Arc<StdMutex<BTreeMap<String, watch::Sender<()>>>>,
    network_events: NetworkDefaultEventPlane,
    network_event_source: Arc<StdMutex<Option<Arc<dyn NetworkDefaultEventSource>>>>,
    automation_cancellations: Arc<StdMutex<BTreeMap<String, Arc<crate::AutomationCancellation>>>>,
    canonical_changes: Arc<Notify>,
}

impl<P, A, E, C, H> Clone for RuntimeCore<P, A, E, C, H> {
    fn clone(&self) -> Self {
        Self {
            persistence: Arc::clone(&self.persistence),
            artifacts: Arc::clone(&self.artifacts),
            executions: Arc::clone(&self.executions),
            capabilities: Arc::clone(&self.capabilities),
            host_control: Arc::clone(&self.host_control),
            mutation: Arc::clone(&self.mutation),
            leaf_permits: Arc::clone(&self.leaf_permits),
            synchronous_waiters: Arc::clone(&self.synchronous_waiters),
            network_events: self.network_events.clone(),
            network_event_source: Arc::clone(&self.network_event_source),
            automation_cancellations: Arc::clone(&self.automation_cancellations),
            canonical_changes: Arc::clone(&self.canonical_changes),
        }
    }
}

impl<P, A, E, C, H> RuntimeCore<P, A, E, C, H>
where
    P: PersistencePort + 'static,
    A: ArtifactPort + 'static,
    E: ExecutionPort + 'static,
    C: CapabilityPort + 'static,
    H: HostControlPort + 'static,
{
    pub fn new(
        persistence: P,
        artifacts: A,
        executions: E,
        capabilities: C,
        host_control: H,
    ) -> Self {
        Self {
            persistence: Arc::new(persistence),
            artifacts: Arc::new(artifacts),
            executions: Arc::new(executions),
            capabilities: Arc::new(capabilities),
            host_control: Arc::new(host_control),
            mutation: Arc::new(Mutex::new(())),
            leaf_permits: Arc::new(Semaphore::new(MAX_RUNNING_TASKS as usize)),
            synchronous_waiters: Arc::new(StdMutex::new(BTreeMap::new())),
            network_events: NetworkDefaultEventPlane::new(),
            network_event_source: Arc::new(StdMutex::new(None)),
            automation_cancellations: Arc::new(StdMutex::new(BTreeMap::new())),
            canonical_changes: Arc::new(Notify::new()),
        }
    }

    /// The coalescing signal raised after every committed canonical state change, and by a host
    /// whose readiness changed without a commit. Its single consumer, the Automation scheduler,
    /// re-reads due eligibility on it (S-LIFE-003), so no consumer polls the store.
    pub fn canonical_changes(&self) -> Arc<Notify> {
        Arc::clone(&self.canonical_changes)
    }

    /// Registers the in-memory wake-up of one running AutomationExecution. The durable Task
    /// cancel request remains the truth; the registration only lets the execution observe it.
    pub(crate) fn register_automation_cancellation(
        &self,
        execution_id: &UuidV4,
    ) -> Result<Arc<crate::AutomationCancellation>, DomainError> {
        let mut registry = self.automation_cancellations.lock().map_err(|_| {
            DomainError::new(
                ErrorCode::InternalError,
                "Automation cancellation registry lock failed",
            )
        })?;
        Ok(Arc::clone(
            registry
                .entry(execution_id.as_str().to_owned())
                .or_default(),
        ))
    }

    pub(crate) fn release_automation_cancellation(&self, execution_id: &UuidV4) {
        if let Ok(mut registry) = self.automation_cancellations.lock() {
            registry.remove(execution_id.as_str());
        }
    }

    fn request_automation_cancellation(&self, execution_id: &UuidV4) {
        if let Ok(registry) = self.automation_cancellations.lock()
            && let Some(cancellation) = registry.get(execution_id.as_str())
        {
            cancellation.request();
        }
    }

    pub fn with_network_default_event_source(
        mut self,
        source: Arc<dyn NetworkDefaultEventSource>,
    ) -> Self {
        self.network_event_source = Arc::new(StdMutex::new(Some(source)));
        self
    }

    pub fn replace_network_default_event_source(
        &self,
        source: Arc<dyn NetworkDefaultEventSource>,
    ) -> Result<(), DomainError> {
        *self.network_event_source.lock().map_err(|_| {
            DomainError::new(ErrorCode::InternalError, "network source lock failed")
        })? = Some(Arc::clone(&source));
        if let Some(generation) = self.network_events.active_subscription_generation()? {
            self.network_events.replace_source(generation, source)?;
        }
        Ok(())
    }

    pub fn subscribe_network_default_events(
        &self,
    ) -> Result<NetworkDefaultSubscription, DomainError> {
        let fence = self.capabilities.current()?.fence;
        let source = self
            .network_event_source
            .lock()
            .map_err(|_| DomainError::new(ErrorCode::InternalError, "network source lock failed"))?
            .clone()
            .ok_or_else(|| {
                DomainError::new(
                    ErrorCode::CapabilityUnavailable,
                    "network event source is unavailable",
                )
            })?;
        self.network_events.subscribe(fence, source)
    }

    pub fn observe_network_default_event(
        &self,
        registration: NetworkDefaultSourceRegistration,
        event: NetworkDefaultChangedEvent,
    ) -> Result<NetworkEventDelivery, DomainError> {
        if self.capabilities.current()?.fence != registration.fence {
            return Ok(NetworkEventDelivery::IgnoredStale);
        }
        self.network_events.observe(registration, event)
    }

    pub fn network_default_event_plane(&self) -> &NetworkDefaultEventPlane {
        &self.network_events
    }

    pub fn artifact_port(&self) -> &A {
        self.artifacts.as_ref()
    }

    pub(crate) fn execution_port(&self) -> &E {
        self.executions.as_ref()
    }

    pub(crate) fn capability_snapshot(&self) -> Result<CapabilitySnapshot, DomainError> {
        self.capabilities.current()
    }

    pub fn capability_projection(&self) -> Result<contract::EffectiveCapabilities, DomainError> {
        let snapshot = self.capabilities.current()?;
        derive_capabilities(&snapshot.grants, snapshot.context)
    }

    pub async fn run_synchronous(
        &self,
        admission: SynchronousAdmission,
        ended_at: String,
        terminal_at_ms: u64,
    ) -> Result<serde_json::Value, PublicError> {
        let (execution, permit) = loop {
            let wait_for_terminal;
            {
                let _guard = self.mutation.lock().await;
                let mut state = self
                    .persistence
                    .load()
                    .map_err(|error| public_error(error.code, &admission.operation, false))?;
                prune_synchronous_history(&mut state, admission.now_ms);
                match state
                    .dedup
                    .decide_and_reserve(
                        admission.request_id.clone(),
                        admission.payload_sha256.clone(),
                        admission.now_ms,
                        false,
                    )
                    .map_err(|error| public_error(error.code, &admission.operation, false))?
                {
                    DedupDecision::Replay => {
                        let existing = state
                            .synchronous_executions
                            .iter()
                            .find(|record| record.request_id == admission.request_id)
                            .ok_or_else(|| {
                                public_error(ErrorCode::InternalError, &admission.operation, false)
                            })?;
                        if existing.state.is_terminal() {
                            return synchronous_result(existing);
                        }
                        let receiver = self
                            .synchronous_waiters
                            .lock()
                            .expect("synchronous waiter lock")
                            .get(admission.request_id.as_str())
                            .ok_or_else(|| {
                                public_error(ErrorCode::InternalError, &admission.operation, false)
                            })?
                            .subscribe();
                        wait_for_terminal = receiver;
                    }
                    DedupDecision::Admit => {
                        let permit = self.leaf_permits.try_acquire().map_err(|_| {
                            public_error(ErrorCode::ResourceLimit, &admission.operation, false)
                        })?;
                        let capability = self.capabilities.current().map_err(|error| {
                            public_error(error.code, &admission.operation, false)
                        })?;
                        require_ready(capability.context.readiness).map_err(|error| {
                            public_error(error.code, &admission.operation, false)
                        })?;
                        let executor = crate::resolve_execution(&capability, admission.route)
                            .map_err(|error| {
                                public_error(error.code, &admission.operation, false)
                            })?;
                        let reservation = admission.settlement_bound_bytes.max(RESERVE_FLOOR_BYTES);
                        let total = state
                            .total_committed_and_reserved()
                            .and_then(|value| value.checked_add(SYNCHRONOUS_RECORD_BYTES))
                            .and_then(|value| value.checked_add(reservation))
                            .ok_or_else(|| {
                                public_error(ErrorCode::ResourceLimit, &admission.operation, false)
                            })?;
                        if total > STORE_LIMIT_BYTES {
                            return Err(public_error(
                                ErrorCode::ResourceLimit,
                                &admission.operation,
                                false,
                            ));
                        }
                        let execution = AdmittedExecution {
                            execution_id: admission.execution_id.clone(),
                            task_id: None,
                            executor: ExecutorRecord::from(&executor),
                            payload: admission.payload.clone(),
                        };
                        state.used_bytes = state
                            .used_bytes
                            .checked_add(SYNCHRONOUS_RECORD_BYTES)
                            .ok_or_else(|| {
                            public_error(ErrorCode::ResourceLimit, &admission.operation, false)
                        })?;
                        state.reserved_bytes = state
                            .reserved_bytes
                            .checked_add(reservation)
                            .ok_or_else(|| {
                                public_error(ErrorCode::ResourceLimit, &admission.operation, false)
                            })?;
                        state
                            .synchronous_executions
                            .push(SynchronousExecutionRecord {
                                request_id: admission.request_id.clone(),
                                execution_id: admission.execution_id.clone(),
                                operation: admission.operation.clone(),
                                state: SynchronousExecutionState::Running,
                                ended_at: None,
                                executor: execution.executor.clone(),
                                route: admission.route,
                                payload: admission.payload.clone(),
                                result: None,
                                error: None,
                                reserved_bytes: reservation,
                                terminal_bytes: 0,
                            });
                        let (sender, _) = watch::channel(());
                        self.synchronous_waiters
                            .lock()
                            .expect("synchronous waiter lock")
                            .insert(admission.request_id.as_str().to_owned(), sender);
                        if let Err(error) = self.commit(state) {
                            self.synchronous_waiters
                                .lock()
                                .expect("synchronous waiter lock")
                                .remove(admission.request_id.as_str());
                            return Err(public_error(error.code, &admission.operation, false));
                        }
                        break (execution, permit);
                    }
                    DedupDecision::BypassForTaskCancel => {
                        return Err(public_error(
                            ErrorCode::InternalError,
                            &admission.operation,
                            false,
                        ));
                    }
                }
            }
            let mut receiver = wait_for_terminal;
            let _ = receiver.changed().await;
        };

        let current_executor = self.capabilities.current().and_then(|current| {
            require_ready(current.context.readiness)?;
            crate::resolve_execution(&current, admission.route)
        });
        match current_executor {
            Ok(current) if ExecutorRecord::from(&current) == execution.executor => {}
            Ok(_) => {
                drop(permit);
                return self
                    .settle_synchronous(
                        &admission.request_id,
                        ExecutionOutcome::Failed {
                            error: public_error(
                                ErrorCode::StaleAuthority,
                                &admission.operation,
                                false,
                            ),
                            encoded_bytes: RESERVE_FLOOR_BYTES,
                        },
                        true,
                        ended_at,
                        terminal_at_ms,
                    )
                    .await;
            }
            Err(error) => {
                drop(permit);
                return self
                    .settle_synchronous(
                        &admission.request_id,
                        ExecutionOutcome::Failed {
                            error: public_error(error.code, &admission.operation, false),
                            encoded_bytes: RESERVE_FLOOR_BYTES,
                        },
                        true,
                        ended_at,
                        terminal_at_ms,
                    )
                    .await;
            }
        }
        let completion = self.executions.claim_and_start(execution.clone()).await;
        drop(permit);
        match completion {
            Ok(completion)
                if completion.fence.runtime_epoch == execution.executor.fence.runtime_epoch
                    && completion.fence.host_generation
                        == execution.executor.fence.host_generation
                    && completion.fence.runtime_instance_id
                        == execution.executor.fence.runtime_instance_id
                    && completion.capability_generation
                        == execution.executor.capability_generation =>
            {
                self.settle_synchronous(
                    &admission.request_id,
                    completion.outcome,
                    completion.cleanup_verified,
                    ended_at,
                    terminal_at_ms,
                )
                .await
            }
            Ok(completion) => {
                self.settle_synchronous(
                    &admission.request_id,
                    completion.outcome,
                    false,
                    ended_at,
                    terminal_at_ms,
                )
                .await
            }
            Err(failure) => {
                self.settle_synchronous(
                    &admission.request_id,
                    ExecutionOutcome::Failed {
                        error: public_error(failure.error.code, &admission.operation, false),
                        encoded_bytes: RESERVE_FLOOR_BYTES,
                    },
                    failure.cleanup_verified,
                    ended_at,
                    terminal_at_ms,
                )
                .await
            }
        }
    }

    pub async fn admit_task(
        &self,
        admission: TaskAdmission,
    ) -> Result<TaskAdmissionResult, DomainError> {
        let _guard = self.mutation.lock().await;
        let mut state = self.persistence.load()?;
        match state.dedup.decide_and_reserve(
            admission.request_id.clone(),
            admission.payload_sha256,
            admission.now_ms,
            false,
        )? {
            DedupDecision::Replay => {
                let existing = state
                    .tasks
                    .iter()
                    .find(|task| task.request_id() == Some(&admission.request_id))
                    .ok_or_else(|| {
                        DomainError::new(
                            ErrorCode::InternalError,
                            "retained Task request has no canonical Task",
                        )
                    })?;
                return Ok(TaskAdmissionResult::Replay(existing.snapshot()));
            }
            DedupDecision::Admit => {}
            DedupDecision::BypassForTaskCancel => {
                return Err(DomainError::new(
                    ErrorCode::InternalError,
                    "Task admission cannot bypass dedup",
                ));
            }
        }
        prune_task_history(&mut state, admission.now_ms)?;
        let pinned_tasks = state
            .tasks
            .iter()
            .filter(|task| task_is_pinned(&state, task, admission.now_ms))
            .count();
        if pinned_tasks >= MAX_TERMINAL_TASKS {
            return Err(DomainError::new(
                ErrorCode::ResourceLimit,
                "Task replay retention capacity is full",
            ));
        }
        let capability = self.capabilities.current()?;
        require_ready(capability.context.readiness)?;
        let executor = crate::resolve_execution(&capability, admission.route)?;
        let queued = state
            .tasks
            .iter()
            .filter(|task| task.state() == TaskState::Queued)
            .count();
        if queued >= MAX_QUEUED_TASKS as usize {
            return Err(DomainError::new(
                ErrorCode::ResourceLimit,
                "Task queue is full",
            ));
        }
        let reservation = admission.settlement_bound_bytes.max(RESERVE_FLOOR_BYTES);
        let total = state
            .total_committed_and_reserved()
            .and_then(|value| value.checked_add(TASK_RECORD_BYTES))
            .and_then(|value| value.checked_add(reservation))
            .ok_or_else(|| DomainError::new(ErrorCode::ResourceLimit, "store capacity overflow"))?;
        if total > STORE_LIMIT_BYTES {
            return Err(DomainError::new(
                ErrorCode::ResourceLimit,
                "store settlement capacity is full",
            ));
        }
        let mut lifecycle = TaskLifecycle::new();
        lifecycle.apply(TaskEvent::Queue)?;
        state.used_bytes += TASK_RECORD_BYTES;
        state.reserved_bytes += reservation;
        state.tasks.push(TaskRecord {
            task_id: admission.task_id,
            execution_id: admission.execution_id,
            lifecycle,
            tool: admission.tool,
            action: admission.action,
            created_at: admission.created_at,
            started_at: None,
            ended_at: None,
            waiting_reason: None,
            origin: TaskOrigin::Request {
                request_id: admission.request_id,
                executor: ExecutorRecord::from(&executor),
                route: admission.route,
                payload: Box::new(admission.payload),
            },
            result: None,
            error: None,
            reserved_bytes: reservation,
        });
        let snapshot = state.tasks.last().expect("Task was appended").snapshot();
        self.commit(state)?;
        Ok(TaskAdmissionResult::Admitted(snapshot))
    }

    pub async fn run_task(
        &self,
        task_id: &TaskId,
        started_at: String,
        terminal_at_ms: u64,
    ) -> Result<TaskSnapshot, DomainError> {
        let permit =
            self.leaf_permits.acquire().await.map_err(|_| {
                DomainError::new(ErrorCode::InternalError, "execution permits closed")
            })?;
        let (execution, completion) = {
            let _guard = self.mutation.lock().await;
            let mut state = self.persistence.load()?;
            let task_index = state
                .tasks
                .iter()
                .position(|task| &task.task_id == task_id)
                .ok_or_else(|| DomainError::new(ErrorCode::NotFound, "Task not found"))?;
            if state.tasks[task_index].lifecycle.is_terminal() {
                return Ok(state.tasks[task_index].snapshot());
            }
            let TaskOrigin::Request {
                executor: admitted_executor,
                route,
                payload,
                ..
            } = state.tasks[task_index].origin.clone()
            else {
                return Err(DomainError::new(
                    ErrorCode::Unsupported,
                    "an AutomationExecution drives its own container Task",
                ));
            };
            let current = self.capabilities.current().and_then(|current| {
                require_ready(current.context.readiness)?;
                crate::resolve_execution(&current, route)
            });
            let current_executor = match current {
                Ok(executor) => executor,
                Err(error) => {
                    drop(_guard);
                    drop(permit);
                    return self
                        .settle(
                            task_id,
                            ExecutionOutcome::Failed {
                                error: public_error(error.code, "runtime.execution", false),
                                encoded_bytes: RESERVE_FLOOR_BYTES,
                            },
                            true,
                            settlement_instant()?,
                            terminal_at_ms,
                        )
                        .await;
                }
            };
            if ExecutorRecord::from(&current_executor) != admitted_executor {
                drop(_guard);
                drop(permit);
                return self
                    .interrupt_before_execution(task_id, settlement_instant()?, terminal_at_ms)
                    .await;
            }
            state.tasks[task_index].lifecycle.apply(TaskEvent::Start)?;
            state.tasks[task_index].started_at = Some(started_at);
            let execution = AdmittedExecution {
                execution_id: state.tasks[task_index].execution_id.clone(),
                task_id: Some(state.tasks[task_index].task_id.clone()),
                executor: admitted_executor,
                payload: *payload,
            };
            self.commit(state)?;
            let completion = self.executions.claim_and_start(execution.clone());
            (execution, completion)
        };
        let completion = match completion.await {
            Ok(completion) => completion,
            Err(failure) => {
                drop(permit);
                let outcome = if failure.error.code == ErrorCode::Cancelled {
                    ExecutionOutcome::Cancelled {
                        error: public_error(ErrorCode::Cancelled, "runtime.execution", false),
                        encoded_bytes: RESERVE_FLOOR_BYTES,
                    }
                } else {
                    ExecutionOutcome::Failed {
                        error: public_error(failure.error.code, "runtime.execution", false),
                        encoded_bytes: RESERVE_FLOOR_BYTES,
                    }
                };
                return self
                    .settle(
                        task_id,
                        outcome,
                        failure.cleanup_verified,
                        settlement_instant()?,
                        terminal_at_ms,
                    )
                    .await;
            }
        };
        drop(permit);
        if completion.fence.runtime_epoch != execution.executor.fence.runtime_epoch
            || completion.fence.host_generation != execution.executor.fence.host_generation
            || completion.fence.runtime_instance_id != execution.executor.fence.runtime_instance_id
            || completion.capability_generation != execution.executor.capability_generation
        {
            return self
                .interrupt_after_stale_completion(task_id, settlement_instant()?, terminal_at_ms)
                .await;
        }
        self.settle(
            task_id,
            completion.outcome,
            completion.cleanup_verified,
            settlement_instant()?,
            terminal_at_ms,
        )
        .await
    }

    pub async fn cancel_task(
        &self,
        task_id: &TaskId,
        ended_at: String,
        terminal_at_ms: u64,
    ) -> Result<TaskSnapshot, DomainError> {
        let (execution_id, operation) = {
            let _guard = self.mutation.lock().await;
            let mut state = self.persistence.load()?;
            let pruned = prune_task_history(&mut state, terminal_at_ms)?;
            let task_index = match state.tasks.iter().position(|task| &task.task_id == task_id) {
                Some(index) => index,
                None => {
                    if pruned {
                        self.commit(state)?;
                    }
                    return Err(DomainError::new(ErrorCode::NotFound, "Task not found"));
                }
            };
            if state.tasks[task_index].lifecycle.is_terminal()
                || state.tasks[task_index].lifecycle.cancel_requested()
            {
                let snapshot = state.tasks[task_index].snapshot();
                if pruned {
                    self.commit(state)?;
                }
                return Ok(snapshot);
            }
            state.tasks[task_index]
                .lifecycle
                .apply(TaskEvent::RequestCancel)?;
            if matches!(
                state.tasks[task_index].origin,
                TaskOrigin::AutomationExecution { .. }
            ) {
                // A queued AutomationExecution settles with its container Task in this commit; a
                // running one observes the durable cancel request (S-AUTO-002).
                let execution_id = state.tasks[task_index].execution_id.clone();
                if matches!(
                    state.tasks[task_index].state(),
                    TaskState::Created | TaskState::Queued
                ) {
                    let operation = task_operation(&state.tasks[task_index]);
                    crate::settle_automation_in_state(
                        &mut state,
                        &execution_id,
                        crate::AutomationExecutionOutcome::Cancelled(public_error(
                            ErrorCode::Cancelled,
                            &operation,
                            false,
                        )),
                        &ended_at,
                    )?;
                }
                let snapshot = state
                    .task(task_id)
                    .map(TaskRecord::snapshot)
                    .ok_or_else(|| DomainError::new(ErrorCode::NotFound, "Task not found"))?;
                prune_task_history(&mut state, terminal_at_ms)?;
                self.commit(state)?;
                // Wake the running execution only after its cancel request is durable.
                self.request_automation_cancellation(&execution_id);
                return Ok(snapshot);
            }
            if matches!(
                state.tasks[task_index].state(),
                TaskState::Created | TaskState::Queued
            ) {
                let operation = task_operation(&state.tasks[task_index]);
                let error = public_error(ErrorCode::Cancelled, &operation, false);
                let terminal_growth = encoded_cancel_terminal_bytes(&ended_at, &error)?;
                let reservation = state.tasks[task_index].reserved_bytes;
                if terminal_growth > reservation {
                    return Err(DomainError::new(
                        ErrorCode::ResourceLimit,
                        "Task cancellation exceeds its settlement reservation",
                    ));
                }
                state.tasks[task_index]
                    .lifecycle
                    .apply(TaskEvent::SettleCancellation {
                        cleanup_verified: true,
                    })?;
                state.tasks[task_index].ended_at = Some(ended_at);
                state.tasks[task_index].error = Some(error);
                let request_id = state.tasks[task_index].request_id().cloned();
                state.used_bytes =
                    state
                        .used_bytes
                        .checked_add(terminal_growth)
                        .ok_or_else(|| {
                            DomainError::new(ErrorCode::ResourceLimit, "store size overflow")
                        })?;
                state.reserved_bytes =
                    state
                        .reserved_bytes
                        .checked_sub(reservation)
                        .ok_or_else(|| {
                            DomainError::new(ErrorCode::InternalError, "reservation underflow")
                        })?;
                if let Some(request_id) = &request_id {
                    state.dedup.settle(request_id, terminal_at_ms)?;
                }
                let snapshot = state.tasks[task_index].snapshot();
                prune_task_history(&mut state, terminal_at_ms)?;
                self.commit(state)?;
                return Ok(snapshot);
            }
            let execution_id = state.tasks[task_index].execution_id.clone();
            let operation = task_operation(&state.tasks[task_index]);
            self.commit(state)?;
            (execution_id, operation)
        };
        let cancel_outcome = self.executions.cancel(&execution_id).await.map_err(|_| {
            DomainError::new(
                ErrorCode::CancelFailed,
                "owning executor could not complete cancellation",
            )
        })?;
        match cancel_outcome {
            crate::ExecutionCancelOutcome::Cancelled { cleanup_verified } => {
                let error = public_error(ErrorCode::Cancelled, &operation, false);
                let encoded_bytes = encoded_cancel_terminal_bytes(&ended_at, &error)?;
                self.settle(
                    task_id,
                    ExecutionOutcome::Cancelled {
                        error,
                        encoded_bytes,
                    },
                    cleanup_verified,
                    ended_at,
                    terminal_at_ms,
                )
                .await
            }
            crate::ExecutionCancelOutcome::CompletionWon => {
                tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    loop {
                        let snapshot = self.get_task(task_id, terminal_at_ms).await?;
                        if matches!(
                            snapshot.state,
                            TaskState::Completed
                                | TaskState::Failed
                                | TaskState::Cancelled
                                | TaskState::Interrupted
                        ) {
                            return Ok(snapshot);
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .map_err(|_| {
                    DomainError::new(
                        ErrorCode::IoError,
                        "completed execution did not settle its Task",
                    )
                })?
            }
        }
    }

    pub async fn recover_old_instance(
        &self,
        old_instance_id: &UuidV4,
        ended_at: String,
        terminal_at_ms: u64,
    ) -> Result<usize, DomainError> {
        if self.host_control.recover(old_instance_id)? != RecoveryProof::Clean {
            return Err(DomainError::new(
                ErrorCode::IoError,
                "prior execution cleanup is unverified",
            ));
        }
        let _guard = self.mutation.lock().await;
        let mut state = self.persistence.load()?;
        // Old-instance AutomationExecutions settle with their container Tasks and tombstones first;
        // the generic Task pass then skips those terminal Tasks.
        let mut interrupted = crate::interrupt_old_instance_automation_executions(
            &mut state,
            old_instance_id,
            &ended_at,
        )?;
        for task in &mut state.tasks {
            if !task.lifecycle.is_terminal() && &task.fence().runtime_instance_id == old_instance_id
            {
                task.lifecycle.apply(TaskEvent::HostLost)?;
                task.ended_at = Some(ended_at.clone());
                task.error = Some(public_error(
                    ErrorCode::IoError,
                    &format!("{}.{}", tool_token(task.tool), task.action),
                    false,
                ));
                state.reserved_bytes = state
                    .reserved_bytes
                    .checked_sub(task.reserved_bytes)
                    .ok_or_else(|| {
                        DomainError::new(ErrorCode::InternalError, "reservation underflow")
                    })?;
                state.used_bytes = state
                    .used_bytes
                    .checked_add(RESERVE_FLOOR_BYTES.min(task.reserved_bytes))
                    .ok_or_else(|| {
                        DomainError::new(ErrorCode::ResourceLimit, "store size overflow")
                    })?;
                if let Some(request_id) = task.request_id() {
                    state.dedup.settle(request_id, terminal_at_ms)?;
                }
                interrupted += 1;
            }
        }
        for execution in &mut state.synchronous_executions {
            if !execution.state.is_terminal()
                && &execution.executor.fence.runtime_instance_id == old_instance_id
            {
                execution.state = SynchronousExecutionState::Interrupted;
                execution.ended_at = Some(ended_at.clone());
                execution.error = Some(public_error(
                    ErrorCode::IoError,
                    &execution.operation,
                    false,
                ));
                execution.terminal_bytes = RESERVE_FLOOR_BYTES.min(execution.reserved_bytes);
                state.reserved_bytes = state
                    .reserved_bytes
                    .checked_sub(execution.reserved_bytes)
                    .ok_or_else(|| {
                    DomainError::new(ErrorCode::InternalError, "reservation underflow")
                })?;
                state.used_bytes = state
                    .used_bytes
                    .checked_add(execution.terminal_bytes)
                    .ok_or_else(|| {
                        DomainError::new(ErrorCode::ResourceLimit, "store size overflow")
                    })?;
                state.dedup.settle(&execution.request_id, terminal_at_ms)?;
                interrupted += 1;
            }
        }
        prune_task_history(&mut state, terminal_at_ms)?;
        prune_synchronous_history(&mut state, terminal_at_ms);
        self.commit(state)?;
        Ok(interrupted)
    }

    pub async fn handle_task_control(
        &self,
        call: TaskControlCall,
        ended_at: String,
        now_ms: u64,
    ) -> Result<serde_json::Value, DomainError> {
        let result = match call {
            TaskControlCall::List(input) => serde_json::to_value(TaskListResult {
                tasks: self
                    .list_tasks(input.states.as_deref(), input.limit as usize, now_ms)
                    .await?,
            }),
            TaskControlCall::Get(input) => {
                serde_json::to_value(self.get_task(&input.task_id, now_ms).await?)
            }
            TaskControlCall::Cancel(input) => {
                serde_json::to_value(self.cancel_task(&input.task_id, ended_at, now_ms).await?)
            }
        };
        result.map_err(|_| {
            DomainError::new(
                ErrorCode::InternalError,
                "Task-control result serialization failed",
            )
        })
    }

    pub async fn get_task(
        &self,
        task_id: &TaskId,
        now_ms: u64,
    ) -> Result<TaskSnapshot, DomainError> {
        let _guard = self.mutation.lock().await;
        let mut state = self.persistence.load()?;
        let pruned = prune_task_history(&mut state, now_ms)?;
        let snapshot = state
            .task(task_id)
            .map(TaskRecord::snapshot)
            .ok_or_else(|| DomainError::new(ErrorCode::NotFound, "Task not found"));
        if pruned {
            self.commit(state)?;
        }
        snapshot
    }

    pub async fn list_tasks(
        &self,
        states: Option<&[TaskState]>,
        limit: usize,
        now_ms: u64,
    ) -> Result<Vec<TaskSummary>, DomainError> {
        if !(1..=500).contains(&limit) || states.is_some_and(|states| states.len() > 7) {
            return Err(DomainError::invalid("Task list input is out of bounds"));
        }
        let _guard = self.mutation.lock().await;
        let mut state = self.persistence.load()?;
        let pruned = prune_task_history(&mut state, now_ms)?;
        let mut tasks = state
            .tasks
            .iter()
            .filter(|task| states.is_none_or(|states| states.contains(&task.state())))
            .map(TaskRecord::summary)
            .collect::<Vec<_>>();
        tasks.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| right.task_id.as_str().cmp(left.task_id.as_str()))
        });
        tasks.truncate(limit.min(MAX_TERMINAL_TASKS));
        if pruned {
            self.commit(state)?;
        }
        Ok(tasks)
    }

    async fn interrupt_before_execution(
        &self,
        task_id: &TaskId,
        ended_at: String,
        terminal_at_ms: u64,
    ) -> Result<TaskSnapshot, DomainError> {
        let _guard = self.mutation.lock().await;
        let mut state = self.persistence.load()?;
        let task_index = state
            .tasks
            .iter()
            .position(|task| &task.task_id == task_id)
            .ok_or_else(|| DomainError::new(ErrorCode::NotFound, "Task not found"))?;
        if state.tasks[task_index].lifecycle.is_terminal() {
            return Ok(state.tasks[task_index].snapshot());
        }
        let reservation = state.tasks[task_index].reserved_bytes;
        let request_id = state.tasks[task_index].request_id().cloned();
        let operation = task_operation(&state.tasks[task_index]);
        state.tasks[task_index]
            .lifecycle
            .apply(TaskEvent::HostLost)?;
        state.tasks[task_index].ended_at = Some(ended_at);
        state.tasks[task_index].error =
            Some(public_error(ErrorCode::StaleAuthority, &operation, false));
        state.reserved_bytes = state
            .reserved_bytes
            .checked_sub(reservation)
            .ok_or_else(|| DomainError::new(ErrorCode::InternalError, "reservation underflow"))?;
        state.used_bytes = state
            .used_bytes
            .checked_add(RESERVE_FLOOR_BYTES.min(reservation))
            .ok_or_else(|| DomainError::new(ErrorCode::ResourceLimit, "store size overflow"))?;
        if let Some(request_id) = &request_id {
            state.dedup.settle(request_id, terminal_at_ms)?;
        }
        let snapshot = state.tasks[task_index].snapshot();
        prune_task_history(&mut state, terminal_at_ms)?;
        self.commit(state)?;
        Ok(snapshot)
    }

    async fn interrupt_after_stale_completion(
        &self,
        task_id: &TaskId,
        ended_at: String,
        terminal_at_ms: u64,
    ) -> Result<TaskSnapshot, DomainError> {
        let error = public_error(ErrorCode::StaleAuthority, "runtime.execution", false);
        self.settle(
            task_id,
            ExecutionOutcome::Failed {
                error,
                encoded_bytes: RESERVE_FLOOR_BYTES,
            },
            false,
            ended_at,
            terminal_at_ms,
        )
        .await
    }

    async fn settle(
        &self,
        task_id: &TaskId,
        outcome: ExecutionOutcome,
        cleanup_verified: bool,
        ended_at: String,
        terminal_at_ms: u64,
    ) -> Result<TaskSnapshot, DomainError> {
        let _guard = self.mutation.lock().await;
        let mut state = self.persistence.load()?;
        let task = state
            .tasks
            .iter_mut()
            .find(|task| &task.task_id == task_id)
            .ok_or_else(|| DomainError::new(ErrorCode::NotFound, "Task not found"))?;
        if !cleanup_verified {
            self.host_control.cleanup_unverified(
                &domain::AdmissionFence {
                    runtime_epoch: task.fence().runtime_epoch.clone(),
                    host_generation: task.fence().host_generation,
                    runtime_instance_id: task.fence().runtime_instance_id.clone(),
                },
                &task.execution_id,
            )?;
        }
        if task.lifecycle.is_terminal() {
            return Ok(task.snapshot());
        }
        let (encoded_bytes, mut event, result, error) = match outcome {
            ExecutionOutcome::Completed {
                result,
                encoded_bytes,
            } => (
                encoded_bytes,
                TaskEvent::Complete {
                    postcondition_verified: true,
                    cleanup_verified,
                },
                Some(result),
                None,
            ),
            ExecutionOutcome::Failed {
                error,
                encoded_bytes,
            } => (
                encoded_bytes,
                TaskEvent::Fail { cleanup_verified },
                None,
                Some(error),
            ),
            ExecutionOutcome::Cancelled {
                error,
                encoded_bytes,
            } => (
                encoded_bytes,
                TaskEvent::SettleCancellation { cleanup_verified },
                None,
                Some(error),
            ),
            ExecutionOutcome::SynchronousCompleted { encoded_bytes, .. } => (
                encoded_bytes,
                TaskEvent::Fail { cleanup_verified },
                None,
                Some(public_error(
                    ErrorCode::InternalError,
                    "runtime.execution",
                    false,
                )),
            ),
        };
        let terminal_growth = if encoded_bytes <= task.reserved_bytes && cleanup_verified {
            task.result = result;
            task.error = error;
            encoded_bytes
        } else {
            event = TaskEvent::Fail { cleanup_verified };
            task.result = None;
            task.error = Some(if cleanup_verified {
                public_error(ErrorCode::ResourceLimit, "runtime.execution", false)
            } else {
                cleanup_unverified_error("runtime.execution")
            });
            RESERVE_FLOOR_BYTES.min(task.reserved_bytes)
        };
        if let Some(error) = &mut task.error
            && error.operation == "runtime.execution"
        {
            error.operation = format!("{}.{}", tool_token(task.tool), task.action);
        }
        task.lifecycle.apply(event)?;
        task.ended_at = Some(ended_at);
        state.used_bytes = state
            .used_bytes
            .checked_add(terminal_growth)
            .ok_or_else(|| DomainError::new(ErrorCode::ResourceLimit, "store size overflow"))?;
        state.reserved_bytes = state
            .reserved_bytes
            .checked_sub(task.reserved_bytes)
            .ok_or_else(|| DomainError::new(ErrorCode::InternalError, "reservation underflow"))?;
        if let Some(request_id) = task.request_id() {
            state.dedup.settle(request_id, terminal_at_ms)?;
        }
        let snapshot = task.snapshot();
        prune_task_history(&mut state, terminal_at_ms)?;
        self.commit(state)?;
        Ok(snapshot)
    }

    async fn settle_synchronous(
        &self,
        request_id: &RequestId,
        outcome: ExecutionOutcome,
        cleanup_verified: bool,
        ended_at: String,
        terminal_at_ms: u64,
    ) -> Result<serde_json::Value, PublicError> {
        let _guard = self.mutation.lock().await;
        let mut state = self
            .persistence
            .load()
            .map_err(|error| public_error(error.code, "runtime.execution", false))?;
        let record_index = state
            .synchronous_executions
            .iter()
            .position(|record| &record.request_id == request_id)
            .ok_or_else(|| public_error(ErrorCode::NotFound, "runtime.execution", false))?;
        if state.synchronous_executions[record_index]
            .state
            .is_terminal()
        {
            return synchronous_result(&state.synchronous_executions[record_index]);
        }
        if !cleanup_verified {
            let record = &state.synchronous_executions[record_index];
            self.host_control
                .cleanup_unverified(
                    &domain::AdmissionFence {
                        runtime_epoch: record.executor.fence.runtime_epoch.clone(),
                        host_generation: record.executor.fence.host_generation,
                        runtime_instance_id: record.executor.fence.runtime_instance_id.clone(),
                    },
                    &record.execution_id,
                )
                .map_err(|error| public_error(error.code, &record.operation, false))?;
        }
        let operation = state.synchronous_executions[record_index].operation.clone();
        let reservation = state.synchronous_executions[record_index].reserved_bytes;
        let outcome = measured_execution_outcome(outcome, &operation)?;
        let (encoded_bytes, record_state, result, mut error) = match outcome {
            ExecutionOutcome::SynchronousCompleted {
                result,
                encoded_bytes,
            } if cleanup_verified && encoded_bytes <= reservation => (
                encoded_bytes,
                SynchronousExecutionState::Completed,
                Some(result),
                None,
            ),
            ExecutionOutcome::Failed {
                error,
                encoded_bytes,
            }
            | ExecutionOutcome::Cancelled {
                error,
                encoded_bytes,
            } if cleanup_verified && encoded_bytes <= reservation => (
                encoded_bytes,
                SynchronousExecutionState::Failed,
                None,
                Some(error),
            ),
            _ if !cleanup_verified => (
                RESERVE_FLOOR_BYTES.min(reservation),
                SynchronousExecutionState::Interrupted,
                None,
                Some(cleanup_unverified_error(&operation)),
            ),
            _ => (
                RESERVE_FLOOR_BYTES.min(reservation),
                SynchronousExecutionState::Failed,
                None,
                Some(public_error(ErrorCode::ResourceLimit, &operation, false)),
            ),
        };
        if let Some(error) = &mut error {
            error.operation = operation.clone();
        }
        // This call is answered with the whole result; a replay is answered only from what the
        // record keeps, and a result over the retention bound is not kept.
        let answer = match (&result, &error) {
            (Some(result), _) => Ok(result.clone()),
            (None, Some(error)) => Err(error.clone()),
            (None, None) => Err(public_error(ErrorCode::InternalError, &operation, false)),
        };
        let (result, retained_bytes) = match result {
            Some(_) if encoded_bytes > RETAINED_RESULT_LIMIT_BYTES => {
                // What the record still yields is the replay refusal, so that is what it costs.
                let refusal = serde_json::to_vec(&unretained_replay_error(&operation))
                    .map_err(|_| public_error(ErrorCode::InternalError, &operation, false))?;
                (None, refusal.len() as u64)
            }
            result => (result, encoded_bytes),
        };
        {
            let record = &mut state.synchronous_executions[record_index];
            record.state = record_state;
            record.ended_at = Some(ended_at);
            record.result = result;
            record.error = error;
            record.terminal_bytes = retained_bytes;
        }
        state.used_bytes = state
            .used_bytes
            .checked_add(retained_bytes)
            .ok_or_else(|| public_error(ErrorCode::ResourceLimit, &operation, false))?;
        state.reserved_bytes = state
            .reserved_bytes
            .checked_sub(reservation)
            .ok_or_else(|| public_error(ErrorCode::InternalError, &operation, false))?;
        state
            .dedup
            .settle(request_id, terminal_at_ms)
            .map_err(|error| public_error(error.code, &operation, false))?;
        self.commit(state)
            .map_err(|error| public_error(error.code, &operation, false))?;
        if let Some(sender) = self
            .synchronous_waiters
            .lock()
            .expect("synchronous waiter lock")
            .remove(request_id.as_str())
        {
            let _ = sender.send(());
        }
        answer
    }

    /// Applies one Core-owned state transition under the mutation lock. The transition reports
    /// whether it changed canonical state; only a change is committed, and a failed transition
    /// commits nothing.
    pub(crate) async fn state_transition<T>(
        &self,
        transition: impl FnOnce(
            &mut RuntimeState,
            &CapabilitySnapshot,
        ) -> Result<(T, bool), DomainError>,
    ) -> Result<T, DomainError> {
        let _guard = self.mutation.lock().await;
        let mut state = self.persistence.load()?;
        let capability = self.capabilities.current()?;
        let (result, changed) = transition(&mut state, &capability)?;
        if changed {
            self.commit(state)?;
        }
        Ok(result)
    }

    /// Runs one read against the current canonical state under the mutation lock.
    pub(crate) async fn read_state<T>(
        &self,
        read: impl FnOnce(&RuntimeState) -> Result<T, DomainError>,
    ) -> Result<T, DomainError> {
        let _guard = self.mutation.lock().await;
        let state = self.persistence.load()?;
        read(&state)
    }

    /// Commits one Core-only mutation that has no executor and retains its public result for the
    /// request's dedup window (S-CONTRACT-005). A rejected mutation commits nothing, so its
    /// request_id is not retained and a corrected request may reuse it.
    pub(crate) async fn retained_mutation(
        &self,
        request_id: RequestId,
        payload_sha256: String,
        now_ms: u64,
        mutate: impl FnOnce(&mut RuntimeState) -> Result<serde_json::Value, DomainError>,
    ) -> Result<serde_json::Value, DomainError> {
        let _guard = self.mutation.lock().await;
        let mut state = self.persistence.load()?;
        prune_synchronous_history(&mut state, now_ms);
        prune_retained_mutations(&mut state, now_ms);
        match state
            .dedup
            .decide_and_reserve(request_id.clone(), payload_sha256, now_ms, false)?
        {
            DedupDecision::Replay => {
                return state
                    .retained_mutations
                    .iter()
                    .find(|record| record.request_id == request_id)
                    .map(|record| record.result.clone())
                    .ok_or_else(|| {
                        DomainError::new(
                            ErrorCode::InternalError,
                            "retained request has no retained mutation result",
                        )
                    });
            }
            DedupDecision::Admit => {}
            DedupDecision::BypassForTaskCancel => {
                return Err(DomainError::new(
                    ErrorCode::InternalError,
                    "a retained mutation cannot bypass dedup",
                ));
            }
        }
        require_ready(self.capabilities.current()?.context.readiness)?;
        let result = mutate(&mut state)?;
        let growth = retained_mutation_bytes(&result)?;
        let total = state
            .total_committed_and_reserved()
            .and_then(|value| value.checked_add(growth))
            .ok_or_else(|| DomainError::new(ErrorCode::ResourceLimit, "store capacity overflow"))?;
        if total > STORE_LIMIT_BYTES {
            return Err(DomainError::new(
                ErrorCode::ResourceLimit,
                "store capacity is full",
            ));
        }
        state.used_bytes = state
            .used_bytes
            .checked_add(growth)
            .ok_or_else(|| DomainError::new(ErrorCode::ResourceLimit, "store size overflow"))?;
        state.dedup.settle(&request_id, now_ms)?;
        state.retained_mutations.push(RetainedMutationRecord {
            request_id,
            result: result.clone(),
        });
        self.commit(state)?;
        Ok(result)
    }

    fn commit(&self, mut state: RuntimeState) -> Result<(), DomainError> {
        let active_tasks = state
            .tasks
            .iter()
            .filter(|task| !task.lifecycle.is_terminal())
            .count();
        let expected = state.revision;
        state.revision = state.revision.checked_add(1).ok_or_else(|| {
            DomainError::new(ErrorCode::ResourceLimit, "store revision exhausted")
        })?;
        let canonical_revision = state.revision;
        self.persistence.compare_and_commit(expected, state)?;
        self.host_control
            .task_activity_changed(active_tasks, canonical_revision);
        self.canonical_changes.notify_one();
        Ok(())
    }
}

fn require_ready(readiness: RuntimeReadiness) -> Result<(), DomainError> {
    if readiness == RuntimeReadiness::Ready {
        Ok(())
    } else {
        Err(DomainError::new(
            ErrorCode::CapabilityUnavailable,
            "Runtime is not ready for execution",
        ))
    }
}

fn prune_task_history(state: &mut RuntimeState, now_ms: u64) -> Result<bool, DomainError> {
    let pinned_requests = state
        .dedup
        .entries()
        .iter()
        .filter(|entry| entry.expires_at_ms.is_none_or(|expiry| expiry > now_ms))
        .map(|entry| entry.request_id.as_str().to_owned())
        .collect::<HashSet<_>>();
    let cutoff_ms = now_ms.saturating_sub(TASK_HISTORY_RETENTION_MS);
    let mut remove = vec![false; state.tasks.len()];

    for (index, task) in state.tasks.iter().enumerate() {
        if task.lifecycle.is_terminal()
            && !task
                .request_id()
                .is_some_and(|request_id| pinned_requests.contains(request_id.as_str()))
            && terminal_millis(task)? <= cutoff_ms
        {
            remove[index] = true;
        }
    }

    let retained_terminal = state
        .tasks
        .iter()
        .enumerate()
        .filter(|(index, task)| task.lifecycle.is_terminal() && !remove[*index])
        .count();
    if retained_terminal > MAX_TERMINAL_TASKS {
        let mut candidates = state
            .tasks
            .iter()
            .enumerate()
            .filter(|(index, task)| {
                task.lifecycle.is_terminal()
                    && !remove[*index]
                    && !task
                        .request_id()
                        .is_some_and(|request_id| pinned_requests.contains(request_id.as_str()))
            })
            .map(|(index, task)| (index, task.created_at.as_str(), task.task_id.as_str()))
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| left.1.cmp(right.1).then_with(|| left.2.cmp(right.2)));
        let excess = retained_terminal - MAX_TERMINAL_TASKS;
        if candidates.len() < excess {
            return Err(DomainError::new(
                ErrorCode::ResourceLimit,
                "Task history is pinned by replay retention",
            ));
        }
        for (index, _, _) in candidates.into_iter().take(excess) {
            remove[index] = true;
        }
    }

    let removed = remove.iter().filter(|value| **value).count();
    if removed == 0 {
        return Ok(false);
    }
    let mut index = 0_usize;
    state.tasks.retain(|_| {
        let keep = !remove[index];
        index += 1;
        keep
    });
    state.used_bytes = state
        .used_bytes
        .saturating_sub(TASK_RECORD_BYTES.saturating_mul(removed as u64));
    Ok(true)
}

fn task_is_pinned(state: &RuntimeState, task: &TaskRecord, now_ms: u64) -> bool {
    state.dedup.entries().iter().any(|entry| {
        task.request_id() == Some(&entry.request_id)
            && entry.expires_at_ms.is_none_or(|expiry| expiry > now_ms)
    })
}

fn prune_synchronous_history(state: &mut RuntimeState, now_ms: u64) {
    let retained_requests = state
        .dedup
        .entries()
        .iter()
        .filter(|entry| entry.expires_at_ms.is_none_or(|expiry| expiry > now_ms))
        .map(|entry| entry.request_id.as_str().to_owned())
        .collect::<HashSet<_>>();
    let mut removed_bytes = 0_u64;
    state.synchronous_executions.retain(|execution| {
        let retain = !execution.state.is_terminal()
            || retained_requests.contains(execution.request_id.as_str());
        if !retain {
            removed_bytes = removed_bytes
                .saturating_add(SYNCHRONOUS_RECORD_BYTES)
                .saturating_add(execution.terminal_bytes);
        }
        retain
    });
    state.used_bytes = state.used_bytes.saturating_sub(removed_bytes);
}

fn retained_mutation_bytes(result: &serde_json::Value) -> Result<u64, DomainError> {
    let encoded = serde_json::to_vec(result).map_err(|_| {
        DomainError::new(
            ErrorCode::InternalError,
            "retained mutation result encoding failed",
        )
    })?;
    u64::try_from(encoded.len())
        .ok()
        .and_then(|bytes| bytes.checked_add(RETAINED_MUTATION_RECORD_BYTES))
        .ok_or_else(|| DomainError::new(ErrorCode::ResourceLimit, "store size overflow"))
}

fn prune_retained_mutations(state: &mut RuntimeState, now_ms: u64) {
    let retained_requests = state
        .dedup
        .entries()
        .iter()
        .filter(|entry| entry.expires_at_ms.is_none_or(|expiry| expiry > now_ms))
        .map(|entry| entry.request_id.as_str().to_owned())
        .collect::<HashSet<_>>();
    let mut removed_bytes = 0_u64;
    state.retained_mutations.retain(|record| {
        let retain = retained_requests.contains(record.request_id.as_str());
        if !retain {
            removed_bytes =
                removed_bytes.saturating_add(retained_mutation_bytes(&record.result).unwrap_or(0));
        }
        retain
    });
    state.used_bytes = state.used_bytes.saturating_sub(removed_bytes);
}

fn measured_execution_outcome(
    outcome: ExecutionOutcome,
    operation: &str,
) -> Result<ExecutionOutcome, PublicError> {
    match outcome {
        ExecutionOutcome::SynchronousCompleted {
            result,
            encoded_bytes: _,
        } => {
            let measured = serde_json::to_vec(&result)
                .map_err(|_| public_error(ErrorCode::InternalError, operation, false))?
                .len() as u64;
            Ok(ExecutionOutcome::SynchronousCompleted {
                result,
                encoded_bytes: measured,
            })
        }
        ExecutionOutcome::Failed {
            error,
            encoded_bytes: _,
        } => {
            let measured = serde_json::to_vec(&error)
                .map_err(|_| public_error(ErrorCode::InternalError, operation, false))?
                .len() as u64;
            Ok(ExecutionOutcome::Failed {
                error,
                encoded_bytes: measured,
            })
        }
        ExecutionOutcome::Cancelled {
            error,
            encoded_bytes: _,
        } => {
            let measured = serde_json::to_vec(&error)
                .map_err(|_| public_error(ErrorCode::InternalError, operation, false))?
                .len() as u64;
            Ok(ExecutionOutcome::Cancelled {
                error,
                encoded_bytes: measured,
            })
        }
        task => Ok(task),
    }
}

fn synchronous_result(
    record: &SynchronousExecutionRecord,
) -> Result<serde_json::Value, PublicError> {
    match record.state {
        // A result over the retention bound answered its own call only: a replay is told so and
        // must not run the operation a second time under the same request.
        SynchronousExecutionState::Completed => record
            .result
            .clone()
            .ok_or_else(|| unretained_replay_error(&record.operation)),
        SynchronousExecutionState::Failed | SynchronousExecutionState::Interrupted => Err(record
            .error
            .clone()
            .unwrap_or_else(|| public_error(ErrorCode::InternalError, &record.operation, false))),
        SynchronousExecutionState::Running => Err(public_error(
            ErrorCode::InternalError,
            &record.operation,
            false,
        )),
    }
}

/// The wall-clock instant of a settlement, which only the settling path can observe.
fn settlement_instant() -> Result<String, DomainError> {
    crate::BoottimeClock.wall().map(|(timestamp, _)| timestamp)
}

fn terminal_millis(task: &TaskRecord) -> Result<u64, DomainError> {
    let ended_at = task.ended_at.as_deref().ok_or_else(|| {
        DomainError::new(ErrorCode::IoError, "terminal Task has no end timestamp")
    })?;
    DateTime::parse_from_rfc3339(ended_at)
        .map_err(|_| DomainError::new(ErrorCode::IoError, "terminal Task timestamp is invalid"))?
        .timestamp_millis()
        .try_into()
        .map_err(|_| DomainError::new(ErrorCode::IoError, "terminal Task timestamp is invalid"))
}

fn encoded_cancel_terminal_bytes(ended_at: &str, error: &PublicError) -> Result<u64, DomainError> {
    serde_json::to_vec(&serde_json::json!({
        "ended_at": ended_at,
        "cancel_requested": true,
        "error": error,
    }))
    .map(|bytes| bytes.len() as u64)
    .map_err(|_| {
        DomainError::new(
            ErrorCode::InternalError,
            "Task cancellation settlement encoding failed",
        )
    })
}

/// What a replay of a request whose result was not retained answers with.
fn unretained_replay_error(operation: &str) -> PublicError {
    PublicError {
        message: Some(
            "This request already ran and its result was too large to keep for replay. Send the \
             call again as a new request."
                .to_owned(),
        ),
        ..public_error(ErrorCode::ResourceLimit, operation, false)
    }
}

fn public_error(code: ErrorCode, operation: &str, retryable: bool) -> PublicError {
    PublicError {
        code,
        operation: operation.to_owned(),
        retryable,
        message: None,
        capability: None,
        details: None,
    }
}

fn cleanup_unverified_error(operation: &str) -> PublicError {
    PublicError {
        code: ErrorCode::IoError,
        operation: operation.to_owned(),
        retryable: false,
        message: None,
        capability: None,
        details: Some(std::collections::BTreeMap::from([(
            "cleanup_unverified".to_owned(),
            contract::ErrorDetailValue::Boolean(true),
        )])),
    }
}

fn task_operation(task: &TaskRecord) -> String {
    format!("{}.{}", tool_token(task.tool), task.action)
}

fn tool_token(tool: MotherTool) -> &'static str {
    match tool {
        MotherTool::Context => "context",
        MotherTool::Filesystem => "filesystem",
        MotherTool::Command => "command",
        MotherTool::Network => "network",
        MotherTool::Visual => "visual",
        MotherTool::Android => "android",
        MotherTool::Automation => "automation",
        MotherTool::TaskControl => "task_control",
    }
}
