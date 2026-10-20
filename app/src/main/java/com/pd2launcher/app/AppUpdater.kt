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
import android.net.ConnectivityManager
import android.os.StatFs
import android.os.SystemClock
import android.util.Log
import com.pd2launcher.app.AppUpdate.CheckProblem
import com.pd2launcher.app.AppUpdate.DownloadProblem
import com.pd2launcher.app.AppUpdate.InstallOutcome
import com.pd2launcher.app.AppUpdate.InstallProblem
import com.pd2launcher.app.AppUpdate.State
import com.pd2launcher.app.AppUpdate.offer
import java.io.File
import java.io.IOException
import java.net.ConnectException
import java.net.SocketTimeoutException
import java.net.UnknownHostException
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
import kotlinx.serialization.KSerializer
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/**
 * App updates ([AppUpdate]): looks for a newer release of this app on its GitHub releases (once a
 * day while automatic checks are on, or when the player asks), downloads its APK into the app's own
 * folder when the player asks, checks it, and hands it to Android's installer, which asks the
 * player to confirm. One worker thread does the work; the screens follow [state]. What it does
 * goes to `files/logs/update.log`, which Share logs packs.
 */
object AppUpdater {
    private const val TAG = "pd2-update"
    private const val PREFERENCES = "app_update"
    private const val ETAG = "etag"
    private const val OFFER = "offer"
    private const val LAST_SUCCESS = "last_success"
    private const val LAST_ATTEMPT = "last_attempt"
    private const val NOT_BEFORE = "not_before"
    private const val NOTIFIED = "notified"
    private const val DOWNLOAD = "download"
    private const val TEST_FEED = "test_feed"
    private const val FOLDER = "app-update"
    private const val CHANNEL = "app_update"
    private const val NOTIFICATION_ID = 2
    /** The most a `.sha256` file may hold. */
    private const val CHECKSUM_LIMIT = 4096
    /** HTTP statuses that say the release's file is gone, or has no such part: a check follows. */
    private val GONE = setOf(404, 410, 416)

    /** The install session's status, to [AppUpdateReceiver]. */
    const val ACTION_STATUS = "com.pd2launcher.app.action.APP_UPDATE_STATUS"
    /** Opens About: the notice of a new version. */
    const val EXTRA_ABOUT = "com.pd2launcher.app.extra.ABOUT"

    private val json = Json { ignoreUnknownKeys = true }
    private val worker = Executors.newSingleThreadExecutor { Thread(it, "pd2-update") }
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

    private val _state = MutableStateFlow<State>(State.Idle)
    val state: StateFlow<State> = _state.asStateFlow()

    /** Android's confirmation window for the committed update, for the activity to open while it is in front. */
    private val _confirmation = MutableStateFlow<Intent?>(null)
    val confirmation: StateFlow<Intent?> = _confirmation.asStateFlow()

    /** The download in hand: what it is of, and the SHA-256 it must have. */
    @Serializable
    private data class Download(val offer: Offer, val sha256: String)

    /**
     * Whether app updates work in this build: a release feed (in a debug build only the test feed,
     * [setTestFeed]) and a certificate updates must be signed with ([signer]).
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

    /** The app came to the front: a check when one is due. */
    fun resumed(context: Context) {
        val app = context.applicationContext
        if (started.get()) worker.execute { checkIfDue(app) }
    }

    /** Check now (About): at any time but during GitHub's wait. */
    fun checkNow(context: Context) {
        val app = context.applicationContext
        worker.execute { check(app, automatic = false) }
    }

    /** Download, Resume: the offer's APK, then its checks. */
    fun download(context: Context) {
        val app = context.applicationContext
        val current = _state.value
        if (current !is State.Available) return
        stopRequested = false
        // Only from the state shown: a check that replaced it meanwhile wins.
        if (!_state.compareAndSet(current, State.Downloading(current.offer, current.downloaded))) return
        worker.execute { downloadNow(app, current.offer) }
    }

    /** Stop: the download ends at its next block and keeps what came in, for Resume. */
    fun stop() {
        stopRequested = true
        _state.update { if (it is State.Downloading) it.copy(stopping = true) else it }
    }

    /**
     * Install: the checked download goes to Android's installer, which asks the player. Only when
     * nothing runs that the update would end ([AppUpdate.waitFor]) and Android lets this app install
     * updates; About shows why otherwise.
     */
    fun install(context: Context) {
        val app = context.applicationContext
        val current = _state.value
        if (current !is State.Ready && current !is State.Installing) return
        if (wait() != null || !app.packageManager.canRequestPackageInstalls()) return
        // One copy for Android at a time: a second tap meanwhile does nothing.
        if (!installRunning.compareAndSet(false, true)) return
        if (!_state.compareAndSet(current, State.Installing(current.offer))) {
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
        val installing = _state.value as? State.Installing ?: return
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
        val offer = _state.value.offer ?: return
        if (status == PackageInstaller.STATUS_PENDING_USER_ACTION) {
            val confirm = intent.getParcelableExtra(Intent.EXTRA_INTENT, Intent::class.java)
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
     * Debug builds only: the test feed ([AppUpdateHost]: `--es update_feed <url>`, empty for
     * none), which replaces everything known about updates, then a check as an automatic one (its
     * notice too).
     */
    fun setTestFeed(context: Context, url: String) {
        if (!BuildConfig.DEBUG) return
        val app = context.applicationContext
        worker.execute {
            abandonSessions(app)
            prefs(app).edit().clear().putString(TEST_FEED, url).apply()
            clearDownload(app)
            _state.value = State.Idle
            log(app, "test feed: ${url.ifEmpty { "none" }}")
            if (url.isNotEmpty()) check(app, automatic = true)
        }
    }

    /** Whether the active network counts data (mobile data, a metered hotspot). */
    fun metered(context: Context): Boolean =
        context.getSystemService(ConnectivityManager::class.java)?.isActiveNetworkMetered ?: false

    // ------------------------------------------------------------ worker thread

    private fun restore(context: Context) {
        // A session an earlier process committed can still have Android's window open (one in
        // its own task): this process cannot take it back, so it goes. Install makes a new one.
        abandonSessions(context)
        val prefs = prefs(context)
        val download = held(prefs)
        if (download == null) {
            // No record that reads (none, or one of another app version's form): no file of the
            // folder belongs to a download, so all go.
            clearDownload(context)
        } else if (download.offer.versionCode <= BuildConfig.VERSION_CODE) {
            // An update that installed since, or a download older than this app: what it left goes.
            log(context, "now ${BuildConfig.VERSION_NAME}: the download of ${download.offer.versionName} is removed")
            clearDownload(context)
            context.getSystemService(NotificationManager::class.java)?.cancel(NOTIFICATION_ID)
        }
        val offer = cachedOffer(prefs)
        if (offer != null && offer.versionCode <= BuildConfig.VERSION_CODE) prefs.edit().remove(OFFER).remove(ETAG).apply()
        _state.value = restored(context)
    }

    private fun restored(context: Context): State {
        val prefs = prefs(context)
        val download = held(prefs)
        if (download != null) {
            return if (apkFile(context, download.offer).isFile) {
                State.Ready(download.offer)
            } else {
                State.Available(download.offer, partFile(context, download.offer).length())
            }
        }
        val offer = cachedOffer(prefs)
        return when {
            offer != null -> State.Available(offer)
            prefs.getLong(LAST_SUCCESS, 0) > 0 -> State.UpToDate
            else -> State.Idle
        }
    }

    private fun checkIfDue(context: Context) {
        val prefs = prefs(context)
        val due = AppUpdate.checkDue(
            System.currentTimeMillis(), AppSettingsStore.current.value.updateChecks,
            prefs.getLong(LAST_SUCCESS, 0), prefs.getLong(LAST_ATTEMPT, 0), prefs.getLong(NOT_BEFORE, 0),
        )
        if (due) check(context, automatic = true)
    }

    private fun check(context: Context, automatic: Boolean) {
        if (!available(context) || _state.value is State.Installing) return
        val prefs = prefs(context)
        val now = System.currentTimeMillis()
        val held = _state.value.offer
        val notBefore = prefs.getLong(NOT_BEFORE, 0)
        if (AppUpdate.waitingForGitHub(now, notBefore)) {
            if (held == null) _state.value = State.CheckFailed(CheckProblem.BUSY, notBefore)
            return
        }
        prefs.edit().putLong(LAST_ATTEMPT, now).apply()
        // With a newer release in hand, About keeps it through the check.
        if (held == null) _state.value = State.Checking
        val answer = try {
            AppUpdateHttp.ask(feed(context), prefs.getString(ETAG, null), now)
        } catch (error: Exception) {
            log(context, "check failed: ${AppUpdate.describe(error)}")
            if (held == null) {
                val offline = error is UnknownHostException || error is ConnectException || error is SocketTimeoutException
                _state.value = State.CheckFailed(if (offline) CheckProblem.OFFLINE else CheckProblem.FAILED)
            }
            return
        }
        val offer: Offer? = when (answer) {
            is AppUpdateHttp.Answer.Wait -> {
                log(context, "GitHub asks to wait until ${answer.until}")
                prefs.edit().putLong(NOT_BEFORE, answer.until).apply()
                if (held == null) _state.value = State.CheckFailed(CheckProblem.BUSY, answer.until)
                return
            }
            AppUpdateHttp.Answer.NotModified -> cachedOffer(prefs)
            AppUpdateHttp.Answer.NoRelease -> null
            // A release build takes files only from its own repository's releases; a debug build's
            // test feed may name any HTTPS URL, or HTTP.
            is AppUpdateHttp.Answer.Found -> AppUpdate.offer(
                answer.releases, BuildConfig.VERSION_CODE, BuildConfig.UPDATE_APK_PREFIX,
                allowHttp = BuildConfig.DEBUG, filesUnder = BuildConfig.UPDATE_RELEASES.takeUnless { BuildConfig.DEBUG },
            )
        }
        val etag = (answer as? AppUpdateHttp.Answer.Found)?.etag
        prefs.edit().apply {
            putLong(LAST_SUCCESS, now)
            if (etag != null) putString(ETAG, etag) else if (answer != AppUpdateHttp.Answer.NotModified) remove(ETAG)
            if (offer != null) putString(OFFER, json.encodeToString(Offer.serializer(), offer)) else remove(OFFER)
        }.apply()
        val unchanged = if (answer == AppUpdateHttp.Answer.NotModified) "unchanged, " else ""
        log(context, "check: $unchanged${offer?.let { "${it.versionName} is out" } ?: "up to date"}")
        found(context, offer, held, automatic)
    }

    /**
     * What the check found: [offer] (null: nothing newer) replaces what was [held] unless it is the
     * same. An automatic check gives the notice of a new version ([notice], once per version); a
     * check the player asked for shows it in About, so it needs none.
     */
    private fun found(context: Context, offer: Offer?, held: Offer?, automatic: Boolean) {
        if (offer != held || held == null) {
            if (held != null) {
                // A newer release, a release built again, or one that was taken down: its download goes.
                log(context, "the download of ${held.versionName} is removed")
                clearDownload(context)
            }
            _state.value = if (offer != null) State.Available(offer) else State.UpToDate
        }
        if (offer == null) return
        if (automatic) {
            notice(context, offer)
        } else if (prefs(context).getInt(NOTIFIED, 0) < offer.versionCode) {
            prefs(context).edit().putInt(NOTIFIED, offer.versionCode).apply()
        }
    }

    private fun downloadNow(context: Context, offer: Offer) {
        // A check that ran first found another release, which replaced this one.
        if (_state.value.offer != offer) return
        val part = partFile(context, offer)
        // Stop came before the file's request: the server is not asked for it, and the part stays.
        fun stoppedBefore(): Boolean {
            if (!stopRequested) return false
            log(context, "download of ${offer.versionName} stopped at ${part.length()} bytes, before its request")
            _state.value = State.Available(offer, part.length())
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
                    _state.value = State.Available(offer, problem = DownloadProblem.CHECKSUM)
                    return
                }
                Download(offer, expected).also { prefs(context).edit().putString(DOWNLOAD, json.encodeToString(Download.serializer(), it)).apply() }
            }
            part.parentFile!!.mkdirs()
            val needed = AppUpdate.spaceNeeded(offer.apkSize, part.length(), unpackBytes(context))
            val free = StatFs(part.parentFile!!.path).availableBytes
            if (free < needed) {
                log(context, "not enough space for ${offer.versionName}: ${needed shr 20} MB needed, ${free shr 20} MB free")
                _state.value = State.Available(offer, part.length(), DownloadProblem.NO_SPACE, needed, free)
                return
            }
            if (stoppedBefore()) return
            log(context, "download of ${offer.versionName} from ${part.length()} bytes")
            AppUpdateHttp.fetch(offer.apkUrl, offer.apkSize, part, { stopRequested }, answered = { log(context, "download: $it") }) { done ->
                _state.value = State.Downloading(offer, done, stopRequested)
            }
            if (stopRequested) {
                log(context, "download stopped at ${part.length()} bytes")
                _state.value = State.Available(offer, part.length())
                return
            }
            _state.value = State.Verifying(offer)
            verify(context, download, part)
        } catch (error: Exception) {
            log(context, "download of ${offer.versionName} failed: ${AppUpdate.describe(error)}")
            val problem = when {
                stopRequested -> null
                error is IOException -> DownloadProblem.NETWORK
                else -> DownloadProblem.FAILED
            }
            _state.value = State.Available(offer, part.length(), problem)
            // The release's file is gone or another one: a check finds out whether the release still
            // offers it (else its offer and download go), also while automatic checks are off.
            val gone = error is AppUpdateHttp.OtherFile || (error as? AppUpdateHttp.Status)?.code in GONE
            if (gone && !stopRequested) check(context, automatic = false)
        }
    }

    /**
     * The download's checks ([AppUpdate.refusal]): a file that passes becomes the APK that Install
     * hands over; one that fails is deleted, with the record of its SHA-256, so Download starts again.
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
            _state.value = State.Available(offer, problem = DownloadProblem.REFUSED)
            return
        }
        if (!part.renameTo(apkFile(context, offer))) throw IOException("cannot keep the checked download")
        log(context, "the download of ${offer.versionName} passed its checks")
        _state.value = State.Ready(offer)
    }

    private fun installNow(context: Context, offer: Offer) {
        if (_state.value.offer != offer) return
        val download = held(prefs(context))?.takeIf { it.offer == offer }
        val file = apkFile(context, offer)
        if (download == null || !file.isFile) {
            log(context, "install of ${offer.versionName}: the checked download is gone")
            clearDownload(context)
            _state.value = State.Available(offer)
            return
        }
        val installer = context.packageManager.packageInstaller
        // Sessions an earlier try left (Install pressed again, or the app closed meanwhile) go
        // first; their answers no longer count.
        abandonSessions(context)
        val params = PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL).apply {
            setAppPackageName(context.packageName)
            setSize(file.length())
            setInstallReason(PackageManager.INSTALL_REASON_USER)
            setPackageSource(PackageInstaller.PACKAGE_SOURCE_DOWNLOADED_FILE)
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
                _state.value = State.Available(offer, problem = DownloadProblem.REFUSED)
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
            _state.update { if (it is State.Installing) State.Ready(offer, InstallProblem.FAILED) else it }
        }
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
        _state.value = State.Ready(offer, InstallProblem.GAME_STARTED)
    }

    private fun finished(offer: Offer, problem: InstallProblem) {
        session = null
        _confirmation.value = null
        _state.value = State.Ready(offer, problem)
    }

    private fun wait(): AppUpdate.Wait? = AppUpdate.waitFor(SessionService.status.value, copying(), SystemClock.elapsedRealtime())

    private fun copying(): Boolean = SourceImport.copy.value is SourceCopy.Copying

    /**
     * The notice of a new version: an Android notification, once per version, from automatic
     * checks. Without a channel yet (a first start) it waits for a later check: the session's first
     * channel brings Android's notification prompt over Check, not this notice.
     */
    private fun notice(context: Context, offer: Offer) {
        val prefs = prefs(context)
        if (prefs.getInt(NOTIFIED, 0) >= offer.versionCode) return
        val manager = context.getSystemService(NotificationManager::class.java) ?: return
        if (manager.notificationChannels.isEmpty()) return
        if (manager.getNotificationChannel(CHANNEL) == null) {
            manager.createNotificationChannel(
                NotificationChannel(CHANNEL, context.getString(R.string.app_update_channel), NotificationManager.IMPORTANCE_LOW),
            )
        }
        // The app's own activity, with what was on top of it in its task closed (Android's
        // settings page, its installer window, a picker), not a second activity over them.
        val about = Intent(context, MainActivity::class.java).putExtra(EXTRA_ABOUT, true)
            .addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP)
        val open = PendingIntent.getActivity(context, NOTIFICATION_ID, about, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
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
        prefs.edit().putInt(NOTIFIED, offer.versionCode).apply()
    }

    /** Where checks ask: the release feed, or in a debug build only its test feed. */
    private fun feed(context: Context): String =
        if (BuildConfig.DEBUG) prefs(context).getString(TEST_FEED, null).orEmpty() else BuildConfig.UPDATE_FEED

    /**
     * The certificate an update must be signed with (SHA-256, lower-case hex): the release
     * certificate, while this app is signed with it too (a build signed with another key gets no
     * updates from the releases, which Android would refuse); in a debug build this app's own.
     */
    private fun signer(context: Context): String? {
        if (!signerRead) {
            val own = try {
                val flags = PackageManager.PackageInfoFlags.of(PackageManager.GET_SIGNING_CERTIFICATES.toLong())
                context.packageManager.getPackageInfo(context.packageName, flags).signingInfo?.apkContentsSigners
                    ?.singleOrNull()?.let { sha256(it.toByteArray()) }
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
     * GET_SIGNATURES, so it is asked for too; only signingInfo is read.
     */
    @Suppress("DEPRECATION")
    private fun archive(context: Context, file: File): AppUpdate.Archive? {
        val flags = PackageManager.PackageInfoFlags.of((PackageManager.GET_SIGNING_CERTIFICATES or PackageManager.GET_SIGNATURES).toLong())
        val info = context.packageManager.getPackageArchiveInfo(file.path, flags) ?: return null
        val signers = info.signingInfo?.apkContentsSigners?.map { sha256(it.toByteArray()) } ?: return null
        return AppUpdate.Archive(info.packageName, info.longVersionCode, signers)
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

    private fun held(prefs: SharedPreferences): Download? = prefs.getString(DOWNLOAD, null)?.let { decode(Download.serializer(), it) }

    private fun cachedOffer(prefs: SharedPreferences): Offer? = prefs.getString(OFFER, null)?.let { decode(Offer.serializer(), it) }

    /** The download's files and its record. */
    private fun clearDownload(context: Context) {
        folder(context).listFiles()?.forEach { it.delete() }
        prefs(context).edit().remove(DOWNLOAD).apply()
    }

    /** A stored value, or null when it no longer reads (an older form). */
    private fun <T> decode(serializer: KSerializer<T>, text: String): T? = runCatching { json.decodeFromString(serializer, text) }.getOrNull()

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
