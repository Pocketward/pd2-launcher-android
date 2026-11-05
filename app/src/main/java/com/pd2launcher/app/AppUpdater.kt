package com.pd2launcher.app

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.SharedPreferences
import android.content.pm.PackageInfo
import android.content.pm.PackageInstaller
import android.content.pm.PackageManager
import android.os.Build
import android.os.Process
import android.os.StatFs
import android.os.SystemClock
import android.util.Log
import androidx.core.content.IntentCompat
import com.pd2launcher.app.AppUpdate.Check
import com.pd2launcher.app.AppUpdate.CheckProblem
import com.pd2launcher.app.AppUpdate.DownloadProblem
import com.pd2launcher.app.AppUpdate.InstallOutcome
import com.pd2launcher.app.AppUpdate.InstallProblem
import com.pd2launcher.app.AppUpdate.Work
import java.io.File
import java.io.IOException
import java.security.MessageDigest
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/**
 * App updates ([AppUpdate]): reads GitHub's list of this app's releases ([Offers]; each hour while
 * the app is open, and when the player asks), downloads a build's APK into the app's own folder when
 * the player asks, checks it, and hands it to Android's installer, which asks the player to confirm.
 * One worker thread does the work; the screens follow [offers], [check] and [work]. What it does goes to
 * `files/logs/update.log`, which Save logs packs.
 */
object AppUpdater {
    private const val TAG = "pd2-update"
    private const val PREFERENCES = "app_update"
    private const val ETAG = "etag"
    private const val OFFERS = "offers"
    /** Older keys: the offer found, and the last notice's code. Removed at the start. */
    private const val OLD_OFFER = "offer"
    private const val OLD_NOTIFIED = "notified"
    private const val LAST_SUCCESS = "last_success"
    private const val LAST_ATTEMPT = "last_attempt"
    private const val NOT_BEFORE = "not_before"
    /** The version of the last notice. */
    private const val NOTIFIED = "notified_version"
    private const val DOWNLOAD = "download"
    /** The uploads that the app does not offer again ([AppUpdate.upload]). */
    private const val REFUSED = "refused"
    private const val TEST_FEED = "test_feed"
    private const val FOLDER = "app-update"
    private const val CHANNEL = "app_update"
    private const val NOTIFICATION_ID = 2
    /** The most a `.sha256` file may hold. */
    private const val CHECKSUM_LIMIT = 4096
    /** The shortest time a check the player asked for shows as running ([check]). */
    private const val CHECK_SHOWN_MS = 1_000L
    /** HTTP statuses that say the release's file is gone, or has no such part: a check follows. */
    private val GONE = setOf(404, 410, 416)

    /** The install session's status, to [AppUpdateReceiver]. */
    const val ACTION_STATUS = "com.pd2launcher.app.action.APP_UPDATE_STATUS"
    /** Opens Settings → App: the notice of a new version. */
    const val EXTRA_APP_PAGE = "com.pd2launcher.app.extra.APP_PAGE"

    private val json = Json { ignoreUnknownKeys = true }
    /** The one worker thread, at background priority: a check while the game runs gives the CPU to the game first. */
    private val worker = Executors.newSingleThreadExecutor { work ->
        Thread({ Process.setThreadPriority(Process.THREAD_PRIORITY_BACKGROUND); work.run() }, "pd2-update")
    }
    private val started = AtomicBoolean(false)
    /** An install's copy for Android is queued or runs: a second Install waits for it. */
    private val installRunning = AtomicBoolean(false)
    @Volatile private var stopRequested = false
    /** The install session Android has, until it answers or the update is taken back. */
    @Volatile private var session: Int? = null
    /** [signer] as read once per process, and whether it was read. */
    @Volatile private var signerCertificate: String? = null
    @Volatile private var signerRead = false
    private var updateLog: RotatingLog? = null

    private val _offers = MutableStateFlow<Offers?>(null)
    /** The two channels' builds on the last list of releases that read; null: none yet. */
    val offers: StateFlow<Offers?> = _offers.asStateFlow()

    private val _check = MutableStateFlow<Check>(Check.Idle)
    val check: StateFlow<Check> = _check.asStateFlow()

    private val _work = MutableStateFlow<Work?>(null)
    /** The download in hand; null: none. */
    val work: StateFlow<Work?> = _work.asStateFlow()

    private val _refused = MutableStateFlow<Set<String>>(emptySet())
    /** The uploads refused for good ([AppUpdate.remembers]), for the rows. */
    val refused: StateFlow<Set<String>> = _refused.asStateFlow()

    /** Android's confirmation window for the committed update, for the activity to open while it is in front. */
    private val _confirmation = MutableStateFlow<Intent?>(null)
    val confirmation: StateFlow<Intent?> = _confirmation.asStateFlow()

    /** The download in hand: what it is of, and the SHA-256 it must have. */
    @Serializable
    private data class Download(val offer: Offer, val sha256: String)

    /**
     * Whether app updates work in this build: a release feed (in a debug build only the test
     * feed, [setTestFeed]) and a certificate updates must be signed with ([signer]).
     */
    fun available(context: Context): Boolean = feed(context).isNotEmpty() && signer(context) != null

    /**
     * Once per process, from the activity: puts back where the update stood, removes what an
     * update that has installed since left behind, checks when a check is due, and follows the
     * session for the life of the process ([follow]), also while Android's window is in front
     * and Android has ended the activity.
     */
    fun start(context: Context) {
        if (!started.compareAndSet(false, true)) return
        val app = context.applicationContext
        worker.execute {
            restore(app)
            checkIfDue(app)
        }
        CoroutineScope(SupervisorJob() + Dispatchers.Main).launch {
            combine(SessionService.status, SourceImport.copy) { status, _ -> status }.collect { follow(app, it) }
        }
    }

    /**
     * The app came to the front, and each [AppUpdate.TIMER_MS] while it stays there
     * ([AppUpdateHost]): a check when one is due, on the worker thread.
     */
    fun inFront(context: Context) {
        val app = context.applicationContext
        if (started.get()) worker.execute { checkIfDue(app) }
    }

    /** Check now (Settings → App): at any time but during the server's wait. */
    fun checkNow(context: Context) {
        val app = context.applicationContext
        worker.execute { check(app, automatic = false) }
    }

    /**
     * The download sign and Try again: [offer]'s APK, then its checks, then Android's question. It
     * replaces a download of another build that does not run, and starts nothing while one runs.
     */
    fun download(context: Context, offer: Offer) {
        val app = context.applicationContext
        val current = _work.value
        if (AppUpdate.runs(current)) return
        // The rows lock while the game or the official PD2 launcher runs, or the game starts.
        if (!pressAllowed()) return
        stopRequested = false
        val kept = (current as? Work.Kept)?.takeIf { it.offer == offer }?.downloaded ?: 0
        // Only from the state shown: a check that replaced it meanwhile wins.
        if (!_work.compareAndSet(current, Work.Downloading(offer, kept))) return
        worker.execute { downloadNow(app, offer) }
    }

    /** Stop: the download ends at its next block and keeps what came in, for Try again. */
    fun stop() {
        stopRequested = true
        _work.update { if (it is Work.Downloading) it.copy(stopping = true) else it }
    }

    /**
     * Android's question: the checked download goes to Android's installer, which asks the player.
     * It follows the download by itself, and the sign opens it again. Only when nothing runs that
     * the update would end ([AppUpdate.waitFor]) and Android lets this app install updates; the row
     * shows why otherwise.
     */
    fun install(context: Context) {
        val app = context.applicationContext
        val current = _work.value
        if (current !is Work.Ready && current !is Work.Installing) return
        if (wait() != null || !app.packageManager.canRequestPackageInstalls()) return
        // One copy for Android at a time: a second tap meanwhile does nothing.
        if (!installRunning.compareAndSet(false, true)) return
        if (!_work.compareAndSet(current, Work.Installing(current.offer))) {
            installRunning.set(false)
            return
        }
        _confirmation.value = null
        worker.execute {
            try {
                installNow(app, current.offer)
            } finally {
                installRunning.set(false)
            }
        }
    }

    /**
     * Each session status and folder copy (main thread): a game, a job, a launch or other work
     * ([AppUpdate.waitFor]) that starts while Android asks the player takes the update back (its
     * session is abandoned), so a later Yes cannot end them.
     */
    private fun follow(context: Context, status: SessionStatus) {
        val installing = _work.value as? Work.Installing ?: return
        if (AppUpdate.waitFor(status, copying(), SystemClock.elapsedRealtime()) == null) return
        takeBack(context, installing.offer, "something started while Android asked")
    }

    /** The activity opened Android's confirmation window. */
    fun confirmationShown() {
        _confirmation.value = null
    }

    /** A session status from Android's installer ([AppUpdateReceiver]). */
    fun installStatus(context: Context, intent: Intent) {
        val app = context.applicationContext
        val id = intent.getIntExtra(PackageInstaller.EXTRA_SESSION_ID, -1)
        val status = intent.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE)
        val message = intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE)
        log(app, "session $id: status $status${message?.let { " ($it)" }.orEmpty()}")
        // A session taken back, or one of an earlier try.
        if (id != session) return
        val offer = _work.value?.offer ?: return
        if (status == PackageInstaller.STATUS_PENDING_USER_ACTION) {
            val confirm = IntentCompat.getParcelableExtra(intent, Intent.EXTRA_INTENT, Intent::class.java)
            when {
                confirm == null -> finished(offer, InstallProblem.FAILED)
                wait() != null -> takeBack(app, offer, "something started before Android asked")
                else -> _confirmation.value = confirm
            }
            return
        }
        session = null
        when (AppUpdate.installOutcome(status)) {
            // Android replaces the app and ends its process; rarely heard.
            InstallOutcome.INSTALLED -> _confirmation.value = null
            InstallOutcome.CANCELLED -> finished(offer, InstallProblem.CANCELLED)
            InstallOutcome.NO_SPACE -> finished(offer, InstallProblem.NO_SPACE)
            InstallOutcome.FAILED -> finished(offer, InstallProblem.FAILED)
        }
    }

    /**
     * Debug builds only: the test feed, a list of releases at any address ([AppUpdateHost]:
     * `--es update_feed <url>`, empty for none), which replaces everything known about updates, a
     * download in hand too, then a check as an automatic one (its notice too).
     */
    fun setTestFeed(context: Context, url: String) {
        if (!BuildConfig.DEBUG) return
        val app = context.applicationContext
        worker.execute {
            abandonSessions(app)
            prefs(app).edit().clear().putString(TEST_FEED, url).apply()
            clearDownload(app)
            _work.value = null
            _offers.value = null
            _refused.value = emptySet()
            _check.value = Check.Idle
            log(app, "test feed: ${url.ifEmpty { "none" }}")
            if (url.isNotEmpty()) check(app, automatic = true)
        }
    }

    /**
     * The player picked the other update channel (Settings → App, [AppUpdateHost.pickChannel]). The
     * list of releases names both channels, so nothing is fetched for it; a download in hand goes on, as only
     * Stop ends it. The notice of the old channel's version goes.
     */
    fun channelChanged(context: Context) {
        val app = context.applicationContext
        // Here, not on the worker thread: a download holds that thread until it ends.
        log(app, "update channel: ${channel().name.lowercase()}")
        worker.execute {
            dropNotice(app)
            checkIfDue(app)
        }
    }

    /** The old channel's notice goes: its version is no longer the selected channel's. */
    private fun dropNotice(context: Context) {
        prefs(context).edit().remove(NOTIFIED).apply()
        context.getSystemService(NotificationManager::class.java)?.cancel(NOTIFICATION_ID)
    }

    /** Beta opened or closed by itself ([AppUpdate.betaOpen]) and so the channel in use moved from [before]: as [channelChanged]. */
    private fun followChannel(context: Context, before: UpdateChannel) {
        val now = channel()
        if (now == before) return
        log(context, "update channel: ${now.name.lowercase()} (beta ${if (now == UpdateChannel.BETA) "opened" else "closed"})")
        dropNotice(context)
    }

    // ------------------------------------------------------------ worker thread

    private fun restore(context: Context) {
        // A session an earlier process committed can still have Android's window open (one in
        // its own task): this process cannot take it back, so it goes. Install makes a new one.
        abandonSessions(context)
        val prefs = prefs(context)
        prefs.edit().remove(OLD_OFFER).remove(OLD_NOTIFIED).apply()
        val offers = storedOffers(prefs)
        // No offers that read: the first check runs at once, whenever the last one was.
        if (offers == null) prefs.edit().remove(OFFERS).remove(ETAG).remove(LAST_SUCCESS).remove(LAST_ATTEMPT).apply()
        _offers.value = offers
        _refused.value = prefs.getStringSet(REFUSED, null).orEmpty().toSet()
        val download = held(prefs)
        if (download == null) {
            // No record that reads (none, or one of another app version's form): no file of the
            // folder belongs to a download, so all go.
            clearDownload(context)
        } else if (!AppUpdate.keptAtStart(download.offer, BuildConfig.VERSION_NAME, BuildConfig.VERSION_CODE)) {
            // An update that installed since, or a download this app cannot install: what it left goes.
            log(context, "now ${BuildConfig.VERSION_NAME}: the download of ${download.offer.versionName} is removed")
            clearDownload(context)
            context.getSystemService(NotificationManager::class.java)?.cancel(NOTIFICATION_ID)
        }
        _work.value = restored(context)
        val checkedAt = prefs.getLong(LAST_SUCCESS, 0)
        _check.value = if (offers != null && checkedAt > 0) Check.Done(checkedAt) else Check.Idle
    }

    private fun restored(context: Context): Work? {
        val offer = held(prefs(context))?.offer ?: return null
        if (apkFile(context, offer).isFile) return Work.Ready(offer)
        val downloaded = partFile(context, offer).length()
        if (downloaded > 0) return Work.Kept(offer, downloaded)
        clearDownload(context)
        return null
    }

    private fun checkIfDue(context: Context) {
        val prefs = prefs(context)
        val due = AppUpdate.checkDue(
            System.currentTimeMillis(), prefs.getLong(LAST_SUCCESS, 0), prefs.getLong(LAST_ATTEMPT, 0), prefs.getLong(NOT_BEFORE, 0),
        )
        if (due) check(context, automatic = true)
    }

    private fun check(context: Context, automatic: Boolean) {
        if (!available(context) || _work.value is Work.Installing) return
        val prefs = prefs(context)
        val now = System.currentTimeMillis()
        val notBefore = prefs.getLong(NOT_BEFORE, 0)
        if (AppUpdate.waiting(now, notBefore)) {
            _check.value = Check.Failed(CheckProblem.BUSY, notBefore)
            return
        }
        prefs.edit().putLong(LAST_ATTEMPT, now).apply()
        // A check the player asked for shows "Looking for a new version…" long enough to be seen,
        // also when the answer comes at once and is the same as before.
        if (!automatic) _check.value = Check.Running
        val started = SystemClock.elapsedRealtime()
        fun shown() {
            if (!automatic) SystemClock.sleep((CHECK_SHOWN_MS - (SystemClock.elapsedRealtime() - started)).coerceAtLeast(0))
        }
        // A bad or missing answer changes nothing but the status: the offers in hand and a download stay.
        fun failed(problem: CheckProblem, retryAt: Long = 0) {
            shown()
            _check.value = Check.Failed(problem, retryAt)
        }
        val answer = try {
            AppUpdateHttp.ask(feed(context), AppUpdate.etagToSend(prefs.getString(ETAG, null), _offers.value), now)
        } catch (error: Exception) {
            log(context, "check failed: ${AppUpdate.describe(error)}")
            failed(AppUpdate.checkProblem(error))
            return
        }
        val end = AppUpdate.afterAnswer(answer, _offers.value, rules())
        val offers = end.offers
        if (end.problem != null || offers == null) {
            end.note?.let { log(context, it) }
            if (end.retryAt > 0) prefs.edit().putLong(NOT_BEFORE, end.retryAt).apply()
            _offers.value = end.offers
            failed(end.problem ?: CheckProblem.FAILED, end.retryAt)
            return
        }
        if (answer is AppUpdateHttp.Answer.Found) {
            prefs.edit().apply {
                putString(OFFERS, json.encodeToString(Offers.serializer(), offers))
                if (answer.etag != null) putString(ETAG, answer.etag) else remove(ETAG)
            }.apply()
        }
        val refused = AppUpdate.stillRefused(_refused.value, offers)
        if (refused != _refused.value) setRefused(context, refused)
        // When the answer came: "Checked … ago" counts from here, also after a slow answer.
        val checkedAt = System.currentTimeMillis()
        prefs.edit().putLong(LAST_SUCCESS, checkedAt).apply()
        val unchanged = if (answer == AppUpdateHttp.Answer.NotModified) "unchanged, " else ""
        log(context, "check: ${unchanged}stable ${offers.stable?.versionName ?: "none"}, beta ${offers.beta?.versionName ?: "none"}")
        shown()
        found(context, end, offers, automatic)
        _check.value = Check.Done(checkedAt)
    }

    /**
     * What the check found: [offers], at its [end]. A download in hand may go
     * ([AppUpdate.dropsDownload]); a new version of the selected channel gets the notice when the
     * check was automatic ([notice], once per version).
     */
    private fun found(context: Context, end: AppUpdate.CheckEnd, offers: Offers, automatic: Boolean) {
        val before = channel()
        _offers.value = offers
        val held = _work.value
        if (AppUpdate.dropsDownload(end, held)) {
            log(context, "the download of ${held?.offer?.versionName} is removed")
            clearDownload(context)
            _work.value = null
        }
        followChannel(context, before)
        val channel = channel()
        val offer = AppUpdate.newOffer(offers, channel, BuildConfig.VERSION_NAME, BuildConfig.VERSION_CODE, _work.value, _refused.value)
        log(context, "${channel.name.lowercase()} channel: ${offer?.let { "offer ${it.versionName} (code ${it.versionCode})" } ?: "no offer"}")
        if (offer == null) return
        if (automatic) {
            notice(context, offer)
        } else {
            prefs(context).edit().putString(NOTIFIED, offer.versionName).apply()
        }
    }

    private fun downloadNow(context: Context, offer: Offer) {
        // A check that ran first removed this build.
        if (_work.value?.offer != offer) return
        val part = partFile(context, offer)
        // Stop came before the file's request: the server is not asked for it, and the part stays.
        fun stoppedBefore(): Boolean {
            if (!stopRequested) return false
            log(context, "download of ${offer.versionName} stopped at ${part.length()} bytes, before its request")
            stopped(context, offer, part)
            return true
        }
        if (stoppedBefore()) return
        try {
            val download = held(prefs(context))?.takeIf { it.offer == offer } ?: run {
                clearDownload(context)
                val text = AppUpdateHttp.text(offer.checksumUrl, CHECKSUM_LIMIT)
                val expected = AppUpdate.expectedSha256(AppUpdate.checksum(text, offer.apkName), offer.githubSha256)
                if (expected == null) {
                    log(context, "no usable SHA-256 for ${offer.apkName}: nothing is downloaded")
                    _work.value = Work.Kept(offer, 0, DownloadProblem.UNCHECKED)
                    return
                }
                Download(offer, expected).also { prefs(context).edit().putString(DOWNLOAD, json.encodeToString(Download.serializer(), it)).apply() }
            }
            part.parentFile!!.mkdirs()
            val needed = AppUpdate.spaceNeeded(offer.apkSize, part.length(), unpackBytes(context))
            val free = StatFs(part.parentFile!!.path).availableBytes
            if (free < needed) {
                log(context, "not enough space for ${offer.versionName}: ${needed shr 20} MB needed, ${free shr 20} MB free")
                _work.value = Work.Kept(offer, part.length(), DownloadProblem.NO_SPACE, needed, free)
                return
            }
            if (stoppedBefore()) return
            log(context, "download of ${offer.versionName}: asks from ${part.length()} bytes")
            AppUpdateHttp.fetch(offer.apkUrl, offer.apkSize, part, { stopRequested }, answered = { log(context, "download: $it") }) { done ->
                _work.value = Work.Downloading(offer, done, stopRequested)
            }
            if (stopRequested) {
                log(context, "download stopped at ${part.length()} bytes")
                stopped(context, offer, part)
                return
            }
            _work.value = Work.Verifying(offer)
            verify(context, download, part)
        } catch (error: Exception) {
            log(context, "download of ${offer.versionName} failed: ${AppUpdate.describe(error)}")
            val problem = when {
                stopRequested -> null
                error is IOException -> DownloadProblem.NETWORK
                else -> DownloadProblem.FAILED
            }
            if (problem == null) stopped(context, offer, part) else _work.value = Work.Kept(offer, part.length(), problem)
            // The release's file is gone or another one: a check finds out whether the list still
            // offers it (else its download goes).
            val gone = error is AppUpdateHttp.OtherFile || (error as? AppUpdateHttp.Status)?.code in GONE
            if (gone && !stopRequested) check(context, automatic = false)
        }
    }

    /** The player's Stop: what came in stays for Try again; with nothing kept, no download is left. */
    private fun stopped(context: Context, offer: Offer, part: File) {
        if (part.length() > 0) {
            _work.value = Work.Kept(offer, part.length())
        } else {
            clearDownload(context)
            _work.value = null
        }
    }

    /**
     * The download's checks ([AppUpdate.refusal]): a file that passes becomes the APK that Android's
     * question installs, which follows at once only while its channel is the selected one
     * ([AppUpdate.asksAtOnce]; else it waits on its own row with its install sign); one that fails is
     * deleted, with its record, so the download sign starts again, unless the same upload can only
     * fail again ([AppUpdate.remembers]).
     */
    private fun verify(context: Context, download: Download, part: File) {
        val offer = download.offer
        val refusal = AppUpdate.refusal(
            offer, part.length(), sha256(part), download.sha256, archive(context, part),
            context.packageName, BuildConfig.VERSION_CODE, signer(context).orEmpty(),
        )
        if (refusal != null) {
            log(context, "the download of ${offer.versionName} is refused ($refusal) and deleted")
            clearDownload(context)
            if (AppUpdate.remembers(refusal, offer)) {
                log(context, "the app does not offer this upload of ${offer.versionName} again")
                val before = channel()
                setRefused(context, _refused.value + AppUpdate.upload(offer))
                _work.value = null
                followChannel(context, before)
            } else {
                _work.value = Work.Kept(offer, 0, DownloadProblem.REFUSED)
            }
            return
        }
        if (!part.renameTo(apkFile(context, offer))) throw IOException("cannot keep the checked download")
        log(context, "the download of ${offer.versionName} passed its checks")
        _work.value = Work.Ready(offer)
        if (AppUpdate.asksAtOnce(offer, channel())) {
            install(context)
        } else {
            log(context, "${offer.versionName} waits on its own row: ${channel().name.lowercase()} is selected")
        }
    }

    private fun installNow(context: Context, offer: Offer) {
        if (_work.value?.offer != offer) return
        val download = held(prefs(context))?.takeIf { it.offer == offer }
        val file = apkFile(context, offer)
        if (download == null || !file.isFile) {
            log(context, "install of ${offer.versionName}: the checked download is gone")
            clearDownload(context)
            _work.value = null
            return
        }
        val installer = context.packageManager.packageInstaller
        // Sessions an earlier try left (the sign pressed again, or the app closed meanwhile) go
        // first; their answers no longer count.
        abandonSessions(context)
        val params = PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL).apply {
            setAppPackageName(context.packageName)
            setSize(file.length())
            setInstallReason(PackageManager.INSTALL_REASON_USER)
            // Where the APK came from, a label only: Android 13 and later.
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) setPackageSource(PackageInstaller.PACKAGE_SOURCE_DOWNLOADED_FILE)
            // The update stays where the app is. A session's default is internal storage only,
            // which Android refuses for an app on adopted storage.
            setInstallLocation(PackageInfo.INSTALL_LOCATION_AUTO)
        }
        var id: Int? = null
        try {
            id = installer.createSession(params)
            // The bytes Android gets are hashed as they go: they must be the checked ones.
            val written = installer.openSession(id).use { opened ->
                val digest = MessageDigest.getInstance("SHA-256")
                opened.openWrite("base.apk", 0, file.length()).use { out ->
                    file.inputStream().use { input ->
                        val buffer = ByteArray(64 shl 10)
                        while (true) {
                            val n = input.read(buffer)
                            if (n < 0) break
                            digest.update(buffer, 0, n)
                            out.write(buffer, 0, n)
                        }
                    }
                    opened.fsync(out)
                }
                hex(digest.digest())
            }
            if (written != download.sha256) {
                log(context, "the checked download of ${offer.versionName} changed: it is deleted")
                installer.abandonSession(id)
                clearDownload(context)
                _work.value = Work.Kept(offer, 0, DownloadProblem.REFUSED)
                return
            }
            val status = Intent(context, AppUpdateReceiver::class.java).setAction(ACTION_STATUS)
            val pending = PendingIntent.getBroadcast(context, id, status, PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_MUTABLE)
            session = id
            installer.openSession(id).use { it.commit(pending.intentSender) }
            log(context, "update to ${offer.versionName}: session $id is Android's")
            // Something may have started while the copy ran.
            if (wait() != null) takeBack(context, offer, "something started during the copy")
        } catch (error: Exception) {
            log(context, "install of ${offer.versionName} failed: ${AppUpdate.describe(error)}")
            session = null
            id?.let { runCatching { installer.abandonSession(it) } }
            // A take-back meanwhile has said why already.
            _work.update { if (it is Work.Installing) Work.Ready(offer, InstallProblem.FAILED) else it }
        }
    }

    private fun setRefused(context: Context, refused: Set<String>) {
        prefs(context).edit().putStringSet(REFUSED, refused).apply()
        _refused.value = refused
    }

    /** Abandons every install session of this app; their answers no longer count. */
    private fun abandonSessions(context: Context) {
        session = null
        _confirmation.value = null
        val installer = context.packageManager.packageInstaller
        installer.mySessions.forEach { runCatching { installer.abandonSession(it.sessionId) } }
    }

    // ------------------------------------------------------------ helpers

    /** The committed update goes back to Ready: its session is abandoned, so Android's window can no longer install it. */
    private fun takeBack(context: Context, offer: Offer, why: String) {
        val id = session ?: return
        session = null
        _confirmation.value = null
        runCatching { context.packageManager.packageInstaller.abandonSession(id) }
        log(context, "session $id taken back: $why")
        _work.value = Work.Ready(offer, InstallProblem.GAME_STARTED)
    }

    private fun finished(offer: Offer, problem: InstallProblem) {
        session = null
        _confirmation.value = null
        _work.value = Work.Ready(offer, problem)
    }

    /** Whether a press on the channel rows may act now ([AppUpdate.pressAllowed]), from the session as it is now. */
    fun pressAllowed(): Boolean = AppUpdate.pressAllowed(SessionService.status.value, copying(), SystemClock.elapsedRealtime())

    private fun wait(): AppUpdate.Wait? = AppUpdate.waitFor(SessionService.status.value, copying(), SystemClock.elapsedRealtime())

    private fun copying(): Boolean = SourceImport.copy.value is SourceCopy.Copying

    /**
     * The notice of a new version: an Android notification, once per version, from automatic
     * checks. Without a channel yet (a first start) it waits for a later check: the session's first
     * channel brings Android's notification prompt over Check, not this notice.
     */
    private fun notice(context: Context, offer: Offer) {
        val prefs = prefs(context)
        if (prefs.getString(NOTIFIED, null) == offer.versionName) return
        val manager = context.getSystemService(NotificationManager::class.java) ?: return
        if (manager.notificationChannels.isEmpty()) return
        if (manager.getNotificationChannel(CHANNEL) == null) {
            manager.createNotificationChannel(
                NotificationChannel(CHANNEL, context.getString(R.string.app_update_channel), NotificationManager.IMPORTANCE_LOW),
            )
        }
        // The app's own activity, with what was on top of it in its task closed (Android's
        // settings page, its installer window, a picker), not a second activity over them.
        val page = Intent(context, MainActivity::class.java).putExtra(EXTRA_APP_PAGE, true)
            .addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP)
        val open = PendingIntent.getActivity(context, NOTIFICATION_ID, page, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
        manager.notify(
            NOTIFICATION_ID,
            Notification.Builder(context, CHANNEL)
                .setSmallIcon(android.R.drawable.stat_sys_download_done)
                .setContentTitle(context.getString(R.string.app_update_notice_title))
                .setContentText(context.getString(R.string.app_update_notice_text, offer.versionName))
                .setContentIntent(open)
                .setAutoCancel(true)
                .build(),
        )
        prefs.edit().putString(NOTIFIED, offer.versionName).apply()
    }

    /** The update channel the checks follow ([AppUpdate.channel]): the player's pick, else this build's own. */
    private fun channel(): UpdateChannel = AppUpdate.channel(
        AppSettingsStore.current.value.updateChannel, BuildConfig.VERSION_NAME,
        AppUpdate.betaOpen(_offers.value, BuildConfig.VERSION_CODE, _refused.value, _work.value),
    )

    /** Where checks ask: the release feed, or in a debug build only its test feed. */
    private fun feed(context: Context): String = AppUpdate.feedOf(BuildConfig.DEBUG, prefs(context).getString(TEST_FEED, null))

    /** What a release's files must be for this build ([AppUpdate.feedRules]). */
    private fun rules() = AppUpdate.rulesOf(BuildConfig.DEBUG)

    /**
     * The certificate an update must be signed with (SHA-256, lower-case hex): the release
     * certificate, while this app is signed with it too (a build signed with another key gets no
     * updates from the releases, which Android would refuse); in a debug build this app's own.
     */
    private fun signer(context: Context): String? {
        if (!signerRead) {
            val own = try {
                val pm = context.packageManager
                val info = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                    pm.getPackageInfo(context.packageName, PackageManager.PackageInfoFlags.of(PackageManager.GET_SIGNING_CERTIFICATES.toLong()))
                } else {
                    @Suppress("DEPRECATION")
                    pm.getPackageInfo(context.packageName, PackageManager.GET_SIGNING_CERTIFICATES)
                }
                info.signingInfo?.apkContentsSigners?.singleOrNull()?.let { sha256(it.toByteArray()) }
            } catch (error: PackageManager.NameNotFoundException) {
                null
            }
            signerCertificate = if (BuildConfig.DEBUG) own else own?.takeIf { it == BuildConfig.UPDATE_CERTIFICATE }
            signerRead = true
        }
        return signerCertificate
    }

    /**
     * The downloaded file as Android reads it: its package, version and signers; null when it is no
     * app Android can read. The first Android 13 releases collect an archive's certificates only for
     * GET_SIGNATURES, so it is asked for too; only signingInfo is read. Android 11 and 12 take the
     * flags as an int.
     */
    @Suppress("DEPRECATION")
    private fun archive(context: Context, file: File): AppUpdate.Archive? {
        val flags = PackageManager.GET_SIGNING_CERTIFICATES or PackageManager.GET_SIGNATURES
        val pm = context.packageManager
        val read = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            pm.getPackageArchiveInfo(file.path, PackageManager.PackageInfoFlags.of(flags.toLong()))
        } else {
            pm.getPackageArchiveInfo(file.path, flags)
        }
        val info = read ?: return null
        val signers = info.signingInfo?.apkContentsSigners?.map { sha256(it.toByteArray()) } ?: return null
        return AppUpdate.Archive(info.packageName, info.versionName, info.longVersionCode, signers)
    }

    /** The space the next version's game support files need when it unpacks them, from this version's; 0 when unknown. */
    private fun unpackBytes(context: Context): Long =
        runCatching { DeviceCheck.requiredBytes(UserlandAssets.manifest(context).fileBytes) }.getOrDefault(0L)

    private fun sha256(file: File): String {
        val digest = MessageDigest.getInstance("SHA-256")
        file.inputStream().use { input ->
            val buffer = ByteArray(64 shl 10)
            while (true) {
                val n = input.read(buffer)
                if (n < 0) break
                digest.update(buffer, 0, n)
            }
        }
        return hex(digest.digest())
    }

    private fun sha256(bytes: ByteArray): String = hex(MessageDigest.getInstance("SHA-256").digest(bytes))

    private fun hex(bytes: ByteArray): String = bytes.joinToString("") { "%02x".format(it) }

    private fun folder(context: Context) = File(context.filesDir, FOLDER)

    private fun partFile(context: Context, offer: Offer) = File(folder(context), "${offer.apkName}.part")

    private fun apkFile(context: Context, offer: Offer) = File(folder(context), offer.apkName)

    /** The download in hand; null: none, or a record of another app version's form. */
    private fun held(prefs: SharedPreferences): Download? =
        prefs.getString(DOWNLOAD, null)?.let { runCatching { json.decodeFromString(Download.serializer(), it) }.getOrNull() }

    /** The offers of the last check, if they read. */
    private fun storedOffers(prefs: SharedPreferences): Offers? =
        prefs.getString(OFFERS, null)?.let { runCatching { json.decodeFromString(Offers.serializer(), it) }.getOrNull() }

    /** The download's files and its record. */
    private fun clearDownload(context: Context) {
        folder(context).listFiles()?.forEach { it.delete() }
        prefs(context).edit().remove(DOWNLOAD).apply()
    }

    private fun prefs(context: Context): SharedPreferences = context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)

    private fun log(context: Context, message: String) {
        Log.i(TAG, message)
        val file = synchronized(this) {
            updateLog ?: RotatingLog(SessionPaths(DeviceCheck.dataDir(context)).logs, "update").also { updateLog = it }
        }
        file.line(message)
    }
}

/** Android's answers about an app update's install session ([AppUpdater.installStatus]). */
class AppUpdateReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action == AppUpdater.ACTION_STATUS) AppUpdater.installStatus(context, intent)
    }
}
