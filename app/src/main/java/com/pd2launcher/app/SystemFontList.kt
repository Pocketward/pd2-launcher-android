package com.pd2launcher.app

import android.graphics.fonts.FontStyle
import android.graphics.fonts.SystemFonts
import java.io.File
import java.util.Locale

/**
 * The phone's own fonts for the game's Wine (issue 19: a launcher's Chinese text showed as boxes,
 * because Wine has no font for it). Android lists its fonts with the languages each one is for
 * ([SystemFonts.getAvailableFonts], Android 10); the app writes the font of each language of
 * [LANGUAGES] into a file, whose path the engine gets as [VARIABLE]. The engine links these fonts
 * into Wine, so that a Windows font without a letter takes it from the phone's font for that
 * language. Nothing is copied or shipped: the fonts stay where the phone keeps them.
 */
object SystemFontList {
    /** The engine's variable with the path of the list. */
    const val VARIABLE = "PD2_SYSTEM_FONTS"

    /**
     * The languages whose phone font Wine gets, as [languageKey] names them. Owner (2026-11-09):
     * Simplified Chinese until a player reports boxes for another language.
     */
    val LANGUAGES = setOf("zh-Hans")

    /** One font that Android lists: its languages (Android's tags), file, index in a collection, weight and slant. */
    data class Font(val languages: String, val path: String, val index: Int, val weight: Int, val italic: Boolean)

    /**
     * The lines `languages<TAB>index<TAB>path` of the list: for each language tag set of [fonts]
     * whose first tag is a language of [LANGUAGES], the upright font closest to the normal weight
     * (400; then the first path), by their tags.
     */
    fun lines(fonts: List<Font>): List<String> = fonts
        .filter { !it.italic && languageKey(it.languages.split(',').first()) in LANGUAGES }
        .groupBy { it.languages }
        .map { (_, group) -> group.minWith(compareBy<Font>({ Math.abs(it.weight - 400) }, { it.path })) }
        .sortedBy { it.languages }
        .map { "${it.languages}\t${it.index}\t${it.path}" }

    /**
     * The key of a language tag (Android gives full ones, such as `zh-Hans-CN` and `ja-Jpan-JP`):
     * `zh-Hans` or `zh-Hant` for Chinese (by the script, else Traditional for Taiwan, Hong Kong and
     * Macao), else the language and its script, without the region (`ja-Jpan`, `und-Thai`).
     */
    internal fun languageKey(tag: String): String {
        val locale = Locale.forLanguageTag(tag)
        if (locale.language == "zh") {
            val traditional = locale.script == "Hant" || (locale.script.isEmpty() && locale.country in setOf("TW", "HK", "MO"))
            return if (traditional) "zh-Hant" else "zh-Hans"
        }
        // Java names the undetermined language ("und") with an empty string.
        return listOf(locale.language.ifEmpty { "und" }, locale.script).filter { it.isNotEmpty() }.joinToString("-")
    }

    /** Android's fonts that have a file ([SystemFonts.getAvailableFonts]). */
    private fun phoneFonts(): List<Font> = SystemFonts.getAvailableFonts().mapNotNull { font ->
        val file = font.file ?: return@mapNotNull null
        Font(
            font.localeList.toLanguageTags(),
            file.path,
            font.ttcIndex,
            font.style.weight,
            font.style.slant == FontStyle.FONT_SLANT_ITALIC,
        )
    }

    /**
     * Write the list into [file] (the session's runtime folder) and return the number of lines; 0
     * when Android's list cannot be read or has no font of [LANGUAGES] (the engine then logs which
     * language has none).
     */
    fun write(file: File): Int {
        val lines = try {
            lines(phoneFonts())
        } catch (error: RuntimeException) {
            emptyList()
        }
        file.parentFile?.mkdirs()
        file.writeText(lines.joinToString("") { "$it\n" })
        return lines.size
    }
}
