package com.pd2launcher.app

import java.io.IOException
import java.nio.file.Files
import java.nio.file.LinkOption
import java.nio.file.Path
import java.nio.file.Paths
import java.nio.file.attribute.BasicFileAttributes

/**
 * The check of [manifest]'s unpacked version before an engine start ([UserlandInstaller.forgetIfChanged]).
 * Every program starts through `<userland>/lib/ld-linux-aarch64.so.1`, and `lib` is a link, so
 * nothing on the way is taken on trust: each path is walked one part at a time from the data folder
 * without following a link. Each folder must be a real folder, each link must name the target the
 * manifest gives it (then the walk goes on there), and the file at the end must be the manifest's:
 * its placeholders, rewritten by the unpack, are put back for the comparison, so the reference is
 * the APK's own hash, not a record inside the tree that the same code could rewrite.
 */
internal class CriticalFiles(
    private val manifest: UserlandManifest,
    private val relocation: Relocation,
    /** `<data>/files/userland`, the folder that holds `current` and the versions. */
    private val userlandRoot: Path,
) {
    private val entries = manifest.entries.associateBy { it.path }

    /** What differs, one line each; empty when everything is as the manifest says. */
    fun problems(): List<String> {
        chain()?.let { return listOf(it) }
        val tree = userlandRoot.resolve(manifest.version)
        return manifest.critical.mapNotNull { walk(tree, it, mustExist = true) } +
            manifest.absent.mapNotNull { walk(tree, it, mustExist = false) }
    }

    /**
     * From the data folder to the version: the folders between it and `current` are real folders,
     * `current` is a link naming the version, and the version is a real folder.
     */
    private fun chain(): String? {
        val parts = manifest.prefix.removePrefix(manifest.placeholder).trim('/').split('/')
        var path = Paths.get(relocation.dataDir)
        for (part in parts.dropLast(1)) {
            path = path.resolve(part)
            if (attributes(path)?.isDirectory != true) return "$path: not a folder"
        }
        val current = path.resolve(parts.last())
        if (current != userlandRoot.resolve(UserlandInstaller.CURRENT)) return "$current: not the installer's"
        if (attributes(current)?.isSymbolicLink != true || target(current) != manifest.version) return "$current: does not name ${manifest.version}"
        val tree = userlandRoot.resolve(manifest.version)
        return if (attributes(tree)?.isDirectory != true) "$tree: not a folder" else null
    }

    /**
     * [path] below [tree] as a program opens it. With [mustExist] false, every folder on the way
     * must be as the manifest says and the last part must not exist.
     */
    private fun walk(tree: Path, path: String, mustExist: Boolean): String? {
        var parts = path.split('/')
        var hops = 0
        var i = 0
        while (i < parts.size) {
            val rel = parts.subList(0, i + 1).joinToString("/")
            val onDisk = tree.resolve(rel)
            val found = attributes(onDisk)
            if (!mustExist && i == parts.lastIndex) return if (found == null) null else "$rel: must not exist"
            val wanted = entries[rel] ?: return "$rel: not in the manifest"
            if (found == null) return "$rel: missing"
            when (wanted.type) {
                "dir" -> if (!found.isDirectory) return "$rel: not a folder"
                "symlink" -> {
                    if (!found.isSymbolicLink || target(onDisk) != wanted.target) return "$rel: not the link to ${wanted.target}"
                    // The tree's links are relative and stay inside it (check-tree.sh).
                    val joined = join(parts.subList(0, i) + wanted.target!!.split('/') + parts.subList(i + 1, parts.size))
                    if (joined == null || ++hops > MAX_LINKS) return "$rel: its link leads out of the tree"
                    parts = joined
                    i = 0
                    continue
                }
                "file" -> {
                    if (i != parts.lastIndex) return "$rel: a file where a folder should be"
                    if (!found.isRegularFile) return "$rel: not a regular file"
                    if (!matches(onDisk, found, wanted)) return "$rel: changed"
                }
                else -> return "$rel: unknown type ${wanted.type}"
            }
            i++
        }
        return null
    }

    /** True when [file] is [wanted] as the unpack wrote it. */
    private fun matches(file: Path, found: BasicFileAttributes, wanted: ManifestEntry): Boolean {
        if (relocation.offsetProblem(wanted) != null || found.size() != relocation.rewrittenSize(wanted)) return false
        return try {
            restoredSha256(file, relocation.rewrittenAt(wanted), relocation.replacement(wanted), relocation.placeholder) == wanted.sha256
        } catch (_: IOException) {
            false
        }
    }

    private fun attributes(path: Path): BasicFileAttributes? = try {
        Files.readAttributes(path, BasicFileAttributes::class.java, LinkOption.NOFOLLOW_LINKS)
    } catch (_: IOException) {
        null
    }

    private fun target(link: Path): String? = try {
        Files.readSymbolicLink(link).toString()
    } catch (_: IOException) {
        null
    }

    private companion object {
        const val MAX_LINKS = 16

        /** [parts] with `.` and empty parts dropped and `..` applied; null if it climbs out. */
        fun join(parts: List<String>): List<String>? {
            val joined = ArrayList<String>()
            for (part in parts) {
                when (part) {
                    "", "." -> Unit
                    ".." -> if (joined.isEmpty()) return null else joined.removeAt(joined.lastIndex)
                    else -> joined += part
                }
            }
            return joined.takeIf { it.isNotEmpty() }
        }
    }
}
