package com.pd2launcher.app

import android.content.Context
import android.net.wifi.WifiManager
import android.os.PowerManager
import android.os.SystemClock
import com.pd2launcher.app.control.Job
import com.pd2launcher.app.control.Snapshot

/**
 * Keeps the device working while the engine runs a job that downloads or installs: a partial wake
 * lock, so the processor does not sleep with the screen off, and a Wi-Fi lock, so Wi-Fi stays fast
 * (Android 13; from Android 14 Wi-Fi keeps its connection by itself and the lock does nothing).
 * Without them a long download (Blizzard's takes about 18 minutes) pauses when the screen turns
 * off, though Install says the player can leave the app. Held only while such a job runs
 * ([wanted]), released when it ends, when the engine goes and when the service stops; each hold
 * also ends by itself after [HOLD_MS], should a release never come, and a job that runs longer
 * renews it ([RENEW_MS]).
 */
class WorkLocks(context: Context) {
    private val wake = context.getSystemService(PowerManager::class.java)
        .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "pd2:work")
        .apply { setReferenceCounted(false) }

    @Suppress("DEPRECATION")
    private val wifi = context.applicationContext.getSystemService(WifiManager::class.java)
        ?.createWifiLock(WifiManager.WIFI_MODE_FULL_HIGH_PERF, "pd2:work")
        ?.apply { setReferenceCounted(false) }

    /** Whether the locks are held now; read on other threads too. */
    @Volatile
    var held = false
        private set

    private var acquiredAt = 0L

    /**
     * Holds the locks while [wanted] (each call while held renews them once [RENEW_MS] passed, so a
     * job longer than [HOLD_MS] keeps them), releases them otherwise.
     */
    fun hold(wanted: Boolean, now: Long = SystemClock.elapsedRealtime()) {
        if (wanted) {
            if (held && now - acquiredAt < RENEW_MS) return
            wake.acquire(HOLD_MS)
            wifi?.acquire()
            acquiredAt = now
            held = true
        } else if (held) {
            if (wake.isHeld) wake.release()
            if (wifi?.isHeld == true) wifi.release()
            held = false
        }
    }

    companion object {
        /** The longest one hold lasts: longer than a whole install (about 40 minutes on a slow phone). */
        const val HOLD_MS = 3 * 60 * 60 * 1000L

        /** How often a hold is renewed while its job runs, well within [HOLD_MS]. */
        const val RENEW_MS = 60 * 60 * 1000L

        /** The jobs that download or install, and not the official PD2 launcher's session. */
        private val WORK = setOf(
            Job.DOWNLOAD, Job.INSTALL_BASE_GAME, Job.INSTALL_PROJECT_DIABLO, Job.UPDATE, Job.REPAIR, Job.RELOCATION,
        )

        /** Whether [snapshot]'s running job should keep the device working. */
        fun wanted(snapshot: Snapshot?): Boolean = snapshot?.activeJob in WORK
    }
}
