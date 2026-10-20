package com.pd2launcher.app

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import java.io.File
import java.io.FileOutputStream
import java.io.IOException
import java.io.InputStream
import java.nio.file.AccessDeniedException
import java.nio.file.FileVisitResult
import java.nio.file.Files
import java.nio.file.LinkOption
import java.nio.file.Path
import java.nio.file.Paths
import java.nio.file.SimpleFileVisitor
import java.nio.file.StandardCopyOption
import java.nio.file.attribute.BasicFileAttributes
import java.nio.file.attribute.PosixFilePermission
import java.nio.file.attribute.PosixFilePermissions
import java.security.MessageDigest

/** userland/pack.sh's manifest.json (the archive's own fields are not needed here). */
@Serializable
data class UserlandManifest(
    val version: String,
    /** The data folder every program is built for; the unpack rewrites it to the real one ([Relocation]). */
    val placeholder: String,
    /** Where the tree is built to run: [placeholder] followed by the userland's place in the data folder. */
    val prefix: String,
    val entries: List<ManifestEntry>,
    /** The paths checked again before every engine start ([CriticalFiles]), as programs open them. */
    val critical: List<String>,
    /** Paths that must not exist in the tree (glibc would read them). */
    val absent: List<String>,
) {
    /** Bytes of every regular file, unpacked. */
    val fileBytes: Long get() = entries.sumOf { it.size ?: 0L }

    companion object {
        fun parse(text: String): UserlandManifest = json.decodeFromString(serializer(), text)
    }
}

/**
 * One path of the tree: `type` is `file` (with size and sha256 as packed), `dir` or `symlink`
 * (with target). A file that holds the placeholder lists the offset of each copy ([placeholders]);
 * [text] files get the real data folder exactly, the others in place ([Relocation]).
 */
@Serializable
data class ManifestEntry(
    val path: String,
    val type: String,
    val mode: String,
    val size: Long? = null,
    val sha256: String? = null,
    val target: String? = null,
    val placeholders: List<Long> = emptyList(),
    val text: Boolean = false,
)

/**
 * `<version>/record.json`, written last: the version, the [prefix] it was rewritten for
 * (`<data>/files/userland/current`) and the SHA-256 of every file as written.
 */
@Serializable
data class UserlandRecord(val version: String, val prefix: String, val files: Map<String, String>)

/** Whether a program from the App userland runs now; `current` is only switched when none does. */
fun interface UserlandProcesses {
    fun anyRunning(): Boolean
}

data class InstallResult(val outcome: Outcome, val version: String, val detail: String = "") {
    enum class Outcome {
        /** `current` already named this version and it is complete. */
        ALREADY_CURRENT,
        /** The version is complete (unpacked now or before) and `current` names it now. */
        SWITCHED,
        /** The version is complete but a userland program runs, so `current` was left as it was. */
        DEFERRED,
        /**
         * The archive does not match the manifest, or this data folder cannot take it; nothing of it
         * was kept and `current` is unchanged.
         */
        REFUSED,
    }
}

/**
 * Unpacks the App userland into `<root>/<version>` and points `<root>/current` at it. The root is
 * `<data>/files/userland` of the app's canonical data folder ([SessionPaths.userlandRoot]).
 *
 * The tar stream unpacks into `<version>.staging`; every entry is checked against the
 * manifest (type, mode, symlink target, SHA-256 of each file as packed), hard links and anything
 * not in the manifest are refused, each placeholder the manifest lists is rewritten to the data
 * folder ([Relocation]), `record.json` is written last, and the staging folder is renamed
 * to `<version>`. `current` is a relative symlink replaced by renaming a temporary one over it.
 * The previous version stays until the current one has started once ([markStarted] removes it).
 */
class UserlandInstaller(
    private val root: File,
    private val processes: UserlandProcesses,
    /**
     * Files of the archive this device never loads (the other GPU family's Vulkan driver): checked
     * against the manifest like every file, but not written, so they take no space.
     */
    private val skip: Set<String> = emptySet(),
    /** Makes a folder's entries durable (fsync on the folder); the JVM tests record the calls. */
    private val syncDirectory: (Path) -> Unit = ::fsyncDirectory,
    private val log: (String) -> Unit = {},
) {
    private val current: Path get() = root.toPath().resolve(CURRENT)

    /** The version `current` names, or null when it is absent. */
    fun currentVersion(): String? =
        if (Files.isSymbolicLink(current)) Files.readSymbolicLink(current).toString() else null

    /**
     * True when `<version>/record.json` names [version] and was written for this root's `current`
     * (a data folder that moved needs the placeholders rewritten again).
     */
    fun isComplete(version: String): Boolean = try {
        val record = File(root, "$version/$RECORD")
        record.isFile && json.decodeFromString(UserlandRecord.serializer(), record.readText())
            .let { it.version == version && it.prefix == current.toString() }
    } catch (_: Exception) {
        false
    }

    /**
     * Why [manifest] cannot be unpacked here, or null: `<root>/current` must be its prefix with the
     * placeholder replaced by a data folder [Relocation] takes.
     */
    fun refusal(manifest: UserlandManifest): String? {
        val below = manifest.prefix.removePrefix(manifest.placeholder)
        if (below == manifest.prefix || !below.startsWith("/")) return "prefix ${manifest.prefix} is not below its placeholder"
        val here = current.toString()
        if (!here.endsWith(below)) return "built for <data folder>$below, not $here"
        return Relocation.problem(here.removeSuffix(below), manifest.placeholder.length)
    }

    private fun relocation(manifest: UserlandManifest) =
        Relocation(manifest.placeholder, current.toString().removeSuffix(manifest.prefix.removePrefix(manifest.placeholder)))

    /**
     * Checks [manifest]'s unpacked version again, as [CriticalFiles] walks it: code under Wine runs
     * as the app and can reach this tree, and a changed loader, library, userland.env or FEX would
     * run in every later engine start, the one that gets the CD keys too. Anything that differs
     * makes the version incomplete (its record goes, or whatever is in its folder's place, a link
     * say), so [install] unpacks it again through staging, without whatever was added. Nothing is
     * removed while the way to the versions is not as it must be ([misplaced]): it would lead
     * through a link; [install] puts that right first. Returns what differed.
     */
    fun forgetIfChanged(manifest: UserlandManifest): List<String> {
        val changed = refusal(manifest)?.let { listOf(it) }
            ?: CriticalFiles(manifest, relocation(manifest), root.toPath()).problems()
        if (changed.isNotEmpty()) {
            log("$changed in ${manifest.version}; it is unpacked again")
            val tree = File(root, manifest.version).toPath()
            if (misplaced() != null) {
                // [install] removes the wrong entry, and with it the way to this version.
            } else if (Files.isDirectory(tree, LinkOption.NOFOLLOW_LINKS)) {
                Files.deleteIfExists(tree.resolve(RECORD))
                syncDirectory(tree)
            } else if (Files.exists(tree, LinkOption.NOFOLLOW_LINKS)) {
                deleteTree(tree)
                syncDirectory(root.toPath())
            }
        }
        return changed
    }

    /**
     * Makes [manifest]'s version current and checks it ([forgetIfChanged]) after whatever [install]
     * did, as the last step before an engine start: `current` must name the version and its
     * critical files must be the APK's, whatever its record says. A version the check finds
     * changed is unpacked again and checked once more. Returns the last [install]'s result, or
     * null when `current` named the version and the check found nothing. Throws, and the engine
     * must not start, when [install] refuses the version or fails, leaves `current` naming another
     * (a userland program still runs: [InstallResult.Outcome.DEFERRED]) or the check still finds
     * changes after the second unpack. A link in place of `files` or `files/userland`, or anything
     * but a link in `current`'s place, is removed first ([repairPlaces]). [progress] also gets
     * (0, total) before each [install].
     */
    fun prepare(
        manifest: UserlandManifest,
        openTar: () -> InputStream,
        progress: (Long, Long) -> Unit = { _, _ -> },
    ): InstallResult? {
        val version = manifest.version
        if (currentVersion() == version && isComplete(version) && forgetIfChanged(manifest).isEmpty()) return null
        var changed = emptyList<String>()
        repeat(2) {
            progress(0, manifest.fileBytes)
            val result = install(manifest, openTar, progress)
            log("${result.outcome} $version ${result.detail}".trim())
            if (result.outcome == InstallResult.Outcome.REFUSED) throw IOException(result.detail.ifEmpty { "$version refused" })
            val current = currentVersion()
            if (current != version) throw IOException("current names ${current ?: "nothing"}, not $version (${result.outcome})")
            changed = forgetIfChanged(manifest)
            if (changed.isEmpty()) return result
        }
        throw IOException("$version still differs from the APK after unpacking it again: $changed")
    }

    /**
     * The entry on the way from the data folder to the versions that is not what it must be, read
     * without following links: `files` and `files/userland` must be real folders (or missing) and
     * `current` a link (or missing); null when each is. Below a wrong one nothing is looked at.
     */
    private fun misplaced(): Path? {
        val userland = root.toPath()
        for (folder in listOf(userland.parent, userland)) {
            val found = attributes(folder) ?: return null
            if (!found.isDirectory) return folder
        }
        val found = attributes(current) ?: return null
        return if (found.isSymbolicLink) null else current
    }

    /**
     * Removes what [misplaced] finds without following links: a link where a folder belongs goes
     * itself, never what it leads to, and anything but a link in `current`'s place goes with all it
     * holds, its links not followed either. It is in the app's own data folder, and the unpack
     * makes again what went (a `files` link takes the app's other files with it, which their own
     * code makes again). Returns what was removed, or null.
     */
    fun repairPlaces(): String? {
        val wrong = misplaced() ?: return null
        log("$wrong is not what it must be; it is removed")
        deleteTree(wrong)
        syncDirectory(wrong.parent)
        return wrong.toString()
    }

    private fun attributes(path: Path): BasicFileAttributes? = try {
        Files.readAttributes(path, BasicFileAttributes::class.java, LinkOption.NOFOLLOW_LINKS)
    } catch (_: IOException) {
        null
    }

    /**
     * Records that the current version has started once and removes every other version (and
     * any interrupted unpack): only the one `current` names is kept.
     */
    fun markStarted() {
        val version = currentVersion() ?: return
        File(root, "$version/$STARTED").createNewFile()
        removeLeftovers(version)
    }

    /**
     * Makes [manifest]'s version current, unpacking [openTar] (an uncompressed tar stream) unless
     * that version is already complete. [progress] gets the file bytes written and the total.
     * A userland this root cannot take ([refusal]) is refused before anything is touched. A
     * complete version is taken by its record, which sits in the writable tree: an engine start
     * goes through [prepare], which checks the version afterwards.
     */
    fun install(
        manifest: UserlandManifest,
        openTar: () -> InputStream,
        progress: (Long, Long) -> Unit = { _, _ -> },
    ): InstallResult {
        val version = manifest.version
        require(isPlainName(version)) { "bad version: $version" }
        refusal(manifest)?.let {
            log("refused $version: $it")
            return InstallResult(InstallResult.Outcome.REFUSED, version, it)
        }
        val relocation = relocation(manifest)
        repairPlaces()
        root.mkdirs()
        removeLeftovers(version)

        if (!isComplete(version)) {
            val target = File(root, version).toPath()
            val staging = File(root, "$version$STAGING").toPath()
            // Only an incomplete tree can be here: an interrupted unpack, or the current one whose
            // files changed ([forgetIfChanged]), which no userland program runs from yet.
            deleteTree(target)
            Files.createDirectories(staging)
            try {
                openTar().use { unpack(manifest, relocation, it, staging, progress) }
            } catch (error: Exception) {
                deleteTree(staging)
                log("refused $version: $error")
                return InstallResult(InstallResult.Outcome.REFUSED, version, error.message ?: error.toString())
            }
            Files.move(staging, target, StandardCopyOption.ATOMIC_MOVE)
            syncDirectory(root.toPath())
            log("unpacked $version")
        }

        if (currentVersion() == version) return InstallResult(InstallResult.Outcome.ALREADY_CURRENT, version)
        if (processes.anyRunning()) return InstallResult(InstallResult.Outcome.DEFERRED, version)
        val temporary = root.toPath().resolve(CURRENT_TEMPORARY)
        Files.deleteIfExists(temporary)
        Files.createSymbolicLink(temporary, Paths.get(version))
        Files.move(temporary, current, StandardCopyOption.ATOMIC_MOVE)
        syncDirectory(root.toPath())
        log("current -> $version")
        return InstallResult(InstallResult.Outcome.SWITCHED, version)
    }

    /** Interrupted unpacks and switches; versions other than `current` once it has started. */
    private fun removeLeftovers(installing: String) {
        val currentVersion = currentVersion()
        val started = currentVersion != null && File(root, "$currentVersion/$STARTED").exists()
        for (name in root.list().orEmpty()) {
            val leftover = name.endsWith(STAGING) || name == CURRENT_TEMPORARY ||
                (started && name != CURRENT && name != currentVersion && name != installing)
            if (leftover) {
                log("removing $name")
                deleteTree(root.toPath().resolve(name))
            }
        }
    }

    private fun unpack(
        manifest: UserlandManifest,
        relocation: Relocation,
        input: InputStream,
        staging: Path,
        progress: (Long, Long) -> Unit,
    ) {
        val expected = manifest.entries.associateBy { it.path }
        val seen = HashSet<String>()
        val hashes = sortedMapOf<String, String>()
        val directories = ArrayList<Pair<Path, Int>>()
        val total = manifest.fileBytes
        var written = 0L
        val buffer = ByteArray(1 shl 18)
        // The file as packed, which the manifest names, and as written.
        val digest = MessageDigest.getInstance("SHA-256")
        val writtenDigest = MessageDigest.getInstance("SHA-256")
        val tar = TarReader(input)
        val links = HashSet<String>()

        while (true) {
            val entry = tar.next() ?: break
            val path = entry.name.trimEnd('/')
            if (path.isEmpty() || path.startsWith("/") || path.split('/').any { it == "" || it == "." || it == ".." }) {
                throw CorruptUserland("unsafe path in archive: ${entry.name}")
            }
            val wanted = expected[path] ?: throw CorruptUserland("not in the manifest: $path")
            if (!seen.add(path)) throw CorruptUserland("twice in the archive: $path")
            // Writing below a link the archive made would land outside the staging folder.
            val parents = path.split('/').dropLast(1).runningReduce { a, b -> "$a/$b" }
            parents.firstOrNull { it in links }?.let { throw CorruptUserland("$path is below the symlink $it") }
            val out = staging.resolve(path)
            when (entry.type) {
                TarReader.DIRECTORY -> {
                    expect(wanted, "dir", entry.mode)
                    Files.createDirectories(out)
                    directories += out to entry.mode
                }
                TarReader.FILE, TarReader.FILE_OLD -> {
                    expect(wanted, "file", entry.mode)
                    if (entry.size != wanted.size) throw CorruptUserland("size of $path: ${entry.size}, manifest ${wanted.size}")
                    relocation.offsetProblem(wanted)?.let { throw CorruptUserland(it) }
                    digest.reset()
                    writtenDigest.reset()
                    if (path in skip) {
                        while (true) {
                            val n = tar.read(buffer)
                            if (n < 0) break
                            digest.update(buffer, 0, n)
                        }
                        val hash = digest.digest().toHex()
                        if (hash != wanted.sha256) throw CorruptUserland("SHA-256 of $path: $hash, manifest ${wanted.sha256}")
                        written += entry.size
                        progress(written, total)
                        continue
                    }
                    FileOutputStream(out.toFile()).use { file ->
                        val write = { bytes: ByteArray, offset: Int, length: Int ->
                            file.write(bytes, offset, length)
                            writtenDigest.update(bytes, offset, length)
                        }
                        val rewriter = wanted.placeholders.takeIf { it.isNotEmpty() }
                            ?.let { PlaceholderWriter(it, relocation.placeholder, relocation.replacement(wanted), write) }
                        while (true) {
                            val n = tar.read(buffer)
                            if (n < 0) break
                            digest.update(buffer, 0, n)
                            if (rewriter != null) rewriter.write(buffer, n) else write(buffer, 0, n)
                        }
                        rewriter?.finish()
                        file.fd.sync()
                    }
                    val hash = digest.digest().toHex()
                    if (hash != wanted.sha256) throw CorruptUserland("SHA-256 of $path: $hash, manifest ${wanted.sha256}")
                    Files.setPosixFilePermissions(out, permissions(entry.mode))
                    hashes[path] = writtenDigest.digest().toHex()
                    written += entry.size
                    progress(written, total)
                }
                TarReader.SYMLINK -> {
                    if (wanted.type != "symlink" || entry.linkName != wanted.target) {
                        throw CorruptUserland("symlink $path -> ${entry.linkName}, manifest ${wanted.type} ${wanted.target}")
                    }
                    Files.createSymbolicLink(out, Paths.get(entry.linkName))
                    links += path
                }
                else -> throw CorruptUserland("unsupported tar entry type '${entry.type}' (hard links are refused): $path")
            }
        }
        val missing = expected.keys - seen
        if (missing.isNotEmpty()) throw CorruptUserland("${missing.size} manifest paths missing, first ${missing.min()}")
        // Deepest first, so a read-only folder does not block its contents.
        for ((dir, mode) in directories.asReversed()) Files.setPosixFilePermissions(dir, permissions(mode))

        val record = staging.resolve(RECORD).toFile()
        FileOutputStream(record).use {
            val written = UserlandRecord(manifest.version, current.toString(), hashes)
            it.write(json.encodeToString(UserlandRecord.serializer(), written).toByteArray())
            it.fd.sync()
        }
        // The files are synced; their names live in the folders, deepest first, staging last.
        for ((dir, _) in directories.asReversed()) syncDirectory(dir)
        syncDirectory(staging)
    }

    private fun expect(wanted: ManifestEntry, type: String, mode: Int) {
        if (wanted.type != type) throw CorruptUserland("${wanted.path} is a $type in the archive, ${wanted.type} in the manifest")
        if (mode != wanted.mode.toInt(8)) throw CorruptUserland("mode of ${wanted.path}: %04o, manifest ${wanted.mode}".format(mode))
        if (mode and "7000".toInt(8) != 0) throw CorruptUserland("setuid, setgid or sticky bit on ${wanted.path}")
    }

    companion object {
        const val CURRENT = "current"
        const val RECORD = "record.json"
        const val STARTED = "started"
        private const val STAGING = ".staging"
        private const val CURRENT_TEMPORARY = "current.tmp"

        private fun isPlainName(name: String) =
            name.isNotEmpty() && name != CURRENT && !name.contains('/') && !name.startsWith(".") && !name.endsWith(STAGING)

        private fun permissions(mode: Int): Set<PosixFilePermission> {
            // PosixFilePermission's order is owner r/w/x, group r/w/x, others r/w/x.
            return PosixFilePermission.entries.filterIndexed { i, _ -> mode and (0x100 shr i) != 0 }.toSet()
        }

        private fun ByteArray.toHex(): String = joinToString("") { "%02x".format(it) }

        /** Deletes [path] and everything below it without following symlinks. */
        fun deleteTree(path: Path) = deleteTree(path, unlocked = null)

        /** [unlocked]: the folder just made listable, which is not unlocked again. */
        private fun deleteTree(path: Path, unlocked: Path?) {
            if (!Files.exists(path, LinkOption.NOFOLLOW_LINKS)) return
            Files.walkFileTree(path, object : SimpleFileVisitor<Path>() {
                override fun visitFile(file: Path, attrs: BasicFileAttributes): FileVisitResult {
                    Files.delete(file)
                    return FileVisitResult.CONTINUE
                }

                override fun preVisitDirectory(dir: Path, attrs: BasicFileAttributes): FileVisitResult {
                    // A folder without write or search permission must have both to be emptied:
                    // owner-only, set through the folder opened without following a link.
                    Files.setAttribute(dir, "posix:permissions", PosixFilePermissions.fromString("rwx------"), LinkOption.NOFOLLOW_LINKS)
                    return FileVisitResult.CONTINUE
                }

                override fun postVisitDirectory(dir: Path, exc: IOException?): FileVisitResult {
                    if (exc != null) throw exc
                    Files.delete(dir)
                    return FileVisitResult.CONTINUE
                }

                override fun visitFileFailed(file: Path, exc: IOException): FileVisitResult {
                    // A folder without read permission cannot be listed, so preVisitDirectory never
                    // sees it: a real folder (checked without following links) is made owner-only
                    // and walked again, once.
                    if (file == unlocked || exc !is AccessDeniedException || !Files.isDirectory(file, LinkOption.NOFOLLOW_LINKS)) throw exc
                    Files.setPosixFilePermissions(file, PosixFilePermissions.fromString("rwx------"))
                    deleteTree(file, unlocked = file)
                    return FileVisitResult.CONTINUE
                }
            })
        }
    }
}

class CorruptUserland(message: String) : IOException(message)

/** fsync on a folder, through Android's `Os` (libcore's FileChannel refuses to open a folder). */
internal fun fsyncDirectory(dir: Path) {
    val fd = android.system.Os.open(dir.toString(), android.system.OsConstants.O_RDONLY, 0)
    try {
        android.system.Os.fsync(fd)
    } finally {
        android.system.Os.close(fd)
    }
}

private val json = Json { ignoreUnknownKeys = true }

/**
 * A reader for the GNU tar stream pack.sh writes: regular files, folders, symlinks, and GNU
 * long names ('L') and long link targets ('K'). Other entry types are returned for the caller
 * to refuse. A truncated stream or a bad header checksum throws [CorruptUserland].
 */
internal class TarReader(private val input: InputStream) {
    class Entry(val name: String, val type: Char, val mode: Int, val size: Long, val linkName: String)

    private val header = ByteArray(BLOCK)
    private var remaining = 0L
    private var padding = 0L

    /** The next entry, or null at the end-of-archive block. Skips any unread data of the previous one. */
    fun next(): Entry? {
        skip(remaining + padding)
        remaining = 0
        padding = 0
        var longName: String? = null
        var longLink: String? = null
        while (true) {
            readFully(header, BLOCK)
            if (header.all { it == 0.toByte() }) return null
            val stored = octal(148, 8)
            var sum = 0L
            for (i in 0 until BLOCK) sum += if (i in 148 until 156) 0x20 else header[i].toInt() and 0xFF
            if (stored != sum) throw CorruptUserland("tar header checksum mismatch")
            val type = (header[156].toInt() and 0xFF).toChar()
            val size = octal(124, 12)
            if (type == 'L' || type == 'K') {
                if (size > 65536) throw CorruptUserland("tar long name of $size bytes")
                val data = ByteArray(size.toInt())
                readFully(data, data.size)
                skip(pad(size))
                val text = cString(data, 0, data.size)
                if (type == 'L') longName = text else longLink = text
                continue
            }
            var name = cString(header, 0, 100)
            // POSIX ustar ("ustar\0") keeps a prefix; GNU tar ("ustar  ") uses that area for other fields.
            if (String(header, 257, 6, Charsets.US_ASCII) == "ustar\u0000") {
                val prefix = cString(header, 345, 155)
                if (prefix.isNotEmpty()) name = "$prefix/$name"
            }
            remaining = if (type == FILE || type == FILE_OLD) size else 0
            padding = if (type == FILE || type == FILE_OLD) pad(size) else 0
            return Entry(longName ?: name, type, octal(100, 8).toInt(), size, longLink ?: cString(header, 157, 100))
        }
    }

    /** Reads the current entry's data; -1 at its end. */
    fun read(buffer: ByteArray): Int {
        if (remaining == 0L) return -1
        val n = input.read(buffer, 0, minOf(buffer.size.toLong(), remaining).toInt())
        if (n < 0) throw CorruptUserland("tar stream truncated")
        remaining -= n
        return n
    }

    private fun pad(size: Long) = (BLOCK - size % BLOCK) % BLOCK

    private fun octal(offset: Int, length: Int): Long {
        if (header[offset].toInt() and 0x80 != 0) throw CorruptUserland("unsupported base-256 tar number")
        val text = cString(header, offset, length).trim()
        if (text.isEmpty()) return 0
        return text.toLongOrNull(8) ?: throw CorruptUserland("bad tar number: $text")
    }

    private fun cString(bytes: ByteArray, offset: Int, length: Int): String {
        var end = offset
        while (end < offset + length && bytes[end] != 0.toByte()) end++
        return String(bytes, offset, end - offset, Charsets.UTF_8)
    }

    private fun readFully(buffer: ByteArray, length: Int) {
        var done = 0
        while (done < length) {
            val n = input.read(buffer, done, length - done)
            if (n < 0) throw CorruptUserland("tar stream truncated")
            done += n
        }
    }

    private fun skip(count: Long) {
        var left = count
        val scratch = ByteArray(BLOCK)
        while (left > 0) {
            val n = input.read(scratch, 0, minOf(left, BLOCK.toLong()).toInt())
            if (n < 0) throw CorruptUserland("tar stream truncated")
            left -= n
        }
    }

    companion object {
        const val BLOCK = 512
        const val FILE = '0'
        const val FILE_OLD = '\u0000'
        const val SYMLINK = '2'
        const val DIRECTORY = '5'
    }
}
