package com.pd2launcher.app

import android.content.Context
import android.content.SharedPreferences

/**
 * What the app keeps of the games on the route of the pick, in preferences of their own, which no
 * screen saves over (the screens save the whole [AppSettings]): the games in a row that ended with
 * no frame ([noFrameGame]), the route that a game confirmed ([confirmed]: no "Does the game work?"
 * card there), Play's error line of the last failure until the next game ([keepNotice]), and
 * whether the engine can run the Software driver ([engineSoftware]). None of it changes the route:
 * only the pick does ([DriverPage.route]; the owner, 2026-11-04). The games with no frame and the
 * error line count only in the app version that wrote them ([APP_VERSION]: the versionName and the
 * versionCode).
 */
object RouteState {
    /** This build's app version for the records: "versionName/versionCode", as "0.4.0-beta1/30899". */
    val APP_VERSION = appVersion(BuildConfig.VERSION_NAME, BuildConfig.VERSION_CODE)

    /** The app version of the build [name] with [code] ([APP_VERSION]). */
    fun appVersion(name: String, code: Int): String = "$name/$code"

    private const val PREFERENCES = "graphics_route"
    private const val NO_FRAME_GAMES = "venus_no_frame_games"
    private const val NO_FRAME_VERSION = "venus_no_frame_version"
    private const val CONFIRMED = "confirmed_route"
    private const val FAILED = "driver_failed_route"
    private const val FAILED_VERSION = "driver_failed_version"
    private const val ENGINE_SOFTWARE = "engine_software_driver"

    /**
     * The records of the automatic fallback of earlier builds (the routes left, the built-in
     * route's failures and rescue, the automatic Software driver, the old notice): not read,
     * removed once ([dropOldRecords]), with the old confirmations per route and app version.
     */
    internal val OLD_KEYS = setOf(
        "venus_left_in", "virgl_left_in", "libhybris_left_in", "radv_left_in", "mali_panvk_left_in", "built_in_left_in",
        "built_in_failed_in", "built_in_rescue_failed_in", "software_in", "driver_notice", "driver_notice_version",
    )

    private fun preferences(context: Context) = context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)

    /** The app version that [key] holds; null: none, or an older build's versionCode (an Int). */
    private fun version(prefs: SharedPreferences, key: String): String? = prefs.all[key] as? String

    /**
     * Whether the records of [all] hold route records of the 0.4.0 line's earlier builds ([OLD_KEYS],
     * the per-route confirmations): not a 0.3.8 player ([DriverPage.unchangedRoute]). Read before
     * [dropOldRecords], which the device check comes before.
     */
    fun lineRecords(all: Map<String, *>): Boolean = all.keys.any { it in OLD_KEYS || it.endsWith("_confirmed_in") || it.endsWith("_rescue_frames_in") }

    /** [lineRecords] of this app's records. */
    fun lineRecords(context: Context): Boolean = lineRecords(preferences(context).all)

    /** The records of earlier builds' automatic fallback go ([OLD_KEYS]). */
    fun dropOldRecords(context: Context) = dropOldRecords(preferences(context))

    /** [dropOldRecords] in [prefs]. */
    fun dropOldRecords(prefs: SharedPreferences) {
        val old = prefs.all.keys.filter { it in OLD_KEYS || it.endsWith("_confirmed_in") || it.endsWith("_rescue_frames_in") }
        if (old.isEmpty()) return
        prefs.edit().apply { old.forEach { remove(it) } }.apply()
    }

    /**
     * Whether the engine of this install ([key]: [PhoneDriver.installKey]) said in its welcome that
     * it can run the Software driver (the capability `gdi_adjmouse`): kept, so the app knows it
     * before the engine starts.
     */
    fun engineSoftware(context: Context, key: String): Boolean = engineSoftware(preferences(context), key)

    /** [engineSoftware] in [prefs]. */
    fun engineSoftware(prefs: SharedPreferences, key: String): Boolean = prefs.all[ENGINE_SOFTWARE] == key

    /** The engine's welcome for the install [key]: it can run the Software driver ([can]) or not. */
    fun engineSoftware(context: Context, key: String, can: Boolean) = engineSoftware(preferences(context), key, can)

    /** [engineSoftware] in [prefs]. */
    fun engineSoftware(prefs: SharedPreferences, key: String, can: Boolean) {
        prefs.edit().apply { if (can) putString(ENGINE_SOFTWARE, key) else remove(ENGINE_SOFTWARE) }.apply()
    }

    /** The route that a game confirmed last (It works on the card, or a game with frames); null: none. */
    fun confirmed(context: Context): GraphicsRoute? = confirmed(preferences(context))

    /** [confirmed] in [prefs]. */
    fun confirmed(prefs: SharedPreferences): GraphicsRoute? =
        prefs.getString(CONFIRMED, null)?.let { name -> GraphicsRoute.entries.firstOrNull { it.name == name } }

    /** A game confirmed [route]: no card on it until another route ran ([PhoneDriverFailure.asks]). */
    fun confirm(context: Context, route: GraphicsRoute) = confirm(preferences(context), route)

    /** [confirm] in [prefs]. */
    fun confirm(prefs: SharedPreferences, route: GraphicsRoute) {
        if (prefs.getString(CONFIRMED, null) != route.name) prefs.edit().putString(CONFIRMED, route.name).apply()
    }

    /** One more game with no frame in app version [version]: the count in a row, this one included. */
    fun noFrameGame(context: Context, version: String): Int = noFrameGame(preferences(context), version)

    /** [noFrameGame] in [prefs]. */
    @Synchronized
    fun noFrameGame(prefs: SharedPreferences, version: String): Int {
        val before = if (version(prefs, NO_FRAME_VERSION) == version) prefs.getInt(NO_FRAME_GAMES, 0) else 0
        prefs.edit().putString(NO_FRAME_VERSION, version).putInt(NO_FRAME_GAMES, before + 1).apply()
        return before + 1
    }

    /** A game showed frames: the games with no frame are no longer in a row. */
    fun framesShown(context: Context) = framesShown(preferences(context))

    /** [framesShown] in [prefs]. */
    fun framesShown(prefs: SharedPreferences) {
        if (prefs.contains(NO_FRAME_GAMES)) prefs.edit().remove(NO_FRAME_GAMES).apply()
    }

    /**
     * Play's error line after a failure of [route] ([PhoneDriverFailure.tells]), kept until the next
     * game starts or the pick changes ([clearNotice]): a session that starts again (the app closed,
     * or Android ended the session while the app was away) shows it too. Only in the app version
     * [version] that kept it.
     */
    fun keepNotice(context: Context, route: GraphicsRoute, version: String) = keepNotice(preferences(context), route, version)

    /** [keepNotice] in [prefs]. */
    fun keepNotice(prefs: SharedPreferences, route: GraphicsRoute, version: String) {
        prefs.edit().putString(FAILED, route.name).putString(FAILED_VERSION, version).apply()
    }

    /** The route whose failure [keepNotice] kept in this app version [version]; null: none. */
    fun notice(context: Context, version: String): GraphicsRoute? = notice(preferences(context), version)

    /** [notice] in [prefs]. */
    fun notice(prefs: SharedPreferences, version: String): GraphicsRoute? {
        if (version(prefs, FAILED_VERSION) != version) return null
        val name = prefs.getString(FAILED, null) ?: return null
        return GraphicsRoute.entries.firstOrNull { it.name == name }
    }

    /** A new game starts, or the pick changes: the error line goes. */
    fun clearNotice(context: Context) {
        val prefs = preferences(context)
        if (prefs.contains(FAILED)) prefs.edit().remove(FAILED).remove(FAILED_VERSION).apply()
    }
}
