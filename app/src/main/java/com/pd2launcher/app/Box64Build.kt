package com.pd2launcher.app

import java.io.File
import java.io.FileInputStream
import java.io.InputStream
import java.nio.file.Files
import java.nio.file.LinkOption
import java.nio.file.StandardCopyOption
import java.security.MessageDigest
import java.time.Instant
import java.time.ZoneOffset

/**
 * Settings → Testing's "Box64 build" (test builds only, and only when the APK carries a second
 * wowbox64.dll in its assets, [ASSET_FOLDER]): which Box64 the game runs on, the runtime's own
 * (pinned) or the second one. Wine loads wowbox64.dll from the runtime ([RUNTIME_DLL]); each
 * prefix and template links to that file, and no file names its hash, so a copy of the chosen DLL
 * into its place, while no Wine runs, is the whole switch ([apply]). The runtime's own DLL is kept
 * beside it first ([KEEP_FOLDER]) and comes back for Pinned. A new runtime (an app update) puts its
 * own DLL in place again, which then counts as the pinned one.
 */
object Box64Build {
    /** The runtime's DLL, below the managed root. */
    const val RUNTIME_DLL = "runner/wine-11.0-arm64/lib/wine/aarch64-windows/wowbox64.dll"

    /** The APK's assets folder of the second DLL: `wowbox64.dll`, its `wowbox64.dll.sha256` and the build's `BUILDINFO`. */
    const val ASSET_FOLDER = "box64-alt"

    /** Below the managed root: the kept copies (`pinned.dll`, `other.dll`). */
    const val KEEP_FOLDER = "box64-test"

    /** The runtime's Box64: its commit (android/runtime/config.sh BOX64_COMMIT) and the commit's date. */
    const val PINNED_COMMIT = "a489cfe7"
    const val PINNED_DATE = "2026-10-09"

    /** The service log's and device.txt's line, before the build ([line]). */
    const val LINE = "box64 build"

    /** The player's choice; [PINNED] is the default and what Reset all gives. */
    enum class Choice { PINNED, UPSTREAM }

    /** The second DLL: its Box64 [commit] (full), the commit's [date] (yyyy-mm-dd) and the DLL's [sha256]. */
    data class Other(val commit: String, val date: String, val sha256: String) {
        val short: String get() = commit.take(8)
    }

    /** The stored value (an enum name) as a choice; anything else is [Choice.PINNED]. */
    fun parse(stored: String?): Choice = Choice.entries.firstOrNull { it.name == stored } ?: Choice.PINNED

    /** [choice] as the stored value; null for the default. */
    fun stored(choice: Choice): String? = choice.name.takeIf { choice != Choice.PINNED }

    /** The choice that applies: the stored one in a test build that has the second DLL ([other]), else Pinned. */
    fun active(stored: String?, testBuild: Boolean, other: Other?): Choice =
        if (testBuild && other != null) parse(stored) else Choice.PINNED

    /** "box64 build: pinned a489cfe7" or "box64 build: upstream 50855a1e". */
    fun line(choice: Choice, other: Other?): String = when (choice) {
        Choice.PINNED -> "$LINE: pinned $PINNED_COMMIT"
        Choice.UPSTREAM -> "$LINE: upstream ${other?.short ?: "unknown"}"
    }

    /**
     * The second DLL's facts from its assets: [buildInfo] (its first line `box64=<commit> <archive
     * sha256> <date epoch>`) and [sha256File] (`<sha256>` first); null when either does not say.
     */
    fun other(buildInfo: String?, sha256File: String?): Other? {
        val fields = buildInfo?.lineSequence()?.firstOrNull()?.removePrefix("box64=")?.split(' ') ?: return null
        if (buildInfo.startsWith("box64=").not() || fields.size < 3) return null
        val commit = fields[0].takeIf { it.length >= 8 && it.all { c -> c in '0'..'9' || c in 'a'..'f' } } ?: return null
        val epoch = fields[2].toLongOrNull() ?: return null
        val sha = sha256File?.trim()?.substringBefore(' ')?.lowercase()?.takeIf { it.length == 64 && it.all { c -> c in '0'..'9' || c in 'a'..'f' } }
            ?: return null
        val date = Instant.ofEpochSecond(epoch).atZone(ZoneOffset.UTC).toLocalDate().toString()
        return Other(commit, date, sha)
    }

    /** The second DLL's facts from the APK's assets ([read]: a text asset by its path, null when missing). */
    fun otherFromAssets(read: (String) -> String?): Other? =
        other(read("$ASSET_FOLDER/BUILDINFO"), read("$ASSET_FOLDER/wowbox64.dll.sha256"))

    /** The SHA-256 of [file], in lower-case hex. */
    fun sha256(file: File): String = FileInputStream(file).use { sha256(it) }

    fun sha256(stream: InputStream): String {
        val digest = MessageDigest.getInstance("SHA-256")
        val buffer = ByteArray(1 shl 16)
        while (true) {
            val read = stream.read(buffer)
            if (read < 0) break
            digest.update(buffer, 0, read)
        }
        return digest.digest().joinToString("") { "%02x".format(it) }
    }

    /** What [apply] did. */
    enum class Outcome {
        /** No runtime DLL (the runtime is not unpacked). */
        NO_RUNTIME,

        /** The chosen DLL is in place already. */
        UNCHANGED,

        /** The chosen DLL was copied into place and its SHA-256 checked. */
        WRITTEN,

        /** Pinned is chosen, but the runtime's own DLL was never kept (and is not in place). */
        NO_PINNED_COPY,
    }

    /**
     * The DLL of [choice] into the runtime of the managed root [root] (the caller holds the locks and
     * no Wine runs). [other] is the second DLL's facts, [openOther] its bytes (the APK's asset). A
     * DLL in place that is not the second one is the runtime's own: it is kept first
     * ([KEEP_FOLDER]/pinned.dll). Each copy goes to a temporary file beside its target, is synced,
     * has its SHA-256 checked, and is renamed into place, whose SHA-256 is checked again; a check
     * that fails throws, and the file in place stays as it was. The DLL in place keeps its mode.
     */
    fun apply(root: File, choice: Choice, other: Other, openOther: () -> InputStream): Outcome {
        val dll = File(root, RUNTIME_DLL)
        if (!Files.isRegularFile(dll.toPath(), LinkOption.NOFOLLOW_LINKS)) return Outcome.NO_RUNTIME
        val keep = File(root, KEEP_FOLDER)
        val pinned = File(keep, "pinned.dll")
        val current = sha256(dll)
        if (current != other.sha256 && (!pinned.isFile || sha256(pinned) != current)) {
            keep.mkdirs()
            copyChecked({ FileInputStream(dll) }, pinned, current, mode = null)
        }
        val (source, wanted) = when (choice) {
            Choice.UPSTREAM -> {
                val kept = File(keep, "other.dll")
                if (!kept.isFile || sha256(kept) != other.sha256) {
                    keep.mkdirs()
                    copyChecked(openOther, kept, other.sha256, mode = null)
                }
                kept to other.sha256
            }
            Choice.PINNED -> {
                if (!pinned.isFile) return if (current != other.sha256) Outcome.UNCHANGED else Outcome.NO_PINNED_COPY
                pinned to sha256(pinned)
            }
        }
        if (current == wanted) return Outcome.UNCHANGED
        copyChecked({ FileInputStream(source) }, dll, wanted, mode = dll)
        return Outcome.WRITTEN
    }

    /**
     * [open]'s bytes into [target] through a temporary file beside it (synced, its SHA-256 checked
     * against [sha256], with the mode of [mode] when given), renamed over [target], whose SHA-256 is
     * checked again.
     */
    private fun copyChecked(open: () -> InputStream, target: File, sha256: String, mode: File?) {
        val temporary = File(target.parentFile, target.name + ".new")
        open().use { input ->
            java.io.FileOutputStream(temporary).use { output ->
                input.copyTo(output, 1 shl 16)
                output.fd.sync()
            }
        }
        val written = sha256(temporary)
        if (written != sha256) {
            temporary.delete()
            throw java.io.IOException("${target.name}: the copy has SHA-256 $written, not $sha256")
        }
        if (mode != null) {
            runCatching { Files.setPosixFilePermissions(temporary.toPath(), Files.getPosixFilePermissions(mode.toPath())) }
        }
        Files.move(temporary.toPath(), target.toPath(), StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE)
        val placed = sha256(target)
        if (placed != sha256) throw java.io.IOException("${target.name}: in place it has SHA-256 $placed, not $sha256")
    }

    /** Whether [choice] is not the DLL in place in [root] (false without a runtime DLL). */
    fun differs(root: File, choice: Choice, other: Other?): Boolean {
        val dll = File(root, RUNTIME_DLL)
        if (other == null || !dll.isFile) return false
        return (sha256(dll) == other.sha256) != (choice == Choice.UPSTREAM)
    }
}
