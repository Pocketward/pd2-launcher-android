package com.pd2launcher.app

import android.content.pm.PackageInstaller
import com.pd2launcher.app.control.Game
import com.pd2launcher.app.control.Job
import com.pd2launcher.app.control.OperationStatus
import com.pd2launcher.app.ui.Play
import com.pd2launcher.app.ui.PageLine
import java.net.ConnectException
import java.net.SocketTimeoutException
import java.net.UnknownHostException
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/**
 * Which app updates the app offers (Settings → App → Update channel): the newest build of each
 * channel on GitHub's list of releases ([AppUpdate.offers]). A release must say its channel twice,
 * by its flag and by its tag.
 */
enum class UpdateChannel {
    /** The newest published release that is not a pre-release, tagged `v<x.y.z>`. */
    STABLE,

    /** The newest published pre-release, tagged `v<x.y.z>-beta<n>`. */
    BETA,
}

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

/** The stops of the d-pad along a channel's row, left to right ([AppUpdate.parts]). */
enum class RowPart { ROW, CHIP, ACTION, SIGN }

/** A release of this app that the app can download and install ([AppUpdate.offerOf]). */
@Serializable
data class Offer(
    val versionName: String,
    val versionCode: Int,
    /** The APK's name on the release ([AppUpdate.offerOf]). */
    val apkName: String,
    val apkUrl: String,
    val apkSize: Long,
    /** GitHub's SHA-256 of the APK, lower-case hex; null when GitHub gives none. */
    val githubSha256: String?,
    /** The release's `<apkName>.sha256`, which its build wrote. */
    val checksumUrl: String,
    /**
     * The release's page (the change log): only its own tag page, `.../releases/tag/<tag>`, under the
     * rules of links; null for any other address (a test feed's base page, say), so the chip opens nothing.
     */
    val page: String?,
)

/** The newest build of each channel on the list of releases ([AppUpdate.offers]); null: none out now. */
@Serializable
data class Offers(val stable: Offer?, val beta: Offer?)

/**
 * App updates (a newer release of this app itself, not an update of Project Diablo 2): the rules of
 * the two channels on GitHub's list of releases, of the download's checks and of the safe moment to
 * install, without Android's services. [AppUpdater] does the work and keeps the [Check], the [Work]
 * and the [Offers].
 */
object AppUpdate {
    /**
     * An hour between automatic checks that worked: one request of the 60 an hour that GitHub
     * allows each IP address without a token. A 304 for the ETag counts too: only a request with
     * a token gets it free.
     */
    const val CHECK_INTERVAL_MS = 60 * 60 * 1000L
    /**
     * 15 min after a try that failed. Most fail with no network and send nothing to GitHub; one
     * that reaches it costs at most 4 requests an hour. The server's own refusals wait as it says ([retryAt]).
     */
    const val RETRY_INTERVAL_MS = 15 * 60 * 1000L
    /**
     * How often the app asks [checkDue] while it is in front ([AppUpdateHost]); a request goes out
     * only when a check is due. Each ask is a few reads on the updater's own thread.
     */
    const val TIMER_MS = 5 * 60 * 1000L
    private const val DAY_MS = 24 * 60 * 60 * 1000L
    /** The page says when the app checked only after a check this recent ([status]). */
    const val RECENT_CHECK_MS = 7 * DAY_MS
    /** The shortest wait after the server refused a check without saying how long ([retryAt]). */
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
     * A version name's place in the order of versions: MAJOR.MINOR.PATCH[-betaN] is ((MAJOR * 100 +
     * MINOR) * 100 + PATCH) * 100 + N, with 99 for a Stable. That is also a Stable's versionCode
     * (app/build.gradle.kts); a beta has the versionCode of the Stable it was made after, so inside
     * a channel only the name says which build is newer. Null for any other name, and for one whose
     * value does not fit.
     */
    fun rank(name: String): Int? {
        val (major, minor, patch, beta) = VERSION.matchEntire(name)?.destructured ?: return null
        val code = ((major.toLong() * 100 + minor.toLong()) * 100 + patch.toLong()) * 100 + (beta.toLongOrNull() ?: 99)
        return code.takeIf { it <= Int.MAX_VALUE }?.toInt()
    }

    /** A version name as the page shows it: a non-breaking hyphen, so "0.4.0-beta1" never breaks in two. */
    fun shown(version: String): String = version.replace('-', '\u2011')

    /**
     * The channel of a versionName: Beta for one with `-beta` (MAJOR.MINOR.PATCH-betaN), else
     * Stable. A build uses its own channel until the player picks one ([channel]).
     */
    fun channelOf(versionName: String): UpdateChannel = if ("-beta" in versionName) UpdateChannel.BETA else UpdateChannel.STABLE

    /**
     * The channel the checks follow: the player's pick ([picked]), else, before a pick, the
     * channel of this build's [versionName] ([channelOf]). While Beta is closed ([betaOpen] false:
     * no beta is out that this build can take) always Stable: a stored Beta pick, or a beta build,
     * follows Stable, and the stored pick stays for when a beta is out again. Stable never offers a
     * lower code ([rows]).
     */
    fun channel(picked: UpdateChannel?, versionName: String, betaOpen: Boolean = true): UpdateChannel =
        if (!betaOpen) UpdateChannel.STABLE else picked ?: channelOf(versionName)

    /**
     * Whether the Beta channel is open: a beta is out that this build can take (the owner,
     * 2026-11-03, "When a beta is out": the Beta row is greyed like Loot filters until one is out,
     * and opens with no app update once it is). "Can take": the feed's newest pre-release passed
     * the feed rules ([offerOf]: a published pre-release tagged `v<x.y.z>-beta<n>`, its APK named
     * with its code, its links), its code is not below this build's ([installedCode]: never a
     * downgrade), and its upload was not refused for good ([refused], [upload]). The APK's
     * certificate is checked only after the download ([refusal]), so it cannot open or close the row.
     * A beta download in hand ([work]) keeps the row open, also when the feed no longer offers it.
     */
    fun betaOpen(offers: Offers?, installedCode: Int, refused: Set<String>, work: Work? = null): Boolean {
        if (work != null && channelOf(work.offer.versionName) == UpdateChannel.BETA) return true
        val beta = offers?.beta ?: return false
        return beta.versionCode >= installedCode && upload(beta) !in refused
    }

    /**
     * What a press on the row of [pressed] stores as the player's pick: null, nothing (a closed Beta
     * takes no pick, and a press on the channel already in use writes nothing, so a press on the
     * Stable row that a closed Beta shows keeps a stored Beta pick); else [pressed]. Before any pick
     * ([stored] null) a press stores the player's own pick, which then wins over the build's.
     */
    fun storedPick(stored: UpdateChannel?, pressed: UpdateChannel, versionName: String, betaOpen: Boolean): UpdateChannel? = when {
        pressed == UpdateChannel.BETA && !betaOpen -> null
        stored == null -> pressed
        pressed == channel(stored, versionName, betaOpen) -> null
        else -> pressed
    }

    /**
     * Whether Android's question opens by itself when the download of [offer] passed its checks: only
     * while its channel is still the [selected] one (the owner, 2026-11-04, "Wait on its own row";
     * this replaces the rule of 2026-10-30 that the question always follows the download). Else the
     * download waits on its own row with its install sign, and a press on it selects that row first.
     */
    fun asksAtOnce(offer: Offer, selected: UpdateChannel): Boolean = channelOf(offer.versionName) == selected

    /**
     * What [offerOf] asks of a release's files: the APK names' [apkPrefix]; [filesUnder], a release
     * build's own releases, holds every link ([usableUrl]); [allowHttp]: a debug build's test feed.
     */
    data class FeedRules(val apkPrefix: String, val filesUnder: String?, val allowHttp: Boolean)

    /**
     * The rules of a build: a release build ([debug] false) takes links only below its own
     * [releases], over HTTPS; a debug build's test feed may name any HTTPS address, or HTTP.
     */
    fun feedRules(apkPrefix: String, releases: String, debug: Boolean) =
        FeedRules(apkPrefix, filesUnder = releases.takeUnless { debug }, allowHttp = debug)

    /** The rules of this app's own builds ([feedRules]). */
    fun rulesOf(debug: Boolean) = feedRules(BuildConfig.UPDATE_APK_PREFIX, BuildConfig.UPDATE_RELEASES, debug)

    /** Where checks ask: the release feed, or in a [debug] build only its [testFeed]. */
    fun feedOf(debug: Boolean, testFeed: String?): String = if (debug) testFeed.orEmpty() else BuildConfig.UPDATE_FEED

    /**
     * The build [release] offers: a published release (never a draft) tagged `v<versionName>`, whose
     * flag and tag name the same channel, with its complete APK of a sane size and that file's
     * `.sha256`. A Stable's code comes from its name, and its APK is [FeedRules.apkPrefix]
     * `<versionName>.apk`, the name that the updater of 0.3.x reads. A beta's code comes only from
     * its one APK's name ([betaCode]), as the code of the Stable it was made after does not follow
     * from the beta's own name. Null: none. A page that breaks the rules of links is left out.
     */
    fun offerOf(release: Release, rules: FeedRules): Offer? {
        if (release.draft || !release.tag.startsWith("v")) return null
        val name = release.tag.substring(1)
        val rank = rank(name) ?: return null
        val channel = channelOf(name)
        if (release.prerelease != (channel == UpdateChannel.BETA)) return null
        fun usable(asset: Release.Asset) = (asset.state == null || asset.state == "uploaded") && usableUrl(asset.url, rules.allowHttp, rules.filesUnder)
        val apks = release.assets.filter(::usable).mapNotNull { asset ->
            when (channel) {
                UpdateChannel.STABLE -> rank.takeIf { asset.name == "${rules.apkPrefix}$name.apk" }
                UpdateChannel.BETA -> betaCode(asset.name, rules.apkPrefix, name)
            }?.let { asset to it }
        }
        val (apk, code) = apks.singleOrNull() ?: return null
        val checksum = release.assets.firstOrNull { it.name == "${apk.name}.sha256" && usable(it) } ?: return null
        if (apk.size !in 1..MAX_APK_BYTES) return null
        return Offer(
            name, code, apk.name, apk.url, apk.size, sha256Of(apk.digest), checksum.url,
            release.page?.takeIf { usableUrl(it, rules.allowHttp, rules.filesUnder) && it.endsWith("/releases/tag/${release.tag}") },
        )
    }

    /**
     * The page the chip of [offer] opens: its release's own tag page (`.../releases/tag/v<versionName>`);
     * null for any other address, also in offers stored by an earlier build.
     */
    fun releasePage(offer: Offer): String? = offer.page?.takeIf { it.endsWith("/releases/tag/v${offer.versionName}") }

    /** The versionCode in the [name] of a beta's APK, `<apkPrefix><version>-<code>.apk`; null for any other name. */
    fun betaCode(name: String, apkPrefix: String, version: String): Int? {
        val start = "$apkPrefix$version-"
        if (!name.startsWith(start) || !name.endsWith(".apk")) return null
        val code = name.substring(start.length, name.length - ".apk".length)
        return if (BETA_CODE.matches(code)) code.toIntOrNull() else null
    }

    /**
     * The newest build of each channel on a list of [releases], by its name ([rank]). A later fix of
     * an older line does not hide a newer version, and a release that [offerOf] does not take does
     * not hide the one below it.
     */
    fun offers(releases: List<Release>, rules: FeedRules): Offers {
        val all = releases.mapNotNull { offerOf(it, rules) }
        fun newest(channel: UpdateChannel) = all.filter { channelOf(it.versionName) == channel }.maxByOrNull { rank(it.versionName) ?: 0 }
        return Offers(newest(UpdateChannel.STABLE), newest(UpdateChannel.BETA))
    }

    /** The offer of [channel]; null: none out now. */
    fun offer(offers: Offers?, channel: UpdateChannel): Offer? = if (channel == UpdateChannel.STABLE) offers?.stable else offers?.beta

    /** The ETag a check sends: only with [offers] in hand, as a 304 says "the same as what you hold". */
    fun etagToSend(stored: String?, offers: Offers?): String? = stored.takeIf { offers != null }

    /**
     * Where a check ends: the [offers] after it, and on a failure its [problem] and the server's
     * wait ([retryAt]). [note]: what the update log says of a failure.
     */
    data class CheckEnd(val offers: Offers?, val problem: CheckProblem? = null, val retryAt: Long = 0, val note: String? = null)

    /**
     * What [answer] does with the [offers] in hand, under [rules]. A bad answer changes nothing but
     * the status. An empty list is one: this repository always has releases, so it says nothing
     * about them.
     */
    fun afterAnswer(answer: AppUpdateHttp.Answer, offers: Offers?, rules: FeedRules): CheckEnd = when (answer) {
        is AppUpdateHttp.Answer.Wait -> CheckEnd(offers, CheckProblem.BUSY, answer.until, "the server asks to wait until ${answer.until}")
        AppUpdateHttp.Answer.NotModified -> offers?.let { CheckEnd(it) } ?: CheckEnd(null, CheckProblem.FAILED, note = "check failed: unchanged, with no offers in hand")
        is AppUpdateHttp.Answer.Found -> if (answer.releases.isEmpty()) {
            CheckEnd(offers, CheckProblem.FAILED, note = "check failed: the list of releases is empty")
        } else {
            CheckEnd(offers(answer.releases, rules))
        }
    }

    /** The problem of a check that got no answer that reads. */
    fun checkProblem(error: Exception): CheckProblem =
        if (error is UnknownHostException || error is ConnectException || error is SocketTimeoutException) CheckProblem.OFFLINE else CheckProblem.FAILED

    /**
     * Whether the download in hand goes after the check that ended at [end]: its build is no longer
     * on the list (a newer build, a build made again, or one that was taken down). Never after a
     * failed check, and never while it runs ([runs]).
     */
    fun dropsDownload(end: CheckEnd, work: Work?): Boolean =
        end.problem == null && work != null && !runs(work) && offer(end.offers, channelOf(work.offer.versionName)) != work.offer

    /** The new version of [channel] for the build that runs, for the notice; null: none. */
    fun newOffer(offers: Offers, channel: UpdateChannel, installedName: String, installedCode: Int, work: Work?, refused: Set<String>): Offer? =
        rows(offers, channel, installedName, installedCode, work, null, true, refused).first { it.selected }.let { row -> row.offer.takeIf { row.fresh } }

    /**
     * What tells one upload of a release's APK from any other: its name, which holds the tag's
     * version, and GitHub's digest, or its size where GitHub gives none.
     */
    fun upload(offer: Offer): String = "${offer.apkName} ${offer.githubSha256 ?: offer.apkSize}"

    /**
     * Whether the app keeps [offer] from a second download after [refusal]: a beta whose whole file
     * passed its SHA-256 and is not the build that its name says. The same upload can only end the
     * same way, so a press would cost the whole APK again ([upload], [stillRefused]).
     */
    fun remembers(refusal: Refusal, offer: Offer): Boolean = refusal == Refusal.VERSION && channelOf(offer.versionName) == UpdateChannel.BETA

    /** The [refused] uploads that the list still offers: a new release or a new upload clears the others. */
    fun stillRefused(refused: Set<String>, offers: Offers): Set<String> {
        val offered = listOfNotNull(offers.stable, offers.beta).map(::upload)
        return refused.filterTo(LinkedHashSet()) { it in offered }
    }

    /**
     * Whether a download in hand stays at the app's start, for the build that runs ([installedName],
     * [installedCode]): not when it is that build, or one that Android would refuse for its lower
     * code. So a ready build outlives the process: Android's kill loses nothing.
     */
    fun keptAtStart(offer: Offer, installedName: String, installedCode: Int): Boolean =
        offer.versionName != installedName && offer.versionCode >= installedCode

    private val PATH_REST = Regex("[A-Za-z0-9._~/-]*")
    private val BETA_CODE = Regex("[1-9][0-9]{0,9}")

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
     * Whether an automatic check is due at [now] (wall-clock milliseconds): the server's wait has
     * passed ([notBefore], [waiting]), an hour has passed since the last check that worked
     * ([lastSuccess]) and 15 min since the last try ([lastAttempt]). A time after [now] (the clock
     * went back) holds nothing off.
     */
    fun checkDue(now: Long, lastSuccess: Long, lastAttempt: Long, notBefore: Long): Boolean {
        if (waiting(now, notBefore)) return false
        fun passed(at: Long, interval: Long) = at <= 0 || at > now || now - at >= interval
        return passed(lastSuccess, CHECK_INTERVAL_MS) && passed(lastAttempt, RETRY_INTERVAL_MS)
    }

    /** The server asked for no request before [notBefore]; a wait longer than a day is taken as a clock change. */
    fun waiting(now: Long, notBefore: Long): Boolean = notBefore > now && notBefore - now <= DAY_MS

    /**
     * When the server allows the next request after it answered [code] ([header] reads the answer's
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
        return minOf(at, now + DAY_MS)
    }

    private val CONTENT_RANGE = Regex("""bytes (\d+)-\d+/(\d+|\*)""")

    /** The start of a `Content-Range: bytes <start>-<end>/<total>`; null for any other value. */
    fun rangeStart(contentRange: String?): Long? =
        contentRange?.let { CONTENT_RANGE.matchEntire(it.trim()) }?.groupValues?.get(1)?.toLongOrNull()

    private val LOCAL_END = Regex("""\s*from /\S+ \(port \d+\)""")
    private val SPACES = Regex("""\s+""")
    /** The most the update log keeps of an error's message, which can hold the body of a bad answer. */
    private const val LOG_EXCERPT = 200

    /**
     * An error for the update log: its class and a short excerpt of its message, on one line,
     * without the phone's own address (Android's connection errors name it: " from /<address>
     * (port <n>)").
     */
    fun describe(error: Throwable): String {
        val message = error.message?.replace(LOCAL_END, "")?.trim()?.replace(SPACES, " ")
        val excerpt = message?.let { if (it.length > LOG_EXCERPT) it.take(LOG_EXCERPT) + "…" else it }
        return error.javaClass.simpleName + (excerpt?.let { ": $it" } ?: "")
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

    /**
     * Whether a press on the channel rows may act now: no wait that locks them ([lock]) holds in the session as it is at the
     * press, not as the rows were drawn (a press drawn one frame before the game starts).
     */
    fun pressAllowed(status: SessionStatus, copying: Boolean, now: Long): Boolean = lock(waitFor(status, copying, now)) == null

    /** What a downloaded file says about itself, as Android's package manager reads it. */
    data class Archive(val packageName: String, val versionName: String?, val versionCode: Long, val signers: List<String>)

    /** Why a download is refused: it is deleted and never handed to Android. */
    enum class Refusal { SIZE, CHECKSUM, NOT_AN_APP, OTHER_APP, VERSION, SIGNER }

    /**
     * Whether the download of [offer] is what it must be: [size] bytes with the SHA-256 [expected]
     * ([sha256] as read); an app ([archive]; null: Android cannot read it as one) of this app's
     * [packageName] with the offer's name and code (a beta shares its code, so only the name tells
     * it), not lower than [installed] (an equal code replaces the app: a beta and the Stable it was
     * made after); signed by exactly one certificate, whose SHA-256 is [signer]. Null when it is,
     * else the first reason it is not.
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
        archive.versionName != offer.versionName -> Refusal.VERSION
        archive.versionCode != offer.versionCode.toLong() || archive.versionCode < installed -> Refusal.VERSION
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

    /** Where the look at the list of releases stands. */
    sealed interface Check {
        /** Never checked. */
        data object Idle : Check

        /** A check that the player asked for runs. */
        data object Running : Check

        /** The last check worked; [checkedAt]: when its answer came (wall-clock milliseconds). */
        data class Done(val checkedAt: Long) : Check

        /** The last check failed, the answer was bad or missing; [retryAt]: the server's wait, for [CheckProblem.BUSY]. */
        data class Failed(val problem: CheckProblem, val retryAt: Long = 0) : Check
    }

    /**
     * The download of one build, from its start to Android's question. It stays on the row of its
     * own channel also when the player selects the other channel: only Stop ends it.
     */
    sealed interface Work {
        val offer: Offer

        /**
         * The download stopped: [downloaded] bytes are kept for Try again; [problem]: why it stopped,
         * with the space it [needed] and the [free] space for [DownloadProblem.NO_SPACE]; null: Stop.
         */
        data class Kept(
            override val offer: Offer,
            val downloaded: Long,
            val problem: DownloadProblem? = null,
            val needed: Long = 0,
            val free: Long = 0,
        ) : Work

        /** [done] bytes are in; Stop was pressed when [stopping]. */
        data class Downloading(override val offer: Offer, val done: Long, val stopping: Boolean = false) : Work

        /** The download's checks run. */
        data class Verifying(override val offer: Offer) : Work

        /** The checked download waits for Android's question; [problem]: why the last install did not happen. */
        data class Ready(override val offer: Offer, val problem: InstallProblem? = null) : Work

        /** Android has the update and asks the player. */
        data class Installing(override val offer: Offer) : Work
    }

    enum class CheckProblem { OFFLINE, BUSY, FAILED }

    /** [REFUSED]: the file failed its checks ([Refusal]). [UNCHECKED]: no usable SHA-256, so nothing was downloaded. */
    enum class DownloadProblem { NETWORK, NO_SPACE, REFUSED, UNCHECKED, FAILED }

    /** [GAME_STARTED]: the game or other work ([waitFor]) started while Android asked, so the update was taken back. */
    enum class InstallProblem { CANCELLED, GAME_STARTED, NO_SPACE, FAILED }

    /** The sign at a row's end: [DOWNLOAD] starts the download, [RETRY] starts it again, [INSTALL] opens Android's question, [INSTALLED] is the check mark. */
    enum class Sign { DOWNLOAD, RETRY, INSTALL, INSTALLED }

    /** What the player can do on a [WorkView]'s line. */
    enum class Action { STOP, ALLOW }

    /**
     * What a row shows of its [Work]: the state in words ([line], a warning or an error by [tone]),
     * a bar ([bar], [progress] 0..1, null without an amount, faint when [held]), a [note] under it
     * (a warning when [noteWarns]) and an [action] (disabled unless [enabled]).
     */
    data class WorkView(
        val line: Line,
        val tone: Tone,
        val bar: Boolean = false,
        val progress: Float? = null,
        val held: Boolean = false,
        val note: Line? = null,
        val noteWarns: Boolean = false,
        val action: Action? = null,
        val enabled: Boolean = true,
    )

    /**
     * One channel as Settings → App shows it. [offer]: the channel's newest build (null: none out
     * now), which the version chip names and links to. [installed]: it is the build that runs.
     * [canInstall]: the player can move to it now: a newer build of the channel the app runs, or the
     * other channel's build with a code that is not lower. [fresh]: the amber dot in the chip, a new
     * version of this channel, selected or not; a Stable with the code of the beta that runs is the
     * way back, not a new version. [sign]: the sign at the row's end. [info]: why the closed Beta row
     * is greyed while a beta is out that this build cannot take. [work]: the download of this channel's build.
     */
    data class ChannelRow(
        val channel: UpdateChannel,
        val selected: Boolean,
        val offer: Offer?,
        val installed: Boolean,
        val canInstall: Boolean,
        val fresh: Boolean,
        val sign: Sign?,
        val info: Line?,
        val work: WorkView?,
        /** The channel is closed: no beta is out that this build can take ([betaOpen]); greyed, not selectable, no chip, dot or sign. */
        val closed: Boolean = false,
        /**
         * A running download on this row while another is selected (the owner, 2026-11-04, "Small progress ring"): a small
         * teal ring in the sign's place, no text; null otherwise.
         */
        val ring: Ring? = null,
        /**
         * The game or the official PD2 launcher runs, or the game starts ([lock]): a press on the row, its chip or its sign
         * selects nothing, and no sign downloads or installs; the radio and the sign look disabled; a chip with a release page
         * still opens it, and a running download's Stop works.
         */
        val locked: Boolean = false,
    )

    /** The waits that lock the channel rows (the owner, 2026-11-04: no install and no channel switch while the game or the launcher runs). */
    private val LOCKS = setOf(Wait.GAME, Wait.OFFICIAL_LAUNCHER, Wait.STARTING)

    /** The one line above the channel rows while [wait] locks them ([ChannelRow.locked]): the wait's own words; null: no lock. */
    fun lock(wait: Wait?): Line? = wait?.takeIf { it in LOCKS }?.let(::waitLine)

    /** The progress ring: [fraction] done and its [percent]; both null while the download is checked (the ring spins). */
    data class Ring(val fraction: Float?, val percent: Int?)

    /** What TalkBack reads for [row]'s ring: "Downloading <version>, <percent> %", or "Checking the download…". */
    fun ringLine(row: ChannelRow): Line {
        val ring = row.ring
        val version = row.offer?.versionName.orEmpty()
        return if (ring?.percent != null) Line(R.string.app_update_ring, shown(version), ring.percent) else Line(R.string.app_update_verifying)
    }

    /** The ring of [work] on a row that is not selected; null where the work does not run. */
    private fun ringOf(work: Work): Ring? = when (work) {
        is Work.Downloading -> Ring(fraction(work.done, work.offer.apkSize), percent(work.done, work.offer.apkSize))
        is Work.Verifying -> Ring(null, null)
        else -> null
    }

    /**
     * The two channels' rows, for the build that runs ([installedName], [installedCode]) with
     * [selected] picked, from the [offers] and the [work] in hand. Android refuses only a lower
     * code, so a switch works both ways; inside a channel the name says which build is newer
     * ([rank]). [wait] and [allowed] say whether Android's question can open now ([waitFor]). A
     * beta in [refused] ([upload]), or one below [installedCode], closes the Beta row ([betaOpen]),
     * and the greyed row says that this beta cannot install over the build that runs ([ChannelRow.info]).
     */
    fun rows(
        offers: Offers?,
        selected: UpdateChannel,
        installedName: String,
        installedCode: Int,
        work: Work?,
        wait: Wait?,
        allowed: Boolean,
        refused: Set<String> = emptySet(),
        betaOpen: Boolean = betaOpen(offers, installedCode, refused, work),
    ): List<ChannelRow> = UpdateChannel.entries.map { channel ->
        // While Beta is closed the checks follow Stable ([channel]), so Stable is the selected row.
        val selected = if (betaOpen) selected else UpdateChannel.STABLE
        // A closed Beta (no beta out that this build can take): greyed, with nothing from a beta but the reason when one is out
        // ("Grey, with a reason", the owner, 2026-11-04).
        if (channel == UpdateChannel.BETA && !betaOpen) {
            val reason = Line(R.string.app_update_cannot_install, shown(installedName)).takeIf { offers?.beta != null }
            return@map ChannelRow(channel, false, null, false, false, false, null, reason, null, closed = true)
        }
        val job = work?.takeIf { channelOf(it.offer.versionName) == channel }
        val offer = offer(offers, channel) ?: job?.offer
        val on = channel == selected
        val installed = offer?.versionName == installedName
        val allowedCode = offer != null && offer.versionCode >= installedCode
        val otherChannel = channelOf(installedName) != channel
        val barred = offer != null && upload(offer) in refused
        val canInstall = offer != null && !installed && allowedCode && !barred && (otherChannel || newer(offer.versionName, installedName))
        ChannelRow(
            channel, on, offer, installed, canInstall,
            locked = wait in LOCKS,
            fresh = canInstall && !(channel == UpdateChannel.STABLE && otherChannel && offer?.versionCode == installedCode),
            sign = when {
                job != null -> jobSign(job, wait, allowed, on)
                !on -> null
                canInstall -> Sign.DOWNLOAD.takeUnless { runs(work) }
                installed -> Sign.INSTALLED
                else -> null
            },
            info = null,
            // Lines only on the selected row (the owner, 2026-11-04: "those text should only be shown when that channel is
            // selected"); a row that is not selected keeps its name, chip and sign.
            work = job?.takeIf { on }?.let { workView(it, wait, allowed) },
            ring = job?.takeIf { !on }?.let(::ringOf),
        )
    }

    private fun newer(name: String, than: String): Boolean = (rank(name) ?: 0) > (rank(than) ?: 0)

    /** What one press on a row does, in order ([presses]). */
    enum class Press {
        /** Select the row's channel, as a press on the row: the radio moves, the pick is stored. */
        SELECT,

        /** The sign's own action: download, try again, or install. */
        SIGN,

        /** The chip's release page ([releasePage]). */
        PAGE,
    }

    /**
     * What a press on [part] of [row] does (the owner, 2026-11-04: "if the user tabs elements of the stable channel of a
     * different channel, it should switch the channel first"): on a row that is not selected it first selects it, then the
     * control's own action in the same press; on the selected row only the action. The row itself, and the progress ring,
     * which has no press of its own, only select. A focus move selects nothing.
     */
    fun presses(row: ChannelRow, part: RowPart): List<Press> = buildList {
        // Locked: only a chip's release page opens, with no pick.
        if (row.locked) {
            if (part == RowPart.CHIP && row.offer?.let(::releasePage) != null) add(Press.PAGE)
            return@buildList
        }
        if (!row.selected) add(Press.SELECT)
        when (part) {
            RowPart.SIGN -> if (row.sign != null && row.sign != Sign.INSTALLED) add(Press.SIGN)
            RowPart.CHIP -> if (row.offer?.let(::releasePage) != null) add(Press.PAGE)
            RowPart.ROW, RowPart.ACTION -> Unit
        }
    }

    /** What the d-pad can stop on in [row] now: the check mark is no stop, as a press on it does nothing. */
    fun parts(row: ChannelRow): List<RowPart> = listOfNotNull(
        RowPart.ROW,
        // A chip with no page is no control on the selected row: a press would do nothing.
        RowPart.CHIP.takeIf { row.offer != null && ((!row.selected && !row.locked) || releasePage(row.offer) != null) },
        RowPart.ACTION.takeIf { row.work?.action != null },
        RowPart.SIGN.takeIf { row.sign != null && row.sign != Sign.INSTALLED && !row.locked },
    )

    /**
     * Where the focus goes when the chip, the sign or Stop goes from under it: Stop or Allow, else Try again,
     * else the row. It never lands by itself on a sign that starts a new download or opens Android's
     * question: at a download's end, Allow when Allow shows, else the row.
     */
    fun focusAfter(row: ChannelRow): RowPart = when {
        RowPart.ACTION in parts(row) -> RowPart.ACTION
        row.sign == Sign.RETRY -> RowPart.SIGN
        else -> RowPart.ROW
    }

    /** Whether the focus that [focusAfter] put on the row waits there for what the row's download does next. */
    fun focusWaits(row: ChannelRow): Boolean = row.work != null && focusAfter(row) == RowPart.ROW

    /** Whether [work] runs: one download at a time, and only its Stop ends it. */
    fun runs(work: Work?): Boolean = work is Work.Downloading || work is Work.Verifying || work is Work.Installing

    /**
     * The sign of [work] on its row: a ready download shows the install sign on a row that is not [selected] whatever [wait]
     * and [allowed] say (its press selects the row first, and the selected row then shows Allow or the wait); on the selected
     * row only while Android's question can open.
     */
    private fun jobSign(work: Work, wait: Wait?, allowed: Boolean, selected: Boolean): Sign? = when (work) {
        is Work.Kept -> if (work.problem == DownloadProblem.REFUSED || work.problem == DownloadProblem.UNCHECKED) Sign.DOWNLOAD else Sign.RETRY
        is Work.Ready -> Sign.INSTALL.takeIf { !selected || (wait == null && allowed) }
        is Work.Installing -> Sign.INSTALL
        is Work.Downloading, is Work.Verifying -> null
    }

    /**
     * Whether Play's Version shows the amber dot: only for the selected channel, while it has a new
     * version or the download of that version is in hand.
     */
    fun newVersionDot(rows: List<ChannelRow>): Boolean = rows.any { it.selected && (it.fresh || it.work != null) }

    /**
     * The App page's line in the list of pages: the download's state, wherever it belongs, in the
     * words of its row, with the version of a ready build, as it can be of the channel that is not
     * selected; else the selected channel with its new version or the version that runs. A new
     * version of the selected channel has the small amber dot beside its words ([PageLine.news]; the
     * owner, 2026-11-03: "reuse this yellow dot for the App update section as well for updates").
     */
    fun pageLine(rows: List<ChannelRow>, work: Work?, installedName: String): PageLine {
        val selected = rows.first { it.selected }
        return when {
            work is Work.Downloading -> PageLine(
                listOf(Line(R.string.settings_list_downloading), Line(R.string.settings_list_percent, percent(work.done, work.offer.apkSize))),
            )
            work is Work.Kept && work.problem == DownloadProblem.REFUSED -> PageLine(listOf(Line(R.string.settings_list_download_failed)), Tone.ERROR)
            work is Work.Kept && work.problem == DownloadProblem.UNCHECKED -> PageLine(listOf(Line(R.string.app_update_unchecked)), Tone.ERROR)
            work is Work.Kept -> PageLine(listOf(Line(R.string.settings_list_download_stopped)), Tone.WARNING)
            work is Work.Verifying -> PageLine(listOf(Line(R.string.app_update_verifying)))
            work != null -> PageLine(listOf(Line(R.string.app_update_sign_install, shown(work.offer.versionName))))
            selected.canInstall -> PageLine(
                listOf(channelName(selected.channel), Line(R.string.settings_list_version_out, shown(selected.offer!!.versionName))), news = selected.fresh,
            )
            else -> PageLine(listOf(channelName(selected.channel), Line(R.string.settings_list_version, shown(installedName))))
        }
    }

    private fun channelName(channel: UpdateChannel) = Line(
        when (channel) {
            UpdateChannel.STABLE -> R.string.app_update_channel_stable
            UpdateChannel.BETA -> R.string.app_update_channel_beta
        },
    )

    /** The line under "You have ...", and whether Check now works. */
    data class Status(val line: Line, val tone: Tone, val canCheck: Boolean = true)

    /**
     * The status block's line for [check], at wall-clock [now]: when the last check was, or why it
     * failed. During the server's wait Check now is off, as a check then does nothing; once the wait
     * is over, the line says so.
     */
    fun status(check: Check, now: Long): Status = when (check) {
        Check.Idle -> Status(Line(R.string.app_update_idle), Tone.PENDING)
        Check.Running -> Status(Line(R.string.app_update_checking), Tone.PENDING, canCheck = false)
        is Check.Done -> checked(check.checkedAt, now)
        is Check.Failed -> when (check.problem) {
            CheckProblem.OFFLINE -> Status(Line(R.string.app_update_check_offline), Tone.WARNING)
            CheckProblem.BUSY -> if (waiting(now, check.retryAt)) {
                Status(Line(R.string.app_update_check_busy, minutesUntil(check.retryAt, now)), Tone.WARNING, canCheck = false)
            } else {
                Status(Line(R.string.app_update_check_busy_over), Tone.PENDING)
            }
            CheckProblem.FAILED -> Status(Line(R.string.app_update_check_failed), Tone.WARNING)
        }
    }

    /**
     * The last check worked at [checkedAt]: when it looked ([checkedLine]), while that check is
     * recent ([RECENT_CHECK_MS]). After that, or for a time more than [AHEAD_MS] after [now] (the
     * clock went back), the line says that the app has not looked for a while, so an old answer does
     * not show as a new one.
     */
    private fun checked(checkedAt: Long, now: Long): Status {
        val age = now - checkedAt
        if (age > RECENT_CHECK_MS || age < -AHEAD_MS) return Status(Line(R.string.app_update_not_checked_lately), Tone.PENDING)
        return Status(checkedLine(age), Tone.OK)
    }

    /** When the last check was, [age] milliseconds ago: just now (a little ahead too), minutes, hours, then days. */
    private fun checkedLine(age: Long): Line = when {
        age < MINUTE_MS -> Line(R.string.app_update_checked_now)
        age < HOUR_MS -> Line(R.string.app_update_checked_minutes, age / MINUTE_MS)
        age < 2 * HOUR_MS -> Line(R.string.app_update_checked_one_hour)
        age < 2 * DAY_MS -> Line(R.string.app_update_checked_hours, age / HOUR_MS)
        else -> Line(R.string.app_update_checked_days, age / DAY_MS)
    }

    /**
     * How long until the status line for [check] can change with the clock alone, at wall-clock
     * [now]: the server's wait counts down by whole minutes and then ends, and "Checked … ago" moves
     * on by the minute. At most a minute; the page reads the clock again then.
     */
    fun refreshIn(check: Check, now: Long): Long {
        val step = when (check) {
            is Check.Failed -> (check.retryAt - now).takeIf { it > 0 }?.let { Math.floorMod(it, MINUTE_MS) }
            is Check.Done -> MINUTE_MS - Math.floorMod(now - check.checkedAt, MINUTE_MS)
            else -> null
        }
        return step?.takeIf { it > 0 } ?: MINUTE_MS
    }

    private const val MINUTE_MS = 60_000L
    private const val HOUR_MS = 60 * MINUTE_MS
    /** The most a last check may be ahead of the clock and still count as "just now" ([checked]). */
    private const val AHEAD_MS = 5 * MINUTE_MS

    /** What a row shows of [work]: [wait] says what the install waits for, [allowed] whether Android lets this app install updates. */
    fun workView(work: Work, wait: Wait?, allowed: Boolean): WorkView = when (work) {
        is Work.Downloading -> WorkView(
            Line(R.string.app_update_downloading, megabytes(work.done), megabytes(work.offer.apkSize, up = true)), Tone.WORKING,
            bar = true, progress = fraction(work.done, work.offer.apkSize),
            note = Line(R.string.app_update_then_android, shown(work.offer.versionName)), action = Action.STOP, enabled = !work.stopping,
        )
        is Work.Kept -> kept(work)
        is Work.Verifying -> WorkView(Line(R.string.app_update_verifying), Tone.WORKING, bar = true)
        is Work.Ready -> ready(work, wait, allowed)
        is Work.Installing -> WorkView(
            Line(R.string.app_update_android_asks, shown(work.offer.versionName)), Tone.WORKING, note = Line(R.string.app_update_app_closes),
        )
    }

    private fun kept(work: Work.Kept): WorkView {
        val offer = work.offer
        val size = megabytes(offer.apkSize, up = true)
        val bar = work.downloaded > 0
        val progress = fraction(work.downloaded, offer.apkSize)
        val note = Line(R.string.app_update_kept, megabytes(work.downloaded)).takeIf { bar }
        return when (work.problem) {
            null -> WorkView(Line(R.string.app_update_paused, shown(offer.versionName), megabytes(work.downloaded), size), Tone.PENDING, bar, progress, held = true, note = note)
            DownloadProblem.NETWORK -> WorkView(Line(R.string.app_update_download_network), Tone.WARNING, bar, progress, held = true, note = note)
            DownloadProblem.NO_SPACE -> WorkView(Line(R.string.app_update_no_space, megabytes(work.needed, up = true), megabytes(work.free)), Tone.WARNING, bar, progress, held = true)
            DownloadProblem.REFUSED -> WorkView(Line(R.string.app_update_refused), Tone.ERROR)
            DownloadProblem.UNCHECKED -> WorkView(Line(R.string.app_update_unchecked), Tone.ERROR)
            DownloadProblem.FAILED -> WorkView(Line(R.string.app_update_download_failed), Tone.WARNING, bar, progress, held = true, note = note)
        }
    }

    private fun ready(work: Work.Ready, wait: Wait?, allowed: Boolean): WorkView {
        val line = when (work.problem) {
            null -> Line(R.string.app_update_ready, shown(work.offer.versionName))
            InstallProblem.CANCELLED -> Line(R.string.app_update_cancelled)
            InstallProblem.GAME_STARTED -> Line(R.string.app_update_put_off)
            InstallProblem.NO_SPACE -> Line(R.string.app_update_install_no_space)
            InstallProblem.FAILED -> Line(R.string.app_update_install_failed)
        }
        val tone = if (work.problem == InstallProblem.NO_SPACE || work.problem == InstallProblem.FAILED) Tone.WARNING else Tone.PENDING
        return when {
            // A wait that locks the rows has its one line above them ([lock]), not on the row.
            wait in LOCKS -> WorkView(line, tone)
            wait != null -> WorkView(line, tone, note = waitLine(wait), noteWarns = true)
            !allowed -> WorkView(line, tone, note = Line(R.string.app_update_allow_note), action = Action.ALLOW)
            else -> WorkView(line, tone, note = Line(R.string.app_update_app_closes))
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

    private fun percent(done: Long, total: Long): Int = (fraction(done, total) * 100).toInt()

    /** Whole minutes from [now] to [at], at least one. */
    private fun minutesUntil(at: Long, now: Long): Long = ((at - now + 59_999) / 60_000).coerceAtLeast(1)
}
