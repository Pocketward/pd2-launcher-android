package com.pd2launcher.app

import com.pd2launcher.app.control.Command
import com.pd2launcher.app.control.Installation
import com.pd2launcher.app.control.Job
import com.pd2launcher.app.control.OperationStatus
import com.pd2launcher.app.control.Snapshot
import com.pd2launcher.app.control.isProductKey

/**
 * The CD keys the player entered for Install Diablo II, in this process's memory only: never on
 * disk, in an Intent or in a log (keys are never stored). Stop pauses the install and
 * the engine forgets the keys when its job ends, so Resume sends these again without asking.
 * They are overwritten once Diablo II and Lord of Destruction are in or their install failed
 * ([spent]), on Start over and on an import ([forgets]), and gone with the process; after that
 * the key page asks again.
 */
class HeldKeys {
    private var d2: CharArray? = null
    private var lod: CharArray? = null

    /**
     * Holds copies of [d2Key] and [lodKey] in place of any held; the caller still clears its own.
     * Keys the Launcher would not take ([isProductKey]) are ignored: a second submit of the key
     * page sends its emptied fields, which must not replace the keys held.
     */
    @Synchronized
    fun hold(d2Key: CharArray, lodKey: CharArray) {
        if (!isProductKey(d2Key) || !isProductKey(lodKey)) return
        clear()
        d2 = d2Key.copyOf()
        lod = lodKey.copyOf()
    }

    /** Keys are held. */
    val held: Boolean
        @Synchronized get() = d2 != null

    /** Copies of the held keys for one send, which the receiver clears; null when none are held. */
    @Synchronized
    fun copies(): Pair<CharArray, CharArray>? {
        val d2Key = d2 ?: return null
        val lodKey = lod ?: return null
        return d2Key.copyOf() to lodKey.copyOf()
    }

    /** Overwrites the held keys and drops them. */
    @Synchronized
    fun clear() {
        d2?.fill('\u0000')
        lod?.fill('\u0000')
        d2 = null
        lod = null
    }

    companion object {
        /**
         * The held keys are done with after [next] (the snapshot before it: [previous]): Diablo II
         * and Lord of Destruction are in, so Blizzard's installers need no key any more; or Install
         * Diablo II has just failed, when the keys may have been wrong and a retry asks again.
         */
        fun spent(previous: Snapshot?, next: Snapshot): Boolean =
            next.installation in INSTALLED || (installFailed(next) && !installFailed(previous))

        /** After Start over or an import no paused Install Diablo II goes on: the held keys go. */
        fun forgets(command: Command): Boolean = command is Command.StartOver || command is Command.ImportExistingInstall

        private val INSTALLED = setOf(Installation.NEEDS_CLIENT, Installation.NEEDS_RUNTIME, Installation.READY)

        private fun installFailed(snapshot: Snapshot?): Boolean =
            snapshot?.operation?.let { it.job == Job.INSTALL_BASE_GAME && it.status == OperationStatus.FAILED } == true
    }
}
