package com.pd2launcher.app

import android.content.Context
import android.net.ConnectivityManager
import android.net.LinkProperties
import android.net.Network
import java.io.File
import java.net.Inet6Address
import java.net.InetAddress
import java.nio.file.Files
import java.nio.file.StandardOpenOption

/**
 * The userland's `resolv.conf`: the default network's DNS servers, written when the session
 * starts, again whenever they change, and again at every later session start ([rewrite]): an
 * unpack puts back the packed file, which names no server. The userland's glibc reads it from
 * `<userland>/etc` (its SYSCONFDIR patch, userland/glibc/patches); Android itself has none. The
 * same file goes to every one of [targets] whose folder exists. The ARM64 Wine's Unix side uses the
 * same glibc, so Wine resolves names through this file too. When Android gives the network no DNS
 * server (issue #2 of the Android app, a Pixel 8a on Android 17), the file names public ones
 * ([FALLBACK]): a file with none makes glibc ask 127.0.0.1, where nothing answers, so every
 * download fails.
 * [emptyForTest] (debug builds) takes Android's servers as none, to check that on a device.
 */
class Dns(private val targets: List<File>, private val log: (String) -> Unit, private val emptyForTest: Boolean = false) {
    private var callback: ConnectivityManager.NetworkCallback? = null
    /** The servers Android last gave; null before its first callback. */
    private var servers: List<InetAddress>? = null
    /** Whether the next write logs its servers also when the file has them already: once each session start. */
    private var announce = true

    /** Follows the default network until [stop]; Android calls back at once with the current one. */
    fun start(context: Context) {
        if (callback != null) return
        if (emptyForTest) log("resolv.conf: debug, Android's DNS servers taken as none")
        val manager = context.getSystemService(ConnectivityManager::class.java)
        val follow = object : ConnectivityManager.NetworkCallback() {
            override fun onLinkPropertiesChanged(network: Network, properties: LinkProperties) {
                onServers(properties.dnsServers)
            }
        }
        manager.registerDefaultNetworkCallback(follow)
        callback = follow
    }

    fun stop(context: Context) {
        val follow = callback ?: return
        callback = null
        context.getSystemService(ConnectivityManager::class.java).unregisterNetworkCallback(follow)
    }

    /** The default network's servers, as Android gives them; [emptyForTest] takes them as none. */
    internal fun onServers(servers: List<InetAddress>) = write(if (emptyForTest) emptyList() else servers)

    /** Writes the servers Android last gave again, to every target that no longer has them, and logs them. */
    @Synchronized
    fun rewrite() {
        announce = true
        servers?.let(::write)
    }

    /**
     * Writes `resolv.conf` for [servers] to every target whose folder exists and that does not have
     * it, and logs each one written; once each session start ([announce]), also each one that has it.
     */
    @Synchronized
    private fun write(servers: List<InetAddress>) {
        this.servers = servers
        val text = render(servers)
        val announced = announce
        announce = false
        for (target in targets) {
            if (target.parentFile?.isDirectory != true) continue
            if (target.isFile && target.readText() == text) {
                if (announced) log("resolv.conf ${target.path}: ${describe(servers)} (unchanged)")
                continue
            }
            try {
                val staging = File(target.parentFile, "${target.name}.tmp")
                // Created anew: a link left at its name goes, and is never written through.
                Files.deleteIfExists(staging.toPath())
                Files.write(staging.toPath(), text.toByteArray(), StandardOpenOption.CREATE_NEW, StandardOpenOption.WRITE)
                if (!staging.renameTo(target)) throw java.io.IOException("rename failed")
                log("resolv.conf ${target.path}: ${describe(servers)}")
            } catch (error: Exception) {
                log("resolv.conf ${target.path} not written: $error")
            }
        }
    }

    companion object {
        /** glibc reads at most three servers (`MAXNS`). */
        private const val MAX_SERVERS = 3

        /** Public DNS servers for a network that names none, as addresses: they need no lookup. */
        val FALLBACK: List<InetAddress> = listOf("1.1.1.1", "8.8.8.8", "2606:4700:4700::1111").map(InetAddress::getByName)

        /**
         * `resolv.conf` for [servers], in order, or for [FALLBACK] when there are none: never one
         * without a server. IPv6 link-local ones keep their interface.
         */
        fun render(servers: List<InetAddress>): String = buildString {
            if (servers.isEmpty()) {
                append("# Written by the app: Android gave no DNS server, so public ones.\n")
            } else {
                append("# Written by the app from Android's DNS servers.\n")
            }
            for (server in servers.ifEmpty { FALLBACK }.take(MAX_SERVERS)) append("nameserver ").append(address(server)).append('\n')
        }

        /** The log's words for [servers]: Android's, or the public ones when Android gave none. */
        fun describe(servers: List<InetAddress>): String =
            if (servers.isEmpty()) "none from Android, public ${addresses(FALLBACK)}" else addresses(servers)

        private fun addresses(servers: List<InetAddress>) = servers.take(MAX_SERVERS).joinToString(" ") { address(it) }

        private fun address(server: InetAddress): String {
            val text = server.hostAddress ?: ""
            // Java names the scope by number when it has no interface; glibc takes either.
            return if (server is Inet6Address && !server.isLinkLocalAddress) text.substringBefore('%') else text
        }
    }
}
