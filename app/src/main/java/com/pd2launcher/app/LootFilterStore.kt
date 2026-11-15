package com.pd2launcher.app

import android.content.Context
import android.net.Uri
import android.provider.OpenableColumns
import android.util.Log
import java.io.File
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL
import java.util.concurrent.Executors
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/**
 * The Loot filters page's work ([com.pd2launcher.app.ui.LootFiltersPage]) and the update at Play
 * ([updateForPlay]), as the official PD2 launcher does them (docs/design/loot-filters/README.md):
 * PD2's author list (with its ETag), an author's filters (the GitHub folder and its definitions),
 * each read at most once a minute ([LootFilters.due]); the filter in use; the player's own files.
 * One worker thread does every read and write in order; the page reads [state]. The choice, the
 * author list and its ETag are kept in `files/loot-filters.json`.
 */
object LootFilterStore {
    private const val TAG = "LootFilters"
    private const val FILE = "loot-filters.json"
    private const val TIMEOUT_MS = 15_000
    /** Play's update waits at most this for each request, so a slow network holds Play only a short time. */
    const val PLAY_TIMEOUT_MS = 4_000

    enum class Load { NONE, LOADING, OK, FAILED }

    /** Why a change of the game folder's files waits. */
    enum class Wait { GAME, STARTING, WORK, NO_GAME }

    /** An author's filters and when they were read (elapsed time). */
    data class Listing(val filters: List<LootFilters.Filter>, val readAt: Long)

    /** What the slot at the end of a row's name line says after a press there (owner, 2026-11-06: mock e1f2). */
    enum class NoteKind { COPIED, FAILED, REMOVED, USED }

    /** A [kind] note on the row of [file] of [author] (an author's short name, or [LootFilters.LOCAL]). */
    data class Note(val author: String, val file: String, val kind: NoteKind)

    /** A kept copy's filter: its author and name, so Your files shows "Meme — Hyper", not its file name. */
    @Serializable
    data class Copy(val author: String, val display: String)

    /** What the page shows. */
    data class State(
        val authors: List<LootFilters.Author> = emptyList(),
        /** The author list: [Load.FAILED] with saved authors is "no network, the saved list". */
        val authorsLoad: Load = Load.NONE,
        /** The author shown (its short name) or [LootFilters.LOCAL]; null: none yet. */
        val shown: String? = null,
        val listings: Map<String, Listing> = emptyMap(),
        val listLoad: Load = Load.NONE,
        val own: List<LootFilterFiles.Own> = emptyList(),
        val choice: LootFilters.Choice? = null,
        /** The file being downloaded and put in use; null: none. */
        val using: String? = null,
        /** Why the last action could not run, for the pane (an amber note, or a red Banner for [Tone.ERROR]); null: none. */
        val message: Line? = null,
        val messageTone: Tone = Tone.OK,
        /** The slot's note on one row; null: none. */
        val note: Note? = null,
        /** The player's kept copies, by file name. */
        val copies: Map<String, Copy> = emptyMap(),
        /** Who wrote `loot.filter` ([LootFilters.holder]); read at the page's open, after a pick and at Play. */
        val holder: LootFilters.Holder = LootFilters.Holder.NONE,
        /** The official launcher's own pick, when [holder] is [LootFilters.Holder.LAUNCHER]. */
        val launcherPick: LootFilters.LauncherPick? = null,
    ) {
        /**
         * The app's pick while it is the filter in use: also with no `loot.filter` (Start over, Repair), as Play puts it
         * back. Null while the official launcher or another app holds `loot.filter` ([managedOutside]).
         */
        val inUseChoice: LootFilters.Choice?
            get() = choice?.takeIf { holder == LootFilters.Holder.APP || holder == LootFilters.Holder.NONE }

        /** `loot.filter` is not the app's pick: the official launcher (or another app) manages the filter now (owner, 2026-11-14). */
        val managedOutside: Boolean
            get() = holder == LootFilters.Holder.LAUNCHER || holder == LootFilters.Holder.OTHER

        /** The filters of the author shown, or the player's own files as filters. */
        val shownFilters: List<LootFilters.Filter>
            get() = when (shown) {
                null -> emptyList()
                LootFilters.LOCAL -> own.map { LootFilters.Filter(copies[it.name]?.display ?: it.name, "", it.name, null, it.size, "") }
                else -> listings[shown]?.filters.orEmpty()
            }

        fun author(name: String?): LootFilters.Author? = authors.firstOrNull { it.author == name }

        fun inUse(author: String?, file: String): Boolean = inUseChoice?.let { it.author == author && it.file == file } == true

        /** The slot's note for the row of [file] on the author shown, if it has one. */
        fun noteOn(file: String): NoteKind? = note?.takeIf { it.author == shown && it.file == file }?.kind
    }

    @Serializable
    private data class Saved(
        val etag: String? = null,
        val authors: List<LootFilters.Author> = emptyList(),
        val choice: LootFilters.Choice? = null,
        val copies: Map<String, Copy> = emptyMap(),
    )

    /** How long a removed file waits for Undo before it goes (owner, 2026-11-06). */
    const val UNDO_MS = 5_000L

    private val json = Json { ignoreUnknownKeys = true; encodeDefaults = true }
    private val worker = Executors.newSingleThreadScheduledExecutor { Thread(it, "loot-filters").apply { isDaemon = true } }
    /** The removal that waits for Undo ([remove]); null: none. Worker thread only. */
    private var pendingRemove: java.util.concurrent.ScheduledFuture<*>? = null
    private val _state = MutableStateFlow(State())
    val state: StateFlow<State> = _state.asStateFlow()

    @Volatile private var saved: Saved? = null
    private var authorsReadAt: Long? = null
    private var logFile: RotatingLog? = null

    /** Why the game folder's loot filter files cannot change now ([wait]); null: they can. */
    fun wait(gameFolder: File?, running: Boolean, starting: Boolean, busy: Boolean): Wait? = when {
        running -> Wait.GAME
        starting -> Wait.STARTING
        busy -> Wait.WORK
        gameFolder == null -> Wait.NO_GAME
        else -> null
    }

    /** The page opens: the saved list and choice at once, then the author list (at most once a minute) and the shown author's filters. */
    fun open(context: Context) {
        val app = context.applicationContext
        run(app) {
            val s = load(app)
            _state.update { st ->
                st.copy(
                    authors = st.authors.ifEmpty { s.authors },
                    choice = s.choice,
                    copies = s.copies,
                    note = null,
                    // The author in use, else PD2's first author (after the list is read), not the empty Your files.
                    shown = st.shown ?: s.choice?.author ?: s.authors.firstOrNull()?.author,
                    own = files(app)?.own().orEmpty(),
                    message = null,
                )
            }
            readHolder(app, s.choice)
            readAuthors(app)
            _state.value.shown?.let { readListing(app, it) }
        }
    }

    /** The player chose [author] (or [LootFilters.LOCAL]): its filters, read at most once a minute. */
    fun show(context: Context, author: String) {
        val app = context.applicationContext
        _state.update { it.copy(shown = author, message = null, note = null, listLoad = if (author == LootFilters.LOCAL) Load.OK else it.listLoad) }
        run(app) {
            if (pendingRemove != null) {
                pendingRemove?.cancel(false)
                finishRemove(app)
            }
            readListing(app, author)
        }
    }

    /** Try again after the author list or the shown author's filters did not load: reads them now. */
    fun retry(context: Context) {
        val app = context.applicationContext
        run(app) {
            authorsReadAt = null
            readAuthors(app)
            _state.value.shown?.let { shown ->
                _state.update { it.copy(listings = it.listings - shown) }
                readListing(app, shown)
            }
        }
    }

    /** Puts [filter] of the author shown (or the player's own file) in use. */
    fun use(context: Context, filter: LootFilters.Filter) {
        val app = context.applicationContext
        val author = _state.value.shown ?: return
        run(app) {
            val files = writable(app) ?: return@run
            try {
                if (author == LootFilters.LOCAL) {
                    files.useOwn(filter.file)
                    choose(app, LootFilters.Choice(LootFilters.LOCAL, filter.file, filter.file, written = files.inUseSha()))
                } else {
                    _state.update { it.copy(using = filter.file, message = null, note = null) }
                    val bytes = download(filter, TIMEOUT_MS)
                    if (writable(app) == null) return@run
                    files.useOnline(filter.file, bytes)
                    choose(app, LootFilters.Choice(author, filter.file, filter.display, filter.sha, _state.value.author(author)?.url, LootFilters.gitSha(bytes)))
                }
                // The In use tag and the radio move; the row says when it works (PD2 loads loot.filter at each start).
                _state.update { it.copy(message = null, note = Note(author, filter.file, NoteKind.USED)) }
                readHolder(app, _state.value.choice)
                log(app, "in use: $author ${filter.file} (${filter.sha ?: "own file"})")
            } catch (error: IOException) {
                log(app, "use $author ${filter.file} failed: ${AppUpdate.describe(error)}")
                if (author == LootFilters.LOCAL) say(Line(R.string.loot_use_failed_own), Tone.ERROR)
                else _state.update { it.copy(note = Note(author, filter.file, NoteKind.FAILED)) }
            } finally {
                _state.update { it.copy(using = null) }
            }
        }
    }

    /** "Keep my own copy": [filter] of the author shown into the player's own files; the filter in use stays. */
    fun keep(context: Context, filter: LootFilters.Filter) {
        val app = context.applicationContext
        val author = _state.value.shown?.takeIf { it != LootFilters.LOCAL } ?: return
        run(app) {
            val files = writable(app) ?: return@run
            try {
                files.keep(filter.file, download(filter, TIMEOUT_MS))
                val copies = load(app).copies + (filter.file to Copy(author, filter.display))
                store(app, load(app).copy(copies = copies))
                // The page stays on the author (owner, 2026-11-06): the row's slot says where the copy went.
                _state.update { it.copy(own = files.own(), copies = copies, message = null, note = Note(author, filter.file, NoteKind.COPIED)) }
                log(app, "own copy: $author ${filter.file}")
            } catch (error: IOException) {
                log(app, "own copy of $author ${filter.file} failed: ${AppUpdate.describe(error)}")
                _state.update { it.copy(note = Note(author, filter.file, NoteKind.FAILED)) }
            }
        }
    }

    /** "Add a file": the file the player chose in Android's picker ([uri]) into their own files. */
    fun add(context: Context, uri: Uri) {
        val app = context.applicationContext
        run(app) {
            val files = writable(app) ?: return@run
            val name = displayName(app, uri)
            if (name == null || !LootFilters.safeName(name)) {
                say(Line(R.string.loot_not_filter_banner), Tone.ERROR)
                return@run
            }
            try {
                val input = app.contentResolver.openInputStream(uri) ?: throw IOException("the picker's file does not open")
                input.use { files.add(name, it) }
                _state.update { it.copy(own = files.own(), shown = LootFilters.LOCAL, message = null) }
                log(app, "own file added: $name")
            } catch (error: IOException) {
                log(app, "add $name failed: ${AppUpdate.describe(error)}")
                say(Line(R.string.loot_add_failed), Tone.ERROR)
            }
        }
    }

    /**
     * "Remove from your files" (not the file in use): the row says "Removed · Undo" for [UNDO_MS], then the file goes
     * (owner, 2026-11-06). A second Remove, or leaving the author, ends the wait of the first at once.
     */
    fun remove(context: Context, name: String) {
        val app = context.applicationContext
        run(app) {
            if (_state.value.inUse(LootFilters.LOCAL, name)) {
                say(Line(R.string.loot_remove_in_use), Tone.WARNING)
                return@run
            }
            pendingRemove?.let { it.cancel(false); finishRemove(app) }
            _state.update { it.copy(note = Note(LootFilters.LOCAL, name, NoteKind.REMOVED), message = null) }
            pendingRemove = worker.schedule({ finishRemove(app) }, UNDO_MS, java.util.concurrent.TimeUnit.MILLISECONDS)
        }
    }

    /** Undo of a Remove that still waits: the file stays. */
    fun undo(context: Context) {
        val app = context.applicationContext
        run(app) {
            pendingRemove?.cancel(false)
            pendingRemove = null
            _state.update { if (it.note?.kind == NoteKind.REMOVED) it.copy(note = null) else it }
        }
    }

    /** The removal that waited: the file goes now. Worker thread. */
    private fun finishRemove(app: Context) {
        pendingRemove = null
        val note = _state.value.note?.takeIf { it.kind == NoteKind.REMOVED } ?: return
        val files = writable(app) ?: return
        try {
            files.remove(note.file)
            val copies = load(app).copies - note.file
            store(app, load(app).copy(copies = copies))
            _state.update { it.copy(own = files.own(), copies = copies, note = null) }
            log(app, "own file removed: ${note.file}")
        } catch (error: IOException) {
            log(app, "remove ${note.file} failed: ${AppUpdate.describe(error)}")
            _state.update { it.copy(note = null) }
            say(Line(R.string.loot_remove_failed), Tone.ERROR)
        }
    }

    /** A new pick ends the slot's note (and a Remove's wait, which then goes through). */
    fun picked(context: Context) {
        val app = context.applicationContext
        run(app) {
            if (pendingRemove != null) {
                pendingRemove?.cancel(false)
                finishRemove(app)
            }
            _state.update { it.copy(note = null) }
        }
    }

    /** Settings' row: the saved choice into [state], on the worker (not the main thread). */
    fun preload(context: Context) {
        val app = context.applicationContext
        run(app) {
            if (_state.value.choice == null) _state.update { it.copy(choice = load(app).choice) }
            readHolder(app, _state.value.choice)
        }
    }

    /** Whether Play looks for the author's newer file: an author's filter is in use. */
    fun updatesAtPlay(context: Context): Boolean = (saved ?: load(context.applicationContext)).choice?.local == false

    /**
     * Play: an author's filter in use gets the author's newest file first, when the author changed
     * it (the official launcher does this at each start). At most one read of the folder a minute;
     * each request waits at most [PLAY_TIMEOUT_MS]. Any failure keeps the filter in use and lets
     * the game start. Blocks: call it off the main thread, before the launch. Returns a line for the
     * service log; null when there is nothing to do.
     */
    fun updateForPlay(context: Context, game: File?): String? {
        val app = context.applicationContext
        // On the page's worker, so no change of the page runs at the same time (one writer of the
        // saved choice and of loot.filter); at most three requests' time.
        return try {
            worker.submit(java.util.concurrent.Callable { updateNow(app, game) }).get(3L * PLAY_TIMEOUT_MS + 2_000, java.util.concurrent.TimeUnit.MILLISECONDS)
        } catch (error: java.util.concurrent.TimeoutException) {
            "loot filter update took too long; the filter in use stays, and the game starts"
        } catch (error: Exception) {
            "loot filter update failed (${AppUpdate.describe(error)}); the filter in use stays"
        }
    }

    private fun updateNow(app: Context, game: File?): String? {
        val choice = (saved ?: load(app)).choice ?: return null
        if (choice.local) return null
        if (game != null) {
            // Only the app's own pick updates: never write over a filter that the official launcher (or another app)
            // put in use (owner, 2026-11-14).
            val files = LootFilterFiles.of(game)
            val pick = files.launcherPick()
            val holder = LootFilters.holder(files.inUseSha(), choice, null, pick)
            _state.update { it.copy(holder = holder, launcherPick = pick.takeIf { holder == LootFilters.Holder.LAUNCHER }) }
            if (holder == LootFilters.Holder.LAUNCHER || holder == LootFilters.Holder.OTHER) {
                return "loot filter ${choice.author} ${choice.file}: loot.filter is ${if (holder == LootFilters.Holder.LAUNCHER) "the official launcher's pick (${pick?.author} ${pick?.file})" else "not the app's pick"}; not updated".also { log(app, it) }
            }
        }
        val folder = choice.folder ?: return "loot filter ${choice.file}: no author folder kept, not checked"
        if (game == null) return "loot filter ${choice.file}: no game folder, not checked"
        val listing = _state.value.listings[choice.author]
        return try {
            val filters = if (listing != null && !LootFilters.due(listing.readAt, android.os.SystemClock.elapsedRealtime())) {
                listing.filters
            } else {
                // The folder's files only: the update matches the file by its name (no definitions read).
                // Not kept for the page: without the definitions it has no names.
                readFilters(folder, PLAY_TIMEOUT_MS, definitions = false)
            }
            // loot.filter gone (Start over, Repair): the author's file in use comes again.
            val missing = !LootFilterFiles.of(game).inUse.isFile
            val newer = LootFilters.update(choice, filters)
                ?: filters.firstOrNull { missing && it.file == choice.file }
                ?: return "loot filter ${choice.author} ${choice.file}: no newer file"
            val bytes = download(newer, PLAY_TIMEOUT_MS)
            LootFilterFiles.of(game).useOnline(newer.file, bytes)
            choose(app, choice.copy(sha = newer.sha, written = LootFilters.gitSha(bytes)))
            "loot filter ${choice.author} ${choice.file}: updated to the author's newer file (${newer.sha})".also { log(app, it) }
        } catch (error: IOException) {
            "loot filter ${choice.author} ${choice.file}: update check failed (${AppUpdate.describe(error)}); the filter in use stays".also { log(app, it) }
        }
    }

    // ---- the worker's steps

    private fun readAuthors(app: Context) {
        val now = android.os.SystemClock.elapsedRealtime()
        if (!LootFilters.due(authorsReadAt, now) && _state.value.authors.isNotEmpty()) return
        val s = load(app)
        _state.update { it.copy(authorsLoad = Load.LOADING) }
        try {
            val answer = get(LootFilters.AUTHORS_URL, s.etag, TIMEOUT_MS)
            authorsReadAt = now
            if (answer.code == HttpURLConnection.HTTP_NOT_MODIFIED && s.authors.isNotEmpty()) {
                _state.update { it.copy(authors = s.authors, authorsLoad = Load.OK) }
                return
            }
            val authors = LootFilters.authors(answer.body) ?: throw IOException("PD2's author list does not read")
            // Read again: the choice may have changed while the list came.
            store(app, load(app).copy(etag = answer.etag, authors = authors))
            _state.update { st -> st.copy(authors = authors, authorsLoad = Load.OK, shown = st.shown ?: authors.firstOrNull()?.author) }
        } catch (error: IOException) {
            log(app, "author list not read: ${AppUpdate.describe(error)}")
            _state.update { st -> st.copy(authors = st.authors.ifEmpty { s.authors }, authorsLoad = Load.FAILED, shown = st.shown ?: st.authors.ifEmpty { s.authors }.firstOrNull()?.author ?: LootFilters.LOCAL) }
        }
    }

    private fun readListing(app: Context, author: String) {
        if (author == LootFilters.LOCAL) {
            _state.update { it.copy(own = files(app)?.own().orEmpty(), listLoad = Load.OK) }
            return
        }
        val now = android.os.SystemClock.elapsedRealtime()
        val have = _state.value.listings[author]
        if (have != null && !LootFilters.due(have.readAt, now)) {
            _state.update { if (it.shown == author) it.copy(listLoad = Load.OK) else it }
            return
        }
        val url = _state.value.author(author)?.url ?: run {
            // An author no longer on PD2's list (the saved choice's, say): nothing to read.
            _state.update { if (it.shown == author) it.copy(listLoad = Load.FAILED) else it }
            return
        }
        _state.update { if (it.shown == author) it.copy(listLoad = Load.LOADING) else it }
        try {
            val filters = readFilters(url, TIMEOUT_MS)
            _state.update { st ->
                st.copy(listings = st.listings + (author to Listing(filters, now)), listLoad = if (st.shown == author) Load.OK else st.listLoad)
            }
        } catch (error: IOException) {
            log(app, "filters of $author not read: ${AppUpdate.describe(error)}")
            _state.update { if (it.shown == author) it.copy(listLoad = Load.FAILED) else it }
        }
    }

    /** An author's folder [url]: its listing, then its definitions when it has them (official order). */
    private fun readFilters(url: String, timeout: Int, definitions: Boolean = true): List<LootFilters.Filter> {
        if (!LootFilters.usableFolder(url)) throw IOException("not a GitHub folder: $url")
        val entries = LootFilters.entries(get(url, null, timeout).body) ?: throw IOException("the folder's listing does not read")
        if (!definitions) return LootFilters.filters(entries, null)
        val named = LootFilters.definitionsUrl(entries)?.let { d ->
            try {
                LootFilters.definitions(get(d, null, timeout).body)
            } catch (_: IOException) {
                null
            }
        }
        return LootFilters.filters(entries, named)
    }

    private fun download(filter: LootFilters.Filter, timeout: Int): ByteArray {
        if (!LootFilters.usableDownload(filter.url)) throw IOException("not a GitHub file: ${filter.url}")
        val bytes = get(filter.url, null, timeout, LootFilters.MAX_FILTER_BYTES).bytes
        if (bytes.isEmpty()) throw IOException("the filter is empty")
        return bytes
    }

    private class Answer(val code: Int, val bytes: ByteArray, val etag: String?) {
        val body: String get() = String(bytes, Charsets.UTF_8)
    }

    private fun get(url: String, etag: String?, timeout: Int, limit: Int = LootFilters.MAX_LIST_BYTES): Answer {
        val connection = URL(url).openConnection() as HttpURLConnection
        connection.connectTimeout = timeout
        connection.readTimeout = timeout
        connection.setRequestProperty("User-Agent", AppUpdateHttp.userAgent)
        if (url.startsWith("https://api.github.com/")) connection.setRequestProperty("Accept", "application/vnd.github+json")
        if (etag != null) connection.setRequestProperty("If-None-Match", etag)
        try {
            val code = connection.responseCode
            if (code == HttpURLConnection.HTTP_NOT_MODIFIED) return Answer(code, ByteArray(0), etag)
            if (code != HttpURLConnection.HTTP_OK) throw AppUpdateHttp.Status(code, url)
            val bytes = connection.inputStream.use { LootFilterFiles.readAtMost(it, limit + 1) }
            if (bytes.size > limit) throw IOException("the answer is larger than $limit bytes: $url")
            return Answer(code, bytes, connection.getHeaderField("ETag"))
        } finally {
            connection.disconnect()
        }
    }

    // ---- the game folder, the saved choice, the log

    private fun files(app: Context): LootFilterFiles? = gameFolder(app)?.let(LootFilterFiles::of)

    private fun gameFolder(app: Context): File? {
        val paths = SessionPaths(DeviceCheck.dataDir(app))
        return GameFilesProvider.gameFolder(paths, SessionService.status.value.snapshot?.gameFolder?.path)
    }

    /** The files, when they may change now; else says why on the page and returns null. */
    private fun writable(app: Context): LootFilterFiles? {
        val folder = gameFolder(app)
        val why = wait(folder, GameFilesProvider.gameRunning(), GameFilesProvider.gameStarting(), GameFilesProvider.busy())
        if (why != null) {
            say(
                Line(
                    when (why) {
                        Wait.GAME -> R.string.loot_wait_game
                        Wait.STARTING -> R.string.loot_wait_starting
                        Wait.WORK -> R.string.loot_wait_work
                        Wait.NO_GAME -> R.string.loot_wait_no_game
                    },
                ),
                Tone.WARNING,
            )
            return null
        }
        return LootFilterFiles.of(folder!!)
    }

    /** Who holds `loot.filter` now, for the page ([State.holder]): the app's [choice], the official launcher's pick, or another. */
    private fun readHolder(app: Context, choice: LootFilters.Choice?) {
        val files = files(app)
        val pick = files?.launcherPick()
        val holder = files?.let { f -> LootFilters.holder(f.inUseSha(), choice, choice?.takeIf { it.local }?.let { f.ownSha(it.file) }, pick) }
            ?: LootFilters.Holder.NONE
        _state.update { it.copy(holder = holder, launcherPick = pick.takeIf { holder == LootFilters.Holder.LAUNCHER }) }
    }

    @Synchronized
    private fun choose(app: Context, choice: LootFilters.Choice) {
        store(app, load(app).copy(choice = choice))
        _state.update { it.copy(choice = choice) }
    }

    private fun say(line: Line, tone: Tone = Tone.OK) = _state.update { it.copy(message = line, messageTone = tone) }

    @Synchronized
    private fun load(app: Context): Saved = saved ?: try {
        val file = File(app.filesDir, FILE)
        if (file.isFile && file.length() < (1 shl 20)) json.decodeFromString(Saved.serializer(), file.readText()) else Saved()
    } catch (_: Exception) {
        Saved()
    }.also { saved = it }

    @Synchronized
    private fun store(app: Context, s: Saved) {
        saved = s
        LootFilterFiles.write(File(app.filesDir, FILE), json.encodeToString(Saved.serializer(), s).toByteArray())
    }

    private fun displayName(app: Context, uri: Uri): String? = try {
        app.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { c ->
            if (c.moveToFirst()) c.getString(0) else null
        }
    } catch (_: Exception) {
        null
    }

    private fun run(app: Context, task: () -> Unit) = worker.execute {
        try {
            task()
        } catch (error: Exception) {
            log(app, "loot filters: ${AppUpdate.describe(error)}")
            say(Line(R.string.loot_failed), Tone.ERROR)
        }
    }

    private fun log(app: Context, message: String) {
        Log.i(TAG, message)
        val file = synchronized(this) { logFile ?: RotatingLog(SessionPaths(DeviceCheck.dataDir(app)).logs, "loot-filters").also { logFile = it } }
        file.line(message)
    }
}
