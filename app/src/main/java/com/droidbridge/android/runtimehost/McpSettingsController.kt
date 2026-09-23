package com.droidbridge.android.runtimehost

import com.droidbridge.android.product.mcp.MCP_PROTOCOL_VERSION
import android.system.Os
import android.system.OsConstants
import java.io.File
import java.io.FileOutputStream
import java.nio.file.Files
import java.nio.file.StandardCopyOption
import java.security.SecureRandom
import java.util.Base64
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put

/** The native loopback listener as the settings controller drives it. */
internal interface McpListenerPort {
    fun start(port: Int, token: String): Boolean
    fun setToken(token: String): Boolean
    fun stop(): Boolean

    /** `stopped`, `running`, or `failed` once serving ended by itself. */
    fun state(): String
}

internal class NativeMcpListener(private val productVersion: String) : McpListenerPort {
    override fun start(port: Int, token: String): Boolean =
        NativeRuntime.nativeMcpStart(port, token, productVersion)

    override fun setToken(token: String): Boolean = NativeRuntime.nativeMcpSetToken(token)

    override fun stop(): Boolean = NativeRuntime.nativeMcpStop()

    override fun state(): String = NativeRuntime.nativeMcpState() ?: LISTENER_FAILED
}

/** The mode and durability operations the settings file needs from the platform. */
internal interface McpSettingsFileSystem {
    fun restrictToOwner(file: File)
    fun isOwnerOnly(file: File): Boolean
    fun syncDirectory(directory: File)
}

internal class AndroidMcpSettingsFileSystem : McpSettingsFileSystem {
    override fun restrictToOwner(file: File) {
        Os.chmod(file.absolutePath, OWNER_ONLY_MODE)
    }

    override fun isOwnerOnly(file: File): Boolean =
        Os.stat(file.absolutePath).st_mode and PERMISSION_BITS == OWNER_ONLY_MODE

    override fun syncDirectory(directory: File) {
        val descriptor = Os.open(directory.absolutePath, OsConstants.O_RDONLY, 0)
        try {
            Os.fsync(descriptor)
        } finally {
            Os.close(descriptor)
        }
    }

    private companion object {
        const val OWNER_ONLY_MODE = 0b110_000_000
        const val PERMISSION_BITS = 0b111_111_111
    }
}

/**
 * The only writer and in-memory authority of `mcp.json` (S-MCP-003). Every mutation holds one
 * lock and commits the file before the in-memory snapshot, the listener token or the UI reply
 * changes; enable and disable follow the S-MCP-004 foreground/listener order.
 */
internal class McpSettingsController(
    directory: File,
    private val listener: McpListenerPort,
    private val port: Int,
    private val fileSystem: McpSettingsFileSystem,
    private val random: SecureRandom = SecureRandom(),
) {
    private val file = File(directory, FILE_NAME)
    private val lock = Any()
    private var committed: Committed? = null
    private var listenerFailure: String? = null

    private data class Committed(val enabled: Boolean, val token: String)

    val endpoint = "http://127.0.0.1:$port/mcp"

    fun settings(): String = synchronized(lock) {
        load().fold(::status) { IO_ERROR }
    }

    fun setEnabled(enabled: Boolean, foreground: (Boolean) -> Unit): String = synchronized(lock) {
        val current = load().getOrElse { return IO_ERROR }
        if (enabled) {
            val next = if (current.enabled) current else commit(current.copy(enabled = true)).getOrElse { return IO_ERROR }
            startListener(next, foreground)
            status(next)
        } else {
            if (!listener.stop()) return IO_ERROR
            listenerFailure = null
            foreground(false)
            val next = if (!current.enabled) current else commit(current.copy(enabled = false)).getOrElse { return IO_ERROR }
            status(next)
        }
    }

    fun rotate(): String = synchronized(lock) {
        val current = load().getOrElse { return IO_ERROR }
        val next = commit(current.copy(token = newToken())).getOrElse { return IO_ERROR }
        if (!listener.setToken(next.token)) {
            // A listener still holding the replaced token must not outlive the acknowledgment.
            listener.stop()
            if (next.enabled) listenerFailure = MCP_LISTENER_FAILED
        }
        status(next)
    }

    fun reveal(): String = synchronized(lock) {
        load().fold(
            { settings ->
                buildJsonObject {
                    put("schema_version", SCHEMA_VERSION)
                    put("token", settings.token)
                }.toString()
            },
        ) { IO_ERROR }
    }

    /** Stops the listener with its Service, leaving the committed preference for the next UI bind. */
    fun suspendListener() = synchronized(lock) {
        listener.stop()
    }

    /** Restarts a stopped or failed listener while MCP is enabled, for a UI client bind. */
    fun restore(foreground: (Boolean) -> Unit): Unit = synchronized(lock) {
        val current = load().getOrNull() ?: return
        if (current.enabled) startListener(current, foreground)
    }

    private fun startListener(settings: Committed, foreground: (Boolean) -> Unit) {
        if (listener.state() == LISTENER_RUNNING) {
            listenerFailure = null
            return
        }
        try {
            foreground(true)
        } catch (_: RuntimeException) {
            listenerFailure = FGS_START_REJECTED
            return
        }
        if (listener.start(port, settings.token)) {
            listenerFailure = null
        } else {
            listenerFailure = MCP_LISTENER_FAILED
            foreground(false)
        }
    }

    private fun status(settings: Committed): String {
        val observed = listener.state()
        val reason = listenerFailure ?: MCP_LISTENER_FAILED.takeIf { observed == LISTENER_FAILED }
        val state = when {
            settings.enabled && reason != null -> LISTENER_FAILED
            observed == LISTENER_RUNNING -> LISTENER_RUNNING
            else -> LISTENER_STOPPED
        }
        return buildJsonObject {
            put("schema_version", SCHEMA_VERSION)
            put("enabled", settings.enabled)
            put("listener", state)
            if (state == LISTENER_FAILED) put("reason", reason)
            put("endpoint", endpoint)
            put("protocol_version", MCP_PROTOCOL_VERSION)
        }.toString()
    }

    private fun load(): Result<Committed> {
        committed?.let { return Result.success(it) }
        if (!file.exists()) return commit(Committed(enabled = false, token = newToken()))
        return runCatching {
            check(fileSystem.isOwnerOnly(file))
            decode(file.readText())
        }.onSuccess { committed = it }
    }

    private fun commit(next: Committed): Result<Committed> = runCatching {
        val directory = checkNotNull(file.parentFile)
        check(directory.isDirectory || directory.mkdirs())
        val temporary = File(directory, "$FILE_NAME.tmp")
        try {
            FileOutputStream(temporary).use { output ->
                fileSystem.restrictToOwner(temporary)
                output.write(encode(next).encodeToByteArray())
                output.fd.sync()
            }
            Files.move(
                temporary.toPath(),
                file.toPath(),
                StandardCopyOption.ATOMIC_MOVE,
                StandardCopyOption.REPLACE_EXISTING,
            )
            fileSystem.syncDirectory(directory)
            check(fileSystem.isOwnerOnly(file))
        } finally {
            if (temporary.exists()) check(temporary.delete())
        }
        committed = next
        next
    }.onFailure {
        // The file is authoritative again on the next read; the snapshot never runs ahead of it.
        committed = null
    }

    private fun newToken(): String =
        Base64.getUrlEncoder().withoutPadding().encodeToString(ByteArray(TOKEN_BYTES).also(random::nextBytes))

    private companion object {
        const val FILE_NAME = "mcp.json"
        const val SCHEMA_VERSION = 1
        const val TOKEN_BYTES = 32
        const val MCP_LISTENER_FAILED = "MCP_LISTENER_FAILED"
        const val FGS_START_REJECTED = "FGS_START_REJECTED"
        const val IO_ERROR = """{"schema_version":1,"error":"IO_ERROR"}"""
        private val TOKEN = Regex("[A-Za-z0-9_-]{43}")

        fun encode(settings: Committed): String = buildJsonObject {
            put("schema_version", SCHEMA_VERSION)
            put("enabled", settings.enabled)
            put("token", settings.token)
        }.toString()

        fun decode(text: String): Committed {
            val value = Json.parseToJsonElement(text).jsonObject
            require(value.keys == setOf("schema_version", "enabled", "token"))
            val version = value.getValue("schema_version").jsonPrimitive
            require(!version.isString && version.content == SCHEMA_VERSION.toString())
            val enabled = value.getValue("enabled").jsonPrimitive
            require(!enabled.isString)
            val token = value.getValue("token") as JsonPrimitive
            require(token.isString && isToken(token.content))
            return Committed(requireNotNull(enabled.booleanOrNull), token.content)
        }

        fun isToken(value: String): Boolean =
            TOKEN.matches(value) && Base64.getUrlDecoder().decode(value).size == TOKEN_BYTES
    }
}

internal const val LISTENER_RUNNING = "running"
internal const val LISTENER_STOPPED = "stopped"
internal const val LISTENER_FAILED = "failed"
