package com.droidbridge.android.execution.android

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.longOrNull

internal data class NetworkDefaultObservation(
    val runtimeEpoch: String,
    val hostGeneration: Long,
    val runtimeInstanceId: String,
    val subscriptionGeneration: Long,
    val sourceGeneration: Long,
    val networkId: String?,
    val transport: String?,
)

internal interface NetworkDefaultCallback {
    fun onAvailable(networkId: String)
    fun onCapabilitiesChanged(networkId: String, transport: String?)
    fun onLost(networkId: String)
}

internal interface NetworkDefaultCallbackAccess {
    fun register(callback: NetworkDefaultCallback)
    fun unregister(callback: NetworkDefaultCallback)
}

/**
 * Owns the one S-NET-006 App-context registration. The first observation is deliberately
 * not decided here: the Rust event plane owns baseline and deduplication for every source.
 */
internal class NetworkDefaultCallbackAdapter(
    private val access: NetworkDefaultCallbackAccess,
    private val publishes: (NetworkDefaultObservation) -> Unit,
    private val validatesFence: (String, Long, String) -> Boolean,
    private val setsSpecialUse: (Boolean) -> Unit,
) : AndroidExecutionBridge {
    private val lock = Any()
    private val lifecycleLock = Any()
    private var active: ActiveRegistration? = null

    override suspend fun execute(request: AndroidExecutionRequest): AndroidExecutionResult {
        if (!validatesFence(request.runtimeEpoch, request.hostGeneration, request.runtimeInstanceId)) {
            throw AndroidExecutionException("STALE_AUTHORITY")
        }
        if (request.descriptors.isNotEmpty()) throw AndroidExecutionException("INVALID_ARGUMENT")
        if (request.payload.size > MAX_PAYLOAD_BYTES) throw AndroidExecutionException("RESOURCE_LIMIT")
        val control = decode(request.payload)
        return when (request.primitive) {
            AndroidPrimitive.NetworkDefaultSubscribe -> synchronized(lifecycleLock) {
                subscribe(request, control)
            }
            AndroidPrimitive.NetworkDefaultUnsubscribe -> synchronized(lifecycleLock) {
                unsubscribe(control)
            }
            else -> throw AndroidExecutionException("UNSUPPORTED")
        }
    }

    /** Connection-loss owner cleanup for a Magisk companion registration. */
    fun close(): Boolean = synchronized(lifecycleLock) {
        val registration = synchronized(lock) { active } ?: return@synchronized true
        runCatching {
            cleanup(registration)
            synchronized(lock) {
                if (active === registration) active = null
            }
        }.isSuccess
    }

    private fun subscribe(
        request: AndroidExecutionRequest,
        control: SourceControl,
    ): AndroidExecutionResult {
        val registration = synchronized(lock) {
            if (active != null) throw AndroidExecutionException("ALREADY_EXISTS")
            ActiveRegistration(request, control).also { active = it }
        }
        try {
            setsSpecialUse(true)
        } catch (error: RuntimeException) {
            synchronized(lock) {
                if (active === registration) active = null
            }
            throw executionFailure(error)
        }
        registration.foregroundActive = true
        registration.callbackRegistered = true
        try {
            access.register(registration.callback)
        } catch (error: RuntimeException) {
            if (runCatching { access.unregister(registration.callback) }.isSuccess) {
                registration.callbackRegistered = false
            }
            if (!registration.callbackRegistered && runCatching { setsSpecialUse(false) }.isSuccess) {
                registration.foregroundActive = false
                synchronized(lock) {
                    if (active === registration) active = null
                }
            }
            throw executionFailure(error)
        }
        return AndroidExecutionResult(SUBSCRIBED)
    }

    private fun unsubscribe(control: SourceControl): AndroidExecutionResult {
        val registration = synchronized(lock) {
            val current = active ?: throw AndroidExecutionException("STALE_AUTHORITY")
            if (current.control != control) throw AndroidExecutionException("STALE_AUTHORITY")
            current
        }
        cleanup(registration)
        synchronized(lock) {
            if (active === registration) active = null
        }
        return AndroidExecutionResult(UNSUBSCRIBED)
    }

    private fun cleanup(registration: ActiveRegistration) {
        if (registration.callbackRegistered) {
            try {
                access.unregister(registration.callback)
                registration.callbackRegistered = false
            } catch (error: RuntimeException) {
                throw executionFailure(error)
            }
        }
        if (registration.foregroundActive) {
            try {
                setsSpecialUse(false)
                registration.foregroundActive = false
            } catch (error: RuntimeException) {
                throw executionFailure(error)
            }
        }
    }

    private inner class ActiveRegistration(
        private val request: AndroidExecutionRequest,
        val control: SourceControl,
    ) {
        var callbackRegistered = false
        var foregroundActive = false
        private var availableNetworkId: String? = null
        private var currentNetworkId: String? = null

        val callback = object : NetworkDefaultCallback {
            override fun onAvailable(networkId: String) {
                requireEventFact(networkId)
                synchronized(lock) {
                    if (active === this@ActiveRegistration) availableNetworkId = networkId
                }
            }

            override fun onCapabilitiesChanged(networkId: String, transport: String?) {
                requireEventFact(networkId)
                transport?.let(::requireEventFact)
                val observation = synchronized(lock) {
                    if (active !== this@ActiveRegistration || availableNetworkId != networkId) {
                        return
                    }
                    currentNetworkId = networkId
                    observation(networkId, transport)
                }
                publishes(observation)
            }

            override fun onLost(networkId: String) {
                requireEventFact(networkId)
                val observation = synchronized(lock) {
                    if (
                        active !== this@ActiveRegistration ||
                        (availableNetworkId != networkId && currentNetworkId != networkId)
                    ) {
                        return
                    }
                    availableNetworkId = null
                    currentNetworkId = null
                    observation(null, null)
                }
                publishes(observation)
            }
        }

        private fun observation(networkId: String?, transport: String?) = NetworkDefaultObservation(
            request.runtimeEpoch,
            request.hostGeneration,
            request.runtimeInstanceId,
            control.subscriptionGeneration,
            control.sourceGeneration,
            networkId,
            transport,
        )
    }

    private data class SourceControl(
        val subscriptionGeneration: Long,
        val sourceGeneration: Long,
    )

    private fun decode(payload: ByteArray): SourceControl {
        val value: JsonObject = try {
            Json.parseToJsonElement(payload.decodeToString(throwOnInvalidSequence = true)).jsonObject
        } catch (_: RuntimeException) {
            throw AndroidExecutionException("INVALID_ARGUMENT")
        }
        if (value.keys != CONTROL_KEYS) throw AndroidExecutionException("INVALID_ARGUMENT")
        val subscription = value["subscription_generation"]?.jsonPrimitive?.longOrNull
            ?: throw AndroidExecutionException("INVALID_ARGUMENT")
        val source = value["source_generation"]?.jsonPrimitive?.longOrNull
            ?: throw AndroidExecutionException("INVALID_ARGUMENT")
        if (subscription <= 0 || source <= 0) throw AndroidExecutionException("INVALID_ARGUMENT")
        return SourceControl(subscription, source)
    }

    private fun requireEventFact(value: String) {
        if (value.isEmpty() || value.encodeToByteArray().size > EVENT_FACT_BYTES) {
            throw AndroidExecutionException("IO_ERROR")
        }
    }

    private fun executionFailure(error: RuntimeException): AndroidExecutionException = when (error) {
        is AndroidExecutionException -> error
        is SecurityException -> AndroidExecutionException("PERMISSION_DENIED")
        else -> AndroidExecutionException("IO_ERROR")
    }

    private companion object {
        const val MAX_PAYLOAD_BYTES = 1_048_576
        const val EVENT_FACT_BYTES = 128
        val CONTROL_KEYS = setOf("subscription_generation", "source_generation")
        val SUBSCRIBED = "{\"subscribed\":true}".encodeToByteArray()
        val UNSUBSCRIBED = "{\"unsubscribed\":true}".encodeToByteArray()
    }
}
