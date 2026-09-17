package com.droidbridge.android.execution.android

import android.net.ConnectivityManager
import android.net.LinkProperties
import android.net.Network
import android.net.NetworkCapabilities
import android.net.RouteInfo
import java.net.InetAddress

/**
 * The App process's network facts, read from the Android framework on demand.
 *
 * R-NET-011/S-UI-009: this reads the already-declared `ACCESS_NETWORK_STATE` fact only and
 * requests no permission, and it registers no `NetworkCallback`: facts are read when the
 * Runtime asks for them. The default network is the framework's own active one, so a
 * change is observed as a change of the identity this reports and never tracked here.
 */
internal class AndroidConnectivityAccess(
    private val connectivity: ConnectivityManager,
) : NetworkSnapshotAccess {
    override fun snapshot(): AndroidNetworkSnapshot {
        val network = connectivity.activeNetwork
        if (network == null) {
            // An established read of a device with no default network: every list is a
            // fact the framework established, and no identity was observed.
            return AndroidNetworkSnapshot(emptyList(), emptyList(), emptyList(), null)
        }
        // A default network the framework cannot describe is a provider that could not
        // establish its facts, not a device without a default network.
        val link = connectivity.getLinkProperties(network)
            ?: throw AndroidExecutionException("CAPABILITY_UNAVAILABLE")
        val capabilities = connectivity.getNetworkCapabilities(network)
            ?: throw AndroidExecutionException("CAPABILITY_UNAVAILABLE")
        return AndroidNetworkSnapshot(
            interfaces = interfaceFacts(link),
            routes = routeFacts(link),
            dns = link.dnsServers.mapNotNull { server ->
                server.hostAddress?.let(::NetworkDnsFact)
            },
            defaultNetwork = identity(network, capabilities),
        )
    }

    /**
     * One entry per link, because `LinkProperties` describes exactly one link. A link the
     * framework reports without a name cannot be expressed in the R-NET-002 entry shape,
     * so it is omitted rather than named by an invented one.
     */
    private fun interfaceFacts(link: LinkProperties): List<NetworkInterfaceFact> {
        val name = link.interfaceName ?: return emptyList()
        val addresses = link.linkAddresses.mapNotNull { linkAddress ->
            val address = linkAddress.address ?: return@mapNotNull null
            NetworkInterfaceAddressFact(
                address = unscoped(address) ?: return@mapNotNull null,
                prefixLength = linkAddress.prefixLength,
            )
        }
        return listOf(NetworkInterfaceFact(name, link.mtu, addresses))
    }

    /**
     * The link's routes in the R-NET-002 shape. Only unicast routes are routes to this
     * host's destinations: an unreachable or throw route would otherwise answer a
     * `network.diagnose` route query with a destination this host cannot reach.
     */
    private fun routeFacts(link: LinkProperties): List<NetworkRouteFact> =
        link.routes
            .filter { it.type == RouteInfo.RTN_UNICAST }
            .mapNotNull { route ->
                val destination = route.destination
                val address = unscoped(destination.address) ?: return@mapNotNull null
                NetworkRouteFact(
                    destination = "$address/${destination.prefixLength}",
                    gateway = route.gateway?.hostAddress,
                    interfaceName = route.`interface`,
                )
            }

    private fun identity(network: Network, capabilities: NetworkCapabilities): NetworkIdentityFact =
        NetworkIdentityFact(
            networkId = network.networkHandle.toString(),
            transport = networkTransport(capabilities),
        )

    /**
     * The scope of a scoped address is the interface, which the entry already names
     * separately, so the address itself is reported in its plain form.
     */
    private fun unscoped(address: InetAddress): String? =
        address.hostAddress?.substringBefore('%')

}

/** Most-specific fixed transport projection shared by snapshot and callback sources. */
internal fun networkTransport(capabilities: NetworkCapabilities): String? =
    NETWORK_TRANSPORTS.firstOrNull { capabilities.hasTransport(it.first) }?.second

private val NETWORK_TRANSPORTS = listOf(
    NetworkCapabilities.TRANSPORT_VPN to "vpn",
    NetworkCapabilities.TRANSPORT_CELLULAR to "cellular",
    NetworkCapabilities.TRANSPORT_WIFI to "wifi",
    NetworkCapabilities.TRANSPORT_WIFI_AWARE to "wifi_aware",
    NetworkCapabilities.TRANSPORT_BLUETOOTH to "bluetooth",
    NetworkCapabilities.TRANSPORT_ETHERNET to "ethernet",
    NetworkCapabilities.TRANSPORT_USB to "usb",
    NetworkCapabilities.TRANSPORT_LOWPAN to "lowpan",
    NetworkCapabilities.TRANSPORT_SATELLITE to "satellite",
    NetworkCapabilities.TRANSPORT_THREAD to "thread",
)
