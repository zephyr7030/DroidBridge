package com.droidbridge.android.execution.android

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.put

/**
 * One interface's framework link facts, in the R-NET-002 `interfaces` entry shape. The
 * App-native `getifaddrs` facts are the Rust host's own source, so this carries only what
 * that source cannot report: the interface MTU and the link addresses.
 */
internal data class NetworkInterfaceFact(
    val name: String,
    val mtu: Int?,
    val addresses: List<NetworkInterfaceAddressFact>,
)

internal data class NetworkInterfaceAddressFact(
    val address: String,
    val prefixLength: Int?,
)

internal data class NetworkRouteFact(
    val destination: String,
    val gateway: String?,
    val interfaceName: String?,
)

internal data class NetworkDnsFact(
    val server: String,
)

/**
 * The identity of the active default network. This is the whole fact the bounded
 * `event.network_default_changed.v1` announcement carries (S-HANDOFF-011), so a fact the
 * source did not establish is absent rather than guessed.
 */
internal data class NetworkIdentityFact(
    val networkId: String,
    val transport: String?,
)

/**
 * The App process's network facts for one read. An absent fact is omitted rather than
 * invented, and an empty list is a fact the source established (S-NET-001).
 */
internal data class AndroidNetworkSnapshot(
    val interfaces: List<NetworkInterfaceFact>,
    val routes: List<NetworkRouteFact>,
    val dns: List<NetworkDnsFact>,
    val defaultNetwork: NetworkIdentityFact?,
)

/**
 * Reads the App process's own network facts on demand. A source that cannot be consulted
 * raises [AndroidExecutionException] with `CAPABILITY_UNAVAILABLE`, which leaves the
 * families it owns unresolved instead of reporting them as empty.
 */
internal fun interface NetworkSnapshotAccess {
    fun snapshot(): AndroidNetworkSnapshot
}

/**
 * The `AndroidNetworkSnapshot` primitive of the APK execution surface.
 *
 * S-NET-001 assigns this host's App/framework families to a provider in this process, and
 * S-NET-002 keeps the request's admission with the Runtime, so this adapter only reads the
 * facts the Runtime asked for and encodes them in the entry shapes R-NET-002 declares. It
 * requests no permission, runs no watcher, and never substitutes a different provider's
 * facts for an unavailable one.
 */
internal class AndroidNetworkSnapshotAdapter(
    private val access: NetworkSnapshotAccess,
    private val validatesFence: (String, Long, String) -> Boolean,
) : AndroidExecutionBridge {
    override suspend fun execute(request: AndroidExecutionRequest): AndroidExecutionResult {
        if (!validatesFence(request.runtimeEpoch, request.hostGeneration, request.runtimeInstanceId)) {
            throw AndroidExecutionException("STALE_AUTHORITY")
        }
        if (request.primitive != AndroidPrimitive.AndroidNetworkSnapshot) {
            throw AndroidExecutionException("UNSUPPORTED")
        }
        if (request.descriptors.isNotEmpty()) {
            throw AndroidExecutionException("INVALID_ARGUMENT")
        }
        if (request.payload.size > MAX_PAYLOAD_BYTES) {
            throw AndroidExecutionException("RESOURCE_LIMIT")
        }
        requireKeys(parseObject(request.payload), EMPTY_KEYS)
        val snapshot = try {
            access.snapshot()
        } catch (error: AndroidExecutionException) {
            throw error
        } catch (_: SecurityException) {
            throw AndroidExecutionException("PERMISSION_DENIED")
        } catch (_: RuntimeException) {
            throw AndroidExecutionException("IO_ERROR")
        }
        return AndroidExecutionResult(encode(snapshot).encodeToByteArray())
    }

    private fun encode(snapshot: AndroidNetworkSnapshot): String = buildJsonObject {
        put("interfaces", buildJsonArray {
            snapshot.interfaces.forEach { fact ->
                add(buildJsonObject {
                    put("name", requireFact(fact.name))
                    optionalMtu(fact.mtu)?.let { put("mtu", it) }
                    put("addresses", buildJsonArray {
                        fact.addresses.forEach { address ->
                            add(buildJsonObject {
                                put("address", requireFact(address.address))
                                optionalPrefixLength(address.prefixLength)?.let {
                                    put("prefix_length", it)
                                }
                            })
                        }
                    })
                })
            }
        })
        put("routes", buildJsonArray {
            snapshot.routes.forEach { route ->
                add(buildJsonObject {
                    put("destination", requireFact(route.destination))
                    route.gateway?.let { put("gateway", requireFact(it)) }
                    route.interfaceName?.let { put("interface", requireFact(it)) }
                })
            }
        })
        put("dns", buildJsonArray {
            snapshot.dns.forEach { entry ->
                add(buildJsonObject { put("server", requireFact(entry.server)) })
            }
        })
        snapshot.defaultNetwork?.let { identity ->
            put("default_network", buildJsonObject {
                put("network_id", requireEventFact(identity.networkId))
                identity.transport?.let { put("transport", requireEventFact(it)) }
            })
        }
    }.toString()

    private fun parseObject(payload: ByteArray): JsonObject = try {
        Json.parseToJsonElement(payload.decodeToString(throwOnInvalidSequence = true)).jsonObject
    } catch (_: RuntimeException) {
        throw AndroidExecutionException("INVALID_ARGUMENT")
    }

    private fun requireKeys(value: JsonObject, expected: Set<String>) {
        if (value.keys != expected) throw AndroidExecutionException("INVALID_ARGUMENT")
    }

    /** An entry's required text cannot be empty, because the reply's readers index by it. */
    private fun requireFact(value: String): String {
        if (value.isEmpty() || value.contains('\u0000')) {
            throw AndroidExecutionException("IO_ERROR")
        }
        return value
    }

    /** S-HANDOFF-011 bounds each identity fact of the announced event at 128 bytes. */
    private fun requireEventFact(value: String): String =
        requireFact(value).takeIf { it.encodeToByteArray().size <= EVENT_FACT_BYTES }
            ?: throw AndroidExecutionException("IO_ERROR")

    /** R-NET-002 carries an optional prefix length, which no address family exceeds 128. */
    private fun optionalPrefixLength(value: Int?): Int? = when {
        value == null -> null
        value !in 0..128 -> throw AndroidExecutionException("IO_ERROR")
        else -> value
    }

    /** The framework reports no MTU while the default is in use, and no negative one ever. */
    private fun optionalMtu(value: Int?): Int? = when {
        value == null -> null
        value < 0 -> throw AndroidExecutionException("IO_ERROR")
        value == 0 -> null
        else -> value
    }

    private companion object {
        const val MAX_PAYLOAD_BYTES = 1_048_576
        const val EVENT_FACT_BYTES = 128
        val EMPTY_KEYS = emptySet<String>()
    }
}
