package com.droidbridge.android

import com.droidbridge.android.execution.android.AndroidExecutionException
import com.droidbridge.android.execution.android.AndroidExecutionRequest
import com.droidbridge.android.execution.android.AndroidPrimitive
import com.droidbridge.android.execution.android.NetworkDefaultCallback
import com.droidbridge.android.execution.android.NetworkDefaultCallbackAccess
import com.droidbridge.android.execution.android.NetworkDefaultCallbackAdapter
import com.droidbridge.android.execution.android.NetworkDefaultObservation
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class I8_NetSubscriptionAdapterTest {
    @Test
    fun I8_NET_G11_subscriptionUsesOrderedFactsAndOwnsForegroundLifetime() = runBlocking {
        val order = mutableListOf<String>()
        val access = RecordingCallbackAccess(order)
        val observed = mutableListOf<NetworkDefaultObservation>()
        val adapter = NetworkDefaultCallbackAdapter(
            access = access,
            publishes = observed::add,
            validatesFence = ::validatesFence,
            setsSpecialUse = { active -> order += "foreground:$active" },
        )

        assertEquals(
            "{\"subscribed\":true}",
            adapter.execute(request(AndroidPrimitive.NetworkDefaultSubscribe)).payload.decodeToString(),
        )
        assertEquals(listOf("foreground:true", "register"), order)

        access.callback.onAvailable("100")
        assertTrue(observed.isEmpty())
        access.callback.onCapabilitiesChanged("100", "wifi")
        assertEquals(
            listOf(
                NetworkDefaultObservation(
                    RUNTIME_EPOCH,
                    HOST_GENERATION,
                    INSTANCE,
                    SUBSCRIPTION_GENERATION,
                    SOURCE_GENERATION,
                    "100",
                    "wifi",
                ),
            ),
            observed,
        )
        access.callback.onCapabilitiesChanged("100", "wifi")
        access.callback.onLost("stale")
        assertEquals(2, observed.size)
        access.callback.onLost("100")
        assertEquals(null, observed.last().networkId)
        assertEquals(null, observed.last().transport)

        assertEquals(
            "{\"unsubscribed\":true}",
            adapter.execute(request(AndroidPrimitive.NetworkDefaultUnsubscribe)).payload.decodeToString(),
        )
        assertEquals(
            listOf("foreground:true", "register", "unregister", "foreground:false"),
            order,
        )
    }

    @Test
    fun I8_NET_G12_generationMismatchAndDuplicateRegistrationAreRejected() {
        val access = RecordingCallbackAccess(mutableListOf())
        val adapter = adapter(access)
        runBlocking { adapter.execute(request(AndroidPrimitive.NetworkDefaultSubscribe)) }

        assertEquals(
            "ALREADY_EXISTS",
            failureOf { adapter.execute(request(AndroidPrimitive.NetworkDefaultSubscribe)) },
        )
        assertEquals(
            "STALE_AUTHORITY",
            failureOf {
                adapter.execute(
                    request(
                        AndroidPrimitive.NetworkDefaultUnsubscribe,
                        sourceGeneration = SOURCE_GENERATION + 1,
                    ),
                )
            },
        )
        assertTrue(access.registered)
        runBlocking { adapter.execute(request(AndroidPrimitive.NetworkDefaultUnsubscribe)) }
        assertFalse(access.registered)
    }

    @Test
    fun I8_NET_G12_failedUnregisterRetainsOwnershipUntilVerifiedRetry() {
        val order = mutableListOf<String>()
        val access = RecordingCallbackAccess(order)
        val foreground = mutableListOf<Boolean>()
        val adapter = NetworkDefaultCallbackAdapter(
            access,
            publishes = {},
            validatesFence = ::validatesFence,
            setsSpecialUse = foreground::add,
        )
        runBlocking { adapter.execute(request(AndroidPrimitive.NetworkDefaultSubscribe)) }
        access.failUnregister = true

        assertEquals(
            "IO_ERROR",
            failureOf { adapter.execute(request(AndroidPrimitive.NetworkDefaultUnsubscribe)) },
        )
        assertEquals(listOf(true), foreground)
        assertTrue(access.registered)

        access.failUnregister = false
        runBlocking { adapter.execute(request(AndroidPrimitive.NetworkDefaultUnsubscribe)) }
        assertEquals(listOf(true, false), foreground)
        assertFalse(access.registered)
    }

    @Test
    fun I8_NET_G12_companionLossCleansTheCallbackLocallyBeforeSourceReplacement() {
        val order = mutableListOf<String>()
        val access = RecordingCallbackAccess(order)
        val foreground = mutableListOf<Boolean>()
        val adapter = NetworkDefaultCallbackAdapter(
            access,
            publishes = {},
            validatesFence = ::validatesFence,
            setsSpecialUse = foreground::add,
        )
        runBlocking { adapter.execute(request(AndroidPrimitive.NetworkDefaultSubscribe)) }

        assertTrue(adapter.close())
        assertFalse(access.registered)
        assertEquals(listOf(true, false), foreground)
        assertTrue("connection-loss cleanup is idempotent", adapter.close())
    }

    @Test
    fun I8_NET_G12_sourceControlValidatesFencePayloadAndDescriptorBoundary() {
        val adapter = adapter(RecordingCallbackAccess(mutableListOf()))
        assertEquals(
            "STALE_AUTHORITY",
            failureOf {
                adapter.execute(
                    request(AndroidPrimitive.NetworkDefaultSubscribe, hostGeneration = 99),
                )
            },
        )
        assertEquals(
            "INVALID_ARGUMENT",
            failureOf {
                adapter.execute(
                    request(
                        AndroidPrimitive.NetworkDefaultSubscribe,
                        payload = "{\"subscription_generation\":1}".encodeToByteArray(),
                    ),
                )
            },
        )
        assertEquals(
            "INVALID_ARGUMENT",
            failureOf {
                adapter.execute(
                    request(
                        AndroidPrimitive.NetworkDefaultSubscribe,
                        subscriptionGeneration = 0,
                    ),
                )
            },
        )
    }

    private class RecordingCallbackAccess(private val order: MutableList<String>) :
        NetworkDefaultCallbackAccess {
        lateinit var callback: NetworkDefaultCallback
        var registered = false
        var failUnregister = false

        override fun register(callback: NetworkDefaultCallback) {
            check(!registered)
            order += "register"
            this.callback = callback
            registered = true
        }

        override fun unregister(callback: NetworkDefaultCallback) {
            check(this.callback === callback)
            order += "unregister"
            if (failUnregister) throw IllegalStateException("scripted cleanup failure")
            registered = false
        }
    }

    private fun adapter(access: NetworkDefaultCallbackAccess) = NetworkDefaultCallbackAdapter(
        access,
        publishes = {},
        validatesFence = ::validatesFence,
        setsSpecialUse = {},
    )

    private fun request(
        primitive: AndroidPrimitive,
        subscriptionGeneration: Long = SUBSCRIPTION_GENERATION,
        sourceGeneration: Long = SOURCE_GENERATION,
        hostGeneration: Long = HOST_GENERATION,
        payload: ByteArray = """
            {
              "subscription_generation": $subscriptionGeneration,
              "source_generation": $sourceGeneration
            }
        """.trimIndent().encodeToByteArray(),
    ) = AndroidExecutionRequest(
        primitive = primitive,
        payload = payload,
        executionId = EXECUTION_ID,
        runtimeEpoch = RUNTIME_EPOCH,
        hostGeneration = hostGeneration,
        runtimeInstanceId = INSTANCE,
    )

    private fun validatesFence(epoch: String, generation: Long, instance: String): Boolean =
        epoch == RUNTIME_EPOCH && generation == HOST_GENERATION && instance == INSTANCE

    private fun failureOf(block: suspend () -> Unit): String? = try {
        runBlocking { block() }
        null
    } catch (error: AndroidExecutionException) {
        error.code
    }

    private companion object {
        const val RUNTIME_EPOCH = "10000000-0000-4000-8000-000000000001"
        const val INSTANCE = "10000000-0000-4000-8000-000000000002"
        const val EXECUTION_ID = "10000000-0000-4000-8000-000000000003"
        const val HOST_GENERATION = 4L
        const val SUBSCRIPTION_GENERATION = 5L
        const val SOURCE_GENERATION = 6L
    }
}
