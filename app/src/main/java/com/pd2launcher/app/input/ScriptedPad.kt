package com.pd2launcher.app.input

/**
 * Debug builds only: where the debug pad injector (`src/debug`, `DebugPadReceiver`) sends the
 * scripted controller's states. The Game view sets
 * [target] while it shows a game, in a debug build only; a release build has no injector and never
 * sets it. Main thread only.
 */
object ScriptedPad {
    /** The Game view's controller as the injector drives it. */
    interface Target {
        /** The scripted controller holds [buttons] and [axes] ([PadInput.onScripted]); null when done, else why not. */
        fun scripted(buttons: Set<PadButton>, axes: PadAxes): String?

        /** Mouse mode on ([on]) or off ([PadInput.useMouseMode]); null when done, else why not. */
        fun scriptedMouseMode(on: Boolean): String?
    }

    var target: Target? = null

    /**
     * The Game view let go of the scripted controller (an overlay opened, the window lost the
     * focus, the game left the screen, the view closed) for the reason given: the injector
     * forgets what it holds, so that no later release time presses the rest again.
     */
    var onReleased: ((why: String) -> Unit)? = null
}
