package com.droidbridge.android.execution.shizuku

import android.os.ParcelFileDescriptor
import com.droidbridge.android.execution.android.AndroidExecutionRequest
import com.droidbridge.android.runtimehost.NativeRuntime
import java.io.File
import java.util.UUID
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.async
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout
import org.json.JSONObject

internal class ShizukuGuardExecutor(
    private val guardPath: String,
    private val packageName: String,
    private val onCleanupUnverified: (ShizukuSessionLease) -> Unit,
    private val onProofsDrained: () -> Unit,
) {
    private data class RemoteCompletion(val exitCode: Int, val errorCode: String)

    private data class GuardIo(
        val proof: ParcelFileDescriptor,
        val stdin: ParcelFileDescriptor,
        val stdoutPipe: Array<ParcelFileDescriptor>,
        val stderrPipe: Array<ParcelFileDescriptor>,
        val stdoutFile: ParcelFileDescriptor?,
    )

    private data class BoundedBytes(val bytes: ByteArray, val truncated: Boolean)

    private val activeProofs = linkedSetOf<String>()

    suspend fun probe(candidate: ShizukuSessionLease): ShizukuProbeSettlement {
        candidate.requireCurrent()
        val executionId = UUID.randomUUID().toString()
        val proofRaw = prepareProof(executionId)
        if (proofRaw < 0) return ShizukuProbeSettlement.Unavailable
        val opened = mutableListOf(ParcelFileDescriptor.adoptFd(proofRaw))
        try {
            opened += ParcelFileDescriptor.open(File("/dev/null"), ParcelFileDescriptor.MODE_READ_ONLY)
            opened += ParcelFileDescriptor.open(File("/dev/null"), ParcelFileDescriptor.MODE_WRITE_ONLY)
            opened += ParcelFileDescriptor.open(File("/dev/null"), ParcelFileDescriptor.MODE_WRITE_ONLY)
        } catch (error: RuntimeException) {
            closeQuietly(*opened.toTypedArray())
            abortProof(executionId)
            return ShizukuProbeSettlement.Unavailable
        }
        val completion = runCatching {
            awaitRemote(
                candidate,
                null,
                executionId,
                "guard_probe",
                "{}".toByteArray(),
                opened[0],
                opened[1],
                opened[2],
                opened[3],
                PROBE_DEADLINE_MS,
            )
        }.getOrNull()
        if (completion == null) {
            val settlement = settleProof(candidate, executionId)
            return if (settlement.optBoolean("cleanup_verified", false)) {
                ShizukuProbeSettlement.Unavailable
            } else {
                ShizukuProbeSettlement.CleanupUnverified
            }
        }
        if (completion.errorCode.isNotEmpty()) {
            val cleanupVerified = abortProof(executionId) ||
                settleProof(candidate, executionId).optBoolean("cleanup_verified", false)
            return if (cleanupVerified) {
                ShizukuProbeSettlement.Unavailable
            } else {
                ShizukuProbeSettlement.CleanupUnverified
            }
        }
        val settlement = settleProof(candidate, executionId)
        return ShizukuLaunchPolicy.settleProbe(
            settlement.optBoolean("cleanup_verified", false),
            settlement.optInt("shell_exit_code").takeIf { settlement.has("shell_exit_code") },
        )
    }

    suspend fun execute(
        candidate: ShizukuSessionLease,
        request: AndroidExecutionRequest,
        executionId: String,
        primitive: String,
        payload: ByteArray,
    ): JSONObject = coroutineScope {
        val plan = ShizukuGuardedPlanCodec.decode(
            primitive,
            payload,
            guardPath,
            packageName,
        )
        val proofRaw = prepareProof(executionId)
        if (proofRaw < 0) throw ShizukuExecutionException("RESOURCE_LIMIT")
        val (inputDescriptor, outputDescriptor) = try {
            when (primitive) {
                "process_start" -> {
                    require(request.descriptors.size <= 1 && request.descriptors.all { it.role == "stdin" })
                    request.descriptors.singleOrNull()?.descriptor to null
                }
                "screen_capture" -> {
                    require(
                        request.descriptors.size == 1 &&
                            request.descriptors.single().role == "visual_output"
                    )
                    null to request.descriptors.single().descriptor
                }
                else -> {
                    require(request.descriptors.isEmpty())
                    null to null
                }
            }
        } catch (error: IllegalArgumentException) {
            ParcelFileDescriptor.adoptFd(proofRaw).close()
            abortProof(executionId)
            throw ShizukuExecutionException("INVALID_ARGUMENT")
        }
        val io = try {
            createGuardIo(proofRaw, inputDescriptor, outputDescriptor)
        } catch (error: RuntimeException) {
            abortProof(executionId)
            throw ShizukuExecutionException("IO_ERROR")
        }
        val stdout = async(Dispatchers.IO) {
            io.stdoutFile?.let { output ->
                copyBounded(io.stdoutPipe[0], output, plan.stdoutLimit)
            } ?: readBounded(io.stdoutPipe[0], plan.stdoutLimit)
        }
        val stderr = async(Dispatchers.IO) { readBounded(io.stderrPipe[0], plan.stdoutLimit) }
        try {
            candidate.requireCurrent(request)
            currentCoroutineContext().ensureActive()
        } catch (error: Throwable) {
            closeQuietly(
                io.proof,
                io.stdin,
                io.stdoutPipe[0],
                io.stdoutPipe[1],
                io.stderrPipe[0],
                io.stderrPipe[1],
            )
            abortProof(executionId)
            throw error
        }
        val completion = try {
            awaitRemote(
                candidate,
                request,
                executionId,
                primitive,
                payload,
                io.proof,
                io.stdin,
                io.stdoutPipe[1],
                io.stderrPipe[1],
                plan.deadlineMs,
            )
        } catch (error: Throwable) {
            closeQuietly(
                io.proof,
                io.stdin,
                io.stdoutPipe[0],
                io.stdoutPipe[1],
                io.stderrPipe[0],
                io.stderrPipe[1],
            )
            if (!settleProof(candidate, executionId).optBoolean("cleanup_verified", false)) {
                throw ShizukuExecutionException("CLEANUP_UNVERIFIED")
            }
            candidate.requireCurrent(request)
            throw error
        }
        if (completion.errorCode.isNotEmpty()) {
            closeQuietly(io.stdoutPipe[0], io.stderrPipe[0])
            val cleanupVerified = abortProof(executionId) ||
                settleProof(candidate, executionId).optBoolean("cleanup_verified", false)
            if (!cleanupVerified) throw ShizukuExecutionException("CLEANUP_UNVERIFIED")
            candidate.requireCurrent(request)
            throw ShizukuExecutionException(completion.errorCode)
        }
        val result = withContext(NonCancellable) {
            val stdoutResult = runCatching { stdout.await() }
            val stderrResult = runCatching { stderr.await() }
            val settlement = settleProof(candidate, executionId)
            if (!settlement.optBoolean("cleanup_verified", false)) {
                throw ShizukuExecutionException("CLEANUP_UNVERIFIED")
            }
            val stdoutBytes = stdoutResult.getOrElse { error ->
                if (error is CancellationException) BoundedBytes(ByteArray(0), false)
                else throw ShizukuExecutionException("IO_ERROR")
            }
            val stderrBytes = stderrResult.getOrElse { error ->
                if (error is CancellationException) BoundedBytes(ByteArray(0), false)
                else throw ShizukuExecutionException("IO_ERROR")
            }
            JSONObject()
                .put("cause", settlement.getString("cause"))
                .apply {
                    if (settlement.has("shell_exit_code")) {
                        put("exit_code", settlement.getInt("shell_exit_code"))
                    }
                }
                .put("stdout_base64", java.util.Base64.getEncoder().encodeToString(stdoutBytes.bytes))
                .put("stdout_truncated", stdoutBytes.truncated)
                .put("stderr_base64", java.util.Base64.getEncoder().encodeToString(stderrBytes.bytes))
                .put("stderr_truncated", stderrBytes.truncated)
        }
        candidate.requireCurrent(request)
        currentCoroutineContext().ensureActive()
        result
    }

    @Synchronized
    fun hasActiveProofs(): Boolean = activeProofs.isNotEmpty()

    private suspend fun awaitRemote(
        candidate: ShizukuSessionLease,
        request: AndroidExecutionRequest?,
        executionId: String,
        primitive: String,
        payload: ByteArray,
        proof: ParcelFileDescriptor,
        stdin: ParcelFileDescriptor,
        stdout: ParcelFileDescriptor,
        stderr: ParcelFileDescriptor,
        deadlineMs: Long,
    ): RemoteCompletion {
        val completion = CompletableDeferred<RemoteCompletion>()
        val callback = object : IShizukuExecutionCallback.Stub() {
            override fun onComplete(callbackExecutionId: String?, exitCode: Int, errorCode: String?) {
                if (callbackExecutionId != executionId) {
                    completion.completeExceptionally(ShizukuExecutionException("STALE_AUTHORITY"))
                } else {
                    completion.complete(RemoteCompletion(exitCode, errorCode.orEmpty()))
                }
            }
        }
        try {
            candidate.requireCurrent(request)
            candidate.remote.executeGuarded(
                candidate.token,
                executionId,
                primitive,
                payload,
                proof,
                stdin,
                stdout,
                stderr,
                callback,
            )
        } catch (error: Throwable) {
            completion.completeExceptionally(error)
        } finally {
            closeQuietly(proof, stdin, stdout, stderr)
        }
        return try {
            withTimeout(deadlineMs + CLEANUP_WAIT_MS) { candidate.awaitCompletion(completion) }
        } catch (cancelled: CancellationException) {
            runCatching {
                candidate.requireCurrent(request, allowCleanupControl = true)
                candidate.remote.cancel(candidate.token, executionId)
            }
            withContext(NonCancellable) {
                withTimeout(CLEANUP_WAIT_MS) { candidate.awaitCompletion(completion) }
            }
        }
    }

    private fun prepareProof(executionId: String): Int {
        val descriptor = NativeRuntime.nativePrepareShizukuGuardProof(executionId)
        if (descriptor >= 0) synchronized(this) { check(activeProofs.add(executionId)) }
        return descriptor
    }

    private fun abortProof(executionId: String): Boolean {
        val aborted = NativeRuntime.nativeAbortShizukuGuardProof(executionId)
        if (aborted) finishProof(executionId)
        return aborted
    }

    private fun settleProof(candidate: ShizukuSessionLease, executionId: String): JSONObject {
        val settlement = runCatching {
            JSONObject(NativeRuntime.nativeSettleShizukuGuardProof(executionId))
        }.getOrElse { JSONObject().put("cleanup_verified", false) }
        if (!settlement.optBoolean("cleanup_verified", false)) onCleanupUnverified(candidate)
        finishProof(executionId)
        return settlement
    }

    private fun finishProof(executionId: String) {
        val drained = synchronized(this) {
            val removed = activeProofs.remove(executionId)
            removed && activeProofs.isEmpty()
        }
        if (drained) onProofsDrained()
    }

    private fun createGuardIo(
        proofRaw: Int,
        inputDescriptor: ParcelFileDescriptor?,
        outputDescriptor: ParcelFileDescriptor?,
    ): GuardIo {
        val opened = mutableListOf(ParcelFileDescriptor.adoptFd(proofRaw))
        return try {
            val stdin = inputDescriptor?.let { ParcelFileDescriptor.dup(it.fileDescriptor) }
                ?: ParcelFileDescriptor.open(File("/dev/null"), ParcelFileDescriptor.MODE_READ_ONLY)
            opened += stdin
            val stdoutPipe = ParcelFileDescriptor.createPipe()
            opened += stdoutPipe
            val stderrPipe = ParcelFileDescriptor.createPipe()
            opened += stderrPipe
            val stdoutFile = outputDescriptor?.let { ParcelFileDescriptor.dup(it.fileDescriptor) }
            if (stdoutFile != null) opened += stdoutFile
            GuardIo(opened[0], stdin, stdoutPipe, stderrPipe, stdoutFile)
        } catch (error: RuntimeException) {
            closeQuietly(*opened.toTypedArray())
            throw error
        }
    }

    private fun readBounded(descriptor: ParcelFileDescriptor, limit: Int): BoundedBytes {
        ParcelFileDescriptor.AutoCloseInputStream(descriptor).use { input ->
            val output = java.io.ByteArrayOutputStream(minOf(limit, 65_536))
            val buffer = ByteArray(16_384)
            var truncated = false
            while (true) {
                val count = input.read(buffer)
                if (count < 0) break
                val remaining = limit - output.size()
                if (remaining > 0) output.write(buffer, 0, minOf(remaining, count))
                if (count > remaining) truncated = true
            }
            return BoundedBytes(output.toByteArray(), truncated)
        }
    }

    private fun copyBounded(
        descriptor: ParcelFileDescriptor,
        outputDescriptor: ParcelFileDescriptor,
        limit: Int,
    ): BoundedBytes {
        ParcelFileDescriptor.AutoCloseInputStream(descriptor).use { input ->
            ParcelFileDescriptor.AutoCloseOutputStream(outputDescriptor).use { output ->
                val buffer = ByteArray(16_384)
                var written = 0
                var truncated = false
                while (true) {
                    val count = input.read(buffer)
                    if (count < 0) break
                    val remaining = limit - written
                    if (remaining > 0) {
                        val accepted = minOf(remaining, count)
                        output.write(buffer, 0, accepted)
                        written += accepted
                    }
                    if (count > remaining) truncated = true
                }
                output.flush()
                output.fd.sync()
                return BoundedBytes(ByteArray(0), truncated)
            }
        }
    }

    private fun closeQuietly(vararg descriptors: ParcelFileDescriptor?) {
        descriptors.forEach { descriptor -> runCatching { descriptor?.close() } }
    }

    private companion object {
        const val PROBE_DEADLINE_MS = 5_000L
        const val CLEANUP_WAIT_MS = 6_000L
    }
}
