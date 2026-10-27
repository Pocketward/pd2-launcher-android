package com.pd2launcher.app.input

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.util.Log

/**
 * Debug builds only: the debug pad injector, for device tests that play the game with the
 * controller and no touch:
 *
 *     am broadcast -f 0x10000000 -a com.pd2launcher.app.DEBUG_PAD \
 *         -n com.pd2launcher.app.dev/com.pd2launcher.app.input.DebugPadReceiver --es cmd '<command>'
 *
 * The command ([DebugPadCommand]) drives a scripted controller in the Game view
 * ([PadInput.onScripted]): a second real controller, so in mouse mode its left stick moves the
 * pointer and its buttons click and type, and it does not fight the on-screen controller. The
 * debug manifest asks the sender for android.permission.DUMP, which the shell has and apps do not.
 * The answer is the broadcast's result data: "ok <command>", "refused: <why>" (the debug pad is then
 * neutral) or "error: <what is wrong>".
 */
class DebugPadReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val answer = DebugPad.run(intent.getStringExtra(EXTRA_COMMAND).orEmpty())
        if (isOrderedBroadcast) setResult(if (answer.startsWith("ok")) 1 else 0, answer, null)
    }

    companion object {
        const val EXTRA_COMMAND = "cmd"
    }
}

/** Debug builds only: runs the debug pad's commands and lets go of each part at its time. Main thread. */
internal object DebugPad {
    const val TAG = "pd2-debug-pad"

    private val handler = Handler(Looper.getMainLooper())
    private val state = DebugPadState()
    private val due = Runnable {
        val (buttons, axes) = state.at(SystemClock.uptimeMillis())
        Log.d(TAG, "due: now ${held(buttons, axes)}${push()?.let { ", refused: $it" } ?: ""}")
    }

    init {
        // The Game view let go of the scripted controller: nothing is pressed again at a later time.
        ScriptedPad.onReleased = { why ->
            if (state.next() != null) Log.d(TAG, "released by the app: $why")
            handler.removeCallbacks(due)
            state.clear()
        }
    }

    /** Runs [text] ([DebugPadCommand]); the answer for the sender. */
    fun run(text: String): String {
        val answer = try {
            answer(DebugPadCommand.parse(text))
        } catch (e: IllegalArgumentException) {
            "error: ${e.message}"
        }
        Log.d(TAG, "'$text': $answer")
        return answer
    }

    private fun answer(command: DebugPadCommand): String = when (command) {
        DebugPadCommand.Neutral -> {
            state.clear()
            push()
            "ok neutral"
        }
        is DebugPadCommand.MouseMode -> {
            val target = ScriptedPad.target
            val refused = if (target == null) "no game shows" else target.scriptedMouseMode(command.on)
            if (refused != null) {
                handler.removeCallbacks(due)
                state.clear()
                "refused: $refused"
            } else {
                "ok ${command.describe()}"
            }
        }
        is DebugPadCommand.Hold -> {
            state.hold(command, SystemClock.uptimeMillis())
            push()?.let { "refused: $it" } ?: "ok ${command.describe()}"
        }
    }

    /**
     * Sends what is held now to the Game view and sets the time of the next release; null when
     * taken, else why not (then nothing stays held).
     */
    private fun push(): String? {
        handler.removeCallbacks(due)
        val (buttons, axes) = state.at(SystemClock.uptimeMillis())
        val target = ScriptedPad.target
        val refused = if (target == null) "no game shows" else target.scripted(buttons, axes)
        if (refused != null) {
            state.clear()
            return refused
        }
        state.next()?.let { handler.postAtTime(due, it) }
        return null
    }

    private fun held(buttons: Set<PadButton>, axes: PadAxes): String =
        if (buttons.isEmpty() && axes == PadAxes()) "neutral" else "buttons=${buttons.map { it.name }.sorted()} axes=$axes"
}
