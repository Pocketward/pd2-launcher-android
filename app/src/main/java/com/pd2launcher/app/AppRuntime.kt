package com.pd2launcher.app

import android.content.Context
import android.content.res.AssetManager
import android.system.Os
import com.github.luben.zstd.ZstdInputStreamNoFinalizer
import com.pd2launcher.app.control.Game
import com.pd2launcher.app.control.Installation
import com.pd2launcher.app.control.Snapshot
import java.io.BufferedInputStream
import java.io.File
import java.io.FileOutputStream
import java.io.InputStream
import java.nio.file.Files
import java.nio.file.LinkOption
import java.nio.file.Paths
import java.nio.file.attribute.PosixFilePermissions

/**
 * The runtime the app provides (runtime/pack.sh): FEX's RootFS with its App-provided
 * FEX record, Wine-GE with the Launcher's receipt, and Wine Gecko and Mono in the Launcher's
 * download cache, unpacked once into the Launcher's managed root [managedRoot], so the engine
 * finds them installed and downloads none of them. A marker naming the version is written last;
 * a newer version replaces the trees the older one unpacked.
 */
class AppRuntime(
    private val managedRoot: File,
    private val log: (String) -> Unit,
    /** Sets a mode with its sticky bit (the RootFS's /tmp is 1777); Java's API cannot. */
    private val chmod: (File, Int) -> Unit = { file, mode -> Os.chmod(file.path, mode) },
) {
    private val marker = File(managedRoot, MARKER)

    fun isCurrent(version: String): Boolean = marker.isFile && marker.readText() == version

    /** Unpacks [tar] (an uncompressed tar stream); [progress] gets the tar bytes read so far. */
    fun install(version: String, tar: InputStream, progress: (Long) -> Unit = {}) {
        ownerOnly(managedRoot)
        marker.delete()
        // What an older version (or a stopped unpack) left.
        for (path in PROVIDED) UserlandInstaller.deleteTree(File(managedRoot, path).toPath())
        val reader = TarReader(tar)
        val directories = ArrayList<Pair<File, Int>>()
        val buffer = ByteArray(1 shl 18)
        var read = 0L
        while (true) {
            val entry = reader.next() ?: break
            val path = entry.name.removePrefix("./").trimEnd('/')
            if (path.isEmpty()) continue
            if (path.startsWith("/") || path.split('/').any { it == "" || it == "." || it == ".." }) {
                throw CorruptUserland("unsafe path in the runtime: ${entry.name}")
            }
            val out = File(managedRoot, path)
            out.parentFile?.let { if (!it.isDirectory) Files.createDirectories(it.toPath()) }
            when (entry.type) {
                TarReader.DIRECTORY -> {
                    if (!out.isDirectory) Files.createDirectory(out.toPath())
                    directories += out to entry.mode
                }
                TarReader.FILE, TarReader.FILE_OLD -> {
                    FileOutputStream(out).use { file ->
                        while (true) {
                            val n = reader.read(buffer)
                            if (n < 0) break
                            file.write(buffer, 0, n)
                        }
                    }
                    chmod(out, entry.mode and 0x1ff)
                    read += entry.size
                    progress(read)
                }
                TarReader.SYMLINK -> {
                    Files.deleteIfExists(out.toPath())
                    Files.createSymbolicLink(out.toPath(), Paths.get(entry.linkName))
                }
                else -> throw CorruptUserland("unsupported tar entry type '${entry.type}' in the runtime: $path")
            }
        }
        // Deepest first, so a read-only folder does not block its contents; at least 0700.
        for ((dir, mode) in directories.asReversed()) {
            if (!Files.isSymbolicLink(dir.toPath())) chmod(dir, (mode and 0x3ff) or 0x1c0)
        }
        marker.writeText(version)
        log("app runtime $version unpacked")
    }

    /**
     * Once the game is installed, the runtime's archives in the download cache ([ARCHIVES]) are
     * only a second copy of what is unpacked: they go, with an older Launcher's Wine-GE archive.
     * The names removed.
     */
    fun removeArchives(): List<String> = (ARCHIVES + LEGACY_WINE_ARCHIVE).filter { File(managedRoot, it).delete() }

    /** Whether an install that sets the game up again would miss one of [ARCHIVES]. */
    fun archivesMissing(): Boolean = ARCHIVES.any { !File(managedRoot, it).isFile }

    /**
     * Lays [ARCHIVES] out again from the runtime's [tar], skipping everything else; pack.sh sorts
     * the tar by name, so `cache/` comes first and the rest is not read. The names restored.
     */
    fun restoreArchives(tar: InputStream): List<String> {
        val reader = TarReader(tar)
        val buffer = ByteArray(1 shl 18)
        val restored = ArrayList<String>()
        while (restored.size < ARCHIVES.size) {
            val entry = reader.next() ?: break
            val path = entry.name.removePrefix("./")
            if (path !in ARCHIVES || (entry.type != TarReader.FILE && entry.type != TarReader.FILE_OLD)) continue
            val out = File(managedRoot, path)
            out.parentFile?.let { Files.createDirectories(it.toPath()) }
            try {
                FileOutputStream(out).use { file ->
                    while (true) {
                        val n = reader.read(buffer)
                        if (n < 0) break
                        file.write(buffer, 0, n)
                    }
                }
            } catch (error: Exception) {
                out.delete()
                throw error
            }
            chmod(out, entry.mode and 0x1ff)
            restored += path
        }
        return restored
    }

    private fun ownerOnly(dir: File) {
        if (!Files.isDirectory(dir.toPath(), LinkOption.NOFOLLOW_LINKS)) {
            Files.createDirectories(dir.toPath(), PosixFilePermissions.asFileAttribute(PosixFilePermissions.fromString("rwx------")))
        }
    }

    /** What the archive copies and the Windows Installer cache need ([sweep]). */
    enum class Sweep {
        /** The installation is Ready: the copies and the Installer cache go. */
        REMOVE,

        /** The installation needs setting up (again): the copies come back from the APK if missing. */
        RESTORE,
    }

    companion object {
        const val MARKER = ".pd2-app-runtime"
        /** The trees an unpack replaces (pack.sh's layout); the Launcher's own files stay. */
        private val PROVIDED = listOf(
            "fex",
            "runner/lutris-GE-Proton8-26-x86_64",
            // In the tar's order (pack.sh sorts it by name), as restoreArchives reads them.
            "cache/artifacts/windowsdesktop-runtime-6.0.36-win-x86.verified",
            "cache/artifacts/wine-gecko-2.47.4-x86.verified",
            "cache/artifacts/wine-mono-11.3.0-x86.verified",
        )

        /**
         * The .NET Desktop Runtime installer, Wine Gecko and Mono: only an install that sets the
         * game up (again) reads them.
         */
        val ARCHIVES = PROVIDED.filter { it.startsWith("cache/") }

        /**
         * What [snapshot]'s state needs: nothing before the engine's audit has ended (its
         * placeholder says needs_base_game), while a job or the game runs, or while the
         * installation is indeterminate.
         */
        private fun need(snapshot: Snapshot?): Sweep? = when {
            snapshot == null || !Onboarding.audited(snapshot) || snapshot.activeJob != null ||
                snapshot.game == Game.RUNNING || snapshot.installation == Installation.INDETERMINATE -> null
            snapshot.installation == Installation.READY -> Sweep.REMOVE
            else -> Sweep.RESTORE
        }

        /**
         * The sweep the engine's [next] snapshot calls for after [previous]: only as the
         * installation comes to stand Ready with nothing running, or in need of setting up with
         * nothing running, not on every snapshot.
         */
        fun sweep(previous: Snapshot?, next: Snapshot): Sweep? = need(next)?.takeIf { it != need(previous) }

        /** Wine-GE's archive, which an older Launcher downloaded; the APK carries Wine-GE unpacked. */
        const val LEGACY_WINE_ARCHIVE = "cache/artifacts/wine-ge-8-26.verified"

        private const val ARCHIVE = "runtime/runtime.tar.zst"
        private const val INFO = "runtime/runtime.info"

        /** The packed version and its tar size in bytes; null when the APK carries none. */
        fun info(context: Context): Pair<String, Long>? = try {
            val lines = context.assets.open(INFO).use { it.readBytes().decodeToString() }.lines()
            lines[0] to lines[1].toLong()
        } catch (_: Exception) {
            null
        }

        fun openTar(context: Context): InputStream {
            val compressed = BufferedInputStream(context.assets.open(ARCHIVE, AssetManager.ACCESS_STREAMING), 1 shl 20)
            return BufferedInputStream(ZstdInputStreamNoFinalizer(compressed), 1 shl 20)
        }
    }
}
