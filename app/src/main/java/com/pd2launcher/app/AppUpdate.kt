package com.pd2launcher.app

import android.content.pm.PackageInstaller
import com.pd2launcher.app.control.Game
import com.pd2launcher.app.control.Job
import com.pd2launcher.app.control.OperationStatus
import com.pd2launcher.app.ui.Play
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/** A release as GitHub's releases API describes it; only what the app update's check reads. */
@Serializable
data class Release(
    @SerialName("tag_name") val tag: String,
    val draft: Boolean = false,
    val prerelease: Boolean = false,
    @SerialName("html_url") val page: String? = null,
    val assets: List<Asset> = emptyList(),
) {
    @Serializable
    data class Asset(
        val name: String,
        val size: Long,
        @SerialName("browser_download_url") val url: String,
        /** GitHub's own SHA-256 of the file, `sha256:<hex>`; null for a file it has none for. */
        val digest: String? = null,
        /** `uploaded` once the file is complete. */
        val state: String? = null,
    )
}

/** A newer release of this app that it can update to ([AppUpdate.offer]). */
@Serializable
data class Offer(
    val versionName: String,
    val versionCode: Int,
    /** The APK's name on the release: the APK prefix, the versionName and `.apk`. */
    val apkName: String,
    val apkUrl: String,
    val apkSize: Long,
    /** GitHub's SHA-256 of the APK, lower-case hex; null when GitHub gives none. */
    val githubSha256: String?,
    /** The release's `<apkName>.sha256`, which its build wrote. */
    val checksumUrl: String,
    /** The release's page (What's new); null when there is none. */
    val page: String?,
)

/**
 * App updates (a newer release of this app itself, not an update of Project Diablo 2): the rules of
 * the check, of the download's checks and of the safe moment to install, without Android's
 * services. [AppUpdater] does the work and keeps the [State].
 */
object AppUpdate {
    /** A day between automatic checks that worked, an hour after one that failed. */
    const val CHECK_INTERVAL_MS = 24 * 60 * 60 * 1000L
    const val RETRY_INTERVAL_MS = 60 * 60 * 1000L
    /** The shortest wait after GitHub refused a check without saying how long ([retryAt]). */
    const val MIN_WAIT_MS = 60_000L
    /** A larger APK is not taken as this app's. */
    const val MAX_APK_BYTES = 1L shl 30

    private val VERSION = Regex("""(\d{1,4})\.(\d{1,2})\.(\d{1,2})(?:-beta([1-9]|[1-8]\d|9[0-8]))?""")
    private val HEX64 = Regex("[0-9a-f]{64}")
    private val CHECKSUM_LINE = Regex("""([0-9A-Fa-f]{64}) [ *](.+)""")
    private val RUNNING = setOf(
        OperationStatus.QUEUED, OperationStatus.PLANNING, OperationStatus.RUNNING,
        OperationStatus.SAFE_STOPPING, OperationStatus.VERIFYING, OperationStatus.COMMITTING,
    )

    /**
     * The versionCode of a versionName as app/build.gradle.kts computes it: MAJOR.MINOR.PATCH[-betaN]
     * is ((MAJOR * 100 + MINOR) * 100 + PATCH) * 100 + N, with 99 for the release itself. Null for
     * any other name, and for one whose code does not fit.
     */
    fun versionCode(name: String): Int? {
        val (major, minor, patch, beta) = VERSION.matchEntire(name)?.destructured ?: return null
        val code = ((major.toLong() * 100 + minor.toLong()) * 100 + patch.toLong()) * 100 + (beta.toLongOrNull() ?: 99)
        return code.takeIf { it <= Int.MAX_VALUE }?.toInt()
    }

    /**
     * The update [release] offers to an app whose versionCode is [installed]: a published release
     * (not a draft, not a pre-release, not a beta by its name) tagged `v<versionName>`, newer than
     * the app, with its complete APK ([apkPrefix]`<versionName>.apk`, of a sane size) and that
     * file's `.sha256`. Null: no update. The files' URLs must be below [filesUnder] (a release
     * build's own releases, [usableUrl]); without it, any HTTPS URL, and with [allowHttp] (a debug
     * build's test feed) HTTP too. A page (What's new) that breaks the rule is left out.
     */
    fun offer(release: Release, installed: Int, apkPrefix: String, allowHttp: Boolean = false, filesUnder: String? = null): Offer? {
        if (release.draft || release.prerelease || !release.tag.startsWith("v")) return null
        val name = release.tag.substring(1)
        val code = versionCode(name)?.takeIf { '-' !in name } ?: return null
        if (code <= installed) return null
        val apkName = "$apkPrefix$name.apk"
        fun usable(asset: Release.Asset) = (asset.state == null || asset.state == "uploaded") && usableUrl(asset.url, allowHttp, filesUnder)
        val apk = release.assets.firstOrNull { it.name == apkName && usable(it) } ?: return null
        val checksum = release.assets.firstOrNull { it.name == "$apkName.sha256" && usable(it) } ?: return null
        if (apk.size !in 1..MAX_APK_BYTES) return null
        return Offer(
            name, code, apkName, apk.url, apk.size, sha256Of(apk.digest), checksum.url,
            release.page?.takeIf { usableUrl(it, allowHttp, filesUnder) },
        )
    }

    /**
     * The update a list of [releases] (GitHub's, newest first) offers: the highest version that
     * [offer] takes from one release. A later fix of an older line does not hide a newer version.
     */
    fun offer(releases: List<Release>, installed: Int, apkPrefix: String, allowHttp: Boolean = false, filesUnder: String? = null): Offer? =
        releases.mapNotNull { offer(it, installed, apkPrefix, allowHttp, filesUnder) }.maxByOrNull { it.versionCode }

    private val PATH_REST = Regex("[A-Za-z0-9._~/-]*")

    /**
     * Whether a file's [url] may be used: below [filesUnder] with only plain path characters after
     * it and no `..` (so it cannot climb out); without [filesUnder], HTTPS, or HTTP too with
     * [allowHttp].
     */
    fun usableUrl(url: String, allowHttp: Boolean, filesUnder: String?): Boolean {
        if (filesUnder != null) {
            if (!url.startsWith(filesUnder)) return false
            val rest = url.substring(filesUnder.length)
            return PATH_REST.matches(rest) && ".." !in rest
        }
        return url.startsWith("https://") || (allowHttp && url.startsWith("http://"))
    }

    /** GitHub's `sha256:<hex>` as lower-case hex; null for anything else. */
    fun sha256Of(digest: String?): String? {
        val value = digest?.lowercase() ?: return null
        if (!value.startsWith("sha256:")) return null
        return value.removePrefix("sha256:").takeIf(HEX64::matches)
    }

    /**
     * The SHA-256 a `.sha256` file names for [apkName] (sha256sum's lines: `<hex>  <name>`, or
     * `<hex> *<name>`), lower-case; null when no line names it.
     */
    fun checksum(text: String, apkName: String): String? =
        text.lineSequence()
            .mapNotNull { CHECKSUM_LINE.matchEntire(it.trimEnd('\r')) }
            .firstOrNull { it.groupValues[2] == apkName }
            ?.groupValues?.get(1)?.lowercase()

    /**
     * The SHA-256 the download must have: the one in the release's `.sha256` ([fromFile]), which
     * GitHub's digest of the APK ([fromGitHub]), when there is one, must equal. Null: none, or the
     * two disagree; nothing is downloaded then.
     */
    fun expectedSha256(fromFile: String?, fromGitHub: String?): String? =
        fromFile?.takeIf { fromGitHub == null || it == fromGitHub }

    /**
     * Whether an automatic check is due at [now] (wall-clock milliseconds): automatic checks are
     * on ([enabled]), GitHub's wait has passed ([notBefore], [waitingForGitHub]), a day has passed
     * since the last check that worked ([lastSuccess]) and an hour since the last try
     * ([lastAttempt]). A time after [now] (the clock went back) holds nothing off.
     */
    fun checkDue(now: Long, enabled: Boolean, lastSuccess: Long, lastAttempt: Long, notBefore: Long): Boolean {
        if (!enabled || waitingForGitHub(now, notBefore)) return false
        fun passed(at: Long, interval: Long) = at <= 0 || at > now || now - at >= interval
        return passed(lastSuccess, CHECK_INTERVAL_MS) && passed(lastAttempt, RETRY_INTERVAL_MS)
    }

    /** GitHub asked for no request before [notBefore]; a wait longer than a day is taken as a clock change. */
    fun waitingForGitHub(now: Long, notBefore: Long): Boolean = notBefore > now && notBefore - now <= CHECK_INTERVAL_MS

    /**
     * When GitHub allows the next request after it answered [code] ([header] reads the answer's
     * headers by name): a refusal (403 or 429) waits for its `retry-after` seconds, else until
     * `x-ratelimit-reset` when no request is left, else at least [MIN_WAIT_MS]; at most a day.
     * Null for any other answer.
     */
    fun retryAt(code: Int, header: (String) -> String?, now: Long): Long? {
        if (code != 403 && code != 429) return null
        val after = header("retry-after")?.trim()?.toLongOrNull()?.takeIf { it >= 0 }?.let { now + it * 1000 }
        val reset = header("x-ratelimit-reset")?.trim()?.toLongOrNull()?.times(1000)
            ?.takeIf { header("x-ratelimit-remaining")?.trim() == "0" }
        val at = maxOf(after ?: reset ?: 0, now + MIN_WAIT_MS)
        return minOf(at, now + CHECK_INTERVAL_MS)
    }

    private val CONTENT_RANGE = Regex("""bytes (\d+)-\d+/(\d+|\*)""")

    /** The start of a `Content-Range: bytes <start>-<end>/<total>`; null for any other value. */
    fun rangeStart(contentRange: String?): Long? =
        contentRange?.let { CONTENT_RANGE.matchEntire(it.trim()) }?.groupValues?.get(1)?.toLongOrNull()

    private val LOCAL_END = Regex("""\s*from /\S+ \(port \d+\)""")

    /**
     * An error for the update log: its class and its message without the phone's own address
     * (Android's connection errors name it: " from /<address> (port <n>)").
     */
    fun describe(error: Throwable): String {
        val message = error.message?.replace(LOCAL_END, "")
        return error.javaClass.simpleName + (message?.let { ": $it" } ?: "")
    }

    /** The whole file's size in a `Content-Range`; null when it is not given (`*`) or for any other value. */
    fun rangeTotal(contentRange: String?): Long? =
        contentRange?.let { CONTENT_RANGE.matchEntire(it.trim()) }?.groupValues?.get(2)?.toLongOrNull()

    /**
     * The free space an update needs in the app's folder: the rest of the download, the copy
     * Android's installer makes, and the unpack of the new version's game support files at its
     * first start ([unpackBytes], estimated from this version's).
     */
    fun spaceNeeded(apkSize: Long, downloaded: Long, unpackBytes: Long): Long =
        (apkSize - downloaded).coerceAtLeast(0) + apkSize + unpackBytes.coerceAtLeast(0)

    /** Why the update waits: something runs that replacing the app would end. */
    enum class Wait { GAME, OFFICIAL_LAUNCHER, WORK, STARTING, PREPARING }

    /**
     * What the update waits for: Android ends every process of the app when it replaces it, the
     * game environment's too. Nothing may run then: no game, no official PD2 launcher, no engine
     * job or change to the installation, no folder [copying], no Play pressed in the last
     * [Play.LAUNCH_PENDING_MS] (at [now], elapsed real time), no unpack of the game support files,
     * and no engine that starts, starts again, has not sent its first state (a restart clears it)
     * or has not ended its first audit. Null: the update may install.
     */
    fun waitFor(status: SessionStatus, copying: Boolean, now: Long): Wait? {
        val snapshot = status.snapshot
        val launching = status.launchRequestedAt?.let { now - it < Play.LAUNCH_PENDING_MS } == true
        val starting = status.engine == SessionStatus.Engine.STARTING || status.engine == SessionStatus.Engine.RESTARTING ||
            (status.engine == SessionStatus.Engine.CONNECTED && snapshot == null) ||
            (snapshot != null && !Onboarding.audited(snapshot))
        return when {
            snapshot?.game == Game.RUNNING || status.launcherGame -> Wait.GAME
            snapshot?.activeJob == Job.OFFICIAL_LAUNCHER -> Wait.OFFICIAL_LAUNCHER
            snapshot?.activeJob != null || copying -> Wait.WORK
            snapshot?.operation?.let { it.mutating && it.status in RUNNING } == true -> Wait.WORK
            launching -> Wait.STARTING
            status.userland is SessionStatus.Userland.Checking || status.userland is SessionStatus.Userland.Unpacking || starting -> Wait.PREPARING
            else -> null
        }
    }

    /** What a downloaded file says about itself, as Android's package manager reads it. */
    data class Archive(val packageName: String, val versionCode: Long, val signers: List<String>)

    /** Why a download is refused: it is deleted and never handed to Android. */
    enum class Refusal { SIZE, CHECKSUM, NOT_AN_APP, OTHER_APP, VERSION, SIGNER }

    /**
     * Whether the download of [offer] is what it must be: [size] bytes with the SHA-256 [expected]
     * ([sha256] as read); an app ([archive]; null: Android cannot read it as one) of this app's
     * [packageName] and of the offer's version, newer than [installed]; signed by exactly one
     * certificate, whose SHA-256 is [signer]. Null when it is, else the first reason it is not.
     */
    fun refusal(
        offer: Offer,
        size: Long,
        sha256: String,
        expected: String,
        archive: Archive?,
        packageName: String,
        installed: Int,
        signer: String,
    ): Refusal? = when {
        size != offer.apkSize -> Refusal.SIZE
        sha256 != expected -> Refusal.CHECKSUM
        archive == null -> Refusal.NOT_AN_APP
        archive.packageName != packageName -> Refusal.OTHER_APP
        archive.versionCode != offer.versionCode.toLong() || archive.versionCode <= installed -> Refusal.VERSION
        signer.isEmpty() || archive.signers != listOf(signer) -> Refusal.SIGNER
        else -> null
    }

    /** How Android's installer ended a session, in the app's terms. */
    enum class InstallOutcome { INSTALLED, CANCELLED, NO_SPACE, FAILED }

    /** [PackageInstaller]'s session status as an [InstallOutcome]; the waits for the player are not one. */
    fun installOutcome(status: Int): InstallOutcome = when (status) {
        PackageInstaller.STATUS_SUCCESS -> InstallOutcome.INSTALLED
        PackageInstaller.STATUS_FAILURE_ABORTED -> InstallOutcome.CANCELLED
        PackageInstaller.STATUS_FAILURE_STORAGE -> InstallOutcome.NO_SPACE
        else -> InstallOutcome.FAILED
    }

    /** Where the app update stands. */
    sealed interface State {
        /** Nothing known yet: never checked, or checks are off. */
        data object Idle : State

        /** A check runs and no newer release is known. */
        data object Checking : State

        /** The last check found no newer release. */
        data object UpToDate : State

        /** The last check failed and no newer release is known; [retryAt]: GitHub's wait, for [CheckProblem.BUSY]. */
        data class CheckFailed(val problem: CheckProblem, val retryAt: Long = 0) : State

        /**
         * [offer] is newer; [downloaded] bytes of it are kept for Resume; [problem]: why the last
         * download stopped, with the space it [needed] and the [free] space for [DownloadProblem.NO_SPACE].
         */
        data class Available(
            override val offer: Offer,
            val downloaded: Long = 0,
            val problem: DownloadProblem? = null,
            val needed: Long = 0,
            val free: Long = 0,
        ) : WithOffer

        /** [done] bytes are in; Stop was pressed when [stopping]. */
        data class Downloading(override val offer: Offer, val done: Long, val stopping: Boolean = false) : WithOffer

        /** The download's checks run. */
        data class Verifying(override val offer: Offer) : WithOffer

        /** The checked download waits for Install; [problem]: why the last install did not happen. */
        data class Ready(override val offer: Offer, val problem: InstallProblem? = null) : WithOffer

        /** Android has the update and asks the player. */
        data class Installing(override val offer: Offer) : WithOffer
    }

    /** A state that has a newer release in hand. */
    sealed interface WithOffer : State {
        val offer: Offer
    }

    enum class CheckProblem { OFFLINE, BUSY, FAILED }

    /** [CHECKSUM]: the release has no usable checksum; [REFUSED]: the file failed its checks ([Refusal]). */
    enum class DownloadProblem { NETWORK, NO_SPACE, CHECKSUM, REFUSED, FAILED }

    /** [GAME_STARTED]: the game or other work ([waitFor]) started while Android asked, so the update was taken back. */
    enum class InstallProblem { CANCELLED, GAME_STARTED, NO_SPACE, FAILED }

    /** The newer release a state has in hand; null for none. */
    val State.offer: Offer? get() = (this as? WithOffer)?.offer

    /** What the player can do about the update. */
    enum class Action { CHECK, DOWNLOAD, RESUME, STOP, ALLOW, INSTALL }

    /**
     * What About shows: the state in words ([line], a warning or an error by [tone]), a bar while
     * work runs ([progress] 0..1, null without an amount), the one [action] (disabled unless
     * [enabled]), a [note] under it (a warning when [noteWarns]) and the release's [page].
     */
    data class View(
        val line: Line,
        val tone: Tone,
        val bar: Boolean = false,
        val progress: Float? = null,
        val action: Action? = null,
        val enabled: Boolean = true,
        val note: Line? = null,
        val noteWarns: Boolean = false,
        val page: String? = null,
    )

    /**
     * About's view of [state]: [wait] says what the install waits for, [metered] whether the network
     * counts data, [allowed] whether Android lets this app install updates ("Install unknown apps");
     * [now] is wall-clock time, for GitHub's wait.
     */
    fun view(state: State, wait: Wait?, metered: Boolean, allowed: Boolean, now: Long): View = when (state) {
        State.Idle -> View(Line(R.string.app_update_idle), Tone.PENDING, action = Action.CHECK)
        State.Checking -> View(Line(R.string.app_update_checking), Tone.WORKING, bar = true)
        State.UpToDate -> View(Line(R.string.app_update_up_to_date), Tone.OK, action = Action.CHECK)
        is State.CheckFailed -> when (state.problem) {
            CheckProblem.OFFLINE -> View(Line(R.string.app_update_check_offline), Tone.WARNING, action = Action.CHECK)
            CheckProblem.BUSY -> View(Line(R.string.app_update_check_busy, minutesUntil(state.retryAt, now)), Tone.WARNING, action = Action.CHECK)
            CheckProblem.FAILED -> View(Line(R.string.app_update_check_failed), Tone.WARNING, action = Action.CHECK)
        }
        is State.Available -> available(state, metered)
        is State.Downloading -> View(
            Line(R.string.app_update_downloading, state.offer.versionName, megabytes(state.done), megabytes(state.offer.apkSize, up = true)),
            Tone.WORKING, bar = true, progress = fraction(state.done, state.offer.apkSize), action = Action.STOP, enabled = !state.stopping,
        )
        is State.Verifying -> View(Line(R.string.app_update_verifying), Tone.WORKING, bar = true)
        is State.Ready -> ready(state, wait, allowed)
        is State.Installing -> View(Line(R.string.app_update_installing), Tone.WORKING, action = Action.INSTALL, note = Line(R.string.app_update_installing_note))
    }

    private fun available(state: State.Available, metered: Boolean): View {
        val offer = state.offer
        val size = megabytes(offer.apkSize, up = true)
        val start = if (state.downloaded > 0) Action.RESUME else Action.DOWNLOAD
        val note = Line(R.string.app_update_metered, size).takeIf { metered }
        val bar = state.downloaded > 0
        val progress = fraction(state.downloaded, offer.apkSize)
        // A stop with nothing kept (none came in, or the server had no such part) starts again with Download.
        val kept = state.downloaded > 0
        return when (state.problem) {
            null -> if (kept) {
                View(Line(R.string.app_update_paused, offer.versionName, megabytes(state.downloaded), size), Tone.PENDING, bar, progress, start, note = note, noteWarns = metered, page = offer.page)
            } else {
                View(Line(R.string.app_update_available, offer.versionName, size), Tone.OK, action = start, note = note, noteWarns = metered, page = offer.page)
            }
            DownloadProblem.NETWORK -> View(
                Line(if (kept) R.string.app_update_download_network else R.string.app_update_download_network_nothing_kept),
                Tone.WARNING, bar, progress, start, note = note, noteWarns = metered,
            )
            DownloadProblem.NO_SPACE -> View(Line(R.string.app_update_no_space, megabytes(state.needed, up = true), megabytes(state.free)), Tone.WARNING, bar, progress, start)
            DownloadProblem.CHECKSUM -> View(Line(R.string.app_update_no_checksum), Tone.ERROR, action = Action.DOWNLOAD)
            DownloadProblem.REFUSED -> View(Line(R.string.app_update_refused), Tone.ERROR, action = Action.DOWNLOAD)
            DownloadProblem.FAILED -> View(
                Line(if (kept) R.string.app_update_download_failed else R.string.app_update_download_failed_nothing_kept),
                Tone.WARNING, bar, progress, start,
            )
        }
    }

    private fun ready(state: State.Ready, wait: Wait?, allowed: Boolean): View {
        val line = when (state.problem) {
            null -> Line(R.string.app_update_ready, state.offer.versionName)
            InstallProblem.CANCELLED -> Line(R.string.app_update_cancelled)
            InstallProblem.GAME_STARTED -> Line(R.string.app_update_put_off)
            InstallProblem.NO_SPACE -> Line(R.string.app_update_install_no_space)
            InstallProblem.FAILED -> Line(R.string.app_update_install_failed)
        }
        val tone = if (state.problem == InstallProblem.NO_SPACE || state.problem == InstallProblem.FAILED) Tone.WARNING else Tone.OK
        return when {
            wait != null -> View(line, tone, action = Action.INSTALL, enabled = false, note = waitLine(wait), noteWarns = true, page = state.offer.page)
            !allowed -> View(line, tone, action = Action.ALLOW, note = Line(R.string.app_update_allow_note), page = state.offer.page)
            else -> View(line, tone, action = Action.INSTALL, note = Line(R.string.app_update_install_note), page = state.offer.page)
        }
    }

    /** Why Install waits, in the player's words. */
    fun waitLine(wait: Wait): Line = Line(
        when (wait) {
            Wait.GAME -> R.string.app_update_wait_game
            Wait.OFFICIAL_LAUNCHER -> R.string.app_update_wait_launcher
            Wait.WORK -> R.string.app_update_wait_work
            Wait.STARTING -> R.string.app_update_wait_starting
            Wait.PREPARING -> R.string.app_update_wait_preparing
        },
    )

    /** Whole MB (2^20 bytes, as the app's other sizes), rounded down, or up for a total. */
    fun megabytes(bytes: Long, up: Boolean = false): Long = (bytes.coerceAtLeast(0) + if (up) MB - 1 else 0) / MB

    private const val MB = 1L shl 20

    private fun fraction(done: Long, total: Long): Float = if (total > 0) (done.toFloat() / total).coerceIn(0f, 1f) else 0f

    /** Whole minutes from [now] to [at], at least one. */
    private fun minutesUntil(at: Long, now: Long): Long = ((at - now + 59_999) / 60_000).coerceAtLeast(1)
}
