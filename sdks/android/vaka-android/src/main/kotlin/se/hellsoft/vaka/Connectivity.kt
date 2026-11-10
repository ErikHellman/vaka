package se.hellsoft.vaka

import android.annotation.SuppressLint
import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.os.Build
import se.hellsoft.vaka.internal.Client

/** Breadcrumbs for connectivity changes (API 24+); also flushes the queue when the network returns. */
internal object Connectivity {
    @SuppressLint("MissingPermission")
    fun register(context: Context, client: Client) {
        if (Build.VERSION.SDK_INT < 24) return
        try {
            val cm = context.getSystemService(Context.CONNECTIVITY_SERVICE) as? ConnectivityManager ?: return
            cm.registerDefaultNetworkCallback(object : ConnectivityManager.NetworkCallback() {
                private var last: String? = null

                override fun onAvailable(network: Network) {
                    client.flush()
                }

                override fun onCapabilitiesChanged(network: Network, caps: NetworkCapabilities) {
                    val type = when {
                        caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) -> "wifi"
                        caps.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR) -> "cellular"
                        caps.hasTransport(NetworkCapabilities.TRANSPORT_ETHERNET) -> "ethernet"
                        else -> "other"
                    }
                    if (type != last) {
                        last = type
                        client.addBreadcrumb("network", "connected ($type)")
                    }
                }

                override fun onLost(network: Network) {
                    last = null
                    client.addBreadcrumb("network", "disconnected")
                }
            })
        } catch (e: Exception) {
            // SecurityException without ACCESS_NETWORK_STATE (merged from our manifest), or OEM bugs.
        }
    }
}
