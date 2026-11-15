package com.pd2launcher.app

import java.io.File
import java.io.IOException
import java.io.InputStream
import java.nio.file.Files
import java.nio.file.StandardCopyOption

/**
 * The loot filter files in the game's `ProjectD2` folder ([projectD2]), where the official PD2
 * launcher keeps them: the filter in use is `loot.filter`; an author's file is downloaded into
 * `filters/online`, the player's own files are in `filters/local`. The owner approved these writes,
 * and only from the Loot filters page and its update at Play (2026-11-05). Every write goes through
 * a temporary file and a rename, so the game never reads half a file. Names are checked first
 * ([LootFilters.safeName]), so no write leaves these folders.
 */
class LootFilterFiles(val projectD2: File) {
    val inUse = File(projectD2, IN_USE)
    val online = File(projectD2, "filters/online")
    val local = File(projectD2, "filters/local")

    /** The official launcher's settings file, where it keeps its own loot filter pick (read only; never written). */
    val launcherSettings = File(projectD2, "AppData/launcherSettings.json")

    /** The git SHA of `loot.filter` ([LootFilters.gitSha]); null: none, too large or unreadable. */
    fun inUseSha(): String? = sha(inUse)

    /** The git SHA of the player's own file [name] now; null: none or unreadable. */
    fun ownSha(name: String): String? = if (LootFilters.safeName(name)) sha(File(local, name)) else null

    /** The official launcher's pick ([LootFilters.launcherPick]); null: none or unreadable. */
    fun launcherPick(): LootFilters.LauncherPick? = try {
        launcherSettings.takeIf { it.isFile && it.length() < (1 shl 20) }?.readText()?.let(LootFilters::launcherPick)
    } catch (_: IOException) {
        null
    }

    private fun sha(file: File): String? = try {
        file.takeIf { it.isFile && it.length() <= LootFilters.MAX_FILTER_BYTES }?.readBytes()?.let(LootFilters::gitSha)
    } catch (_: IOException) {
        null
    }

    /** One of the player's own files. */
    data class Own(val name: String, val size: Long)

    /** The player's own `.filter` files, by name. */
    fun own(): List<Own> =
        local.listFiles { f -> f.isFile && LootFilters.safeName(f.name) }.orEmpty()
            .map { Own(it.name, it.length()) }.sortedBy { it.name.lowercase() }

    /** An author's file [name], downloaded ([bytes]), into `filters/online`, and then in use. */
    fun useOnline(name: String, bytes: ByteArray) {
        val file = File(online, checked(name))
        write(file, bytes)
        write(inUse, bytes)
    }

    /** The player's own file [name] in use. */
    fun useOwn(name: String) {
        val file = File(local, checked(name))
        if (!file.isFile) throw IOException("no file $name in your files")
        write(inUse, file.readBytes())
    }

    /** An author's file [name] ([bytes]) as the player's own copy, which does not update. Replaces an own file of the same name, as the official launcher. */
    fun keep(name: String, bytes: ByteArray) = write(File(local, checked(name)), bytes)

    /**
     * A file the player chose ([input], named [name]) as one of their own: at most
     * [LootFilters.MAX_FILTER_BYTES]; replaces an own file of the same name.
     */
    fun add(name: String, input: InputStream) {
        val bytes = readAtMost(input, LootFilters.MAX_FILTER_BYTES + 1)
        if (bytes.size > LootFilters.MAX_FILTER_BYTES) throw IOException("the file is larger than ${LootFilters.MAX_FILTER_BYTES} bytes")
        if (bytes.isEmpty()) throw IOException("the file is empty")
        write(File(local, checked(name)), bytes)
    }

    /** Removes the player's own file [name]. `loot.filter` keeps what it holds. */
    fun remove(name: String) {
        val file = File(local, checked(name))
        if (file.exists() && !file.delete()) throw IOException("could not remove $name")
    }

    private fun checked(name: String): String {
        if (!LootFilters.safeName(name)) throw IOException("not a loot filter file name: $name")
        return name
    }

    companion object {
        const val IN_USE = "loot.filter"

        /** The `ProjectD2` folder of the game folder [game]. */
        fun of(game: File) = LootFilterFiles(File(game, "ProjectD2"))

        /** At most [limit] bytes of [input] (InputStream.readNBytes is API 33; the app's minSdk is 30). */
        fun readAtMost(input: InputStream, limit: Int): ByteArray {
            val out = java.io.ByteArrayOutputStream()
            val buffer = ByteArray(64 * 1024)
            while (out.size() < limit) {
                val n = input.read(buffer, 0, minOf(buffer.size, limit - out.size()))
                if (n < 0) break
                out.write(buffer, 0, n)
            }
            return out.toByteArray()
        }

        /** [bytes] into [file] through a temporary file beside it and a rename. */
        fun write(file: File, bytes: ByteArray) {
            val folder = file.parentFile!!
            if (!folder.isDirectory && !folder.mkdirs()) throw IOException("could not make $folder")
            val part = File(folder, ".${file.name}.part")
            part.writeBytes(bytes)
            try {
                Files.move(part.toPath(), file.toPath(), StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE)
            } catch (error: IOException) {
                part.delete()
                throw error
            }
        }
    }
}
