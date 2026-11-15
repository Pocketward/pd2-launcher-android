package com.pd2launcher.app

import android.view.Window

/**
 * The window and the layers both ask, because phones differ in which one they follow: on
 * OxygenOS 16 a layer's vote alone stayed at 60 Hz and a window's request got the rate that
 * OxygenOS has for the app; on stock Android 13 each of the two got the mode.
 *
 * It asks when the Game view's surface comes and when the player confirms a new Frame rate, and
 * never because of a reading of the screen's rate ([ScreenRate]). The window keeps its request
 * while the app's other screens show: a request taken back there and made again for the game
 * would change the screen's mode at each visit.
 */
class ScreenRequester(private val window: Window?) {
    fun ask(request: ScreenRequest, copy: FrameCopy?) {
        window?.let {
            if (it.attributes.preferredDisplayModeId != request.modeId) {
                it.attributes = it.attributes.also { attributes -> attributes.preferredDisplayModeId = request.modeId }
            }
        }
        copy?.setFrameRate(request.rate)
        PresentServer.current.value?.setFrameRate(request)
    }
}
