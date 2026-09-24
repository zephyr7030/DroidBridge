package com.droidbridge.android.runtimehost

import com.droidbridge.android.product.mcp.MCP_PROTOCOL_VERSION
import android.net.ConnectivityManager
import android.net.Network
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import java.io.File
import java.io.FileOutputStream
import java.nio.file.Files
import java.nio.file.StandardCopyOption
import java.security.KeyStore
import java.util.Base64
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put

internal interface TunnelRuntimePort {
    fun validate(tunnelId: String, apiKey: String): String
    fun start(tunnelId: String, apiKey: String): Boolean
    fun stop(): Boolean
    fun state(): String
    fun lastCallEpochMs(): Long
    fun lastError(): String? = null
}

internal class NativeTunnelRuntime(
    private val port: Int,
    private val productVersion: String,
) : TunnelRuntimePort {
    override fun validate(tunnelId: String, apiKey: String): String =
        NativeRuntime.nativeTunnelValidate(tunnelId, apiKey, productVersion) ?: TUNNEL_VALIDATION_UNAVAILABLE

    override fun start(tunnelId: String, apiKey: String): Boolean =
        NativeRuntime.nativeTunnelStart(port, tunnelId, apiKey, productVersion)

    override fun stop(): Boolean = NativeRuntime.nativeTunnelStop()

    override fun state(): String = NativeRuntime.nativeTunnelState() ?: TUNNEL_FAILED

    override fun lastCallEpochMs(): Long = NativeRuntime.nativeTunnelLastCall()

    override fun lastError(): String? = NativeRuntime.nativeTunnelLastError()
}

internal data class EncryptedTunnelCredential(val ciphertext: String, val iv: String)

internal interface TunnelCredentialCipher {
    fun encrypt(tunnelId: String, apiKey: String): EncryptedTunnelCredential
    fun decrypt(tunnelId: String, credential: EncryptedTunnelCredential): String
    fun deleteKey()
}

internal class AndroidTunnelCredentialCipher : TunnelCredentialCipher {
    override fun encrypt(tunnelId: String, apiKey: String): EncryptedTunnelCredential {
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.ENCRYPT_MODE, key(create = true))
        cipher.updateAAD(tunnelId.encodeToByteArray())
        return EncryptedTunnelCredential(
            ciphertext = ENCODER.encodeToString(cipher.doFinal(apiKey.encodeToByteArray())),
            iv = ENCODER.encodeToString(cipher.iv),
        )
    }

    override fun decrypt(tunnelId: String, credential: EncryptedTunnelCredential): String {
        val iv = DECODER.decode(credential.iv).also { check(it.size == GCM_IV_BYTES) }
        val ciphertext = DECODER.decode(credential.ciphertext).also { check(it.size <= MAX_CIPHERTEXT_BYTES) }
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(
            Cipher.DECRYPT_MODE,
            key(create = false),
            GCMParameterSpec(GCM_TAG_BITS, iv),
        )
        cipher.updateAAD(tunnelId.encodeToByteArray())
        return cipher.doFinal(ciphertext).toString(Charsets.UTF_8)
    }

    override fun deleteKey() {
        keyStore().run { if (containsAlias(KEY_ALIAS)) deleteEntry(KEY_ALIAS) }
    }

    private fun key(create: Boolean): SecretKey {
        val store = keyStore()
        (store.getKey(KEY_ALIAS, null) as? SecretKey)?.let { return it }
        check(create)
        return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, ANDROID_KEYSTORE).run {
            init(
                KeyGenParameterSpec.Builder(
                    KEY_ALIAS,
                    KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
                )
                    .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                    .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                    .setKeySize(256)
                    .build(),
            )
            generateKey()
        }
    }

    private fun keyStore(): KeyStore =
        KeyStore.getInstance(ANDROID_KEYSTORE).apply { load(null) }

    private companion object {
        const val ANDROID_KEYSTORE = "AndroidKeyStore"
        const val KEY_ALIAS = "droidbridge_tunnel_api_key_v1"
        const val TRANSFORMATION = "AES/GCM/NoPadding"
        const val GCM_TAG_BITS = 128
        const val GCM_IV_BYTES = 12
        const val MAX_CIPHERTEXT_BYTES = 528
        val ENCODER: Base64.Encoder = Base64.getUrlEncoder().withoutPadding()
        val DECODER: Base64.Decoder = Base64.getUrlDecoder()
    }
}

internal interface TunnelNetworkMonitor {
    fun start(changed: (Boolean) -> Unit): Boolean
    fun stop()
}

internal class AndroidTunnelNetworkMonitor(
    private val connectivity: ConnectivityManager,
) : TunnelNetworkMonitor {
    private val lock = Any()
    private var registered: ConnectivityManager.NetworkCallback? = null

    override fun start(changed: (Boolean) -> Unit): Boolean = synchronized(lock) {
        if (registered != null) return true
        val callback = object : ConnectivityManager.NetworkCallback() {
            private var current: Network? = null

            override fun onAvailable(network: Network) {
                synchronized(this) { current = network }
                changed(true)
            }

            override fun onLost(network: Network) {
                val lostCurrent = synchronized(this) {
                    if (current != network) false else true.also { current = null }
                }
                if (lostCurrent) changed(false)
            }
        }
        runCatching { connectivity.registerDefaultNetworkCallback(callback) }
            .onSuccess { registered = callback }
            .isSuccess
    }

    override fun stop() {
        synchronized(lock) {
            registered?.let { callback ->
                runCatching { connectivity.unregisterNetworkCallback(callback) }
                registered = null
            }
        }
    }
}

internal class TunnelSettingsController(
    directory: File,
    private val runtime: TunnelRuntimePort,
    private val network: TunnelNetworkMonitor,
    private val cipher: TunnelCredentialCipher,
    private val fileSystem: McpSettingsFileSystem,
) {
    private val file = File(directory, FILE_NAME)
    private val lock = Any()
    private var committed: Committed? = null
    private var loaded = false
    private var generation = 0L
    private var watching = false
    private var failure: String? = null

    /** Told whether the committed preference keeps a connection enabled, whenever that may have changed. */
    @Volatile
    var enabledObserver: ((Boolean) -> Unit)? = null
        set(value) {
            field = value
            value?.invoke(synchronized(lock) { load().getOrNull()?.enabled == true })
        }

    private data class Committed(
        val enabled: Boolean,
        val tunnelId: String,
        val credential: EncryptedTunnelCredential,
        /** When ChatGPT first called through this tunnel; first setup finishes on it after a restart too. */
        val firstCallEpochMs: Long? = null,
    )

    fun settings(): String = synchronized(lock) {
        load().fold({ status(it) }) { IO_ERROR }
    }

    fun configure(
        tunnelId: String,
        apiKey: String,
        foreground: (Boolean) -> Unit,
    ): String = synchronized(lock) {
        if (!isTunnelId(tunnelId) || !isApiKey(apiKey)) return INVALID_CONFIG
        when (runtime.validate(tunnelId, apiKey)) {
            TUNNEL_VALID -> Unit
            TUNNEL_INVALID_ID -> return TUNNEL_NOT_FOUND
            TUNNEL_INVALID_KEY -> return API_KEY_INVALID
            else -> return OPENAI_UNAVAILABLE
        }
        val current = load().getOrElse { return IO_ERROR }
        val credential = runCatching { cipher.encrypt(tunnelId, apiKey) }.getOrElse { return IO_ERROR }
        val next = commit(Committed(current?.enabled == true, tunnelId, credential)).getOrElse { return IO_ERROR }
        if (next.enabled) restart(foreground)
        status(next)
    }

    fun setEnabled(enabled: Boolean, foreground: (Boolean) -> Unit): String = synchronized(lock) {
        val current = load().getOrElse { return IO_ERROR } ?: return NOT_CONFIGURED
        if (enabled) {
            val next = if (current.enabled) current else commit(current.copy(enabled = true)).getOrElse { return IO_ERROR }
            if (current.enabled) restart(foreground) else start(foreground)
            status(next)
        } else {
            stop(foreground)
            val next = if (!current.enabled) current else commit(current.copy(enabled = false)).getOrElse { return IO_ERROR }
            status(next)
        }
    }

    fun clear(foreground: (Boolean) -> Unit): String = synchronized(lock) {
        load().getOrElse { return IO_ERROR }
        stop(foreground)
        runCatching {
            if (file.exists()) {
                check(file.delete())
                fileSystem.syncDirectory(checkNotNull(file.parentFile))
            }
            cipher.deleteKey()
        }.getOrElse { return IO_ERROR }
        committed = null
        loaded = true
        enabledObserver?.invoke(false)
        status(null)
    }

    fun restore(foreground: (Boolean) -> Unit) {
        synchronized(lock) {
            val current = load().getOrNull() ?: return
            if (current.enabled) start(foreground)
        }
    }

    fun suspendRuntime(foreground: (Boolean) -> Unit) {
        synchronized(lock) { stop(foreground) }
    }

    private fun start(foreground: (Boolean) -> Unit) {
        if (watching) return
        try {
            foreground(true)
        } catch (_: RuntimeException) {
            failure = FGS_START_REJECTED
            return
        }
        if (runtime.state() == TUNNEL_FAILED) runtime.stop()
        failure = null
        generation += 1
        val expected = generation
        if (!network.start { available -> networkChanged(expected, available) }) {
            failure = NETWORK_MONITOR_FAILED
            foreground(false)
            return
        }
        watching = true
    }

    private fun restart(foreground: (Boolean) -> Unit) {
        network.stop()
        watching = false
        runtime.stop()
        generation += 1
        start(foreground)
    }

    private fun stop(foreground: (Boolean) -> Unit) {
        generation += 1
        network.stop()
        watching = false
        runtime.stop()
        failure = null
        foreground(false)
    }

    private fun networkChanged(expected: Long, available: Boolean) {
        synchronized(lock) {
            if (expected != generation) return
            val current = committed?.takeIf { it.enabled } ?: return
            if (!available) {
                runtime.stop()
                return
            }
            val apiKey = runCatching { cipher.decrypt(current.tunnelId, current.credential) }
                .getOrElse {
                    failure = CREDENTIALS_UNAVAILABLE
                    return
                }
            if (!isApiKey(apiKey) || !runtime.start(current.tunnelId, apiKey)) {
                failure = TUNNEL_RUNTIME_FAILED
            } else {
                failure = null
            }
        }
    }

    private fun status(current: Committed?): String {
        // The running tunnel knows only calls since it started; the first one is kept, so a
        // restarted App still knows ChatGPT has called. A failed write keeps only the live value.
        val observedCall = runtime.lastCallEpochMs().takeIf { it > 0 }
        val settings = if (current != null && current.firstCallEpochMs == null && observedCall != null) {
            commit(current.copy(firstCallEpochMs = observedCall)).getOrDefault(current)
        } else {
            current
        }
        val nativeState = runtime.state().takeIf { it in STATES } ?: TUNNEL_FAILED
        val state = when {
            settings?.enabled != true -> TUNNEL_STOPPED
            failure != null || nativeState == TUNNEL_FAILED -> TUNNEL_FAILED
            nativeState == TUNNEL_RUNNING -> TUNNEL_RUNNING
            else -> TUNNEL_CONNECTING
        }
        val reason = failure ?: TUNNEL_RUNTIME_FAILED.takeIf { state == TUNNEL_FAILED }
        return buildJsonObject {
            put("schema_version", SCHEMA_VERSION)
            put("configured", settings != null)
            put("enabled", settings?.enabled == true)
            put("state", state)
            if (settings != null) put("tunnel_id", settings.tunnelId)
            if (reason != null) put("reason", reason)
            (observedCall ?: settings?.firstCallEpochMs)?.let { put("last_call_epoch_ms", it) }
            // Only a tunnel that is enabled but not running has a failure worth naming.
            runtime.lastError()?.takeIf { state == TUNNEL_CONNECTING || state == TUNNEL_FAILED }
                ?.takeIf { LAST_ERROR.matches(it) }?.let { put("last_error", it) }
            put("protocol_version", MCP_PROTOCOL_VERSION)
        }.toString()
    }

    private fun load(): Result<Committed?> {
        if (loaded) return Result.success(committed)
        if (!file.exists()) {
            loaded = true
            return Result.success(null)
        }
        return runCatching {
            check(fileSystem.isOwnerOnly(file))
            decode(file.readText())
        }.onSuccess {
            committed = it
            loaded = true
        }
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
        loaded = true
        enabledObserver?.invoke(next.enabled)
        next
    }.onFailure {
        committed = null
        loaded = false
    }

    private companion object {
        const val FILE_NAME = "tunnel.json"
        const val SCHEMA_VERSION = 1
        const val FGS_START_REJECTED = "FGS_START_REJECTED"
        const val NETWORK_MONITOR_FAILED = "NETWORK_MONITOR_FAILED"
        const val CREDENTIALS_UNAVAILABLE = "CREDENTIALS_UNAVAILABLE"
        const val TUNNEL_RUNTIME_FAILED = "TUNNEL_RUNTIME_FAILED"
        const val IO_ERROR = """{"schema_version":1,"error":"IO_ERROR"}"""
        const val INVALID_CONFIG = """{"schema_version":1,"error":"INVALID_CONFIG"}"""
        const val NOT_CONFIGURED = """{"schema_version":1,"error":"NOT_CONFIGURED"}"""
        const val TUNNEL_NOT_FOUND = """{"schema_version":1,"error":"TUNNEL_NOT_FOUND"}"""
        const val API_KEY_INVALID = """{"schema_version":1,"error":"API_KEY_INVALID"}"""
        const val OPENAI_UNAVAILABLE = """{"schema_version":1,"error":"OPENAI_UNAVAILABLE"}"""
        private val TUNNEL_ID = Regex("tunnel_[a-z0-9]{32}")
        private val LAST_ERROR = Regex("[a-z0-9_]{1,40}")

        fun encode(settings: Committed): String = buildJsonObject {
            put("schema_version", SCHEMA_VERSION)
            put("enabled", settings.enabled)
            put("tunnel_id", settings.tunnelId)
            put("ciphertext", settings.credential.ciphertext)
            put("iv", settings.credential.iv)
            settings.firstCallEpochMs?.let { put("first_call_epoch_ms", it) }
        }.toString()

        fun decode(text: String): Committed {
            val value = Json.parseToJsonElement(text).jsonObject
            val firstCall = value["first_call_epoch_ms"]?.jsonPrimitive?.also { require(!it.isString) }
                ?.content?.toLong()?.also { require(it > 0) }
            require(
                value.keys == setOf("schema_version", "enabled", "tunnel_id", "ciphertext", "iv") +
                    if (firstCall != null) setOf("first_call_epoch_ms") else emptySet(),
            )
            val version = value.getValue("schema_version").jsonPrimitive
            require(!version.isString && version.content == SCHEMA_VERSION.toString())
            val enabled = value.getValue("enabled").jsonPrimitive
            require(!enabled.isString)
            val tunnelId = value.string("tunnel_id")
            val ciphertext = value.string("ciphertext")
            val iv = value.string("iv")
            require(
                isTunnelId(tunnelId) &&
                    ciphertext.isNotEmpty() && ciphertext.length <= MAX_CIPHERTEXT_LENGTH &&
                    iv.isNotEmpty() && iv.length <= MAX_IV_LENGTH,
            )
            return Committed(
                enabled = requireNotNull(enabled.booleanOrNull),
                tunnelId = tunnelId,
                credential = EncryptedTunnelCredential(ciphertext, iv),
                firstCallEpochMs = firstCall,
            )
        }

        fun isTunnelId(value: String): Boolean = TUNNEL_ID.matches(value)

        fun isApiKey(value: String): Boolean =
            value.isNotEmpty() && value.length <= 512 && value.all { it.code in 0x21..0x7e }

        const val MAX_CIPHERTEXT_LENGTH = 1024
        const val MAX_IV_LENGTH = 64

        fun kotlinx.serialization.json.JsonObject.string(name: String): String =
            (getValue(name) as JsonPrimitive).also { require(it.isString) }.content
    }
}

internal const val TUNNEL_STOPPED = "stopped"
internal const val TUNNEL_CONNECTING = "connecting"
internal const val TUNNEL_RUNNING = "running"
internal const val TUNNEL_FAILED = "failed"
internal const val TUNNEL_VALID = "valid"
internal const val TUNNEL_INVALID_ID = "invalid_tunnel"
internal const val TUNNEL_INVALID_KEY = "invalid_key"
internal const val TUNNEL_VALIDATION_UNAVAILABLE = "unavailable"
private val STATES = setOf(TUNNEL_STOPPED, TUNNEL_CONNECTING, TUNNEL_RUNNING, TUNNEL_FAILED)
