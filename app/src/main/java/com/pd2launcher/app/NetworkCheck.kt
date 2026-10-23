package com.pd2launcher.app

import android.content.Context
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import android.net.NetworkInfo
import android.system.ErrnoException
import java.io.IOException
import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.Socket
import java.net.SocketTimeoutException
import java.net.UnknownHostException
import java.util.concurrent.ExecutionException
import java.util.concurrent.FutureTask
import java.util.concurrent.TimeUnit
import java.util.concurrent.TimeoutException

/**
 * Whether this app can reach the servers its downloads need, and if not, why, in a form the
 * screens can act on (Check, and under a Download or Install that failed on the network): Android
 * or the phone's own per-app setting blocks the app's network, there is no network, names cannot
 * be looked up, or a server cannot be reached from this network (Blizzard's from some regions).
 * A block shows first: one empty UDP datagram to the network's own DNS server, which a phone that
 * blocks the app refuses at once (EPERM), while its TCP connections only time out. Then for each
 * server it looks up the name and opens a TCP connection to port 443 within [TIMEOUT_MS], sending
 * nothing over it. Blizzard's own downloader runs under Wine with the same network as the app, so
 * a block found here is one it meets too.
 */
object NetworkCheck {
    /** Where the engine fetches Blizzard's downloaders. */
    const val BLIZZARD = "downloader.battle.net"
    /** Where the engine fetches Project Diablo 2's files. */
    const val PD2 = "pd2-client-files.projectdiablo2.com"
    /** Where the block test's datagram goes when the network names no DNS server of its own. */
    const val LITERAL = "1.1.1.1"
    const val TIMEOUT_MS = 5_000

    /** How one connection attempt ended. */
    enum class Outcome { OK, BLOCKED, DNS, TIMEOUT, REFUSED, UNREACHABLE, FAILED }

    /** What the screens say. */
    enum class Result { OK, BLOCKED, NO_NETWORK, DNS, BLIZZARD_UNREACHABLE, PD2_UNREACHABLE, UNREACHABLE }

    /**
     * What Android says of the default network, for the log only: its [transports] (`other` for
     * none of the named ones, `unknown` when Android gave none), how many DNS servers it names, and
     * the Private DNS mode ([privateDns]). Never a server's address or the Private DNS server's name.
     */
    data class Link(val transports: List<String>, val dnsServers: Int, val privateDns: String) {
        override fun toString(): String =
            "${transports.ifEmpty { listOf("other") }.joinToString("+")}, DNS servers $dnsServers, private DNS $privateDns"
    }

    /**
     * One check: its [result], the block test's [send], how each server answered, and the default
     * network's [link], for the log.
     */
    data class Report(
        val result: Result,
        val send: Outcome,
        val blizzard: Outcome?,
        val pd2: Outcome?,
        val network: Boolean,
        val link: Link? = null,
    ) {
        fun describe(): String =
            "network check: $result (network ${if (network) "yes" else "none"}" + (link?.let { ", $it" } ?: "") +
                ", send $send" + (blizzard?.let { ", $BLIZZARD $it" } ?: "") + (pd2?.let { ", $PD2 $it" } ?: "") + ")"
    }

    /** The kinds of network the log names, in its order. */
    private val TRANSPORTS = listOf(
        NetworkCapabilities.TRANSPORT_WIFI to "wifi",
        NetworkCapabilities.TRANSPORT_CELLULAR to "cellular",
        NetworkCapabilities.TRANSPORT_VPN to "vpn",
        NetworkCapabilities.TRANSPORT_ETHERNET to "ethernet",
    )

    /** The names of the kinds of network [has] says the default network is. */
    fun transports(has: (Int) -> Boolean): List<String> = TRANSPORTS.filter { has(it.first) }.map { it.second }

    /**
     * The Private DNS mode: strict when the player named a server ([named]), auto when Android uses
     * Private DNS without one ([active]), else off: Android does not use Private DNS on this network
     * now (the setting is Off, or Automatic and no server answers it).
     */
    fun privateDns(active: Boolean, named: Boolean): String = when {
        named -> "strict"
        active -> "auto"
        else -> "off"
    }

    // Linux's numbers, which Android's are.
    private const val EPERM = 1
    private const val EACCES = 13
    private const val ENETUNREACH = 101
    private const val ECONNREFUSED = 111
    private const val EHOSTUNREACH = 113

    /**
     * How a failed attempt ended: EPERM or EACCES from the system is a block on this app (Android's
     * or the phone's per-app network setting), never the network's.
     */
    fun outcome(error: Throwable): Outcome {
        val chain = generateSequence(error) { it.cause }.toList()
        val errno = chain.filterIsInstance<ErrnoException>().firstOrNull()?.errno
        val text = chain.mapNotNull { it.message }.joinToString(" ")
        fun means(number: Int, name: String) = errno == number || name in text
        return when {
            chain.any { it is SocketTimeoutException } -> Outcome.TIMEOUT
            means(EPERM, "EPERM") || means(EACCES, "EACCES") -> Outcome.BLOCKED
            means(ECONNREFUSED, "ECONNREFUSED") -> Outcome.REFUSED
            means(ENETUNREACH, "ENETUNREACH") || means(EHOSTUNREACH, "EHOSTUNREACH") -> Outcome.UNREACHABLE
            error is UnknownHostException -> Outcome.DNS
            else -> Outcome.FAILED
        }
    }

    /**
     * The verdict: a block first (Android's own word, the block test's refused datagram, or any
     * attempt the system refused), then no network, then which server could not be reached.
     */
    fun result(network: Boolean, blockedByAndroid: Boolean, send: Outcome, blizzard: Outcome, pd2: Outcome): Result = when {
        blockedByAndroid || Outcome.BLOCKED in listOf(send, blizzard, pd2) -> Result.BLOCKED
        !network -> Result.NO_NETWORK
        blizzard == Outcome.OK && pd2 == Outcome.OK -> Result.OK
        blizzard == Outcome.DNS && pd2 == Outcome.DNS -> Result.DNS
        pd2 == Outcome.OK -> Result.BLIZZARD_UNREACHABLE
        blizzard == Outcome.OK -> Result.PD2_UNREACHABLE
        else -> Result.UNREACHABLE
    }

    /** Runs the check; each step takes [TIMEOUT_MS] at most (about 20 s in all), so never on the main thread. */
    fun run(context: Context): Report {
        val connectivity = context.getSystemService(ConnectivityManager::class.java)
        val active = connectivity.activeNetwork
        val network = active != null
        // Android marks the network BLOCKED for an app it keeps off it (data saver, a restricted app).
        @Suppress("DEPRECATION")
        val blocked = connectivity.activeNetworkInfo?.detailedState == NetworkInfo.DetailedState.BLOCKED
        val properties = active?.let { connectivity.getLinkProperties(it) }
        val capabilities = active?.let { connectivity.getNetworkCapabilities(it) }
        val link = properties?.let {
            Link(
                capabilities?.let { known -> transports(known::hasTransport) } ?: listOf("unknown"),
                it.dnsServers.size,
                privateDns(it.isPrivateDnsActive, it.privateDnsServerName != null),
            )
        }
        val dns = properties?.dnsServers?.firstOrNull()
        val send = send(dns ?: InetAddress.getByName(LITERAL))
        // Blocked: the servers would only time out, so they are not tried.
        if (blocked || send == Outcome.BLOCKED) return Report(Result.BLOCKED, send, null, null, network, link)
        val blizzard = probe(BLIZZARD)
        val pd2 = probe(PD2)
        return Report(result(network, false, send, blizzard, pd2), send, blizzard, pd2, network, link)
    }

    /** One empty datagram to [address]'s DNS port: refused at once (EPERM) when the app is blocked. */
    private fun send(address: InetAddress): Outcome = try {
        DatagramSocket().use { it.send(DatagramPacket(ByteArray(0), 0, address, 53)) }
        Outcome.OK
    } catch (error: IOException) {
        outcome(error)
    } catch (_: SecurityException) {
        Outcome.BLOCKED
    }

    private fun probe(host: String): Outcome {
        val addresses = try {
            lookup(host)
        } catch (error: UnknownHostException) {
            return outcome(error).takeIf { it == Outcome.BLOCKED } ?: Outcome.DNS
        } catch (_: TimeoutException) {
            return Outcome.DNS
        } catch (_: SecurityException) {
            return Outcome.BLOCKED
        }
        var last = Outcome.FAILED
        for (address in addresses.take(2)) {
            last = connect(address)
            if (last == Outcome.OK || last == Outcome.BLOCKED) break
        }
        return last
    }

    /** [host]'s addresses within [TIMEOUT_MS]: Android's lookup has no limit of its own. */
    private fun lookup(host: String): Array<InetAddress> {
        val task = FutureTask { InetAddress.getAllByName(host) }
        Thread(task, "pd2-lookup").apply { isDaemon = true }.start()
        return try {
            task.get(TIMEOUT_MS.toLong(), TimeUnit.MILLISECONDS)
        } catch (error: ExecutionException) {
            throw error.cause ?: error
        }
    }

    private fun connect(address: InetAddress): Outcome = try {
        Socket().use { it.connect(InetSocketAddress(address, 443), TIMEOUT_MS) }
        Outcome.OK
    } catch (error: IOException) {
        outcome(error)
    } catch (_: SecurityException) {
        Outcome.BLOCKED
    }

    /** The failure codes a network problem can cause, under which the check's word is shown. */
    val NETWORK_FAILURES = setOf(
        "download-failed", "client-download-failed", "installer-download-failed", "installer-download-stalled",
        "downloads-required",
    )
}
