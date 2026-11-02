package com.pd2launcher.app

import java.io.File
import java.io.FileOutputStream
import java.io.IOException
import java.io.RandomAccessFile
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.nio.file.Files
import java.nio.file.LinkOption
import java.nio.file.Path
import java.nio.file.Paths
import java.nio.file.StandardCopyOption
import java.nio.file.attribute.BasicFileAttributes
import java.nio.file.attribute.PosixFilePermissions
import java.security.MessageDigest

/**
 * What a disposable prefix needs back, kept outside it in [folder] (`<managed root>/prefix-state`,
 * which Start over removes with the game; the engine's `game_folder::ANDROID_PREFIX_STATE`):
 *  - [playerReg]: the game's own registry values, HKCU `Software\Blizzard Entertainment\Diablo II`
 *    with its subkeys (the video settings, the last command line, the install paths) and HKCU
 *    `Software\Battle.net\Patch` (and `Configuration` when the game wrote it) ([capturePlayer]);
 *  - for the source Blizzard's installers only (the PLAIN template): what those installers and the
 *    PD2 installer wrote outside the game folder, against the PLAIN template ([captureInstallers]):
 *    [installersReg] (their registry keys) and [files] (their files and folders, Blizzard's
 *    uninstaller among them, which is then the player's own copy), which the prefix reaches by links
 *    ([filesList]).
 * A new prefix gets them back by text in its hives (no Wine start) and by links ([apply]), with the
 * link to the game folder outside the prefix ([GAME_LINK]). Fast download and imports need no
 * installers' state: the GOLDEN template has the official installers' state already.
 *
 * Each file is written to a temporary file, synced and renamed, so a stop never leaves half a file;
 * each move of a captured file is a rename, written in [filesList] before it is made, so a stop part
 * way is finished by the next capture. No value goes into a log.
 */
class PrefixState(
    private val root: File,
    private val log: (String) -> Unit,
    /** Makes a folder's entries durable (fsync on the folder); the JVM tests pass their own. */
    private val syncDirectory: (Path) -> Unit = ::fsyncDirectory,
) {
    val folder = File(root, FOLDER)
    val playerReg = File(folder, "player.reg")
    val installersReg = File(folder, "installers.reg")
    val files = File(folder, "files")
    val filesList = File(folder, "files.list")
    /** There from the end of an install, update or repair job until the installers' state is captured ([captureIsDue]). */
    val captureDue = File(folder, ".capture-due")

    /** One file or folder that the installers wrote outside the game folder, at [path] below the prefix. */
    data class Entry(val path: String, val folder: Boolean)

    /**
     * [prefix]'s player values ([PLAYER_KEYS]) into [playerReg]; a path on the `G:` drive of an older
     * import becomes the game folder's `C:` path. Nothing is written when the prefix has no hive,
     * or none of the keys while [playerReg] holds some. Whether it was written.
     */
    fun capturePlayer(prefix: File): Boolean {
        val user = File(prefix, "user.reg")
        if (!user.isFile) return false
        val hive = WineRegistry.parse(user.readText())
        val keys = PLAYER_KEYS.flatMap { WineRegistry.subtree(hive, it) }
            .map { WineRegistry.replacePathPrefix(it, OLD_IMPORT_PATH, GAME_WINDOWS_PATH) }
        if (keys.isEmpty() && playerReg.isFile) return false
        val text = WineRegistry.hivePartsText(PLAYER_COMMENTS, mapOf("user.reg" to keys))
        writeDurably(playerReg, text)
        log("prefix state: the player's registry values kept (${keys.size} keys)")
        return true
    }

    /**
     * What the installers wrote into [prefix] outside the game folder, against the PLAIN template
     * [plain]: the registry keys that are new or have a new or changed value, without the noise
     * ([registryNoise]), our own values and the player's ([capturePlayer]), into
     * [installersReg]; and the files and folders below `drive_c` that are new or changed, without the
     * noise ([fileNoise], [wineFile], an empty folder), the game folder and the `.NET` link. A new
     * folder moves into [files] whole and the prefix gets a folder link to it; a new or changed file
     * moves alone and the prefix gets a file link. What an earlier capture moved is a link already
     * and is kept in the list. Then [cleanUp], for what an earlier one left.
     */
    fun captureInstallers(prefix: File, plain: File) {
        val parts = LinkedHashMap<String, List<WineRegistry.Key>>()
        for (hive in HIVES) {
            val mine = File(prefix, hive).takeIf { it.isFile } ?: continue
            val theirs = File(plain, hive).takeIf { it.isFile }?.readText() ?: ""
            parts[hive] = WineRegistry.difference(WineRegistry.parse(mine.readText()), WineRegistry.parse(theirs), ::registryNoise)
        }
        writeDurably(installersReg, WineRegistry.hivePartsText(INSTALLERS_COMMENTS, parts))
        val entries = (readList() + fileDifference(prefix, plain)).distinctBy { it.path }
        writeList(entries)
        for (entry in entries) moveOut(prefix, entry)
        Files.deleteIfExists(captureDue.toPath())
        syncDirectory(folder.toPath())
        log("prefix state: the installers' state kept (${parts.values.sumOf { it.size }} keys, ${entries.size} files and folders)")
        // The capture is whole: a clean-up that fails is no failed capture.
        try {
            cleanUp(prefix)
        } catch (error: Exception) {
            log("prefix state: not cleaned up ($error); the next capture or rebuild does it")
        }
    }

    /**
     * What a capture listed in [filesList] and a stop kept from moving goes out of [prefix] now
     * ([moveOut]), as the capture would have done: before a rebuild, whose new prefix gets only
     * what [files] keeps ([LegacyPrefix.makeNew]).
     */
    fun finishCapture(prefix: File) {
        val left = readList().filter { entry ->
            val inPrefix = File(prefix, entry.path).toPath()
            !Files.exists(File(files, entry.path).toPath(), LinkOption.NOFOLLOW_LINKS) &&
                Files.exists(inPrefix, LinkOption.NOFOLLOW_LINKS) && !Files.isSymbolicLink(inPrefix)
        }
        for (entry in left) moveOut(prefix, entry)
        if (left.isNotEmpty()) log("prefix state: a capture that a stop left part way is finished (${left.size} files and folders)")
    }

    /** Whether an install, update or repair job ended since the last capture of the installers' state. */
    fun captureIsDue(): Boolean = captureDue.exists()

    /** Whether the installers' state of an install by Blizzard's installers is kept ([installersReg] or [filesList]). */
    fun holdsInstallers(): Boolean = installersReg.isFile || filesList.isFile

    /** Records that an install, update or repair job ended ([captureIsDue]). */
    fun markCaptureDue() {
        if (captureDue.exists()) return
        writeDurably(captureDue, "${System.currentTimeMillis() / 1000}\n")
    }

    /**
     * Into a new prefix [prefix] (a copy of a template, before it takes its place): [installersReg]'s
     * and [playerReg]'s keys written into its hives as text, the link to the game folder, and a link
     * for each captured file and folder of [filesList] in place of the template's. What an earlier
     * capture took before a noise rule named it ([registryNoise], [noise]) stays out of the new
     * prefix: no key and no link. The state itself does not change here: until the new prefix takes
     * the older one's place, the older prefix links to what [files] keeps, and the engine uses it
     * should the rebuild fail. [cleanUp] removes that noise from the state after the swap.
     */
    fun apply(prefix: File) {
        val installers = if (installersReg.isFile) WineRegistry.hiveParts(installersReg.readText()) else emptyMap()
        // The player's keys are noise to the installers' state only.
        val installerKeys = installers.mapValues { (_, all) -> all.filterNot { registryNoise(it.id) } }
        val noiseKeys = installers.values.sumOf { it.size } - installerKeys.values.sumOf { it.size }
        val parts = LinkedHashMap<String, MutableList<WineRegistry.Key>>()
        for ((hive, kept) in installerKeys) parts.getOrPut(hive) { ArrayList() } += kept
        if (playerReg.isFile) {
            for ((hive, kept) in WineRegistry.hiveParts(playerReg.readText())) parts.getOrPut(hive) { ArrayList() } += kept
        }
        for ((hive, keys) in parts) {
            val target = File(prefix, hive)
            if (!target.isFile || keys.isEmpty()) continue
            // A key in both files is the player's ([playerReg] comes last).
            val unique = keys.associateBy { it.id }.values.toList()
            target.writeText(WineRegistry.merge(WineRegistry.parse(target.readText()), unique).text())
        }
        linkGame(prefix)
        val (noise, entries) = readList().partition(::noise)
        if (noise.isNotEmpty() || noiseKeys > 0) {
            log("prefix state: an earlier capture's noise stays out of the new prefix (files and folders: ${noise.size}, keys: $noiseKeys)")
        }
        for (entry in entries) {
            if (!Files.exists(File(files, entry.path).toPath(), LinkOption.NOFOLLOW_LINKS)) {
                log("prefix state: ${entry.path} is not kept; the new prefix has the template's")
                continue
            }
            val link = File(prefix, entry.path).toPath()
            UserlandInstaller.deleteTree(link)
            Files.createDirectories(link.parent)
            Files.createSymbolicLink(link, Paths.get(linkTarget(entry.path)))
        }
    }

    /**
     * Whether a captured [entry] is noise by the rules of today, which an earlier capture may not
     * have had: its path ([fileNoise]), one of Wine's own DLLs ([wineFile]), or an empty folder.
     */
    private fun noise(entry: Entry): Boolean {
        val kept = File(files, entry.path).toPath()
        return fileNoise(entry.path) ||
            Files.isRegularFile(kept, LinkOption.NOFOLLOW_LINKS) && wineFile(kept) ||
            Files.isDirectory(kept, LinkOption.NOFOLLOW_LINKS) && emptyFolder(kept)
    }

    /** [entries] into [filesList], one line each: its kind and its path. */
    private fun writeList(entries: List<Entry>) =
        writeDurably(filesList, entries.joinToString("") { "${if (it.folder) "folder" else "file"}\t${it.path}\n" })

    /**
     * The state without what no new prefix takes back, once the prefix in place [prefix] is one that
     * [apply] made (after the swap, [LegacyPrefix.migrate]) or at the end of a capture
     * ([captureInstallers]):
     *  - the keys of [installersReg] that are noise now ([registryNoise]);
     *  - the entries of [filesList] with nothing kept in [files], and those that are noise now
     *    ([noise]) unless [prefix] still links to them (a prefix that a capture made before the rule:
     *    its links stay good until the next rebuild);
     *  - then all below [files] that no entry names ([removeUnlisted]).
     * The list goes first and the files after it, so a stop part way leaves only files that no
     * entry names, which the next clean-up removes. Without a [filesList], nothing below [files]
     * is removed: a list that is not there is not an empty one. One log line gives the counts.
     */
    fun cleanUp(prefix: File) {
        var noiseKeys = 0
        if (installersReg.isFile) {
            val installers = WineRegistry.hiveParts(installersReg.readText())
            val clean = installers.mapValues { (_, all) -> all.filterNot { registryNoise(it.id) } }
            noiseKeys = installers.values.sumOf { it.size } - clean.values.sumOf { it.size }
            if (noiseKeys > 0) writeDurably(installersReg, WineRegistry.hivePartsText(INSTALLERS_COMMENTS, clean))
        }
        val entries = readList()
        val empty = entries.filter { !Files.exists(File(files, it.path).toPath(), LinkOption.NOFOLLOW_LINKS) }
        val noise = (entries - empty.toSet()).filter { noise(it) && !linked(prefix, it) }
        val kept = entries - (empty + noise).toSet()
        if (kept.size < entries.size) writeList(kept)
        val unlisted = if (filesList.isFile) removeUnlisted(kept) else 0
        if (noiseKeys > 0 || empty.isNotEmpty() || noise.isNotEmpty() || unlisted > 0) {
            log(
                "prefix state: what no new prefix takes is removed (noise keys: $noiseKeys; noise entries: ${noise.size}, " +
                    "entries with nothing kept: ${empty.size}; files and folders that no entry names: $unlisted)",
            )
        }
    }

    /** Whether [prefix] reaches [entry]'s kept file or folder through its link. */
    private fun linked(prefix: File, entry: Entry): Boolean {
        val link = File(prefix, entry.path).toPath()
        return Files.isSymbolicLink(link) && Files.readSymbolicLink(link) == Paths.get(linkTarget(entry.path))
    }

    /**
     * All below [files] that none of [entries] names, removed (links as links): an entry's file or
     * folder stays whole, and so do the folders on the way to it. Each folder that lost an entry is
     * synced. The count of what was removed.
     */
    private fun removeUnlisted(entries: List<Entry>): Int {
        val named = entries.mapTo(HashSet()) { it.path }
        val onTheWay = entries.flatMapTo(HashSet()) { entry ->
            generateSequence(entry.path.substringBeforeLast('/', "").ifEmpty { null }) { it.substringBeforeLast('/', "").ifEmpty { null } }.toList()
        }
        var removed = 0
        fun walk(dir: File, relative: String) {
            val names = dir.list() ?: return
            var here = 0
            for (name in names.sorted()) {
                val path = if (relative.isEmpty()) name else "$relative/$name"
                val child = File(dir, name)
                when {
                    path in named -> {}
                    path in onTheWay && Files.isDirectory(child.toPath(), LinkOption.NOFOLLOW_LINKS) -> walk(child, path)
                    else -> {
                        UserlandInstaller.deleteTree(child.toPath())
                        here++
                    }
                }
            }
            if (here > 0) syncDirectory(dir.toPath())
            removed += here
        }
        if (Files.isDirectory(files.toPath(), LinkOption.NOFOLLOW_LINKS)) walk(files, "")
        return removed
    }

    /** The captured entries ([filesList]); empty without one. */
    fun readList(): List<Entry> {
        if (!filesList.isFile) return emptyList()
        return filesList.readLines().mapNotNull { line ->
            val (kind, path) = line.split('\t', limit = 2).takeIf { it.size == 2 } ?: return@mapNotNull null
            if (!safePath(path)) return@mapNotNull null
            Entry(path, kind == "folder")
        }
    }

    /**
     * [entry] out of [prefix] into [files] by a rename, then a link in its place. Each step can stop
     * and run again: a link in place is done; the entry already in [files] gets its link; a real
     * entry in the prefix while [files] has one too is newer (an install or repair wrote it again):
     * it takes the place of the kept one.
     */
    private fun moveOut(prefix: File, entry: Entry) {
        val inPrefix = File(prefix, entry.path).toPath()
        val kept = File(files, entry.path).toPath()
        val target = Paths.get(linkTarget(entry.path))
        if (Files.isSymbolicLink(inPrefix) && Files.readSymbolicLink(inPrefix) == target) return
        if (Files.exists(inPrefix, LinkOption.NOFOLLOW_LINKS) && !Files.isSymbolicLink(inPrefix)) {
            if (Files.exists(kept, LinkOption.NOFOLLOW_LINKS)) UserlandInstaller.deleteTree(kept)
            ownerOnly(kept.parent.toFile())
            Files.move(inPrefix, kept, StandardCopyOption.ATOMIC_MOVE)
            syncDirectory(kept.parent)
            syncDirectory(inPrefix.parent)
        }
        if (!Files.exists(kept, LinkOption.NOFOLLOW_LINKS)) return
        UserlandInstaller.deleteTree(inPrefix)
        Files.createSymbolicLink(inPrefix, target)
        syncDirectory(inPrefix.parent)
    }

    /**
     * The files and folders below [prefix]'s `drive_c` that [plain] does not have as they are: a new
     * one (a new folder as a whole), or a file whose content, link or kind is other. The noise
     * ([fileNoise]), Wine's own DLLs ([wineFile]), an empty folder and what an earlier capture moved
     * (a link into [files]) are left out. Links are never followed.
     */
    fun fileDifference(prefix: File, plain: File): List<Entry> {
        val out = ArrayList<Entry>()
        val statePath = files.toPath().toAbsolutePath().normalize()
        fun walk(relative: String) {
            val dir = File(prefix, relative).toPath()
            val names = Files.newDirectoryStream(dir).use { stream -> stream.map { it.fileName.toString() }.sorted() }
            for (name in names) {
                val path = "$relative/$name"
                if (fileNoise(path)) continue
                val mine = File(prefix, path).toPath()
                val mineAttrs = Files.readAttributes(mine, BasicFileAttributes::class.java, LinkOption.NOFOLLOW_LINKS)
                if (mineAttrs.isSymbolicLink &&
                    mine.parent.resolve(Files.readSymbolicLink(mine)).toAbsolutePath().normalize().startsWith(statePath)
                ) {
                    continue
                }
                // Wine's own DLLs hold nothing of the installers': after a Wine update, a kept copy
                // would take the place of the new one. Tested first, before any hash of the file.
                if (mineAttrs.isRegularFile && wineFile(mine)) continue
                val theirs = File(plain, path).toPath()
                val theirAttrs = runCatching {
                    Files.readAttributes(theirs, BasicFileAttributes::class.java, LinkOption.NOFOLLOW_LINKS)
                }.getOrNull()
                val other = when {
                    theirAttrs == null -> true
                    mineAttrs.isDirectory && theirAttrs.isDirectory -> {
                        walk(path)
                        false
                    }
                    mineAttrs.isSymbolicLink && theirAttrs.isSymbolicLink -> Files.readSymbolicLink(mine) != Files.readSymbolicLink(theirs)
                    mineAttrs.isRegularFile && theirAttrs.isRegularFile -> mineAttrs.size() != theirAttrs.size() || !sameContent(mine, theirs)
                    else -> true
                }
                // An empty folder holds nothing of the installers' either.
                if (other && !(mineAttrs.isDirectory && emptyFolder(mine))) out += Entry(path, mineAttrs.isDirectory)
            }
        }
        if (Files.isDirectory(File(prefix, "drive_c").toPath(), LinkOption.NOFOLLOW_LINKS)) walk("drive_c")
        return out
    }

    /** The link's target for a captured [path], relative from its folder in the prefix to [files]. */
    private fun linkTarget(path: String): String = "../".repeat(path.count { it == '/' } + 1) + "$FOLDER/files/$path"

    /** [file] with [text], whole or not at all: a synced temporary file renamed over it, its folder synced. */
    private fun writeDurably(file: File, text: String) {
        ownerOnly(file.parentFile!!)
        val next = File(file.parentFile, "${file.name}.pd2-new")
        FileOutputStream(next).use {
            it.write(text.toByteArray())
            it.fd.sync()
        }
        Files.move(next.toPath(), file.toPath(), StandardCopyOption.ATOMIC_MOVE, StandardCopyOption.REPLACE_EXISTING)
        syncDirectory(file.parentFile!!.toPath())
    }

    /** [dir] and its missing parents, owner-only (the app process's umask is 0). */
    private fun ownerOnly(dir: File) {
        if (!dir.isDirectory) {
            Files.createDirectories(dir.toPath(), PosixFilePermissions.asFileAttribute(PosixFilePermissions.fromString("rwx------")))
        }
    }

    companion object {
        /** The folder in the managed root (the engine's `game_folder::ANDROID_PREFIX_STATE`). */
        const val FOLDER = "prefix-state"

        /** The monitor of [locked]. */
        internal val LOCK = Any()

        /**
         * [block] while no capture of the state ([SessionService.keepPrefixState]) and no rebuild of
         * the prefix ([AppRuntime.updatePrefixAtStart], the end of [AppRuntime.install]) runs: the
         * prefix lock. One order only: a capture takes it inside [SaveSyncFolder.locked]; a
         * rebuild takes it alone and never takes the save-sync lock while it holds this one. The
         * main thread takes neither lock (Settings' [SaveSyncFolder.load] has a short lock of its
         * own), so it never waits for a rebuild or an unpack, also not through a capture that
         * waits here. Blocking.
         */
        fun <T> locked(block: () -> T): T = synchronized(LOCK) { block() }

        /** The game folder in the managed root, outside the prefix (the engine's `ANDROID_GAME_RELATIVE`). */
        const val GAME_LOCATION = "Diablo II"

        /** The link in a prefix that leads to [GAME_LOCATION]: `C:\Program Files (x86)\Diablo II`. */
        const val GAME_LINK = "drive_c/Program Files (x86)/Diablo II"

        /** The link's target, relative to its folder (the engine's `GAME_LINK_TARGET`). */
        const val GAME_LINK_TARGET = "../../../Diablo II"

        /** The mark of a template's kind in it and in each copy (the engine's `PREFIX_TEMPLATE_MARK`). */
        const val TEMPLATE_MARK = ".pd2-prefix-template"

        /** A prefix's two hives; a prefix without one of them is made again ([LegacyPrefix.broken]). */
        val HIVES = listOf("system.reg", "user.reg")

        /** The game folder as Windows programs see it, in the hive's escaped form. */
        private const val GAME_WINDOWS_PATH = "C:\\\\Program Files (x86)\\\\Diablo II"

        /** An older import's game folder on the `G:` drive, in the hive's escaped form. */
        private const val OLD_IMPORT_PATH = "G:\\\\Diablo II"

        /** The player's keys in `user.reg`, in the hive's escaped form. */
        val PLAYER_KEYS = listOf(
            WineRegistry.keyName("Software", "Blizzard Entertainment", "Diablo II"),
            WineRegistry.keyName("Software", "Battle.net", "Patch"),
            WineRegistry.keyName("Software", "Battle.net", "Configuration"),
        )

        private val PLAYER_COMMENTS = listOf(
            "PD2 Launcher: the game's own values of this installation (HKCU), which a new prefix gets back.",
        )

        private val INSTALLERS_COMMENTS = listOf(
            "PD2 Launcher: what the installers of this installation wrote outside the game folder, against",
            "the PLAIN template, which a new prefix gets back.",
        )

        /** Our own values, which each template has: never captured. */
        private val OWN_KEYS = setOf(
            WineRegistry.keyName("Software", "Wine", "AppDefaults", "Game.exe"),
            WineRegistry.keyName("Software", "Wine", "X11 Driver"),
            WineRegistry.keyName("Software", "Microsoft", "Avalon.Graphics"),
        ).map { it.lowercase() }.toSet()

        /** Parts of a key's name (without case, escaped form) that are noise: Wine makes them again. */
        private val NOISE_PARTS = listOf(
            "\\\\enum\\\\", "deviceclasses", "control\\\\class\\\\", "centralprocessor", "computername",
            "services\\\\tcpip", "session manager\\\\environment", "timezoneinformation", "time zones",
            "cryptography", "certificates", "mmdevices", "winealsa", "software\\\\wine\\\\fonts",
            "windows nt\\\\currentversion\\\\fonts", "windows\\\\currentversion\\\\fonts",
        )

        /**
         * Whether a key ([id]: its name without case) is noise for the installers' state: hardware,
         * devices, the GPU class, the processor, the names of the device, time zones, crypto keys and
         * certificates, sound devices, Wine's fonts (its three font keys), the console and the
         * volatile environment; our own values; the player's ([PLAYER_KEYS]).
         */
        fun registryNoise(id: String): Boolean =
            id.startsWith("hardware") || id.startsWith("console") || id.startsWith("volatile environment") ||
                id in OWN_KEYS || NOISE_PARTS.any { id.contains(it) || id.endsWith(it.removeSuffix("\\\\")) } ||
                PLAYER_KEYS.any { val key = it.lowercase(); id == key || id.startsWith("$key\\\\") }

        /** The names (without case) of the installers' logs. */
        private val LOGS = setOf("diablo ii install log.html", "installerreplacement.log")

        /**
         * Whether a [path] below the prefix (`drive_c/...`), or a folder above it, is noise for the
         * installers' state: temporary files, the internet cache and cookies, the installers' logs,
         * Windows Installer's cache, Wine's side-by-side assemblies (`winsxs`: Wine's own copies,
         * which a later Wine's template has in their new version), our own folders for downloads and
         * programs, the game folder and the link to the app's .NET.
         */
        fun fileNoise(path: String): Boolean =
            generateSequence(path.lowercase()) { it.substringBeforeLast('/', "").ifEmpty { null } }.any { lower ->
                val parts = lower.split('/')
                lower == GAME_LINK.lowercase() || lower == "drive_c/program files (x86)/dotnet" ||
                    lower == "drive_c/windows/installer" || lower == "drive_c/windows/temp" || lower == "drive_c/windows/winsxs" ||
                    lower == "drive_c/pd2launcher-downloads" || lower == "drive_c/pd2launcher-programs" ||
                    "temp" in parts.drop(3) && parts.getOrNull(1) == "users" || "inetcache" in parts || "inetcookies" in parts ||
                    parts.last() in LOGS
            }

        /** Wine's marks of its own DLLs, each with the NUL after it (wine-11.0 `dlls/setupapi/fakedll.c`). */
        private val WINE_MARKS = listOf("Wine builtin DLL", "Wine placeholder DLL").map { "$it\u0000".toByteArray(Charsets.US_ASCII) }

        /** Where Wine's mark is: the 32 bytes after the 64-byte DOS header. */
        private const val WINE_MARK_OFFSET = 0x40

        /**
         * Whether the regular file [file] is one of Wine's own DLLs, which the runtime's Wine brings
         * and a later Wine's template has in its new version: it starts with "MZ", its new header's
         * offset (`e_lfanew`, at 0x3c, read without a sign as Wine compares it) is at least 0x60, and
         * at 0x40 it has "Wine builtin DLL" or "Wine placeholder DLL" with its NUL. Wine 11 tests a
         * file in the same way before it writes over it (`is_fake_dll` in `dlls/setupapi/fakedll.c`).
         * winebuild writes the mark there: in `tools/winebuild/spec32.c` for 32-bit and 64-bit
         * modules (`output_pe_file`, and `make_builtin_files`, which winegcc runs on each PE
         * builtin), and in `tools/winebuild/spec16.c` for 16-bit placeholders
         * (`output_fake_module16`). A file that cannot be read is no Wine DLL.
         */
        fun wineFile(file: Path): Boolean = runCatching {
            val head = ByteArray(WINE_MARK_OFFSET + 32)
            RandomAccessFile(file.toFile(), "r").use {
                if (it.length() < head.size) return@runCatching false
                it.readFully(head)
            }
            val headerAt = ByteBuffer.wrap(head).order(ByteOrder.LITTLE_ENDIAN).getInt(0x3c).toLong() and 0xffffffffL
            head[0] == 'M'.code.toByte() && head[1] == 'Z'.code.toByte() && headerAt >= head.size &&
                WINE_MARKS.any { mark -> mark.indices.all { head[WINE_MARK_OFFSET + it] == mark[it] } }
        }.getOrDefault(false)

        /**
         * Whether the folder [dir] has no entry. One that cannot be read is not empty: it is kept,
         * and a capture or a rebuild does not fail on it at each start.
         */
        fun emptyFolder(dir: Path): Boolean =
            runCatching { Files.newDirectoryStream(dir).use { !it.iterator().hasNext() } }.getOrDefault(false)

        /** The kinds of template (`runtime/prefix.sh`). */
        enum class Kind(val mark: String) { PLAIN("plain"), GOLDEN("golden") }

        /** The kind that [dir] (a template, or a prefix made from one) names in its [TEMPLATE_MARK]; null without a readable one. */
        fun markedKind(dir: File): Kind? {
            val mark = File(dir, TEMPLATE_MARK)
            val text = runCatching { mark.takeIf { it.isFile && it.length() < 64 }?.readText() }.getOrNull()?.trim() ?: return null
            return Kind.entries.firstOrNull { it.mark == text }
        }

        /**
         * The kind of a [prefix] without a mark (made before generation 3): GOLDEN for the prefix of an
         * older runtime (no "disable" stamp: Wine-GE under FEX, whose files are another Wine's) and for a
         * prefix where Blizzard's installers did not run; PLAIN where they ran: Blizzard's uninstaller
         * folder is there, or its `UninstallPath` in HKLM.
         */
        fun inferredKind(prefix: File): Kind {
            val stamp = runCatching { File(prefix, ".update-timestamp").takeIf { it.isFile && it.length() < 64 }?.readText() }
                .getOrNull()?.trim()
            if (stamp != "disable") return Kind.GOLDEN
            val uninstaller = File(prefix, "drive_c/Program Files (x86)/Common Files/Blizzard Entertainment/Diablo II").toPath()
            if (Files.isDirectory(uninstaller, LinkOption.NOFOLLOW_LINKS)) return Kind.PLAIN
            val system = File(prefix, "system.reg")
            if (!system.isFile) return Kind.GOLDEN
            val key = WineRegistry.keyName("Software", "Wow6432Node", "Blizzard Entertainment", "Diablo II").lowercase()
            val blizzard = WineRegistry.parse(system.readText()).keys.firstOrNull { it.id == key }
            return if (blizzard != null && "uninstallpath" in WineRegistry.values(blizzard)) Kind.PLAIN else Kind.GOLDEN
        }

        /** [markedKind], else [inferredKind]. */
        fun kindOf(prefix: File): Kind = markedKind(prefix) ?: inferredKind(prefix)

        /** Makes [prefix]'s link to the game folder outside it ([GAME_LINK]); a link elsewhere is replaced, anything else refused. */
        fun linkGame(prefix: File) {
            val link = File(prefix, GAME_LINK).toPath()
            if (Files.isSymbolicLink(link)) {
                if (Files.readSymbolicLink(link) == Paths.get(GAME_LINK_TARGET)) return
                Files.delete(link)
            } else if (Files.exists(link, LinkOption.NOFOLLOW_LINKS)) {
                throw IOException("the prefix holds a game folder of its own at $link")
            }
            Files.createDirectories(link.parent)
            Files.createSymbolicLink(link, Paths.get(GAME_LINK_TARGET))
        }

        /** A path below the prefix that is plain: relative, no empty, `.` or `..` part. */
        private fun safePath(path: String): Boolean =
            path.isNotEmpty() && !path.startsWith("/") && path.split('/').none { it.isEmpty() || it == "." || it == ".." }

        private fun sameContent(a: Path, b: Path): Boolean = digest(a).contentEquals(digest(b))

        private fun digest(path: Path): ByteArray {
            val sha = MessageDigest.getInstance("SHA-256")
            Files.newInputStream(path).use { input ->
                val buffer = ByteArray(1 shl 16)
                while (true) {
                    val n = input.read(buffer)
                    if (n < 0) break
                    sha.update(buffer, 0, n)
                }
            }
            return sha.digest()
        }
    }
}
