//! Authenticated daemon/APK companion channel and its private typed execution port.
//!
//! One channel owns one authenticated companion connection together with that
//! connection's correlation ledger, so a replacement connection can never adopt an
//! earlier execution's ids. It carries the port declared by S-HANDOFF-009: one
//! already-admitted S-ANDROID-001 primitive request per S-IPC-DAEMON-004
//! `CompanionExecute`.

use crate::{
    ConnectionLedger, MAX_FILE_DESCRIPTORS, MAX_OUTSTANDING, MessageKind, Operation, WireEnvelope,
    unix_transport::{ReceivedEnvelope, receive_envelope, send_envelope},
};
use contract::{ErrorCode, UuidV4};
use domain::DomainError;
use runtime::{AdmittedExecution, AndroidExecutionDispatch, AndroidPrimitiveResult};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    fs::File,
    net::Shutdown,
    os::fd::{AsRawFd, OwnedFd, RawFd},
    os::unix::net::UnixStream,
    sync::{Arc, Condvar, Mutex, MutexGuard, mpsc},
    thread::{self, JoinHandle},
    time::Duration,
};

/// One already-admitted typed primitive request for the companion execution surface.
pub struct CompanionPrimitiveRequest {
    pub primitive: String,
    pub payload: Value,
    pub execution_id: UuidV4,
    pub descriptors: Vec<(String, OwnedFd)>,
}

/// One typed result and its role-labelled descriptors.
#[derive(Debug)]
pub struct CompanionPrimitiveResult {
    pub payload: Value,
    pub descriptors: Vec<(String, OwnedFd)>,
}

/// The bounded wait for one companion primitive round trip. It is the fixed bound this
/// daemon applies to its authenticated Android primitive family on this socket.
pub const COMPANION_PRIMITIVE_TIMEOUT: Duration = Duration::from_millis(15_000);

/// The live companion connection, shared between the daemon connection loop that owns
/// it and the daemon-side execution surfaces that delegate through it. A surface reads
/// the current channel per request, so a connection loss withdraws the delegation at
/// once instead of leaving a stale handle behind (S-AUTH-003).
#[derive(Clone, Default)]
pub struct CompanionPort {
    channel: Arc<Mutex<Option<Arc<CompanionChannel>>>>,
}

impl CompanionPort {
    pub fn publish(&self, channel: Arc<CompanionChannel>) -> Result<(), DomainError> {
        *self
            .channel
            .lock()
            .map_err(|_| internal_error("companion port state is unavailable"))? = Some(channel);
        Ok(())
    }

    pub fn withdraw(&self) -> Result<(), DomainError> {
        *self
            .channel
            .lock()
            .map_err(|_| internal_error("companion port state is unavailable"))? = None;
        Ok(())
    }

    /// Publishes the live connection's owner fence. The daemon's active Runtime host owns the
    /// instance identity, so a host that activates after this connection was accepted rebinds
    /// its instance here instead of leaving the connection fenced to the instance state it was
    /// accepted under (S-AUTH-004). No live connection accepts the fence silently.
    pub fn set_fence(
        &self,
        runtime_epoch: UuidV4,
        host_generation: u64,
        runtime_instance_id: Option<UuidV4>,
    ) -> Result<(), DomainError> {
        match self.live()? {
            Some(channel) => channel.set_fence(runtime_epoch, host_generation, runtime_instance_id),
            None => Ok(()),
        }
    }

    fn live(&self) -> Result<Option<Arc<CompanionChannel>>, DomainError> {
        Ok(self
            .channel
            .lock()
            .map_err(|_| internal_error("companion port state is unavailable"))?
            .clone())
    }

    /// Issues one companion primitive execution and hands back its in-flight
    /// transaction together with the connection that carries it, so a caller that must
    /// also cancel the same execution addresses the exact connection it started on.
    pub(crate) fn submit(
        &self,
        request: CompanionPrimitiveRequest,
    ) -> Result<(Arc<CompanionChannel>, CompanionTransaction), DomainError> {
        let channel = self.live()?.ok_or_else(|| {
            DomainError::new(
                ErrorCode::CapabilityUnavailable,
                "the authenticated companion execution surface is unavailable",
            )
        })?;
        let transaction = channel.submit(request)?;
        Ok((channel, transaction))
    }
}

/// One in-flight companion execution. It owns the correlation identity and the reply
/// sink of the request it issued, and it reports nothing until that reply arrives.
pub struct CompanionTransaction {
    receiver: mpsc::Receiver<Result<CompanionPrimitiveResult, DomainError>>,
}

impl CompanionTransaction {
    /// Waits at most `slice` on this transaction. `None` means the request is still
    /// outstanding, so the caller keeps polling rather than inventing a settlement.
    pub fn poll(
        &mut self,
        slice: Duration,
    ) -> Option<Result<CompanionPrimitiveResult, DomainError>> {
        match self.receiver.recv_timeout(slice) {
            Ok(result) => Some(result),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => Some(Err(lost(ErrorCode::IoError))),
        }
    }
}

impl AndroidExecutionDispatch for CompanionPort {
    fn dispatch(
        &self,
        primitive: &str,
        payload: &[u8],
        execution: &AdmittedExecution,
    ) -> Result<AndroidPrimitiveResult, DomainError> {
        let channel = self.live()?.ok_or_else(|| {
            DomainError::new(
                ErrorCode::CapabilityUnavailable,
                "the authenticated companion execution surface is unavailable",
            )
        })?;
        let payload: Value = serde_json::from_slice(payload)
            .map_err(|_| DomainError::invalid("companion primitive payload is invalid"))?;
        let result = channel.execute(
            CompanionPrimitiveRequest {
                primitive: primitive.to_owned(),
                payload,
                execution_id: execution.execution_id.clone(),
                descriptors: Vec::new(),
            },
            COMPANION_PRIMITIVE_TIMEOUT,
        )?;
        Ok(AndroidPrimitiveResult {
            payload: serde_json::to_vec(&result.payload).map_err(|_| {
                DomainError::new(ErrorCode::InternalError, "companion result encoding failed")
            })?,
            descriptors: result
                .descriptors
                .into_iter()
                .map(|(role, descriptor)| (role, File::from(descriptor)))
                .collect(),
        })
    }
}

/// Owner-thread observation of the connection: an inbound request or a reply to a
/// daemon-issued request.
#[derive(Debug)]
pub enum CompanionEvent {
    Request(ReceivedEnvelope),
    Response(WireEnvelope),
}

pub struct CompanionChannel {
    inner: Arc<Inner>,
    reader: Mutex<Option<JoinHandle<()>>>,
}

struct Inner {
    writer: Mutex<UnixStream>,
    shared: Mutex<Shared>,
    ready: Condvar,
}

#[derive(Default)]
struct Shared {
    ledger: ConnectionLedger,
    fence: Option<CompanionFence>,
    waiters: HashMap<UuidV4, Waiter>,
    events: VecDeque<CompanionEvent>,
    failure: Option<ErrorCode>,
}

struct CompanionFence {
    runtime_epoch: UuidV4,
    host_generation: u64,
    runtime_instance_id: Option<UuidV4>,
}

type ExecutionSink = mpsc::Sender<Result<CompanionPrimitiveResult, DomainError>>;

enum Waiter {
    Owner,
    Execution(ExecutionSink),
}

impl CompanionChannel {
    /// Splits one authenticated connection into its reader and writer halves. The
    /// reader thread ends the channel on EOF, malformed input or an I/O failure.
    pub fn start(stream: &UnixStream) -> Result<Arc<Self>, DomainError> {
        let reader_stream = stream
            .try_clone()
            .map_err(|_| io_error("cannot split the companion connection"))?;
        let writer_stream = stream
            .try_clone()
            .map_err(|_| io_error("cannot split the companion connection"))?;
        let channel = Arc::new(Self {
            inner: Arc::new(Inner {
                writer: Mutex::new(writer_stream),
                shared: Mutex::new(Shared::default()),
                ready: Condvar::new(),
            }),
            reader: Mutex::new(None),
        });
        let inner = Arc::clone(&channel.inner);
        let handle = thread::Builder::new()
            .name("droidbridge-companion".to_owned())
            .spawn(move || {
                let _guard = ReaderGuard(Arc::clone(&inner));
                let mut stream = reader_stream;
                loop {
                    match receive_envelope(&mut stream)
                        .and_then(|received| inner.dispatch(received))
                    {
                        Ok(()) => {}
                        Err(error) => {
                            inner.lose(error.code);
                            break;
                        }
                    }
                }
            })
            .map_err(|_| io_error("cannot start the companion reader"))?;
        *channel
            .reader
            .lock()
            .map_err(|_| internal_error("companion reader state is unavailable"))? = Some(handle);
        Ok(channel)
    }

    /// The owner thread publishes the live owner fence before handling each message.
    pub fn set_fence(
        &self,
        runtime_epoch: UuidV4,
        host_generation: u64,
        runtime_instance_id: Option<UuidV4>,
    ) -> Result<(), DomainError> {
        self.shared()?.fence = Some(CompanionFence {
            runtime_epoch,
            host_generation,
            runtime_instance_id,
        });
        Ok(())
    }

    pub fn next_event(&self) -> Result<CompanionEvent, DomainError> {
        let mut shared = self.shared()?;
        loop {
            if let Some(code) = shared.failure {
                return Err(lost(code));
            }
            if let Some(event) = shared.events.pop_front() {
                return Ok(event);
            }
            shared = self
                .inner
                .ready
                .wait(shared)
                .map_err(|_| internal_error("companion condition is unavailable"))?;
        }
    }

    /// Issues one daemon-owned request whose reply the owner thread observes as an event.
    pub fn request_control(&self, envelope: &WireEnvelope) -> Result<(), DomainError> {
        {
            let mut shared = self.shared()?;
            if let Some(code) = shared.failure {
                return Err(lost(code));
            }
            shared.ledger.reserve_control_envelope(envelope)?;
            shared
                .waiters
                .insert(envelope.message_id.clone(), Waiter::Owner);
        }
        self.write(envelope, &[])
    }

    /// Answers one request the owner thread received from the companion. The
    /// descriptors are the sender's transferred duplicates and close after the send.
    pub fn respond(
        &self,
        envelope: &WireEnvelope,
        descriptors: Vec<(String, OwnedFd)>,
    ) -> Result<(), DomainError> {
        {
            let mut shared = self.shared()?;
            if let Some(code) = shared.failure {
                return Err(lost(code));
            }
            shared
                .ledger
                .observe_outgoing(envelope.message_id.clone())?;
        }
        let raw: Vec<RawFd> = descriptors
            .iter()
            .map(|(_, descriptor)| descriptor.as_raw_fd())
            .collect();
        let result = self.write(envelope, &raw);
        drop(descriptors);
        result
    }

    /// Issues one companion primitive execution and returns its in-flight transaction.
    /// The caller polls the transaction, so it stays able to deliver a cancellation for
    /// the same execution into this channel while the request is outstanding.
    pub fn submit(
        &self,
        request: CompanionPrimitiveRequest,
    ) -> Result<CompanionTransaction, DomainError> {
        let CompanionPrimitiveRequest {
            primitive,
            payload,
            execution_id,
            descriptors,
        } = request;
        if descriptors.len() > MAX_FILE_DESCRIPTORS {
            return Err(resource_error(
                "companion execution exceeds the descriptor bound",
            ));
        }
        let roles: Vec<String> = descriptors.iter().map(|(role, _)| role.clone()).collect();
        let (envelope, receiver) = {
            let mut shared = self.shared()?;
            if let Some(code) = shared.failure {
                return Err(lost(code));
            }
            let fence = shared.fence.as_ref().ok_or_else(|| {
                DomainError::new(
                    ErrorCode::CapabilityUnavailable,
                    "companion Runtime instance is unavailable",
                )
            })?;
            let instance = fence.runtime_instance_id.clone().ok_or_else(|| {
                DomainError::new(
                    ErrorCode::CapabilityUnavailable,
                    "companion Runtime instance is unavailable",
                )
            })?;
            let envelope = WireEnvelope::request(
                new_uuid()?,
                fence.runtime_epoch.clone(),
                fence.host_generation,
                Some(instance),
                Operation::CompanionExecute,
                json!({
                    "primitive": primitive,
                    "payload": payload,
                    "execution_id": execution_id,
                }),
                roles,
            );
            shared.ledger.reserve_business_envelope(&envelope)?;
            let (sink, receiver) = mpsc::channel();
            shared
                .waiters
                .insert(envelope.message_id.clone(), Waiter::Execution(sink));
            (envelope, receiver)
        };
        let raw: Vec<RawFd> = descriptors
            .iter()
            .map(|(_, descriptor)| descriptor.as_raw_fd())
            .collect();
        let written = self.write(&envelope, &raw);
        drop(descriptors);
        written?;
        Ok(CompanionTransaction { receiver })
    }

    /// Issues one companion primitive execution and waits for its typed result.
    pub fn execute(
        &self,
        request: CompanionPrimitiveRequest,
        deadline: Duration,
    ) -> Result<CompanionPrimitiveResult, DomainError> {
        let mut transaction = self.submit(request)?;
        transaction.poll(deadline).unwrap_or_else(|| {
            Err(DomainError::new(
                ErrorCode::Timeout,
                "companion execution did not settle within its deadline",
            ))
        })
    }

    /// Delivers one cancellation for an execution this daemon issued on this connection.
    /// The companion answers it as a control response the owner thread observes, so this
    /// never waits on the execution it is cancelling.
    pub fn cancel_execution(
        &self,
        primitive: &str,
        execution_id: &UuidV4,
    ) -> Result<(), DomainError> {
        let envelope = {
            let mut shared = self.shared()?;
            if let Some(code) = shared.failure {
                return Err(lost(code));
            }
            let fence = shared.fence.as_ref().ok_or_else(|| {
                DomainError::new(
                    ErrorCode::CapabilityUnavailable,
                    "companion Runtime instance is unavailable",
                )
            })?;
            let instance = fence.runtime_instance_id.clone().ok_or_else(|| {
                DomainError::new(
                    ErrorCode::CapabilityUnavailable,
                    "companion Runtime instance is unavailable",
                )
            })?;
            let envelope = WireEnvelope::request(
                new_uuid()?,
                fence.runtime_epoch.clone(),
                fence.host_generation,
                Some(instance),
                Operation::CompanionCancel,
                json!({
                    "primitive": primitive,
                    "execution_id": execution_id,
                }),
                Vec::new(),
            );
            shared.ledger.reserve_control_envelope(&envelope)?;
            shared
                .waiters
                .insert(envelope.message_id.clone(), Waiter::Owner);
            envelope
        };
        self.write(&envelope, &[])
    }

    pub fn close(&self) {
        let _ = self
            .inner
            .writer
            .lock()
            .map(|stream| stream.shutdown(Shutdown::Both));
        self.inner.lose(ErrorCode::IoError);
        let handle = self.reader.lock().ok().and_then(|mut reader| reader.take());
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }

    fn shared(&self) -> Result<MutexGuard<'_, Shared>, DomainError> {
        self.inner
            .shared
            .lock()
            .map_err(|_| internal_error("companion channel state is unavailable"))
    }

    fn write(&self, envelope: &WireEnvelope, descriptors: &[RawFd]) -> Result<(), DomainError> {
        let result = {
            let mut stream = self
                .inner
                .writer
                .lock()
                .map_err(|_| internal_error("companion writer is unavailable"))?;
            send_envelope(&mut stream, envelope, descriptors)
        };
        if result.is_err() {
            self.inner.lose(ErrorCode::IoError);
        }
        result
    }
}

impl Drop for CompanionChannel {
    fn drop(&mut self) {
        self.close();
    }
}

struct ReaderGuard(Arc<Inner>);

impl Drop for ReaderGuard {
    fn drop(&mut self) {
        self.0.lose(ErrorCode::IoError);
    }
}

impl Inner {
    fn dispatch(&self, received: ReceivedEnvelope) -> Result<(), DomainError> {
        let mut shared = self
            .shared
            .lock()
            .map_err(|_| internal_error("companion channel state is unavailable"))?;
        shared
            .ledger
            .observe_incoming(received.envelope.message_id.clone())?;
        match received.envelope.kind {
            MessageKind::Request => {
                if shared.events.len() >= MAX_OUTSTANDING {
                    return Err(resource_error(
                        "inbound companion requests exceed the outstanding bound",
                    ));
                }
                shared.events.push_back(CompanionEvent::Request(received));
            }
            MessageKind::Response => {
                let ReceivedEnvelope {
                    envelope,
                    descriptors,
                } = received;
                shared.ledger.complete_envelope(&envelope)?;
                let reply_to = envelope
                    .reply_to
                    .clone()
                    .ok_or_else(|| protocol_error("companion reply lacks correlation"))?;
                let roles = envelope.fd_roles.clone();
                match shared.waiters.remove(&reply_to) {
                    Some(Waiter::Owner) => {
                        if !descriptors.is_empty() {
                            return Err(protocol_error(
                                "daemon control response contains descriptors",
                            ));
                        }
                        shared.events.push_back(CompanionEvent::Response(envelope));
                    }
                    Some(Waiter::Execution(sink)) => {
                        let _ = sink.send(result_for(envelope.payload, &roles, descriptors));
                    }
                    None => return Err(protocol_error("unknown companion reply correlation")),
                }
            }
            MessageKind::Cancel => {
                return Err(protocol_error("unexpected cancellation direction"));
            }
        }
        drop(shared);
        self.ready.notify_all();
        Ok(())
    }

    fn lose(&self, code: ErrorCode) {
        let waiters = match self.shared.lock() {
            Ok(mut shared) => Shared::fail(&mut shared, code),
            Err(poisoned) => Shared::fail(&mut poisoned.into_inner(), code),
        };
        for sink in waiters {
            let _ = sink.send(Err(lost(code)));
        }
        self.ready.notify_all();
    }
}

impl Shared {
    fn fail(shared: &mut Self, code: ErrorCode) -> Vec<ExecutionSink> {
        if shared.failure.is_some() {
            return Vec::new();
        }
        shared.failure = Some(code);
        shared.events.clear();
        std::mem::take(&mut shared.waiters)
            .into_values()
            .filter_map(|waiter| match waiter {
                Waiter::Execution(sink) => Some(sink),
                Waiter::Owner => None,
            })
            .collect()
    }
}

fn result_for(
    payload: Value,
    roles: &[String],
    descriptors: Vec<OwnedFd>,
) -> Result<CompanionPrimitiveResult, DomainError> {
    let labelled: Vec<(String, OwnedFd)> = roles.iter().cloned().zip(descriptors).collect();
    if let Some(error) = payload.get("error") {
        let code: ErrorCode = error
            .get("code")
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .ok_or_else(|| protocol_error("companion failure carries no error code"))?;
        if !labelled.is_empty() {
            return Err(protocol_error(
                "companion failure result carries descriptors",
            ));
        }
        return Err(DomainError::new(
            code,
            "companion execution reported a typed failure",
        ));
    }
    let payload = payload
        .get("payload")
        .cloned()
        .ok_or_else(|| protocol_error("companion result carries no payload"))?;
    Ok(CompanionPrimitiveResult {
        payload,
        descriptors: labelled,
    })
}

fn new_uuid() -> Result<UuidV4, DomainError> {
    UuidV4::parse(uuid::Uuid::new_v4().hyphenated().to_string())
        .map_err(|_| internal_error("UUID generation failed"))
}

fn protocol_error(reason: &'static str) -> DomainError {
    DomainError::new(ErrorCode::ProtocolIncompatible, reason)
}

fn resource_error(reason: &'static str) -> DomainError {
    DomainError::new(ErrorCode::ResourceLimit, reason)
}

fn internal_error(reason: &'static str) -> DomainError {
    DomainError::new(ErrorCode::InternalError, reason)
}

fn io_error(reason: &'static str) -> DomainError {
    DomainError::new(ErrorCode::IoError, reason)
}

fn lost(code: ErrorCode) -> DomainError {
    DomainError::new(code, "companion execution channel is closed")
}
