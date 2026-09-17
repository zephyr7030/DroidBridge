package com.droidbridge.android

import com.droidbridge.android.execution.android.AndroidExecutionException
import com.droidbridge.android.execution.android.AndroidExecutionRequest
import com.droidbridge.android.execution.android.AndroidNetworkSnapshot
import com.droidbridge.android.execution.android.AndroidNetworkSnapshotAdapter
import com.droidbridge.android.execution.android.AndroidPrimitive
import com.droidbridge.android.execution.android.NetworkDnsFact
import com.droidbridge.android.execution.android.NetworkIdentityFact
import com.droidbridge.android.execution.android.NetworkInterfaceAddressFact
import com.droidbridge.android.execution.android.NetworkInterfaceFact
import com.droidbridge.android.execution.android.NetworkRouteFact
import com.droidbridge.android.execution.android.NetworkSnapshotAccess
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.Json
import org.junit.Assert.assertEquals
import org.junit.Test

class I8_NetAndroidAdapterTest {
    @Test
    fun I8_NET_G01_snapshotEncodesTheExactPublicEntryShapes() = runBlocking {
        val adapter = AndroidNetworkSnapshotAdapter(
            NetworkSnapshotAccess { OBSERVED },
            validatesFence = { epoch, generation, instance ->
                epoch == RUNTIME_EPOCH && generation == HOST_GENERATION && instance == INSTANCE
            },
        )

        val result = adapter.execute(request())

        assertEquals(
            Json.parseToJsonElement(
                """
                {
                  "interfaces": [
                    {
                      "name": "wlan0",
                      "mtu": 1500,
                      "addresses": [
                        {"address": "192.168.1.42", "prefix_length": 24},
                        {"address": "fe80::1"}
                      ]
                    },
                    {"name": "rmnet_data1", "addresses": []}
                  ],
                  "routes": [
                    {
                      "destination": "0.0.0.0/0",
                      "gateway": "192.168.1.1",
                      "interface": "wlan0"
                    },
                    {"destination": "192.168.1.0/24", "interface": "wlan0"}
                  ],
                  "dns": [{"server": "192.168.1.1"}, {"server": "2001:4860:4860::8888"}],
                  "default_network": {"network_id": "100", "transport": "wifi"}
                }
                """.trimIndent(),
            ),
            Json.parseToJsonElement(result.payload.decodeToString()),
        )
    }

    @Test
    fun I8_NET_G01_anEstablishedSnapshotWithoutADefaultNetworkKeepsItsEmptyArrays() = runBlocking {
        val adapter = AndroidNetworkSnapshotAdapter(
            NetworkSnapshotAccess {
                AndroidNetworkSnapshot(emptyList(), emptyList(), emptyList(), null)
            },
            validatesFence = { _, _, _ -> true },
        )

        val result = adapter.execute(request())

        assertEquals(
            Json.parseToJsonElement("""{"interfaces":[],"routes":[],"dns":[]}"""),
            Json.parseToJsonElement(result.payload.decodeToString()),
        )
    }

    @Test
    fun I8_NET_G02_snapshotRevalidatesTheFenceAndServesOnlyItsOwnPrimitive() {
        val adapter = AndroidNetworkSnapshotAdapter(
            NetworkSnapshotAccess { OBSERVED },
            validatesFence = { epoch, generation, instance ->
                epoch == RUNTIME_EPOCH && generation == HOST_GENERATION && instance == INSTANCE
            },
        )

        assertEquals(
            "STALE_AUTHORITY",
            failureOf {
                adapter.execute(request(hostGeneration = HOST_GENERATION + 1))
            },
        )
        assertEquals(
            "UNSUPPORTED",
            failureOf { adapter.execute(request(primitive = AndroidPrimitive.ContentInspect)) },
        )
    }

    @Test
    fun I8_NET_G03_anUnconsultableProviderIsUnavailableRatherThanEmpty() {
        val unavailable = AndroidNetworkSnapshotAdapter(
            NetworkSnapshotAccess {
                throw AndroidExecutionException("CAPABILITY_UNAVAILABLE")
            },
            validatesFence = { _, _, _ -> true },
        )
        assertEquals("CAPABILITY_UNAVAILABLE", failureOf { unavailable.execute(request()) })

        val denied = AndroidNetworkSnapshotAdapter(
            NetworkSnapshotAccess { throw SecurityException("ACCESS_NETWORK_STATE") },
            validatesFence = { _, _, _ -> true },
        )
        assertEquals("PERMISSION_DENIED", failureOf { denied.execute(request()) })
    }

    @Test
    fun I8_NET_G04_factsAndPayloadsTheReplyCannotCarryAreRefused() {
        val adapter = AndroidNetworkSnapshotAdapter(
            NetworkSnapshotAccess { OBSERVED },
            validatesFence = { _, _, _ -> true },
        )

        assertEquals(
            "INVALID_ARGUMENT",
            failureOf { adapter.execute(request(payload = """{"scope":"all"}""".encodeToByteArray())) },
        )
        assertEquals(
            "INVALID_ARGUMENT",
            failureOf { adapter.execute(request(payload = "not json".encodeToByteArray())) },
        )
        assertEquals(
            "RESOURCE_LIMIT",
            failureOf { adapter.execute(request(payload = " ".repeat(1_048_577).encodeToByteArray())) },
        )

        for (outOfBounds in listOf(outOfBoundsPrefixLength(), oversizeIdentity(), unnamedInterface())) {
            val invalid = AndroidNetworkSnapshotAdapter(
                NetworkSnapshotAccess { outOfBounds },
                validatesFence = { _, _, _ -> true },
            )
            assertEquals("IO_ERROR", failureOf { invalid.execute(request()) })
        }
    }

    private fun outOfBoundsPrefixLength() = AndroidNetworkSnapshot(
        listOf(
            NetworkInterfaceFact(
                "wlan0",
                mtu = 1500,
                addresses = listOf(NetworkInterfaceAddressFact("192.168.1.42", 200)),
            ),
        ),
        emptyList(),
        emptyList(),
        null,
    )

    private fun oversizeIdentity() = AndroidNetworkSnapshot(
        emptyList(),
        emptyList(),
        emptyList(),
        NetworkIdentityFact("9".repeat(129), "wifi"),
    )

    private fun unnamedInterface() = AndroidNetworkSnapshot(
        listOf(NetworkInterfaceFact("", mtu = null, addresses = emptyList())),
        emptyList(),
        emptyList(),
        null,
    )

    private fun request(
        primitive: AndroidPrimitive = AndroidPrimitive.AndroidNetworkSnapshot,
        payload: ByteArray = "{}".encodeToByteArray(),
        hostGeneration: Long = HOST_GENERATION,
    ) = AndroidExecutionRequest(
        primitive = primitive,
        payload = payload,
        executionId = EXECUTION_ID,
        runtimeEpoch = RUNTIME_EPOCH,
        hostGeneration = hostGeneration,
        runtimeInstanceId = INSTANCE,
    )

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

        val OBSERVED = AndroidNetworkSnapshot(
            interfaces = listOf(
                NetworkInterfaceFact(
                    name = "wlan0",
                    mtu = 1500,
                    addresses = listOf(
                        NetworkInterfaceAddressFact("192.168.1.42", 24),
                        NetworkInterfaceAddressFact("fe80::1", null),
                    ),
                ),
                NetworkInterfaceFact(name = "rmnet_data1", mtu = null, addresses = emptyList()),
            ),
            routes = listOf(
                NetworkRouteFact("0.0.0.0/0", "192.168.1.1", "wlan0"),
                NetworkRouteFact("192.168.1.0/24", null, "wlan0"),
            ),
            dns = listOf(
                NetworkDnsFact("192.168.1.1"),
                NetworkDnsFact("2001:4860:4860::8888"),
            ),
            defaultNetwork = NetworkIdentityFact("100", "wifi"),
        )
    }
}
