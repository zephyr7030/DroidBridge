package com.droidbridge.android.execution.android

import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.os.Handler
import android.os.HandlerThread

/** Android's one default-network callback registration on its own serial callback thread. */
internal class AndroidNetworkDefaultCallbackAccess(
    private val connectivity: ConnectivityManager,
) : NetworkDefaultCallbackAccess {
    private val lock = Any()
    private var active: Registration? = null

    override fun register(callback: NetworkDefaultCallback) {
        val thread = HandlerThread(THREAD_NAME).apply { start() }
        val platform = object : ConnectivityManager.NetworkCallback() {
            override fun onAvailable(network: Network) {
                callback.onAvailable(network.networkHandle.toString())
            }

            override fun onCapabilitiesChanged(network: Network, capabilities: NetworkCapabilities) {
                callback.onCapabilitiesChanged(
                    network.networkHandle.toString(),
                    networkTransport(capabilities),
                )
            }

            override fun onLost(network: Network) {
                callback.onLost(network.networkHandle.toString())
            }
        }
        val registration = Registration(callback, platform, thread)
        synchronized(lock) {
            if (active != null) {
                thread.quit()
                throw AndroidExecutionException("ALREADY_EXISTS")
            }
            active = registration
        }
        try {
            connectivity.registerDefaultNetworkCallback(platform, Handler(thread.looper))
        } catch (error: RuntimeException) {
            synchronized(lock) {
                if (active === registration) active = null
            }
            thread.quit()
            throw error
        }
    }

    override fun unregister(callback: NetworkDefaultCallback) {
        val registration = synchronized(lock) {
            val current = active ?: throw AndroidExecutionException("STALE_AUTHORITY")
            if (current.callback !== callback) throw AndroidExecutionException("STALE_AUTHORITY")
            current
        }
        connectivity.unregisterNetworkCallback(registration.platform)
        synchronized(lock) {
            if (active === registration) active = null
        }
        registration.thread.quit()
    }

    private data class Registration(
        val callback: NetworkDefaultCallback,
        val platform: ConnectivityManager.NetworkCallback,
        val thread: HandlerThread,
    )

    private companion object {
        const val THREAD_NAME = "DroidBridge-network-default"
    }
}
