package com.pd2launcher.app

import android.content.Context
import android.net.ConnectivityManager
import android.net.LinkProperties
import android.net.Network
import android.net.NetworkCapabilities
import android.net.TrafficStats
import android.net.wifi.WifiInfo
import android.net.wifi.WifiManager
import android.os.Build
import android.os.Process
import java.net.Inet4Address
import java.net.Inet6Address
import java.net.InetAddress
import java.util.Locale

/**
 * What the phone's network is like, for Detailed logging (issue #17: a join that fails on one
 * player's phone and works on ours may come from the phone's network, its ISP or its Wi-Fi). Read
 * with the app's own permissions only (ACCESS_NETWORK_STATE): a value that needs another
 * permission is "not readable". Never an address, a network name (SSID) or an access point
 * (BSSID): of an IPv4 address only its class ([addressClass]), of IPv6 only whether it is there.
 */
data class NetworkFacts(
    val transport: String,
    val validated: Boolean,
    val metered: Boolean,
    val vpn: Boolean,
    val captivePortal: Boolean,
    /** off, opportunistic (automatic) or strict (a named server); null: not readable. */
    val privateDns: String?,
    val mtu: Int?,
    val dnsServers: Int,
    val routes: Int,
    /** Whether the link has an IPv6 address that is not link-local. */
    val ipv6: Boolean,
    /** Whether the network has a NAT64 prefix (IPv4 over IPv6; Android 11+). */
    val nat64: Boolean?,
    /** Whether a CLAT interface (`v4-...`, IPv4 over IPv6) is up. */
    val clat: Boolean,
    /** The class of each IPv4 address of the link, in order; empty: the link has none of its own. */
    val ipv4Classes: List<String>,
    val downKbps: Int?,
    val upKbps: Int?,
    val signalStrength: Int?,
    val wifi: WifiFacts?,
) {
    /** The line's text after its head: `Wi-Fi, validated, not metered, ...`. */
    fun text(): String {
        val parts = mutableListOf(transport)
        parts += if (validated) "validated" else "NOT validated"
        parts += if (metered) "metered" else "not metered"
        if (vpn) parts += "VPN"
        if (captivePortal) parts += "captive portal"
        parts += "private DNS ${privateDns ?: NOT_READABLE}"
        parts += "MTU ${mtu?.takeIf { it > 0 } ?: "not set"}"
        parts += "$dnsServers DNS servers"
        parts += "$routes routes"
        parts += "IPv6 ${yes(ipv6)}"
        parts += "NAT64 ${nat64?.let(::yes) ?: NOT_READABLE}"
        parts += "CLAT ${yes(clat)}"
        parts += if (ipv4Classes.isEmpty()) "own IPv4 no" else "own IPv4 ${ipv4Classes.joinToString("+")}"
        parts += "bandwidth estimate down ${downKbps ?: "?"} kbps up ${upKbps ?: "?"} kbps"
        signalStrength?.takeIf { it != SIGNAL_UNSPECIFIED }?.let { parts += "signal $it" }
        if (transport == WIFI) parts += wifi?.text() ?: "Wi-Fi details $NOT_READABLE"
        return parts.joinToString(", ")
    }

    /** What changed from [before] to this, as `MTU 1500 -> 1280`, or null when nothing did. */
    fun changesFrom(before: NetworkFacts): String? {
        val changes = mutableListOf<String>()
        fun change(name: String, old: Any?, new: Any?) {
            if (old != new) changes += "$name ${old ?: "?"} -> ${new ?: "?"}"
        }
        change("transport", before.transport, transport)
        change("validated", yes(before.validated), yes(validated))
        change("metered", yes(before.metered), yes(metered))
        change("VPN", yes(before.vpn), yes(vpn))
        change("captive portal", yes(before.captivePortal), yes(captivePortal))
        change("private DNS", before.privateDns, privateDns)
        change("MTU", before.mtu, mtu)
        change("DNS servers", before.dnsServers, dnsServers)
        change("routes", before.routes, routes)
        change("IPv6", yes(before.ipv6), yes(ipv6))
        change("NAT64", before.nat64?.let(::yes), nat64?.let(::yes))
        change("CLAT", yes(before.clat), yes(clat))
        change("own IPv4", before.ipv4Classes.joinToString("+").ifEmpty { "no" }, ipv4Classes.joinToString("+").ifEmpty { "no" })
        change("down kbps", before.downKbps, downKbps)
        change("up kbps", before.upKbps, upKbps)
        change("signal", before.signalStrength, signalStrength)
        change("Wi-Fi", before.wifi?.text(), wifi?.text())
        return changes.takeIf { it.isNotEmpty() }?.joinToString(", ")
    }

    companion object {
        const val WIFI = "Wi-Fi"
        const val NOT_READABLE = "not readable"
        /** NetworkCapabilities.SIGNAL_STRENGTH_UNSPECIFIED. */
        private const val SIGNAL_UNSPECIFIED = Int.MIN_VALUE

        private fun yes(value: Boolean) = if (value) "yes" else "no"

        /**
         * The class of an IPv4 address ([octets], 4 bytes), never the address: private (10/8,
         * 172.16/12, 192.168/16), shared (CGNAT, 100.64/10), link-local (169.254/16), loopback,
         * IETF (192.0.0/24, where a CLAT's address is), or public.
         */
        fun addressClass(octets: ByteArray): String {
            if (octets.size != 4) return "not IPv4"
            val a = octets[0].toInt() and 0xFF
            val b = octets[1].toInt() and 0xFF
            val c = octets[2].toInt() and 0xFF
            return when {
                a == 10 || (a == 172 && b in 16..31) || (a == 192 && b == 168) -> "private"
                a == 100 && b in 64..127 -> "shared (CGNAT)"
                a == 169 && b == 254 -> "link-local"
                a == 127 -> "loopback"
                a == 192 && b == 0 && c == 0 -> "IETF 192.0.0.0/24"
                else -> "public"
            }
        }

        /** Whether an interface with this [name] is a CLAT (Android's IPv4-over-IPv6 link: `v4-wlan0`, `v4-rmnet_data0`). */
        fun isClat(name: String): Boolean = name.startsWith("v4-")

        /** The transport of [capabilities]: Wi-Fi, cellular, Ethernet, VPN, other. */
        fun transport(has: (Int) -> Boolean): String = when {
            has(NetworkCapabilities.TRANSPORT_WIFI) -> WIFI
            has(NetworkCapabilities.TRANSPORT_CELLULAR) -> "cellular"
            has(NetworkCapabilities.TRANSPORT_ETHERNET) -> "Ethernet"
            has(NetworkCapabilities.TRANSPORT_VPN) -> "VPN"
            else -> "other"
        }

        /** The facts of the default network now; null when the phone has none. */
        fun ofDefault(context: Context): NetworkFacts? {
            val manager = context.getSystemService(ConnectivityManager::class.java) ?: return null
            val network = manager.activeNetwork ?: return null
            return of(manager, network, null, null, context)
        }

        /**
         * The facts of [network], from [capabilities] and [properties] when a callback gives them
         * (else read now). Each read that fails leaves its value unknown.
         */
        fun of(manager: ConnectivityManager, network: Network, capabilities: NetworkCapabilities?, properties: LinkProperties?, context: Context?): NetworkFacts? {
            val caps = capabilities ?: runCatching { manager.getNetworkCapabilities(network) }.getOrNull() ?: return null
            val link = properties ?: runCatching { manager.getLinkProperties(network) }.getOrNull()
            return of(caps, link, clatUp(), context)
        }

        fun of(caps: NetworkCapabilities, link: LinkProperties?, clatUp: Boolean, context: Context?): NetworkFacts {
            val addresses = link?.linkAddresses.orEmpty().map { it.address }
            return NetworkFacts(
                transport = transport(caps::hasTransport),
                validated = caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED),
                metered = !caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_NOT_METERED),
                vpn = caps.hasTransport(NetworkCapabilities.TRANSPORT_VPN) || !caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_NOT_VPN),
                captivePortal = caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_CAPTIVE_PORTAL),
                privateDns = link?.let {
                    when {
                        !it.isPrivateDnsActive -> "off"
                        it.privateDnsServerName != null -> "strict"
                        else -> "opportunistic"
                    }
                },
                mtu = link?.mtu,
                dnsServers = link?.dnsServers?.size ?: 0,
                routes = link?.routes?.size ?: 0,
                ipv6 = addresses.any { it is Inet6Address && !it.isLinkLocalAddress },
                nat64 = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) link?.let { it.nat64Prefix != null } else null,
                clat = clatUp || link?.interfaceName?.let(::isClat) == true,
                ipv4Classes = addresses.filterIsInstance<Inet4Address>().map { addressClass(it.address) },
                downKbps = caps.linkDownstreamBandwidthKbps,
                upKbps = caps.linkUpstreamBandwidthKbps,
                signalStrength = caps.signalStrength,
                wifi = WifiFacts.of(caps, context),
            )
        }

        /** Whether an interface of the phone is a CLAT ([isClat]); false when the list is not readable. */
        fun clatUp(): Boolean = runCatching {
            java.net.NetworkInterface.getNetworkInterfaces()?.toList().orEmpty().any { it.isUp && isClat(it.name) }
        }.getOrDefault(false)

        /** The classes of [servers] (DNS servers): IPv4 by class, IPv6 as one word. Never an address. */
        fun classes(servers: List<InetAddress>): String =
            servers.joinToString("+") { if (it is Inet4Address) addressClass(it.address) else "IPv6" }.ifEmpty { "none" }
    }
}

/**
 * The Wi-Fi link: its signal (RSSI, dBm), link speed (Mbps), frequency (MHz) and band, and Wi-Fi
 * standard (Android 11+). From WifiManager's connection info (ACCESS_WIFI_STATE), else from the
 * network's capabilities (Android 12+); never the network's name (SSID), the access point (BSSID)
 * or a MAC address. Null: not readable.
 */
data class WifiFacts(val rssi: Int, val linkMbps: Int, val frequencyMhz: Int, val standard: Int?) {
    fun text(): String {
        val parts = mutableListOf("RSSI $rssi dBm", "link $linkMbps Mbps", "$frequencyMhz MHz ${band(frequencyMhz)}")
        standard?.let { parts += standardName(it) }
        return parts.joinToString(", ")
    }

    /** The signal and the link speed, which a sample looks at. */
    fun signal(): String = "RSSI $rssi dBm, link $linkMbps Mbps"

    companion object {
        fun band(mhz: Int): String = when (mhz) {
            in 2400..2500 -> "(2.4 GHz)"
            in 4900..5900 -> "(5 GHz)"
            in 5925..7125 -> "(6 GHz)"
            in 57000..71000 -> "(60 GHz)"
            else -> "(band unknown)"
        }

        /** ScanResult.WIFI_STANDARD_* as a name. */
        fun standardName(standard: Int): String = when (standard) {
            1 -> "802.11a/b/g"
            4 -> "802.11n (Wi-Fi 4)"
            5 -> "802.11ac (Wi-Fi 5)"
            6 -> "802.11ax (Wi-Fi 6)"
            7 -> "802.11ad"
            8 -> "802.11be (Wi-Fi 7)"
            else -> "standard unknown"
        }

        /** The Wi-Fi link of the default network now; null: not Wi-Fi, or not readable. */
        fun ofDefault(context: Context): WifiFacts? {
            val manager = context.getSystemService(ConnectivityManager::class.java) ?: return null
            val caps = runCatching { manager.activeNetwork?.let(manager::getNetworkCapabilities) }.getOrNull() ?: return null
            return of(caps, context)
        }

        /** The Wi-Fi link of a network with [caps]: WifiManager's first ([context]), else the capabilities'. */
        fun of(caps: NetworkCapabilities, context: Context?): WifiFacts? {
            if (!caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI)) return null
            val wifi = context?.applicationContext?.getSystemService(WifiManager::class.java)
            @Suppress("DEPRECATION")
            val fromManager = runCatching { wifi?.connectionInfo }.getOrNull()?.takeIf { it.rssi > INVALID_RSSI && it.frequency > 0 }
            val fromCaps = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) caps.transportInfo as? WifiInfo else null
            val info = fromManager ?: fromCaps ?: return null
            val standard = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) info.wifiStandard else null
            return WifiFacts(info.rssi, info.linkSpeed, info.frequency, standard)
        }

        /** WifiManager's RSSI when it has none (WifiInfo.INVALID_RSSI). */
        private const val INVALID_RSSI = -127
    }
}

/**
 * The app's network traffic so far (the game's too: it runs under the app's user ID), from
 * [TrafficStats] for the app's user; null: Android does not count it (UNSUPPORTED).
 */
data class Traffic(val rxBytes: Long, val txBytes: Long, val rxPackets: Long, val txPackets: Long) {
    operator fun minus(before: Traffic) =
        Traffic(rxBytes - before.rxBytes, txBytes - before.txBytes, rxPackets - before.rxPackets, txPackets - before.txPackets)

    /** `in 1200 B 4 packets, out 300 B 3 packets`. */
    fun text(): String = String.format(Locale.ROOT, "in %d B %d packets, out %d B %d packets", rxBytes, rxPackets, txBytes, txPackets)

    companion object {
        /** The counts now; null when one is UNSUPPORTED. */
        fun now(uid: Int = Process.myUid()): Traffic? = of(
            TrafficStats.getUidRxBytes(uid),
            TrafficStats.getUidTxBytes(uid),
            TrafficStats.getUidRxPackets(uid),
            TrafficStats.getUidTxPackets(uid),
        )

        fun of(rxBytes: Long, txBytes: Long, rxPackets: Long, txPackets: Long): Traffic? =
            if (listOf(rxBytes, txBytes, rxPackets, txPackets).any { it == TrafficStats.UNSUPPORTED.toLong() || it < 0 }) {
                null
            } else {
                Traffic(rxBytes, txBytes, rxPackets, txPackets)
            }

        /** The traffic between [before] and [now], as a line's text. */
        fun change(before: Traffic?, now: Traffic?): String = when {
            now == null -> "traffic not counted (UNSUPPORTED)"
            before == null -> "traffic from the next sample"
            else -> "traffic " + (now - before).text()
        }
    }
}
