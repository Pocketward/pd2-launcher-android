package com.pd2launcher.app

import androidx.annotation.StringRes

/**
 * A CD key the engine refused, under the field it concerns: [field] 0 is the Diablo II key, 1 the
 * Lord of Destruction key. [text] says why in the player's words; it never holds the key.
 */
data class KeyProblem(val field: Int, @param:StringRes val text: Int)

/**
 * The engine's checks of the CD keys, as Blizzard's installers check them: its refusal of
 * `authorize` (`d2-key-…`, `lod-key-…`), and the same check failed at the start of Install Diablo
 * II (`base-game-key-…`, `expansion-key-…`). Each code names the field and why; the key page
 * shows the text under that field ([KeyProblem]).
 */
object KeyProblems {
    const val D2 = 0
    const val LOD = 1

    /** The problem [code] names; null for any other code (or none). */
    fun of(code: String?): KeyProblem? {
        code ?: return null
        val (field, problem) = when {
            code.startsWith("d2-key-") -> D2 to code.removePrefix("d2-key-")
            code.startsWith("lod-key-") -> LOD to code.removePrefix("lod-key-")
            code.startsWith("base-game-key-") -> D2 to code.removePrefix("base-game-key-")
            code.startsWith("expansion-key-") -> LOD to code.removePrefix("expansion-key-")
            else -> return null
        }
        val text = when (problem) {
            "wrong-length" -> R.string.key_problem_length
            "16-characters" -> R.string.key_problem_16_characters
            "invalid-character" -> R.string.key_problem_character
            "not-valid" -> R.string.key_problem_not_valid
            // A Lord of Destruction key in the Diablo II field, and the other way round.
            "is-lod-key", "is-expansion-key" -> if (field == D2) R.string.key_problem_is_lod_key else return null
            "is-d2-key", "is-base-game-key" -> if (field == LOD) R.string.key_problem_is_d2_key else return null
            // The installer refused the key (Blizzard's installers): its own words on Install.
            else -> return null
        }
        return KeyProblem(field, text)
    }
}
