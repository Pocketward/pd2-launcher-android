package com.pd2launcher.app.ui

import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Typography
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.sp

/**
 * The native screens' tokens: the desktop launcher's look on a dark handheld panel. Teal marks only the main action (and Play without updating under Update), the current step and focus; amber and red
 * mean warning and error. The brand mark's gold ([Gold]) belongs to the brand and to one mark:
 * the new-version dot. Every text colour reaches 4.5:1 on [Background] and [Surface]
 * (PaletteContrastTest).
 */
object Palette {
    val Background = Color(0xFF0E1214)
    val Surface = Color(0xFF141A1D)
    val Raised = Color(0xFF1C2428)
    val Rule = Color(0xFF263036)
    /** The edge of a quiet pill floating over content (Back, Stop): clearer than [Rule], which marks a disabled one. */
    val Edge = Color(0xFF33404A)
    /**
     * A tool button's fill ([Kind.TONAL]: the official PD2 launcher under Play): clearly lighter
     * than the teal glow behind Play's buttons ([Lattice]), so it reads as a button; [Text] is 8:1 on it.
     */
    val Tonal = Color(0xFF31434A)
    /** A tool button's edge, a step over [Tonal]. */
    val TonalEdge = Color(0xFF42565D)
    /** A focused tool button's fill, inside the teal focus ring. */
    val TonalFocus = Color(0xFF3A5058)
    val Text = Color(0xFFE3E7E4)
    val Muted = Color(0xFF8B979C)
    /** Rows not reached yet and disabled labels: dimmer than [Muted], still 4.9:1 and 4.6:1. */
    val Faint = Color(0xFF788489)
    /** A pressed control's tint over the game ([PressTint]): [Text] at 10 %, on a panel about as light as [Rule]. */
    val Pressed = Text.copy(alpha = 0.1f)
    val Teal = Color(0xFF34B8A7)
    val OnTeal = Color(0xFF05201C)
    /** Warnings: an amber-orange between the brand's gold and [Red]. */
    val Amber = Color(0xFFEE8A3A)
    /**
     * The brand's gold, the "II" in ic_brand_mark and the launcher icon: only for the dot on Play's
     * About while a new version of the app is out ([LauncherButton]'s dot). No text and no warning.
     */
    val Gold = Color(0xFFE3AA3E)
    val Red = Color(0xFFE5644F)
}

object Type {
    private val sans = FontFamily.SansSerif
    val Title = TextStyle(fontFamily = sans, fontWeight = FontWeight.SemiBold, fontSize = 22.sp, lineHeight = 28.sp, letterSpacing = (-0.2).sp)
    val Intro = TextStyle(fontFamily = sans, fontSize = 14.sp, lineHeight = 19.sp)
    val Body = TextStyle(fontFamily = sans, fontSize = 14.sp, lineHeight = 19.sp)
    val Hint = TextStyle(fontFamily = sans, fontSize = 13.sp, lineHeight = 17.sp)
    /** Overlines, the smallest text: 11 sp, the floor. */
    val Label = TextStyle(fontFamily = sans, fontWeight = FontWeight.Medium, fontSize = 11.sp, lineHeight = 15.sp, letterSpacing = 0.8.sp)
    val Button = TextStyle(fontFamily = sans, fontWeight = FontWeight.SemiBold, fontSize = 15.sp, letterSpacing = 0.2.sp)
    val Glyph = TextStyle(fontFamily = sans, fontWeight = FontWeight.Bold, fontSize = 12.sp, lineHeight = 14.sp)
    val Brand = TextStyle(fontFamily = sans, fontWeight = FontWeight.Bold, fontSize = 13.sp, letterSpacing = 2.sp)
    /** A step's figure: the download's or install's percentage. */
    val Figure = TextStyle(fontFamily = sans, fontWeight = FontWeight.Light, fontSize = 40.sp, lineHeight = 44.sp, letterSpacing = (-0.8).sp)
    /** Play's own button. */
    val Play = Button.copy(fontWeight = FontWeight.Bold, fontSize = 22.sp, letterSpacing = 2.sp)
}

@Composable
fun LauncherTheme(content: @Composable () -> Unit) {
    MaterialTheme(
        colorScheme = darkColorScheme(
            primary = Palette.Teal,
            onPrimary = Palette.OnTeal,
            background = Palette.Background,
            onBackground = Palette.Text,
            surface = Palette.Surface,
            onSurface = Palette.Text,
            outline = Palette.Rule,
            error = Palette.Red,
        ),
        typography = Typography(bodyLarge = Type.Body),
        content = content,
    )
}
