package com.pd2launcher.app.input

import com.pd2launcher.app.input.DebugPadCommand.Hold
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

class DebugPadTest {
    private fun parse(text: String) = DebugPadCommand.parse(text)

    private fun hold(text: String) = parse(text) as Hold

    /** Why [text] is refused. */
    private fun refused(text: String): String {
        try {
            parse(text)
        } catch (e: IllegalArgumentException) {
            return e.message.orEmpty()
        }
        fail("'$text' was taken")
        return ""
    }

    @Test
    fun pressAndDpadHoldButtonsForAShortTime() {
        assertEquals(Hold(setOf(PadButton.A), ms = 150), parse("press A"))
        assertEquals(Hold(setOf(PadButton.A), ms = 400), parse("press a 400"))
        assertEquals(Hold(setOf(PadButton.L2, PadButton.START), ms = 150), parse("  PRESS  l2 start "))
        assertEquals(Hold(setOf(PadButton.UP), ms = 150), parse("dpad UP"))
        assertEquals(Hold(setOf(PadButton.LEFT), ms = 90), parse("dpad left 90"))
    }

    @Test
    fun holdTakesButtonsSticksAndTriggers() {
        assertEquals(Hold(setOf(PadButton.X), ms = 500), parse("hold X"))
        assertEquals(Hold(left = DebugStick(0f, -1f), ms = 800), parse("hold LSTICK 0.0 -1.0 800"))
        assertEquals(Hold(right = DebugStick(0.5f, 0.25f), ms = 500), parse("hold rstick 0.5 0.25"))
        assertEquals(Hold(leftTrigger = 1f, ms = 300), parse("hold LT 1 300"))
        assertEquals(Hold(rightTrigger = 0.5f, ms = 500), parse("hold RT 0.5"))
        assertEquals(Hold(left = DebugStick(-1f, 0f), ms = 1200), parse("lstick -1 0 1200"))
        assertEquals(Hold(right = DebugStick(1f, 1f), ms = 500), parse("rstick 1 1"))
    }

    @Test
    fun modesAndNeutral() {
        assertEquals(DebugPadCommand.Neutral, parse("neutral"))
        assertEquals(DebugPadCommand.MouseMode(true), parse("mouse-mode"))
        assertEquals(DebugPadCommand.MouseMode(false), parse("Pad-Mode"))
    }

    @Test
    fun wrongCommandsSayWhatIsWrong() {
        assertTrue(refused("").contains("no command"))
        assertTrue(refused("jump").contains("unknown command 'jump'"))
        assertTrue(refused("press Q").contains("unknown button 'Q'"))
        assertTrue(refused("press 100").contains("name one or more buttons"))
        assertTrue(refused("press A 0").contains("not a duration"))
        assertTrue(refused("press A 30001").contains("not a duration"))
        assertTrue(refused("press A 100 200").contains("too many words"))
        assertTrue(refused("dpad A").contains("unknown d-pad direction 'A'"))
        assertTrue(refused("lstick 0").contains("needs x and y"))
        assertTrue(refused("lstick 0 -1.5").contains("'-1.5' is not a number in -1.0..1.0"))
        assertTrue(refused("lstick 0 NaN").contains("not a number"))
        assertTrue(refused("hold LT 2").contains("not a number in 0.0..1.0"))
        assertTrue(refused("hold RT").contains("needs a value"))
        assertTrue(refused("neutral now").contains("takes no more words"))
    }

    @Test
    fun theAnswerNamesTheLengthForPadSh() {
        assertEquals("hold A for 150 ms", parse("press A").describe())
        assertEquals("hold LSTICK 0.00 -1.00 for 800 ms", parse("hold LSTICK 0 -1 800").describe())
        assertEquals("hold L1 START for 500 ms", parse("hold START L1").describe())
        assertEquals("hold RT 0.50 for 500 ms", parse("hold RT 0.5").describe())
        assertEquals("mouse-mode", parse("mouse-mode").describe())
    }

    @Test
    fun eachPartGoesBackToRestAtItsOwnTime() {
        val state = DebugPadState()
        state.hold(hold("hold LSTICK 0 -1 800"), now = 1000)
        state.hold(hold("press A 150"), now = 1100)
        assertEquals(1250L, state.next())
        assertEquals(setOf(PadButton.A) to PadAxes(ly = -1f), state.at(1200))
        assertEquals(emptySet<PadButton>() to PadAxes(ly = -1f), state.at(1250))
        assertEquals(1800L, state.next())
        // A new value of the same part replaces the old one and its time.
        state.hold(hold("lstick 1 0 100"), now = 1300)
        assertEquals(emptySet<PadButton>() to PadAxes(lx = 1f), state.at(1399))
        assertEquals(emptySet<PadButton>() to PadAxes(), state.at(1400))
        assertNull(state.next())
    }

    @Test
    fun clearLetsGoOfEverything() {
        val state = DebugPadState()
        state.hold(hold("hold A B"), now = 0)
        state.hold(hold("hold LT 1"), now = 0)
        state.hold(hold("rstick 0.5 0.5"), now = 0)
        state.clear()
        assertEquals(emptySet<PadButton>() to PadAxes(), state.at(1))
        assertNull(state.next())
    }
}
