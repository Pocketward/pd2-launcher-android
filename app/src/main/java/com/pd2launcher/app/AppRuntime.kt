package com.pd2launcher.app

import android.content.Context
import android.content.res.AssetManager
import android.system.Os
import com.github.luben.zstd.ZstdInputStreamNoFinalizer
import com.pd2launcher.app.control.Game
import com.pd2launcher.app.control.Installation
import com.pd2launcher.app.control.Snapshot
import java.io.BufferedInputStream
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.FileOutputStream
import java.io.InputStream
import java.nio.file.Files
import java.nio.file.LinkOption
import java.nio.file.Path
import java.nio.file.Paths
import java.nio.file.StandardCopyOption
import java.nio.file.attribute.PosixFilePermissions

/**
 * The runtime the app provides (runtime/build.sh): the ARM64 Wine 11.0 with Box64's
 * wowbox64.dll, its prefix template and the official launcher's x86 .NET runtime, unpacked once
 * into the Launcher's managed root [managedRoot] as the engine names them, so the engine finds
 * them installed and downloads none of them. The files that hold the data-folder placeholder
 * (the tar's first entry, `relocate.list`) get [dataDir] in its place as they are unpacked
 * ([Relocation]). A marker naming the version is written last; a newer version replaces the trees
 * the older one unpacked, and an older app's FEX runtime goes ([LEGACY]); a prefix that an older
 * runtime or another generation of the template made becomes a copy of this runtime's template,
 * with the game and its records ([LegacyPrefix]). When that fails, the older game folder goes to
 * [keptGame] for Import instead.
 */
class AppRuntime(
    private val managedRoot: File,
    private val dataDir: String,
    private val log: (String) -> Unit,
    /**
     * Where an older installation's game folder waits for Import when the update in place fails
     * ([SessionPaths.upgradeGame]: outside the managed root, which the engine refuses for Import).
     */
    private val keptGame: () -> File,
    /** The player's characters ([SessionPaths.userSave]): characters left in an older prefix go below it. */
    private val userSave: File,
    /** Sets a mode with its sticky bit; Java's API cannot. */
    private val chmod: (File, Int) -> Unit = { file, mode -> Os.chmod(file.path, mode) },
    /** Makes a folder's entries durable (fsync on the folder); the JVM tests pass their own. */
    private val syncDirectory: (Path) -> Unit = ::fsyncDirectory,
    /** Debug builds only: the update in place fails on purpose ([LegacyPrefix.failForTest]). */
    private val failUpdateForTest: Boolean = false,
    /** Debug builds only: it fails after the new prefix took its place ([LegacyPrefix.failLateForTest]). */
    private val failUpdateLateForTest: Boolean = false,
) {
    private val marker = File(managedRoot, MARKER)

    fun isCurrent(version: String): Boolean = marker.isFile && marker.readText() == version

    /**
     * Unpacks [tar] (an uncompressed tar stream); [progress] gets the tar bytes read so far. As
     * the App userland's unpack does, every file is synced, then every folder (deepest first),
     * and only then is the marker written (a temporary name, synced, renamed, the folder synced):
     * after a power cut the marker never names a version whose files did not all reach the disk.
     * A prefix that an older runtime or another template generation made ([LegacyPrefix.ours]) is
     * moved over to this one's template last, before the marker;
     * when that fails, its game folder waits for Import ([LegacyPrefix.setAside]), and that folder
     * is returned (null: the update in place passed, or there was nothing to update).
     */
    fun install(version: String, tar: InputStream, progress: (Long) -> Unit = {}): File? {
        ownerOnly(managedRoot)
        // Gone for good before any tree goes, or a power cut could bring it back over half a tree.
        marker.delete()
        syncDirectory(managedRoot.toPath())
        // What an older version (or a stopped unpack) left, and an older app's FEX runtime.
        for (path in PROVIDED + LEGACY) UserlandInstaller.deleteTree(File(managedRoot, path).toPath())
        val relocation = Relocation(PLACEHOLDER, dataDir)
        val reader = TarReader(tar)
        val directories = ArrayList<Pair<File, Int>>()
        val buffer = ByteArray(1 shl 18)
        var relocated = emptySet<String>()
        var read = 0L
        while (true) {
            val entry = reader.next() ?: break
            val path = entry.name.removePrefix("./").trimEnd('/')
            if (path.isEmpty()) continue
            if (path.startsWith("/") || path.split('/').any { it == "" || it == "." || it == ".." }) {
                throw CorruptUserland("unsafe path in the runtime: ${entry.name}")
            }
            if (path == RELOCATE_LIST) {
                relocated = String(readAll(reader, buffer)).lines()
                    .mapNotNull { line -> line.trim().takeIf { it.isNotEmpty() } }
                    .map { line ->
                        val (kind, file) = line.split(' ', limit = 2).takeIf { it.size == 2 }
                            ?: throw CorruptUserland("bad relocate.list line: $line")
                        if (kind != "binary") throw CorruptUserland("unsupported relocation '$kind' for $file")
                        file
                    }.toSet()
                continue
            }
            if (PROVIDED.none { path == it || path.startsWith("$it/") }) {
                throw CorruptUserland("the runtime has a path outside its trees: $path")
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
                        if (path in relocated) {
                            file.write(relocate(readAll(reader, buffer), relocation, path))
                        } else {
                            while (true) {
                                val n = reader.read(buffer)
                                if (n < 0) break
                                file.write(buffer, 0, n)
                            }
                        }
                        file.fd.sync()
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
        // The files are synced; their names live in the folders, deepest first, the root last.
        for ((dir, _) in directories.asReversed()) {
            if (!Files.isSymbolicLink(dir.toPath())) syncDirectory(dir.toPath())
        }
        syncDirectory(managedRoot.toPath())
        val kept = updatePrefix()
        val temporary = File(managedRoot, "$MARKER.tmp")
        FileOutputStream(temporary).use {
            it.write(version.toByteArray())
            it.fd.sync()
        }
        Files.move(temporary.toPath(), marker.toPath(), StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE)
        syncDirectory(managedRoot.toPath())
        log("app runtime $version unpacked")
        return kept
    }

    private fun legacyPrefix() = LegacyPrefix(
        managedRoot, File(managedRoot, PREFIX_TEMPLATE), userSave, log,
        failForTest = failUpdateForTest, failLateForTest = failUpdateLateForTest, syncDirectory = syncDirectory,
    )

    /**
     * At a start with this runtime unpacked, before the engine starts: [updatePrefix] when a
     * set-aside ([LegacyPrefix.settingAside]) or a move ([LegacyPrefix.unfinished]) waits to be
     * finished (a stop left it part way: an unpack's move runs again whole, a start's does not),
     * or a copy of an older template is in place ([LegacyPrefix.stale]); else nothing. The game
     * folder for Import, as [updatePrefix].
     */
    fun updatePrefixAtStart(): File? {
        val legacy = legacyPrefix()
        if (!legacy.settingAside() && !legacy.unfinished() && !legacy.stale()) return null
        return updatePrefix()
    }

    /**
     * The prefix moved over to this runtime's template ([LegacyPrefix.migrate]), at [install] and
     * at a start ([updatePrefixAtStart]); a set-aside that a stop left part way is finished first.
     * When the move fails after the older prefix was renamed out of the way, its game folder waits
     * for Import ([LegacyPrefix.setAside]), and that folder is returned.
     *
     * A failure before that rename (no room to copy the template, for example) changes nothing for
     * a copy of a template: the new prefix it began goes, the prefix stays with its game (playable
     * on this runtime; from generation 1 to 2 it only lacks the sound), and every start tries
     * again, with the same log line, until the move passes. A prefix of an older runtime (Wine-GE
     * under FEX, no "disable" stamp) is set aside at once instead: the engine would run Wine's
     * prefix update on it with this runtime's Wine. Null: the move passed, there was nothing to
     * move, or it waits for the next start.
     */
    fun updatePrefix(): File? {
        val legacy = legacyPrefix()
        if (legacy.settingAside()) {
            log("update in place: a set-aside that a stop left part way goes on")
            return legacy.setAside(keptGame())
        }
        val olderRuntime = legacy.olderRuntimeInPlace()
        return try {
            legacy.migrate(keptGame())
            null
        } catch (error: Exception) {
            if (!legacy.moved() && !olderRuntime) {
                log("update in place stopped before it changed the prefix ($error); the next start tries again")
                runCatching { legacy.discardNew() }.onFailure { log("update in place: the new prefix it began stays: $it") }
                return null
            }
            log("update in place failed: $error")
            // Nothing is lost when this fails too: the older prefix still holds the game, and the
            // next start tries again.
            legacy.setAside(keptGame())
        }
    }

    private fun readAll(reader: TarReader, buffer: ByteArray): ByteArray {
        val all = ByteArrayOutputStream()
        while (true) {
            val n = reader.read(buffer)
            if (n < 0) break
            all.write(buffer, 0, n)
        }
        return all.toByteArray()
    }

    /** [data] with each copy of the placeholder replaced in place (the data folder padded with '/'). */
    private fun relocate(data: ByteArray, relocation: Relocation, path: String): ByteArray {
        val placeholder = relocation.placeholder
        val at = ArrayList<Long>()
        var i = indexOf(data, placeholder, 0)
        while (i >= 0) {
            at += i.toLong()
            i = indexOf(data, placeholder, i + placeholder.size)
        }
        if (at.isEmpty()) throw CorruptUserland("$path: no placeholder to relocate")
        val out = ByteArrayOutputStream(data.size)
        val writer = PlaceholderWriter(at, placeholder, relocation.replacement(BINARY)) { bytes, offset, length ->
            out.write(bytes, offset, length)
        }
        writer.write(data, data.size)
        writer.finish()
        return out.toByteArray()
    }

    private fun indexOf(data: ByteArray, needle: ByteArray, from: Int): Int {
        outer@ for (i in from..data.size - needle.size) {
            for (j in needle.indices) if (data[i + j] != needle[j]) continue@outer
            return i
        }
        return -1
    }

    private fun ownerOnly(dir: File) {
        if (!Files.isDirectory(dir.toPath(), LinkOption.NOFOLLOW_LINKS)) {
            Files.createDirectories(dir.toPath(), PosixFilePermissions.asFileAttribute(PosixFilePermissions.fromString("rwx------")))
        }
    }

    /** What the Windows Installer cache needs ([sweep]). */
    enum class Sweep {
        /** The installation is Ready: the Installer cache goes. */
        REMOVE,

        /** The installation needs setting up (again): nothing to do. */
        RESTORE,
    }

    companion object {
        const val MARKER = ".pd2-app-runtime"
        /** The data-folder placeholder the runtime's programs are built for (userland/config.sh). */
        val PLACEHOLDER = "/pd2launcher-data-placeholder-" + "0".repeat(170)
        private const val RELOCATE_LIST = "relocate.list"
        /** The prefix template (the engine's `APP_PREFIX_TEMPLATE`). */
        const val PREFIX_TEMPLATE = "runner/wine-11.0-arm64-prefix"
        /** The runtime's Wine (the engine's `APP_RUNTIME_DIRECTORY`). */
        const val WINE = "runner/wine-11.0-arm64"
        /** The official launcher's x86 .NET runtime, which the prefix's `C:\Program Files (x86)\dotnet` links to (runtime/prefix.sh). */
        const val DOTNET = "runner/dotnet-6.0.36-x86"
        /** The trees an unpack replaces (runtime/pack.py's layout); the Launcher's own files stay. */
        val PROVIDED = listOf(
            DOTNET,
            WINE,
            PREFIX_TEMPLATE,
        )

        /**
         * What an older app's FEX runtime left in the managed root: FEX's RootFS and folders,
         * Wine-GE, and the Wine Gecko, Wine Mono and .NET installers in the download cache.
         */
        val LEGACY = listOf(
            "fex",
            "runner/lutris-GE-Proton8-26-x86_64",
            "cache/artifacts/windowsdesktop-runtime-6.0.36-win-x86.verified",
            "cache/artifacts/wine-gecko-2.47.4-x86.verified",
            "cache/artifacts/wine-mono-11.3.0-x86.verified",
            "cache/artifacts/wine-ge-8-26.verified",
        )

        /** A binary file's relocation (the data folder padded in place). */
        private val BINARY = ManifestEntry(path = "", type = "file", mode = "0755")

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
