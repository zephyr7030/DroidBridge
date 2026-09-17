package com.droidbridge.android.runtimehost

import android.os.ParcelFileDescriptor
import java.io.Closeable
import java.util.concurrent.atomic.AtomicReference

internal const val MCP_ARTIFACT_ROLE = "mcp_artifact"
internal const val ARTIFACT_QUERY_TIMEOUT_MILLIS = 30_000L

/** One S-MCP-006 artifact query answer; a `read` carries its one read-only descriptor. */
internal class McpArtifactQueryReply(
    val payload: ByteArray,
    val descriptor: ParcelFileDescriptor?,
) : Closeable {
    override fun close() {
        descriptor?.close()
    }
}

/**
 * The native MCP listener's only way into the Runtime. Every tool call and artifact query goes
 * through [RuntimeHostController], so the facade never selects a host itself (S-MCP-005/006).
 * A failure this host can name is answered with the Runtime envelope that names it, so the caller
 * reads its code and reason; only an envelope that cannot be read at all returns null, which the
 * facade answers as `-32603`.
 */
internal object McpHostBridge {
    private val host = AtomicReference<RuntimeHostController?>(null)

    fun install(controller: RuntimeHostController) {
        host.set(controller)
    }

    @JvmStatic
    fun submit(envelope: ByteArray): ByteArray? {
        val requestId = submissionRequestId(envelope) ?: return null
        val controller = host.get()
            ?: return runtimeFailureEnvelope(
                requestId,
                DaemonErrorToken.CapabilityUnavailable.wire,
                "no Runtime host is installed in this process",
            )
        return runCatching { controller.submit(envelope) }.getOrElse { failure ->
            runtimeFailureEnvelope(
                requestId,
                runtimeFailureCode(failure),
                runtimeFailureMessage(failure),
            )
        }
    }

    @JvmStatic
    fun queryArtifacts(query: ByteArray, descriptor: IntArray): ByteArray? =
        runCatching {
            val controller = host.get() ?: return null
            controller.queryArtifacts(query).use { reply ->
                // Detached last, so any earlier failure still closes the descriptor with the reply.
                descriptor[0] = reply.descriptor?.detachFd() ?: -1
                reply.payload
            }
        }.getOrNull()
}
