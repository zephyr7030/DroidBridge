package com.droidbridge.android.execution.android

import android.os.ParcelFileDescriptor
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.TimeoutCancellationException
import kotlinx.coroutines.runBlocking

internal object NativeAndroidExecutionDispatcher {
    private val registry = AtomicReference<AndroidExecutionRegistry?>(null)

    fun install(value: AndroidExecutionRegistry) {
        check(registry.compareAndSet(null, value) || registry.get() === value)
    }

    fun uninstall(value: AndroidExecutionRegistry) {
        registry.compareAndSet(value, null)
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
