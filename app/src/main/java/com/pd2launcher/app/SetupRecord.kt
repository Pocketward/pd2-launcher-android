package com.pd2launcher.app

import android.content.Context
import com.pd2launcher.app.control.Installation
import com.pd2launcher.app.control.Job
import com.pd2launcher.app.control.OperationStatus
import com.pd2launcher.app.control.Phase
import com.pd2launcher.app.control.Snapshot
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * How far the installation got, by the app's own record, so a start lands on the right page
 * before the engine's first audit: a first run on Check, an install started and not finished on
 * Install, an installed game on Play.
 */
enum class SetupRecord {
    /** Nothing was set up, or Repair's Start over reset it: the first run, from Check. */
    NONE,

    /** An install job ran and the game is not Ready yet (paused, failed or interrupted): Install. */
    STARTED,

    /** The engine's audit said Ready: Play, from then until Start over. */
    INSTALLED;

    /**
     * The record after [snapshot]: Start over's reset succeeded, none; the audit says Ready,
     * installed; an install job runs, at least started. Nothing else changes it, so an
     * installation the audit later finds broken stays installed (Repair, not the install flow).
     */
    fun after(snapshot: Snapshot): SetupRecord = when {
        snapshot.operation.let { it?.phase == Phase.RESET && it.status == OperationStatus.SUCCEEDED } -> NONE
        snapshot.installation == Installation.READY && Onboarding.audited(snapshot) -> INSTALLED
        this == NONE && (snapshot.activeJob == Job.INSTALL_BASE_GAME || snapshot.activeJob == Job.INSTALL_PROJECT_DIABLO) -> STARTED
        else -> this
    }
}

/**
 * [SetupRecord] in the app's `setup` preferences, and as a flow the screens follow. Android's
 * Clear storage and an uninstall remove it with the rest (`allowBackup` is off, so no restore
 * brings it back without the installation).
 */
object SetupRecordStore {
    private const val PREFERENCES = "setup"
    private const val KEY = "record"
    private val _current = MutableStateFlow(SetupRecord.NONE)

    /** The record [load] read or [follow] wrote last; none before. */
    val current: StateFlow<SetupRecord> = _current.asStateFlow()

    /** Synchronized with [follow], so a load racing a change never puts the older record back. */
    @Synchronized
    fun load(context: Context): SetupRecord {
        val name = context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE).getString(KEY, null)
        return (SetupRecord.entries.firstOrNull { it.name == name } ?: SetupRecord.NONE).also { _current.value = it }
    }

    /**
     * After an update in place that failed ([UpgradeImport]): an install started and not finished,
     * so the app opens on Install, where one press imports the older game folder.
     */
    @Synchronized
    fun setUpAgain(context: Context) {
        context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE).edit().putString(KEY, SetupRecord.STARTED.name).commit()
        _current.value = SetupRecord.STARTED
    }

    /**
     * The record after the engine's [snapshot] ([SetupRecord.after]); a change is written at once
     * (commit, not apply: the next start lands by it, even right after a force stop). Returns the
     * new record when it changed, else null.
     */
    @Synchronized
    fun follow(context: Context, snapshot: Snapshot): SetupRecord? {
        val next = _current.value.after(snapshot)
        if (next == _current.value) return null
        context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE).edit().putString(KEY, next.name).commit()
        _current.value = next
        return next
    }
}
