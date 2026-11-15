package com.pd2launcher.app

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive

/**
 * Loot filters as the official PD2 launcher gets them (docs/design/loot-filters/README.md): PD2's
 * list of authors ([AUTHORS_URL]), each with a GitHub folder whose `.filter` files are the author's
 * filters, named by the folder's `filter_definitions.json` when it has one. The rules only, without
 * Android or the network: [LootFilterStore] does the work, [LootFilterFiles] the game folder's files.
 */
object LootFilters {
    /** PD2's list of filter authors, the one the official launcher reads. */
    const val AUTHORS_URL = "https://raw.githubusercontent.com/Project-Diablo-2/LootFilters/main/filters.json"

    /** "What is a loot filter?": PD2's loot filter page, as the official launcher's Help. */
    const val HELP_URL = "https://github.com/Project-Diablo-2/LootFilters"

    /** Each list (the authors, an author's filters) is read from GitHub at most once in this time (owner, 2026-11-05). */
    const val READ_EVERY_MS = 60_000L

    /** The most read of a list: PD2's author list and a folder's listing are a few kB. */
    const val MAX_LIST_BYTES = 1 shl 20

    /** The largest filter taken: PD2's filters are below 1 MB (Kassahi's 868 kB, 2026-11-05). */
    const val MAX_FILTER_BYTES = 8 shl 20

    /** The author of the player's own files (the folder `filters/local`), as the official launcher's "Local Filter". */
    const val LOCAL = "local"

    private const val API = "https://api.github.com/repos/"
    private const val RAW = "https://raw.githubusercontent.com/"
    private val PATH = Regex("[A-Za-z0-9._~/-]+")
    /** A download address's path: plain characters, `+` and percent escapes (a file "crafting+revealed.filter"). */
    private val FILE_PATH = Regex("[A-Za-z0-9._~/%+-]+")
    private val NAME = Regex("[A-Za-z0-9 _.,'()+&#\\[\\]-]{1,120}")
    private val json = Json { ignoreUnknownKeys = true; isLenient = true }

    /** An author on PD2's list: [name] above the filters, [author] in the list, [url] the GitHub folder (contents API). */
    @Serializable
    data class Author(val name: String = "", val url: String = "", val author: String = "")

    /** An entry of a GitHub folder (contents API). */
    @Serializable
    data class Entry(
        val name: String = "",
        val type: String = "",
        val sha: String? = null,
        val size: Long = 0,
        @SerialName("download_url") val downloadUrl: String? = null,
    )

    /** An author's `filter_definitions.json`: id → its name, description and file. */
    @Serializable
    data class Definitions(@SerialName("filter_info") val info: Map<String, Definition> = emptyMap())

    @Serializable
    data class Definition(
        @SerialName("display_name") val displayName: String = "",
        val description: String = "",
        @SerialName("file_name") val fileName: String = "",
    )

    /** A filter of an author: its [display] name, [description] (empty: none), its [file] in the folder, the file's git [sha], [size] and [url]. */
    @Serializable
    data class Filter(
        val display: String,
        val description: String,
        val file: String,
        val sha: String?,
        val size: Long,
        val url: String,
    )

    /**
     * The filter in use: an author's (by the author's short name, [author]) or the player's own
     * ([LOCAL]); its [file], [display] name, and for an author's the [sha] of the file in use and
     * the author's folder ([folder]), so that Play can look for a newer file.
     */
    @Serializable
    data class Choice(
        val author: String,
        val file: String,
        val display: String,
        val sha: String? = null,
        val folder: String? = null,
        /** The git SHA ([gitSha]) of the bytes that the app wrote to `loot.filter`; null in choices saved before 0.4.0. */
        val written: String? = null,
    ) {
        val local: Boolean get() = author == LOCAL
    }

    /** The authors in PD2's list [text] whose folder is a GitHub folder ([usableFolder]); null: not such a list. */
    fun authors(text: String): List<Author>? = try {
        json.decodeFromString(ListSerializer(Author.serializer()), text)
            // An author named as the player's own files would be taken for them (and share their list key).
            .filter { usableFolder(it.url) && it.author.isNotBlank() && !it.author.equals(LOCAL, ignoreCase = true) }
            .distinctBy { it.author }
    } catch (_: Exception) {
        null
    }

    /** The entries of a GitHub folder's listing [text]; null: not a listing. */
    fun entries(text: String): List<Entry>? = try {
        json.decodeFromString(ListSerializer(Entry.serializer()), text)
    } catch (_: Exception) {
        null
    }

    /** The definitions in [text]; null: none that read. */
    fun definitions(text: String): Definitions? = try {
        json.decodeFromString(Definitions.serializer(), text).takeIf { it.info.isNotEmpty() }
    } catch (_: Exception) {
        null
    }

    /** The folder's `filter_definitions.json` address, when it has one. */
    fun definitionsUrl(entries: List<Entry>): String? =
        entries.firstOrNull { it.type == "file" && it.name.equals("filter_definitions.json", ignoreCase = true) }
            ?.downloadUrl?.takeIf(::usableDownload)

    /**
     * An author's filters, as the official launcher lists them: with [definitions], their names and
     * descriptions in their order, each with its file (a definition without its file is left out);
     * else, or when no definition finds its file, every `.filter` file by its name. Only files that
     * the app can take: a safe name ([safeName]), a GitHub download address, a sane size.
     */
    fun filters(entries: List<Entry>, definitions: Definitions?): List<Filter> {
        val files = entries.filter {
            it.type == "file" && safeName(it.name) && it.size in 1..MAX_FILTER_BYTES && it.downloadUrl?.let(::usableDownload) == true
        }
        fun filter(entry: Entry, display: String, description: String) =
            Filter(display.ifBlank { entry.name }, description.trim(), entry.name, entry.sha, entry.size, entry.downloadUrl!!)
        val named = definitions?.info?.values?.mapNotNull { d ->
            files.firstOrNull { it.name.equals(d.fileName, ignoreCase = true) }?.let { filter(it, d.displayName.trim(), d.description) }
        }.orEmpty()
        // One row per file: a definitions file that names a file twice would give the list two rows of one key.
        return named.ifEmpty { files.map { filter(it, it.name, "") } }.distinctBy { it.file.lowercase() }
    }

    /** A GitHub folder of PD2's list: `https://api.github.com/repos/<owner>/<repo>/contents[/<path>]`, plain path characters, no `..`. */
    fun usableFolder(url: String): Boolean {
        if (!url.startsWith(API)) return false
        val rest = url.removePrefix(API)
        val parts = rest.split('/')
        return PATH.matches(rest) && ".." !in rest && parts.size >= 3 && parts[2] == "contents"
    }

    /** A file's download address: `https://raw.githubusercontent.com/...`, plain path characters, no `..`. */
    fun usableDownload(url: String): Boolean {
        if (!url.startsWith(RAW)) return false
        val rest = url.removePrefix(RAW)
        return FILE_PATH.matches(rest) && ".." !in rest && "%2e" !in rest.lowercase() && "%2f" !in rest.lowercase()
    }

    /**
     * A file name the app writes in the game folder: a `.filter` file, plain characters only (no
     * folder, no leading dot), at most 120 characters.
     */
    fun safeName(name: String): Boolean =
        NAME.matches(name) && name.endsWith(".filter", ignoreCase = true) && !name.startsWith(".") && name.length > ".filter".length

    /**
     * The author's page on GitHub for the folder [url] (contents API), as the official launcher's
     * Authors Page: the repository, or its sub folder (`/tree/HEAD/<path>`); null for any other address.
     */
    fun authorPage(url: String): String? {
        if (!usableFolder(url)) return null
        val parts = url.removePrefix(API).split('/')
        val repo = "https://github.com/${parts[0]}/${parts[1]}"
        val path = parts.drop(3).filter { it.isNotEmpty() }
        return if (path.isEmpty()) repo else "$repo/tree/HEAD/${path.joinToString("/")}"
    }

    /** Whether a list read at [readAt] (null: never) is read again at [now]: at most once in [READ_EVERY_MS]; a clock that went back reads again. */
    fun due(readAt: Long?, now: Long): Boolean = readAt == null || now < readAt || now - readAt >= READ_EVERY_MS

    /**
     * The newer file of the author's filter [choice] in the author's [filters] now: the same file
     * name with another git SHA, as the official launcher's update (its SHA check). Null: no update
     * (the player's own file, the same file, or the file is gone from the folder: the one in use stays).
     */
    fun update(choice: Choice, filters: List<Filter>): Filter? {
        if (choice.local) return null
        val now = filters.firstOrNull { it.file == choice.file } ?: return null
        return now.takeIf { it.sha != null && it.sha != choice.sha }
    }

    /** The official PD2 launcher's own pick, from its `launcherSettings.json`: the author's short [author], the [file] and its git [sha]. */
    data class LauncherPick(val author: String, val file: String, val sha: String?)

    /** Who wrote the `loot.filter` in use, as far as the app can tell ([holder]). */
    enum class Holder {
        /** No `loot.filter`. */
        NONE,

        /** The app's pick ([Choice]): its content is the file that the app put in use. */
        APP,

        /** The official launcher's pick ([LauncherPick]): its content is that file. */
        LAUNCHER,

        /** Another content: the official launcher's own local filter, an edit, another app. */
        OTHER,
    }

    /**
     * Who wrote `loot.filter`, by its git SHA [inUse] (null: no file): the app's [choice] (an author's file by its
     * [Choice.written] or [Choice.sha]; the player's own file by [Choice.written] or [ownSha], the git SHA of that file now), else the official launcher's
     * [launcher] pick, else another. The official launcher saves its pick at once, also without its Save Filter,
     * so only the content tells what is in use.
     */
    fun holder(inUse: String?, choice: Choice?, ownSha: String?, launcher: LauncherPick?): Holder = when {
        inUse == null -> Holder.NONE
        choice != null && (choice.written == inUse || (if (choice.local) ownSha else choice.sha) == inUse) -> Holder.APP
        launcher?.sha != null && launcher.sha == inUse -> Holder.LAUNCHER
        else -> Holder.OTHER
    }

    /** A file's git SHA, as GitHub's folder listing gives it: SHA-1 of "blob <size>\u0000" and the bytes. */
    fun gitSha(bytes: ByteArray): String {
        val digest = java.security.MessageDigest.getInstance("SHA-1")
        digest.update("blob ${bytes.size}\u0000".toByteArray(Charsets.US_ASCII))
        return digest.digest(bytes).joinToString("") { "%02x".format(it) }
    }

    /** The official launcher's pick in its `launcherSettings.json` [text] (`SelectedAuthorAndFilter`); null: none that reads. */
    fun launcherPick(text: String): LauncherPick? = try {
        val root = json.parseToJsonElement(text.removePrefix("\uFEFF")).jsonObject
        val pick = root["SelectedAuthorAndFilter"]?.jsonObject
        val author = pick?.get("selectedAuthor")?.jsonObject?.get("author")?.jsonPrimitive?.contentOrNull
        val filter = pick?.get("selectedFilter")?.jsonObject
        val file = filter?.get("name")?.jsonPrimitive?.contentOrNull
        if (author.isNullOrBlank() || file.isNullOrBlank()) null
        else LauncherPick(author, file, filter["sha"]?.jsonPrimitive?.contentOrNull?.takeIf { it.isNotBlank() })
    } catch (_: Exception) {
        null
    }

    /** "847 KB", "1.2 MB": the size as the page shows it (KB of 1024 bytes). */
    fun size(bytes: Long): String =
        if (bytes >= 1 shl 20) String.format(java.util.Locale.ROOT, "%.1f MB", bytes / 1048576.0) else "${maxOf(1, (bytes + 512) / 1024)} KB"
}
