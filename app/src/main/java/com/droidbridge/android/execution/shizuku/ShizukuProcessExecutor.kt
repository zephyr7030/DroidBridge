package com.droidbridge.android.execution.shizuku

import com.droidbridge.android.execution.android.AndroidExecutionRequest
import com.droidbridge.android.execution.android.AndroidExecutionResult
import java.nio.ByteBuffer
import java.nio.charset.CharacterCodingException
import java.nio.charset.CodingErrorAction
import java.util.UUID
import org.json.JSONObject

internal class ShizukuProcessExecutor(
    private val guardPath: String,
    private val packageName: String,
    private val guardExecutor: ShizukuGuardExecutor,
) {
    suspend fun start(
        candidate: ShizukuSessionLease,
        request: AndroidExecutionRequest,
    ): AndroidExecutionResult {
        val invocation = ShizukuGuardedPlanCodec.decodeInvocation(
            request.payload,
            guardPath,
            packageName,
        )
        return AndroidExecutionResult(
            guardExecutor.execute(
                candidate,
                request,
                request.executionId,
                invocation.primitive,
                invocation.payload,
            ).toString().toByteArray(),
        )
    }

    fun cancel(
        candidate: ShizukuSessionLease,
        request: AndroidExecutionRequest,
    ): AndroidExecutionResult {
        require(request.descriptors.isEmpty())
        val input = JSONObject(strictUtf8(request.payload))
        require(input.keys().asSequence().all { it == "execution_id" })
        val target = (input.opt("execution_id") as? String)
            ?: throw ShizukuExecutionException("INVALID_ARGUMENT")
        require(UUID.fromString(target).toString() == target)
        candidate.requireCurrent(request, allowCleanupControl = true)
        val cancelled = candidate.remote.cancel(candidate.token, target)
        return AndroidExecutionResult(
            JSONObject().put("cancelled", cancelled).toString().toByteArray(),
        )
    }

    private fun strictUtf8(payload: ByteArray): String = try {
        Charsets.UTF_8.newDecoder()
            .onMalformedInput(CodingErrorAction.REPORT)
            .onUnmappableCharacter(CodingErrorAction.REPORT)
            .decode(ByteBuffer.wrap(payload))
            .toString()
    } catch (error: CharacterCodingException) {
        throw ShizukuExecutionException("INVALID_ARGUMENT")
    }
}
