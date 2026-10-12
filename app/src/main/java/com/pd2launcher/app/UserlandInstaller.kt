package com.pd2launcher.app

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import java.io.File
import java.io.FileOutputStream
import java.io.IOException
import java.io.InputStream
import java.nio.file.FileVisitResult
import java.nio.file.Files
import java.nio.file.LinkOption
import java.nio.file.Path
import java.nio.file.Paths
import java.nio.file.SimpleFileVisitor
import java.nio.file.StandardCopyOption
import java.nio.file.attribute.BasicFileAttributes
import java.nio.file.attribute.PosixFilePermission
import java.security.MessageDigest

/** userland/pack.sh's manifest.json (the archive's own fields are not needed here). */
@Serializable
data class UserlandManifest(val version: String, val prefix: String, val entries: List<ManifestEntry>) {
    /** Bytes of every regular file, unpacked. */
    val fileBytes: Long get() = entries.sumOf { it.size ?: 0L }

    companion object {
        fun parse(text: String): UserlandManifest = json.decodeFromString(serializer(), text)
    }
}

/** One path of the tree: `type` is `file` (with size and sha256), `dir` or `symlink` (with target). */
@Serializable
data class ManifestEntry(
    val path: String,
    val type: String,
    val mode: String,
    val size: Long? = null,
    val sha256: String? = null,
    val target: String? = null,
)

/** `<version>/record.json`, written last: the version and the SHA-256 of every file. */
@Serializable
data class UserlandRecord(val version: String, val files: Map<String, String>)

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
        /** The archive does not match the manifest, or is for another prefix; nothing of it was kept and `current` is unchanged. */
        REFUSED,
    }
}

/**
 * Unpacks the App userland into `<root>/<version>` and points `<root>/current` at it.
 *
 * The tar stream unpacks into `<version>.staging`; every entry is checked against the
 * manifest (type, mode, symlink target, SHA-256 of each file), hard links and anything not in
 * the manifest are refused, `record.json` is written last, and the staging folder is renamed
 * to `<version>`. `current` is a relative symlink replaced by renaming a temporary one over it.
 * The previous version stays until the current one has started once ([markStarted] removes it).
 */
class UserlandInstaller(
    private val root: File,
    private val processes: UserlandProcesses,
    /** Makes a folder's entries durable (fsync on the folder); the JVM tests record the calls. */
    private val syncDirectory: (Path) -> Unit = ::fsyncDirectory,
    private val log: (String) -> Unit = {},
) {
    private val current: Path get() = root.toPath().resolve(CURRENT)

    /** The version `current` names, or null when it is absent. */
    fun currentVersion(): String? =
        if (Files.isSymbolicLink(current)) Files.readSymbolicLink(current).toString() else null

    /** True when `<version>/record.json` exists and names [version]. */
    fun isComplete(version: String): Boolean = try {
        val record = File(root, "$version/$RECORD")
        record.isFile && json.decodeFromString(UserlandRecord.serializer(), record.readText()).version == version
    } catch (_: Exception) {
        false
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
     * A userland built for another prefix than `<root>/current` (another app's, such as the
     * release app's in the development build) is refused before anything is touched, so the root
     * must be the canonical data folder's ([SessionPaths.userlandRoot]).
     */
    fun install(
        manifest: UserlandManifest,
        openTar: () -> InputStream,
        progress: (Long, Long) -> Unit = { _, _ -> },
    ): InstallResult {
        val version = manifest.version
        require(isPlainName(version)) { "bad version: $version" }
        if (manifest.prefix != current.toString()) {
            log("refused $version: built for ${manifest.prefix}, not $current")
            return InstallResult(InstallResult.Outcome.REFUSED, version, "built for ${manifest.prefix}, not $current")
        }
        root.mkdirs()
        removeLeftovers(version)

        if (!isComplete(version)) {
            val target = File(root, version).toPath()
            val staging = File(root, "$version$STAGING").toPath()
            deleteTree(target) // only an incomplete tree can be here; current never names one
            Files.createDirectories(staging)
            try {
                openTar().use { unpack(manifest, it, staging, progress) }
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

    private fun unpack(manifest: UserlandManifest, input: InputStream, staging: Path, progress: (Long, Long) -> Unit) {
        val expected = manifest.entries.associateBy { it.path }
        val seen = HashSet<String>()
        val hashes = sortedMapOf<String, String>()
        val directories = ArrayList<Pair<Path, Int>>()
        val total = manifest.fileBytes
        var written = 0L
        val buffer = ByteArray(1 shl 18)
        val digest = MessageDigest.getInstance("SHA-256")
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
                    digest.reset()
                    FileOutputStream(out.toFile()).use { file ->
                        while (true) {
                            val n = tar.read(buffer)
                            if (n < 0) break
                            digest.update(buffer, 0, n)
                            file.write(buffer, 0, n)
                        }
                        file.fd.sync()
                    }
                    val hash = digest.digest().toHex()
                    if (hash != wanted.sha256) throw CorruptUserland("SHA-256 of $path: $hash, manifest ${wanted.sha256}")
                    Files.setPosixFilePermissions(out, permissions(entry.mode))
                    hashes[path] = hash
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
            it.write(json.encodeToString(UserlandRecord.serializer(), UserlandRecord(manifest.version, hashes)).toByteArray())
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
        fun deleteTree(path: Path) {
            if (!Files.exists(path, LinkOption.NOFOLLOW_LINKS)) return
            Files.walkFileTree(path, object : SimpleFileVisitor<Path>() {
                override fun visitFile(file: Path, attrs: BasicFileAttributes): FileVisitResult {
                    Files.delete(file)
                    return FileVisitResult.CONTINUE
                }

                override fun preVisitDirectory(dir: Path, attrs: BasicFileAttributes): FileVisitResult {
                    // A folder unpacked without write permission must be writable to empty it.
                    dir.toFile().setWritable(true, true)
                    return FileVisitResult.CONTINUE
                }

                override fun postVisitDirectory(dir: Path, exc: IOException?): FileVisitResult {
                    if (exc != null) throw exc
                    Files.delete(dir)
                    return FileVisitResult.CONTINUE
                }
            })
        }
    }
}

class CorruptUserland(message: String) : IOException(message)

/** fsync on a folder, through Android's `Os` (libcore's FileChannel refuses to open a folder). */
private fun fsyncDirectory(dir: Path) {
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
