package com.droidbridge.android.runtimehost

import android.net.LocalServerSocket
import android.net.LocalSocket
import android.os.ParcelFileDescriptor
import java.io.Closeable
import java.io.EOFException
import java.io.FileDescriptor
import java.io.InputStream
import java.io.OutputStream
import java.util.UUID
import java.util.concurrent.CompletableFuture
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.put

internal data class DaemonWireEnvelope(
    val kind: DaemonMessageKind,
    val messageId: String,
    val replyTo: String?,
    val runtimeEpoch: String,
    val hostGeneration: Long,
    val runtimeInstanceId: String?,
    val operation: DaemonOperationToken,
    val payload: JsonElement,
    val fdRoles: List<String>,
)

internal interface DaemonCompanionListener {
    fun currentOwner(): DaemonOwnerFence
    fun onHostStatus(connection: DaemonConnection, payload: JsonObject)
    fun onDaemonRequest(
        request: DaemonWireEnvelope,
        descriptors: List<ParcelFileDescriptor>,
    ): DaemonReplyPayload
    fun onDaemonDisconnected(connection: DaemonConnection)
}

internal data class DaemonReplyPayload(
    val payload: JsonElement,
    val fdRoles: List<String> = emptyList(),
    val descriptors: List<ParcelFileDescriptor> = emptyList(),
)

internal class DaemonReceivedMessage(
    val envelope: DaemonWireEnvelope,
    val descriptors: List<ParcelFileDescriptor>,
) : Closeable {
    override fun close() {
        descriptors.forEach { descriptor -> runCatching { descriptor.close() } }
    }
}

internal class MagiskCompanionServer(
    private val packageName: String,
    private val listener: DaemonCompanionListener,
) : Closeable {
    private val running = AtomicBoolean(false)
    private val active = AtomicReference<DaemonConnection?>(null)
    private val connectionLock = Any()
    private val connectionThreads = ConcurrentHashMap.newKeySet<Thread>()
    private var server: LocalServerSocket? = null
    private var thread: Thread? = null

    fun start() {
        if (!running.compareAndSet(false, true)) return
        val localServer = try {
            LocalServerSocket(DaemonProtocol.socketName(packageName))
        } catch (failure: Throwable) {
            running.set(false)
            throw failure
        }
        server = localServer
        thread = Thread({ acceptLoop(localServer) }, "droidbridge-magisk-companion").apply {
            isDaemon = true
            start()
        }
    }

    fun connection(): DaemonConnection? = active.get()?.takeIf { it.isHealthy() }

    private fun acceptLoop(localServer: LocalServerSocket) {
        while (running.get()) {
            val socket = runCatching { localServer.accept() }.getOrNull() ?: break
            val connection = runCatching { authenticate(socket) }
                .getOrElse {
                    runCatching { socket.close() }
                    continue
                }
            val admitted = synchronized(connectionLock) {
                if (running.get() && active.get() == null) {
                    active.set(connection)
                    true
                } else {
                    false
                }
            }
            if (!admitted) {
                connection.close()
                continue
            }
            val connectionThread = Thread(
                { serveConnection(connection) },
                "droidbridge-magisk-connection",
            ).apply { isDaemon = true }
            connectionThreads.add(connectionThread)
            try {
                connectionThread.start()
            } catch (failure: Throwable) {
                connectionThreads.remove(connectionThread)
                releaseConnection(connection, notify = true)
                throw failure
            }
        }
    }

    private fun serveConnection(connection: DaemonConnection) {
        try {
            connection.readLoop()
        } finally {
            releaseConnection(connection, notify = true)
            connectionThreads.remove(Thread.currentThread())
        }
    }

    private fun releaseConnection(connection: DaemonConnection, notify: Boolean) {
        connection.close()
        synchronized(connectionLock) {
            if (active.compareAndSet(connection, null) && notify) {
                listener.onDaemonDisconnected(connection)
            }
        }
    }

    private fun authenticate(socket: LocalSocket): DaemonConnection {
        require(socket.peerCredentials.uid == 0)
        val owner = listener.currentOwner()
        val handshake = readFrame(socket.inputStream, socket).use { frame ->
            require(frame.descriptors.isEmpty())
            DaemonProtocol.decodeHandshake(frame.body)
        }
        require(handshake.accepts(0, packageName, owner))
        val reply = DaemonHandshake(
            protocolVersion = DaemonProtocol.PROTOCOL_VERSION,
            role = DaemonRoleToken.ApkRuntime,
            packageName = packageName,
            userId = 0,
            runtimeEpoch = owner.runtimeEpoch,
            host = owner.host,
            hostGeneration = owner.hostGeneration,
            runtimeInstanceId = owner.runtimeInstanceId,
        )
        writeFrame(socket, DaemonProtocol.encodeHandshake(reply), emptyList())
        return DaemonConnection(socket, listener)
    }

    override fun close() {
        if (!running.compareAndSet(true, false)) return
        synchronized(connectionLock) { active.getAndSet(null) }?.close()
        runCatching { server?.close() }
        thread?.let { current ->
            if (current !== Thread.currentThread()) runCatching { current.join(1_000) }
        }
        connectionThreads.toList().forEach { current ->
            if (current !== Thread.currentThread()) runCatching { current.join(1_000) }
        }
        thread = null
        server = null
    }
}

internal class DaemonConnection(
    private val socket: LocalSocket,
    private val listener: DaemonCompanionListener,
) : Closeable {
    private val healthy = AtomicBoolean(true)
    private val pending = ConcurrentHashMap<String, Pending>()
    private val history = DaemonMessageHistory()
    private val pendingLock = Any()
    private var businessPending = 0
    private val writeLock = Any()
    private val business = java.util.concurrent.Executors.newSingleThreadExecutor { runnable ->
        Thread(runnable, "droidbridge-magisk-business").apply { isDaemon = true }
    }

    private data class Pending(
        val operation: DaemonOperationToken,
        val control: Boolean,
        val owner: DaemonOwnerFence,
        val future: CompletableFuture<DaemonReceivedMessage>,
    )

    fun isHealthy(): Boolean = healthy.get() && socket.isConnected

    /**
     * Sends one request and waits for its own reply. Neither a refused write nor an expired wait is
     * evidence that the transport died, so neither ends it: the read loop is the only observer of
     * the connection's loss, and one request's failure must not become every other request's.
     */
    fun request(
        operation: DaemonOperationToken,
        payload: JsonElement,
        owner: DaemonOwnerFence,
        timeoutMillis: Long,
        fdRoles: List<String> = emptyList(),
        descriptors: List<ParcelFileDescriptor> = emptyList(),
    ): DaemonReceivedMessage {
        check(isHealthy())
        val control = operation.control
        val limit = if (control) MAX_OUTSTANDING else MAX_BUSINESS_OUTSTANDING
        val messageId = UUID.randomUUID().toString()
        val future = CompletableFuture<DaemonReceivedMessage>()
        synchronized(pendingLock) {
            check(pending.size < MAX_OUTSTANDING)
            if (!control) check(businessPending < limit)
            check(pending.putIfAbsent(messageId, Pending(operation, control, owner, future)) == null)
            if (!control) businessPending += 1
        }
        val envelope = DaemonWireEnvelope(
            kind = DaemonMessageKind.Request,
            messageId = messageId,
            replyTo = null,
            runtimeEpoch = owner.runtimeEpoch,
            hostGeneration = owner.hostGeneration,
            runtimeInstanceId = owner.runtimeInstanceId,
            operation = operation,
            payload = payload,
            fdRoles = fdRoles,
        )
        try {
            synchronized(writeLock) {
                history.recordOutgoing(messageId)
                writeFrame(socket, DaemonWireCodec.encode(envelope), descriptors)
            }
            return future.get(timeoutMillis, TimeUnit.MILLISECONDS)
        } finally {
            removePending(messageId)
        }
    }

    fun readLoop() {
        while (healthy.get()) {
            val frame = readFrame(socket.inputStream, socket)
            val message = try {
                DaemonReceivedMessage(
                    DaemonWireCodec.decode(frame.body, frame.descriptors.size),
                    frame.descriptors,
                )
            } catch (failure: Throwable) {
                frame.close()
                throw failure
            }
            try {
                history.recordIncoming(message.envelope.messageId)
            } catch (failure: Throwable) {
                message.close()
                throw failure
            }
            when (message.envelope.kind) {
                DaemonMessageKind.Response -> complete(message)
                DaemonMessageKind.Request -> dispatch(message)
                DaemonMessageKind.Cancel -> cancel(message)
            }
        }
    }

    /**
     * Business requests run one at a time on their own worker so the read loop stays
     * reachable for the control requests that cancel them. They already serialized on the
     * read loop, so this changes which thread owns the run and not the order; the worker
     * owns the message and closes it.
     */
    private fun dispatch(message: DaemonReceivedMessage) {
        if (message.envelope.operation != DaemonOperationToken.CompanionExecute) {
            message.use(::respond)
            return
        }
        try {
            business.execute {
                try {
                    respond(message)
                } catch (_: Throwable) {
                    close()
                } finally {
                    message.close()
                }
            }
        } catch (failure: Throwable) {
            message.close()
            throw failure
        }
    }

    private fun complete(message: DaemonReceivedMessage) {
        val envelope = message.envelope
        val replyTo = requireNotNull(envelope.replyTo)
        val request = removePending(replyTo)
        if (request == null) {
            // Whoever asked stopped waiting before this answer arrived, so the answer owns nothing
            // here; only the answer to a request this connection still waits on can be unknown.
            message.close()
            return
        }
        if (
            request.operation != envelope.operation ||
            !responseFenceMatches(
                request.owner,
                envelope,
                request.operation.instanceFenced,
            )
        ) {
            message.close()
            throw IllegalStateException("unknown daemon reply")
        }
        if (!request.future.complete(message)) message.close()
    }

    private fun cancel(message: DaemonReceivedMessage) {
        val envelope = message.envelope
        val replyTo = requireNotNull(envelope.replyTo)
        val request = removePending(replyTo)
        message.close()
        if (request == null) return
        if (
            request.operation != envelope.operation ||
            !responseFenceMatches(
                request.owner,
                envelope,
                request.operation.instanceFenced,
            )
        ) {
            throw IllegalStateException("unknown daemon cancellation")
        }
        request.future.completeExceptionally(EOFException("daemon cancelled request"))
    }

    private fun respond(message: DaemonReceivedMessage) {
        val request = message.envelope
        require(request.replyTo == null)
        val owner = listener.currentOwner()
        require(request.runtimeEpoch == owner.runtimeEpoch)
        require(request.hostGeneration == owner.hostGeneration)
        val responseMessageId = UUID.randomUUID().toString()
        history.recordOutgoing(responseMessageId)
        val result = when (request.operation) {
            DaemonOperationToken.HostStatus -> {
                require(message.descriptors.isEmpty())
                val status = request.payload.jsonObject
                listener.onHostStatus(this, status)
                DaemonReplyPayload(buildJsonObject { put("accepted", true) })
            }
            DaemonOperationToken.CompanionExecute,
            DaemonOperationToken.CompanionCancel,
            DaemonOperationToken.CapabilitySnapshot,
            ->
                listener.onDaemonRequest(request, message.descriptors)
            else -> throw IllegalStateException("invalid daemon request direction")
        }
        val response = DaemonWireEnvelope(
            kind = DaemonMessageKind.Response,
            messageId = responseMessageId,
            replyTo = request.messageId,
            runtimeEpoch = owner.runtimeEpoch,
            hostGeneration = owner.hostGeneration,
            runtimeInstanceId = companionResponseInstance(request, owner),
            operation = request.operation,
            payload = result.payload,
            fdRoles = result.fdRoles,
        )
        synchronized(writeLock) {
            writeFrame(socket, DaemonWireCodec.encode(response), result.descriptors)
        }
    }

    override fun close() {
        if (!healthy.compareAndSet(true, false)) return
        runCatching { socket.close() }
        business.shutdownNow()
        val disconnected = synchronized(pendingLock) {
            pending.values.toList().also {
                pending.clear()
                businessPending = 0
            }
        }
        disconnected.forEach { it.future.completeExceptionally(EOFException("daemon disconnected")) }
    }

    private fun removePending(messageId: String): Pending? = synchronized(pendingLock) {
        pending.remove(messageId)?.also { request ->
            if (!request.control) businessPending -= 1
        }
    }

    companion object {
        private const val MAX_OUTSTANDING = 64
        private const val MAX_BUSINESS_OUTSTANDING = 60
    }
}

internal fun responseFenceMatches(
    owner: DaemonOwnerFence,
    response: DaemonWireEnvelope,
    requireInstance: Boolean,
): Boolean =
    response.runtimeEpoch == owner.runtimeEpoch &&
        response.hostGeneration == owner.hostGeneration &&
        (!requireInstance ||
            owner.runtimeInstanceId != null &&
            response.runtimeInstanceId == owner.runtimeInstanceId)

internal class DaemonMessageHistory(
    private val maxMessagesPerDirection: Int = MAX_MESSAGES_PER_DIRECTION,
) {
    private val seen = HashSet<String>()
    private var incoming = 0
    private var outgoing = 0

    @Synchronized
    fun recordIncoming(messageId: String) {
        incoming = record(messageId, incoming)
    }

    @Synchronized
    fun recordOutgoing(messageId: String) {
        outgoing = record(messageId, outgoing)
    }

    private fun record(messageId: String, directionCount: Int): Int {
        check(directionCount < maxMessagesPerDirection) {
            "daemon connection message history is exhausted"
        }
        check(seen.add(messageId)) { "duplicate daemon message" }
        return directionCount + 1
    }

    companion object {
        const val MAX_MESSAGES_PER_DIRECTION = 65_536
    }
}

private class DaemonFrame(
    val body: ByteArray,
    val descriptors: List<ParcelFileDescriptor>,
) : Closeable {
    override fun close() {
        descriptors.forEach { descriptor -> runCatching { descriptor.close() } }
    }
}

private fun readFrame(input: InputStream, socket: LocalSocket): DaemonFrame {
    val header = ByteArray(4)
    input.readFully(header)
    val descriptors = ownDescriptors(socket.ancillaryFileDescriptors?.toList().orEmpty())
    val length = java.nio.ByteBuffer.wrap(header).int
    if (length !in 1..DaemonProtocol.MAX_FRAME_BYTES) {
        descriptors.forEach { descriptor -> runCatching { descriptor.close() } }
        throw IllegalArgumentException("invalid daemon frame length")
    }
    return try {
        DaemonFrame(ByteArray(length).also(input::readFully), descriptors)
    } catch (failure: Throwable) {
        descriptors.forEach { descriptor -> runCatching { descriptor.close() } }
        throw failure
    }
}

private fun ownDescriptors(descriptors: List<FileDescriptor>): List<ParcelFileDescriptor> {
    val owned = mutableListOf<ParcelFileDescriptor>()
    try {
        descriptors.forEach { descriptor ->
            try {
                owned += ParcelFileDescriptor.dup(descriptor)
            } finally {
                runCatching { android.system.Os.close(descriptor) }
            }
        }
        return owned
    } catch (failure: Throwable) {
        descriptors.forEach { descriptor -> runCatching { android.system.Os.close(descriptor) } }
        owned.forEach { descriptor -> runCatching { descriptor.close() } }
        throw failure
    }
}

private fun writeFrame(
    socket: LocalSocket,
    body: ByteArray,
    descriptors: List<ParcelFileDescriptor>,
) {
    require(descriptors.size <= 4)
    val rawDescriptors = descriptors.map(ParcelFileDescriptor::getFileDescriptor).toTypedArray()
    socket.setFileDescriptorsForSend(rawDescriptors.takeIf { it.isNotEmpty() })
    try {
        val output: OutputStream = socket.outputStream
        output.write(DaemonProtocol.frame(body))
        output.flush()
    } finally {
        socket.setFileDescriptorsForSend(null)
        descriptors.forEach { descriptor -> runCatching { descriptor.close() } }
    }
}

private fun InputStream.readFully(target: ByteArray) {
    var offset = 0
    while (offset < target.size) {
        val count = read(target, offset, target.size - offset)
        if (count < 0) throw EOFException("daemon disconnected")
        offset += count
    }
}
