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
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.lifecycleScope
import androidx.lifecycle.repeatOnLifecycle
import com.pd2launcher.app.AppUpdate.offer
import com.pd2launcher.app.ui.Play
import com.pd2launcher.app.ui.UpdateUi
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.filterNotNull
import kotlinx.coroutines.launch

/**
 * The activity's part of app updates ([AppUpdater]): it starts the updater, opens Android's
 * confirmation window only while the activity is in front, reads on each return what About needs,
 * and turns About's actions into the updater's. [MainActivity] calls it from its lifecycle.
 */
class AppUpdateHost(private val activity: ComponentActivity) {
    /** Android lets the app install its updates ("Install unknown apps"); read on each return. */
    private val allowed = mutableStateOf(true)
    /** The network counts data (mobile data): About says so before the download. */
    private val metered = mutableStateOf(false)

    /** Whether this build gets app updates (About's group, Settings → App's section). */
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
    }

    /** From onNewIntent. */
    fun newIntent(intent: Intent) = testFeed(intent)

    /** From onResume. */
    fun resumed() {
        allowed.value = activity.packageManager.canRequestPackageInstalls()
        metered.value = AppUpdater.metered(activity)
        AppUpdater.resumed(activity)
    }

    /** About's app update while [status] (and a folder [copying]) holds; null where this build gets none. */
    @Composable
    fun about(status: SessionStatus, copying: Boolean): UpdateUi? {
        val state by AppUpdater.state.collectAsState()
        // Install waits for a Play press until Play.LAUNCH_PENDING_MS has passed; this clock moves
        // then, so About shows the wait's end when nothing else changes.
        var clock by remember { mutableLongStateOf(SystemClock.elapsedRealtime()) }
        LaunchedEffect(status.launchRequestedAt) {
            val at = status.launchRequestedAt ?: return@LaunchedEffect
            delay(at + Play.LAUNCH_PENDING_MS - SystemClock.elapsedRealtime())
            clock = SystemClock.elapsedRealtime()
        }
        if (!available) return null
        val view = AppUpdate.view(state, AppUpdate.waitFor(status, copying, clock), metered.value, allowed.value, System.currentTimeMillis())
        return UpdateUi(view, ::act, ::openPage)
    }

    /** The newer version that Settings → App names; null: none. */
    @Composable
    fun newVersion(): String? = AppUpdater.state.collectAsState().value.offer?.versionName

    private fun act(action: AppUpdate.Action) {
        when (action) {
            AppUpdate.Action.CHECK -> AppUpdater.checkNow(activity)
            AppUpdate.Action.DOWNLOAD, AppUpdate.Action.RESUME -> AppUpdater.download(activity)
            AppUpdate.Action.STOP -> AppUpdater.stop()
            // Android's "Install unknown apps" page for this app; About reads it again on return.
            AppUpdate.Action.ALLOW -> start(Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:${activity.packageName}")))
            AppUpdate.Action.INSTALL -> AppUpdater.install(activity)
        }
    }

    /** What's new: the release's page in the browser. */
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
