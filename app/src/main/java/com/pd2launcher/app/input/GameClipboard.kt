package com.pd2launcher.app.input

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context

/**
 * When Android's clipboard goes to the game, and what the game's own copy must not bring back:
 * the Android side of [GameClipboard], apart from Android so that the JVM tests check it.
 */
class ClipboardSync {
    /** The timestamp (`ClipDescription.getTimestamp`) of the clip looked at last; 0 before any. */
    private var looked = 0L

    /** The text the game copied last, which Android's clipboard holds from [fromGame]. */
    private var fromGame: String? = null

    /** Whether the clip of [timestamp] is one not looked at yet: only then is its text read. */
    fun isNew(timestamp: Long): Boolean = timestamp != looked

    /**
     * The clip of [timestamp], with [text] (null: no text), is looked at: its text for the game, or
     * null for no text, an empty one, or the game's own copy coming back.
     */
    fun toGame(timestamp: Long, text: String?): String? {
        looked = timestamp
        return text?.takeIf { it.isNotEmpty() && it != fromGame }
    }

    /** The game copied [text], which goes on Android's clipboard. */
    fun fromGame(text: String) {
        fromGame = text
    }
}

/**
 * The clipboard between Android and the game (x-input-relay carries it, userland/
 * x-input-relay-clipboard.h): [toGame] gives the game Android's clipboard text when it is new
 * (each time the Game view gets the focus back: after a copy in another app, Android lets an app
 * read the clipboard only while it has the focus), and [fromGame] puts the game's copy (BH's
 * Ctrl+C on an item, PD2's guild link) on Android's clipboard. Plain text only; a clip marked
 * sensitive (a password manager's) never goes to the game. Android 12 and
 * later show their "pasted from your clipboard" notice at each read of a clip's content, so the
 * clip's description (no notice) tells first whether it is new ([ClipboardSync]).
 */
class GameClipboard(private val context: Context) {
    private val manager: ClipboardManager? = context.getSystemService(ClipboardManager::class.java)
    private val sync = ClipboardSync()

    /** Android's clipboard text for the game when it holds a new one; null when not. Main thread. */
    fun toGame(): RelayEvent.Clipboard? {
        val manager = manager ?: return null
        val description = manager.primaryClipDescription ?: return null
        if (!sync.isNew(description.timestamp)) return null
        // A password manager's clip (Android 13's EXTRA_IS_SENSITIVE; the key by name, minSdk is 30) stays out of the game.
        val sensitive = description.extras?.getBoolean(EXTRA_IS_SENSITIVE) == true
        val text = if (!sensitive && description.hasMimeType("text/*")) {
            manager.primaryClip?.takeIf { it.itemCount > 0 }?.getItemAt(0)?.coerceToText(context)?.toString()
        } else {
            null
        }
        return sync.toGame(description.timestamp, text)?.let(RelayEvent.Clipboard::of)
    }

    /** The game copied [text]: it goes on Android's clipboard. Main thread. */
    fun fromGame(text: String) {
        sync.fromGame(text)
        manager?.setPrimaryClip(ClipData.newPlainText(LABEL, text))
    }

    private companion object {
        const val LABEL = "Project Diablo 2"
        const val EXTRA_IS_SENSITIVE = "android.content.extra.IS_SENSITIVE"
    }
}
