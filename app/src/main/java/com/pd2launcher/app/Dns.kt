package com.pd2launcher.app

import android.content.Context
import android.net.ConnectivityManager
import android.net.LinkProperties
import android.net.Network
import java.io.File
import java.net.Inet6Address
import java.net.InetAddress

/**
 * The userland's `resolv.conf`: the default network's DNS servers, written when the session
 * starts and again whenever they change. The userland's glibc reads it from `<userland>/etc`
 * (its SYSCONFDIR patch, userland/glibc/patches); Android itself has none. The same file goes to
 * every one of [targets] whose folder exists: the FEX RootFS's `/etc` too, where Wine's x86 glibc
 * reads it (FEX falls back to Android's `/etc`).
 */
class Dns(private val context: Context, private val targets: List<File>, private val log: (String) -> Unit) {
    private var callback: ConnectivityManager.NetworkCallback? = null

    /** Follows the default network until [stop]; Android calls back at once with the current one. */
    fun start() {
        if (callback != null) return
        val manager = context.getSystemService(ConnectivityManager::class.java)
        val follow = object : ConnectivityManager.NetworkCallback() {
            override fun onLinkPropertiesChanged(network: Network, properties: LinkProperties) {
                write(properties.dnsServers)
            }
        }
        manager.registerDefaultNetworkCallback(follow)
        callback = follow
    }

    fun stop() {
        val follow = callback ?: return
        callback = null
        context.getSystemService(ConnectivityManager::class.java).unregisterNetworkCallback(follow)
    }

    @Synchronized
    private fun write(servers: List<InetAddress>) {
        val text = render(servers)
        for (target in targets) {
            if (target.parentFile?.isDirectory != true) continue
            if (target.isFile && target.readText() == text) continue
            try {
                val staging = File(target.parentFile, "${target.name}.tmp")
                staging.writeText(text)
                if (!staging.renameTo(target)) throw java.io.IOException("rename failed")
                log("resolv.conf ${target.path}: ${servers.take(MAX_SERVERS).joinToString(" ") { address(it) }}")
            } catch (error: Exception) {
                log("resolv.conf ${target.path} not written: $error")
            }
        }
    }

    companion object {
        /** glibc reads at most three servers (`MAXNS`). */
        private const val MAX_SERVERS = 3

        /** `resolv.conf` for [servers], in order; IPv6 link-local ones keep their interface. */
        fun render(servers: List<InetAddress>): String = buildString {
            append("# Written by the app from Android's DNS servers.\n")
            for (server in servers.take(MAX_SERVERS)) append("nameserver ").append(address(server)).append('\n')
        }

        private fun address(server: InetAddress): String {
            val text = server.hostAddress ?: ""
            // Java names the scope by number when it has no interface; glibc takes either.
            return if (server is Inet6Address && !server.isLinkLocalAddress) text.substringBefore('%') else text
        }
    }
}
