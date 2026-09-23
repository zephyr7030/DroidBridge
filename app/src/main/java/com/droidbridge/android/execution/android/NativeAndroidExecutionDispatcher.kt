package com.droidbridge.android.execution.android

import android.os.ParcelFileDescriptor
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.TimeoutCancellationException
import kotlinx.coroutines.runBlocking

internal object NativeAndroidExecutionDispatcher {
    private val registry = AtomicReference<AndroidExecutionRegistry?>(null)
    private var taskActivitySink: ((Long) -> Unit)? = null
    private var taskActivityEpoch: String? = null
    private var taskActivityRevision = -1L
    private var activeTaskCount = 0L
    private var taskActivityFromDaemon = false

    fun install(value: AndroidExecutionRegistry) {
        check(registry.compareAndSet(null, value) || registry.get() === value)
    }

    fun uninstall(value: AndroidExecutionRegistry) {
        registry.compareAndSet(value, null)
    }

    @Synchronized
    fun installTaskActivitySink(value: ((Long) -> Unit)?) {
        taskActivitySink = value
        if (value != null && taskActivityRevision >= 0) value(activeTaskCount)
    }

    @JvmStatic
    @Synchronized
    fun taskActivityChanged(runtimeEpoch: String, activeTasks: Long, canonicalRevision: Long) =
        taskActivityChanged(runtimeEpoch, activeTasks, canonicalRevision, fromDaemon = false)

    /**
     * The same count, published by the root module's daemon for the Runtime it hosts. The App
     * executes those Tasks' Android primitives, so its process must live exactly as long as they
     * do; only the daemon's own count can be forgotten when that daemon goes away.
     */
    @Synchronized
    fun daemonTaskActivityChanged(runtimeEpoch: String, activeTasks: Long, canonicalRevision: Long) =
        taskActivityChanged(runtimeEpoch, activeTasks, canonicalRevision, fromDaemon = true)

    /**
     * The daemon that published the current count disconnected, so nothing here knows what it
     * still runs. A Runtime hosted by this process keeps its own count.
     */
    @Synchronized
    fun forgetDaemonTaskActivity() {
        if (!taskActivityFromDaemon) return
        taskActivityFromDaemon = false
        taskActivityEpoch = null
        taskActivityRevision = -1L
        if (activeTaskCount == 0L) return
        activeTaskCount = 0L
        taskActivitySink?.invoke(0L)
    }

    private fun taskActivityChanged(
        runtimeEpoch: String,
        activeTasks: Long,
        canonicalRevision: Long,
        fromDaemon: Boolean,
    ) {
        if (runtimeEpoch.isEmpty() || activeTasks < 0 || canonicalRevision < 0) return
        val epochChanged = runtimeEpoch != taskActivityEpoch
        if (epochChanged) {
            taskActivityEpoch = runtimeEpoch
            taskActivityRevision = -1L
        }
        if (canonicalRevision <= taskActivityRevision) return
        taskActivityRevision = canonicalRevision
        // One store revision sequence outlives a host handoff, so whichever Runtime published this
        // count owns it until a later one is accepted.
        taskActivityFromDaemon = fromDaemon
        if (!epochChanged && activeTasks == activeTaskCount) return
        activeTaskCount = activeTasks
        taskActivitySink?.invoke(activeTasks)
    }

    @JvmStatic
    fun execute(
        key: String,
        generation: Long,
        primitive: String,
        payload: ByteArray,
        executionId: String,
        runtimeEpoch: String,
        hostGeneration: Long,
        runtimeInstanceId: String,
    ): AndroidExecutionResult? = executeInternal(
        key,
        generation,
        primitive,
        payload,
        executionId,
        runtimeEpoch,
        hostGeneration,
        runtimeInstanceId,
        emptyList(),
    )

    @JvmStatic
    fun executeWithDescriptor(
        key: String,
        generation: Long,
        primitive: String,
        payload: ByteArray,
        executionId: String,
        runtimeEpoch: String,
        hostGeneration: Long,
        runtimeInstanceId: String,
        descriptorRole: String,
        descriptorFd: Int,
    ): AndroidExecutionResult? {
        val descriptor = ParcelFileDescriptor.fromFd(descriptorFd)
        return try {
            executeInternal(
                key,
                generation,
                primitive,
                payload,
                executionId,
                runtimeEpoch,
                hostGeneration,
                runtimeInstanceId,
                listOf(RoleDescriptor(descriptorRole, descriptor)),
            )
        } finally {
            runCatching { descriptor.close() }
        }
    }

    /**
     * Runs one companion-issued primitive. A canonical companion call carries no
     * capability key, so the primitive and its generation select the executor.
     */
    fun executePrimitive(
        primitive: AndroidPrimitive,
        generation: Long,
        payload: ByteArray,
        executionId: String,
        runtimeEpoch: String,
        hostGeneration: Long,
        runtimeInstanceId: String,
        descriptors: List<RoleDescriptor> = emptyList(),
    ): AndroidExecutionResult {
        val executor = registry.get()?.executor(primitive, generation)
            ?: return AndroidExecutionResult(byteArrayOf(), errorCode = "CAPABILITY_UNAVAILABLE")
        return dispatch(
            executor,
            AndroidExecutionRequest(
                primitive = primitive,
                payload = payload,
                executionId = executionId,
                runtimeEpoch = runtimeEpoch,
                hostGeneration = hostGeneration,
                runtimeInstanceId = runtimeInstanceId,
                descriptors = descriptors,
            ),
        )
    }

    private fun executeInternal(
        key: String,
        generation: Long,
        primitive: String,
        payload: ByteArray,
        executionId: String,
        runtimeEpoch: String,
        hostGeneration: Long,
        runtimeInstanceId: String,
        descriptors: List<RoleDescriptor>,
    ): AndroidExecutionResult? {
        val executor = registry.get()?.executor(key, generation) ?: return null
        return dispatch(
            executor,
            AndroidExecutionRequest(
                primitive = AndroidPrimitive.valueOf(primitive),
                payload = payload,
                executionId = executionId,
                runtimeEpoch = runtimeEpoch,
                hostGeneration = hostGeneration,
                runtimeInstanceId = runtimeInstanceId,
                descriptors = descriptors,
            ),
        )
    }

    private fun dispatch(
        executor: AndroidExecutionBridge,
        request: AndroidExecutionRequest,
    ): AndroidExecutionResult = try {
        runBlocking { executor.execute(request) }
    } catch (error: AndroidExecutionException) {
        AndroidExecutionResult(byteArrayOf(), errorCode = error.code)
    } catch (_: TimeoutCancellationException) {
        AndroidExecutionResult(byteArrayOf(), errorCode = "TIMEOUT")
    } catch (_: CancellationException) {
        AndroidExecutionResult(byteArrayOf(), errorCode = "CANCELLED")
    }
}
