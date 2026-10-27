package com.pd2launcher.app.input

/** A stick of the debug pad: [x] and [y] -1..1 as Android reports a stick (down positive). */
data class DebugStick(val x: Float, val y: Float)

/**
 * Debug builds only: one command of the debug pad injector (`DebugPadReceiver`). Words,
 * case-insensitive:
 *
 *     press <button>... [ms]          the buttons for ms (default PRESS_MS)
 *     dpad UP|DOWN|LEFT|RIGHT [ms]    the d-pad direction for ms (default PRESS_MS)
 *     hold <button>... [ms]           the buttons for ms (default HOLD_MS)
 *     hold LSTICK|RSTICK <x> <y> [ms] a stick, -1..1 each, y -1 up (default HOLD_MS)
 *     hold LT|RT <value> [ms]         a trigger, 0..1 (default HOLD_MS)
 *     lstick|rstick <x> <y> [ms]      as hold LSTICK|RSTICK
 *     neutral                         everything back at rest at once
 *     mouse-mode | pad-mode           the controller's mode, as the chord switches it
 *
 * Buttons: [PadButton]'s names (A B X Y L1 R1 L2 R2 L3 R3 START SELECT UP DOWN LEFT RIGHT).
 * A duration is 1 to [MAX_MS] ms; after it the parts the command set go back to rest.
 */
sealed class DebugPadCommand {
    /** Holds what it names for [ms]; what it does not name stays as it is. */
    data class Hold(
        val buttons: Set<PadButton> = emptySet(),
        val left: DebugStick? = null,
        val right: DebugStick? = null,
        val leftTrigger: Float? = null,
        val rightTrigger: Float? = null,
        val ms: Long,
    ) : DebugPadCommand()

    data class MouseMode(val on: Boolean) : DebugPadCommand()

    data object Neutral : DebugPadCommand()

    /** The command in canonical words, for the log and the answer: `pad.sh` reads "for <ms> ms". */
    fun describe(): String = when (this) {
        Neutral -> "neutral"
        is MouseMode -> if (on) "mouse-mode" else "pad-mode"
        is Hold -> buildList {
            if (buttons.isNotEmpty()) add(buttons.sortedBy { it.ordinal }.joinToString(" ") { it.name })
            left?.let { add("LSTICK ${number(it.x)} ${number(it.y)}") }
            right?.let { add("RSTICK ${number(it.x)} ${number(it.y)}") }
            leftTrigger?.let { add("LT ${number(it)}") }
            rightTrigger?.let { add("RT ${number(it)}") }
        }.joinToString(" ", prefix = "hold ", postfix = " for $ms ms")
    }

    companion object {
        /** A press's default length: about four of the game's 25 frames a second. */
        const val PRESS_MS = 150L
        const val HOLD_MS = 500L

        /** The longest hold: nothing stays held for long if the sender goes away. */
        const val MAX_MS = 30_000L

        private fun number(value: Float) = "%.2f".format(java.util.Locale.ROOT, value)

        /** [text] as a command; throws [IllegalArgumentException] with what is wrong. */
        fun parse(text: String): DebugPadCommand {
            val words = text.trim().split(Regex("\\s+")).filter { it.isNotEmpty() }
            require(words.isNotEmpty()) { "no command" }
            val args = words.drop(1)
            return when (val verb = words[0].lowercase()) {
                "neutral" -> none(args, Neutral)
                "mouse-mode" -> none(args, MouseMode(true))
                "pad-mode" -> none(args, MouseMode(false))
                "press" -> buttons(args, PRESS_MS)
                "dpad" -> {
                    require(args.isNotEmpty()) { "dpad needs a direction: UP, DOWN, LEFT or RIGHT" }
                    val direction = PadButton.entries.firstOrNull { it.name.equals(args[0], ignoreCase = true) }
                        ?.takeIf { it in DPAD }
                    require(direction != null) { "unknown d-pad direction '${args[0]}': UP, DOWN, LEFT or RIGHT" }
                    Hold(setOf(direction), ms = duration(args.drop(1), PRESS_MS))
                }
                "lstick", "rstick" -> stick(verb == "lstick", args)
                "hold" -> {
                    require(args.isNotEmpty()) { "hold needs a button, LSTICK, RSTICK, LT or RT" }
                    when (args[0].uppercase()) {
                        "LSTICK", "RSTICK" -> stick(args[0].uppercase() == "LSTICK", args.drop(1))
                        "LT", "RT" -> {
                            require(args.size >= 2) { "hold ${args[0]} needs a value 0..1" }
                            val value = value(args[1], 0f..1f)
                            val ms = duration(args.drop(2), HOLD_MS)
                            if (args[0].uppercase() == "LT") Hold(leftTrigger = value, ms = ms) else Hold(rightTrigger = value, ms = ms)
                        }
                        else -> buttons(args, HOLD_MS)
                    }
                }
                else -> throw IllegalArgumentException(
                    "unknown command '${words[0]}': press, dpad, hold, lstick, rstick, neutral, mouse-mode or pad-mode",
                )
            }
        }

        private val DPAD = setOf(PadButton.UP, PadButton.DOWN, PadButton.LEFT, PadButton.RIGHT)

        private fun none(args: List<String>, command: DebugPadCommand): DebugPadCommand {
            require(args.isEmpty()) { "${command.describe()} takes no more words" }
            return command
        }

        private fun buttons(args: List<String>, defaultMs: Long): Hold {
            val names = args.takeWhile { it.toDoubleOrNull() == null }
            require(names.isNotEmpty()) { "name one or more buttons: ${PadButton.entries.joinToString(" ") { it.name }}" }
            val buttons = names.mapTo(LinkedHashSet()) { name ->
                PadButton.entries.firstOrNull { it.name.equals(name, ignoreCase = true) }
                    ?: throw IllegalArgumentException("unknown button '$name': ${PadButton.entries.joinToString(" ") { it.name }}")
            }
            return Hold(buttons, ms = duration(args.drop(names.size), defaultMs))
        }

        private fun stick(left: Boolean, args: List<String>): Hold {
            require(args.size >= 2) { "a stick needs x and y, -1..1 each (y -1 is up)" }
            val stick = DebugStick(value(args[0], -1f..1f), value(args[1], -1f..1f))
            val ms = duration(args.drop(2), HOLD_MS)
            return if (left) Hold(left = stick, ms = ms) else Hold(right = stick, ms = ms)
        }

        private fun value(word: String, range: ClosedFloatingPointRange<Float>): Float {
            val value = word.toFloatOrNull()
            require(value != null && !value.isNaN() && value in range) { "'$word' is not a number in ${range.start}..${range.endInclusive}" }
            return value
        }

        private fun duration(args: List<String>, default: Long): Long {
            require(args.size <= 1) { "too many words: '${args.joinToString(" ")}'" }
            val word = args.firstOrNull() ?: return default
            val ms = word.toLongOrNull()
            require(ms != null && ms in 1..MAX_MS) { "'$word' is not a duration of 1 to $MAX_MS ms" }
            return ms
        }
    }
}

/**
 * Debug builds only: what the debug pad holds and until when (uptime ms), for [DebugPadCommand.Hold]:
 * each button, stick and trigger goes back to rest at its own time.
 */
class DebugPadState {
    private val buttons = HashMap<PadButton, Long>()
    private var left: Pair<DebugStick, Long>? = null
    private var right: Pair<DebugStick, Long>? = null
    private var leftTrigger: Pair<Float, Long>? = null
    private var rightTrigger: Pair<Float, Long>? = null

    /** Holds what [command] names from [now] for its length; the rest stays. */
    fun hold(command: DebugPadCommand.Hold, now: Long) {
        val until = now + command.ms
        command.buttons.forEach { buttons[it] = until }
        command.left?.let { left = it to until }
        command.right?.let { right = it to until }
        command.leftTrigger?.let { leftTrigger = it to until }
        command.rightTrigger?.let { rightTrigger = it to until }
    }

    fun clear() {
        buttons.clear()
        left = null
        right = null
        leftTrigger = null
        rightTrigger = null
    }

    /** Lets go of what is due by [now]; the buttons and axes held after that. */
    fun at(now: Long): Pair<Set<PadButton>, PadAxes> {
        buttons.values.removeAll { it <= now }
        if (left?.second?.let { it <= now } == true) left = null
        if (right?.second?.let { it <= now } == true) right = null
        if (leftTrigger?.second?.let { it <= now } == true) leftTrigger = null
        if (rightTrigger?.second?.let { it <= now } == true) rightTrigger = null
        return buttons.keys.toSet() to PadAxes(
            lx = left?.first?.x ?: 0f,
            ly = left?.first?.y ?: 0f,
            rx = right?.first?.x ?: 0f,
            ry = right?.first?.y ?: 0f,
            leftTrigger = leftTrigger?.first ?: 0f,
            rightTrigger = rightTrigger?.first ?: 0f,
        )
    }

    /** When the next part goes back to rest; null: nothing is held. */
    fun next(): Long? = (buttons.values + listOfNotNull(left?.second, right?.second, leftTrigger?.second, rightTrigger?.second)).minOrNull()
}
