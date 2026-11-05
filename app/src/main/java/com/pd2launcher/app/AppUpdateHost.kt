package com.pd2launcher.app

import android.content.ActivityNotFoundException
import android.content.Intent
import android.net.Uri
import android.os.SystemClock
import android.provider.Settings
import android.util.Log
import androidx.activity.ComponentActivity
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.lifecycleScope
import androidx.lifecycle.repeatOnLifecycle
import com.pd2launcher.app.ui.AppPageUi
import com.pd2launcher.app.ui.Play
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.filterNotNull
import kotlinx.coroutines.launch

/**
 * The activity's part of app updates ([AppUpdater]): it starts the updater, opens Android's
 * confirmation window only while the activity is in front, asks for a check when one is due on
 * each return and on a timer while it stays in front, reads on each return what Settings → App
 * needs, and turns that page's presses into the updater's. [MainActivity] calls it from its
 * lifecycle.
 */
class AppUpdateHost(private val activity: ComponentActivity) {
    /** Android lets the app install its updates ("Install unknown apps"); read on each return. */
    private val allowed = mutableStateOf(true)
    /** Counts the returns to the front: the page reads the wall clock again then, as its timer does not run while the device sleeps. */
    private val returns = mutableIntStateOf(0)

    /** Whether this build gets app updates (Settings → App). */
    val available: Boolean get() = AppUpdater.available(activity)

    /** From onCreate. [fresh]: a new start, not Android making the activity anew from the same intent. */
    fun create(intent: Intent, fresh: Boolean) {
        if (fresh) testFeed(intent)
        AppUpdater.start(activity)
        activity.lifecycleScope.launch {
            activity.repeatOnLifecycle(Lifecycle.State.RESUMED) {
                AppUpdater.confirmation.filterNotNull().collect { confirm ->
                    AppUpdater.confirmationShown()
                    start(confirm)
                }
            }
        }
        // While the app stays in front (the game too), a check when one is due: the timer only
        // hands the question to the updater's thread. It stops when the activity pauses; onResume asks at once.
        activity.lifecycleScope.launch {
            activity.repeatOnLifecycle(Lifecycle.State.RESUMED) {
                while (true) {
                    delay(AppUpdate.TIMER_MS)
                    AppUpdater.inFront(activity)
                }
            }
        }
    }

    /** From onNewIntent. */
    fun newIntent(intent: Intent) = testFeed(intent)

    /** From onResume. */
    fun resumed() {
        allowed.value = activity.packageManager.canRequestPackageInstalls()
        returns.intValue++
        AppUpdater.inFront(activity)
    }

    /** Settings → App while [status] (and a folder [copying]) holds; null where this build gets none. */
    @Composable
    fun page(status: SessionStatus, copying: Boolean): AppPageUi? {
        val check by AppUpdater.check.collectAsState()
        val work by AppUpdater.work.collectAsState()
        // Android's question waits for a Play press until Play.LAUNCH_PENDING_MS has passed; this
        // clock moves then, so the row shows the wait's end when nothing else changes.
        var clock by remember { mutableLongStateOf(SystemClock.elapsedRealtime()) }
        LaunchedEffect(status.launchRequestedAt) {
            val at = status.launchRequestedAt ?: return@LaunchedEffect
            delay(at + Play.LAUNCH_PENDING_MS - SystemClock.elapsedRealtime())
            clock = SystemClock.elapsedRealtime()
        }
        // The wall clock for the server's wait and "Checked … ago": read again as the check changes,
        // at each return to the front, and while the page stays open whenever the words can change
        // with the time alone.
        var wallClock by remember { mutableLongStateOf(System.currentTimeMillis()) }
        LaunchedEffect(check, returns.intValue) {
            while (true) {
                wallClock = System.currentTimeMillis()
                delay(AppUpdate.refreshIn(check, wallClock))
            }
        }
        val wait = AppUpdate.waitFor(status, copying, clock)
        val rows = rows(wait)
        if (!available) return null
        return AppPageUi(
            AppUpdate.status(check, wallClock), rows, AppUpdate.pageLine(rows, work, BuildConfig.VERSION_NAME), AppUpdate.lock(wait),
            onCheck = { AppUpdater.checkNow(activity) }, onPick = ::pickChannel, onSign = ::sign, onAction = ::act, onPage = ::openPage,
        )
    }

    /** Whether Play's Version shows the new-version dot ([AppUpdate.newVersionDot]); never where this build gets no updates. */
    @Composable
    fun newVersionDot(): Boolean {
        // The state is read first, at each composition: a read that `available` skipped would
        // leave Play deaf to its changes (a debug build's test feed set while Play shows).
        val rows = rows(null)
        return available && AppUpdate.newVersionDot(rows)
    }

    /** The two channels' rows, for the channel the checks follow: the player's pick, else this build's own ([AppUpdate.channel]). */
    @Composable
    private fun rows(wait: AppUpdate.Wait?): List<AppUpdate.ChannelRow> {
        val offers by AppUpdater.offers.collectAsState()
        val work by AppUpdater.work.collectAsState()
        val refused by AppUpdater.refused.collectAsState()
        val picked = AppSettingsStore.current.collectAsState().value.updateChannel
        val open = AppUpdate.betaOpen(offers, BuildConfig.VERSION_CODE, refused, work)
        return AppUpdate.rows(
            offers, AppUpdate.channel(picked, BuildConfig.VERSION_NAME, open), BuildConfig.VERSION_NAME, BuildConfig.VERSION_CODE,
            work, wait, allowed.value, refused, betaOpen = open,
        )
    }

    /**
     * The player pressed [channel]'s row in Settings → App: the pick is stored only where
     * [AppUpdate.storedPick] says (never for a closed Beta; nothing for the channel already in use,
     * so a press on the Stable row that a closed Beta shows keeps a stored Beta pick; before any pick,
     * the player's own, which wins over the build's), and the updater hears of a change
     * ([AppUpdater.channelChanged]).
     */
    private fun pickChannel(channel: UpdateChannel) {
        // The lock holds at the press too: the rows can be one frame older than the game's start.
        if (!AppUpdater.pressAllowed()) return
        val open = AppUpdate.betaOpen(AppUpdater.offers.value, BuildConfig.VERSION_CODE, AppUpdater.refused.value, AppUpdater.work.value)
        val stored = AppSettingsStore.current.value.updateChannel
        val before = AppUpdate.channel(stored, BuildConfig.VERSION_NAME, open)
        val pick = AppUpdate.storedPick(stored, channel, BuildConfig.VERSION_NAME, open) ?: return
        AppSettingsStore.update(activity) { it.copy(updateChannel = pick) }
        if (pick != before) AppUpdater.channelChanged(activity)
    }

    /** The sign at the end of [row]. */
    private fun sign(row: AppUpdate.ChannelRow) {
        if (!AppUpdater.pressAllowed()) return
        when (row.sign) {
            AppUpdate.Sign.DOWNLOAD, AppUpdate.Sign.RETRY -> row.offer?.let { AppUpdater.download(activity, it) }
            AppUpdate.Sign.INSTALL -> AppUpdater.install(activity)
            AppUpdate.Sign.INSTALLED, null -> Unit
        }
    }

    private fun act(action: AppUpdate.Action) {
        when (action) {
            AppUpdate.Action.STOP -> AppUpdater.stop()
            // Android's "Install unknown apps" page for this app; the page reads it again on return.
            AppUpdate.Action.ALLOW -> start(Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:${activity.packageName}")))
        }
    }

    /** The version chip: the release's page in the browser. */
    private fun openPage(page: String) = start(Intent(Intent.ACTION_VIEW, Uri.parse(page)))

    /** A screen of Android or another app; a device that has none changes nothing. */
    private fun start(intent: Intent) {
        try {
            activity.startActivity(intent)
        } catch (error: ActivityNotFoundException) {
            Log.w(TAG, "no screen for ${intent.action}: $error")
        }
    }

    /**
     * Debug builds only: `--es update_feed <url>`, app updates from a test feed instead of the
     * releases; `none` (or an empty value, which `adb shell` cannot pass) ends the test feed.
     */
    private fun testFeed(intent: Intent) {
        if (!BuildConfig.DEBUG) return
        val url = intent.getStringExtra("update_feed") ?: return
        AppUpdater.setTestFeed(activity, url.takeUnless { it == "none" }.orEmpty())
    }

    private companion object {
        const val TAG = "pd2-update"
    }
}
