package com.pd2launcher.app.ui

import androidx.activity.compose.BackHandler
import androidx.annotation.StringRes
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.gestures.BringIntoViewSpec
import androidx.compose.foundation.gestures.LocalBringIntoViewSpec
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imeAnimationTarget
import androidx.compose.foundation.layout.isImeVisible
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.compositionLocalOf
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalWindowInfo
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import com.pd2launcher.app.R
import com.pd2launcher.app.Refusal
import com.pd2launcher.app.Refusals

/** The four first-run flows the header's step marks name. */
enum class FlowStep(@param:StringRes val label: Int) {
    CHECK(R.string.step_check),
    DOWNLOAD(R.string.step_download),
    INSTALL(R.string.step_install),
    PLAY(R.string.step_play),
}

/** How one step mark shows: ✓ done, a filled number for the current one, ! when it stopped, a ring ahead. */
enum class StepMark { DONE, CURRENT, FAILED, AHEAD }

/** The mark of [step] while the player is at [current]; [failed]: the current step stopped on a problem. */
fun stepMark(step: FlowStep, current: FlowStep, failed: Boolean): StepMark = when {
    step < current -> StepMark.DONE
    step > current -> StepMark.AHEAD
    failed -> StepMark.FAILED
    else -> StepMark.CURRENT
}

/**
 * What the step bar shows of a download or install: its percent, 0 to 100, beside the current
 * step ("Install · 97%") and as the line along the header's rule, while that work runs (or
 * stops); null otherwise, and while its amount is unknown.
 */
fun stepProgress(working: Boolean, percent: Int?): Int? = if (working && percent != null) percent.coerceIn(0, 100) else null

/**
 * Whether Back shows in the bottom-left corner: the page has one and no download or install
 * runs on it. Hidden while one runs (the system Back then leaves the app), it comes back beside
 * Resume once that work is paused.
 */
fun backShown(hasBack: Boolean, working: Boolean): Boolean = hasBack && !working

/**
 * A line a flow screen adds beside its cause, the screens having no status line: an error
 * ([PROBLEM], a red banner), what a choice found ([FOUND], a row: what a picked Diablo II folder
 * holds), or why the main action waits ([WAITS], a [WaitNote]). Nothing else is said there.
 */
enum class Note { PROBLEM, FOUND, WAITS }

/** The engine's refusal of the player's last command, and how the player dismisses it. */
class RefusalNotice(val refusal: Refusal, val onDismiss: () -> Unit)

/** Shown by every [FlowShell] screen, so the refusal is on the screen its command came from. */
val LocalRefusal = compositionLocalOf<RefusalNotice?> { null }

/**
 * The height the keyboard must leave above it for the header and the corner pills to stay: a
 * phone held upright keeps them, a handheld or anything held landscape does not.
 */
val ROOM_FOR_BARS = 360.dp

/**
 * Whether the header and the corner pills give way to the field typed into: the keyboard is up
 * and leaves less than [ROOM_FOR_BARS] above it. They come back when it closes.
 */
fun barsGiveWay(keyboardUp: Boolean, heightLeft: Dp): Boolean = keyboardUp && heightLeft < ROOM_FOR_BARS

/** [barsGiveWay] in this window, from where the keyboard is going rather than where its animation is. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun barsGiveWay(): Boolean {
    val density = LocalDensity.current
    val window = LocalWindowInfo.current.containerDpSize.height
    val covered = with(density) { (WindowInsets.safeDrawing.getTop(this) + WindowInsets.imeAnimationTarget.getBottom(this)).toDp() }
    return barsGiveWay(WindowInsets.isImeVisible, window - covered)
}

/**
 * The corner pills' measures: [pill] tall, [margin] above the window's bottom (or the keyboard),
 * at least [narrow] wide when quiet and [wide] when it is the main action or waits.
 */
data class Corners(val pill: Dp, val margin: Dp, val narrow: Dp, val wide: Dp) {
    /**
     * What every scrolling column the pills overlap keeps below its end, inside the scroll: the
     * pills, their margin and [CLEARANCE] of air, so its last row scrolls fully clear above them.
     * A short page does not scroll and shows no gap.
     */
    val phantom: Dp get() = pill + margin + CLEARANCE

    companion object {
        /** The air between the last row, scrolled to the end, and the top of the pills. */
        val CLEARANCE = 20.dp

        /** How far above the pills the content fades into the background. */
        val FADE = 28.dp

        val PHONE = Corners(pill = 52.dp, margin = 16.dp, narrow = 112.dp, wide = 148.dp)
        val HANDHELD = Corners(pill = 48.dp, margin = 12.dp, narrow = 104.dp, wide = 132.dp)

        fun of(phone: Boolean): Corners = if (phone) PHONE else HANDHELD
    }
}

/**
 * The first-run shell: a header with the wordmark and the step marks; the task pane (the step's
 * words and its figure left, the evidence right of a thin rule; one column on phones); and no
 * footer. Back floats in the bottom-left corner and the step's main [action] in the bottom-right,
 * in the same places on every flow screen, over a fade into the background; every column they
 * overlap keeps [Corners.phantom] below its end so it scrolls clear of them. On handhelds Back
 * sits under the words and the action under the evidence, each pane running the full height.
 * No status line: what the page must say (an error, why the action waits) sits beside its cause.
 * Draws edge to edge; system bars, cutouts and the keyboard are kept clear.
 *
 * [step] null: no step marks (Repair). [onBack] takes the system Back; [backButton] also shows it
 * as the Back pill; [secondary] takes Back's corner on a page whose two actions are not Back
 * (Repair). [progress]: a running download's or install's percent ([stepProgress]) for the step
 * bar. While the keyboard leaves little height ([barsGiveWay]) the header and the pills give way.
 */
@Composable
fun FlowShell(
    step: FlowStep?,
    title: String,
    intro: String?,
    action: (@Composable () -> Unit)?,
    failed: Boolean = false,
    onBack: (() -> Unit)? = null,
    backButton: Boolean = false,
    secondary: (@Composable () -> Unit)? = null,
    progress: Int? = null,
    lead: (@Composable ColumnScope.() -> Unit)? = null,
    content: @Composable ColumnScope.() -> Unit,
) {
    BackHandler(enabled = onBack != null) { onBack?.invoke() }
    val phone = LocalFormFactor.current == FormFactor.PHONE
    val corners = Corners.of(phone)
    val refusal = LocalRefusal.current
    val typing = barsGiveWay()
    val backLabel = stringResource(R.string.action_back)
    val start: (@Composable () -> Unit)? = when {
        typing -> null
        secondary != null -> secondary
        onBack != null && backButton -> ({ CornerPill(backLabel, onBack) })
        else -> null
    }
    val end = action.takeUnless { typing }
    val words: @Composable ColumnScope.() -> Unit = {
        ScreenTitle(title)
        if (intro != null) {
            Spacer(Modifier.height(6.dp))
            Text(intro, style = Type.Intro, color = Palette.Muted)
        }
    }
    val body: @Composable ColumnScope.() -> Unit = {
        if (refusal != null) RefusalBanner(refusal)
        content()
    }
    Column(Modifier.fillMaxSize().background(Palette.Background).windowInsetsPadding(WindowInsets.safeDrawing)) {
        if (!typing) Header(step, failed, progress, phone)
        if (phone) {
            Pane(start, end, corners, Modifier.weight(1f).fillMaxWidth(), horizontal = 20.dp, top = 16.dp, bottom = 16.dp) {
                words()
                lead?.let {
                    Spacer(Modifier.height(16.dp))
                    it()
                }
                Spacer(Modifier.height(8.dp))
                body()
            }
        } else {
            Row(Modifier.weight(1f).fillMaxWidth()) {
                Pane(start, null, corners, Modifier.weight(0.44f).fillMaxHeight(), horizontal = 20.dp, top = 20.dp, bottom = 16.dp) {
                    words()
                    lead?.let {
                        Spacer(Modifier.height(20.dp))
                        it()
                    }
                }
                Rule(Modifier.fillMaxHeight().padding(vertical = 16.dp), vertical = true)
                Pane(null, end, corners, Modifier.weight(0.56f).fillMaxHeight(), horizontal = 16.dp, top = 8.dp, bottom = 8.dp, content = body)
            }
        }
    }
}

/**
 * A corner pill: Back or a page's other action at the bottom left, the main action at the
 * bottom right. Sized to its label and never under 48 dp tall; quiet ones (Back, Stop) outlined
 * and opaque, the enabled main action filled teal with a soft shadow, a disabled one outlined
 * with a faint label and still focusable for the d-pad.
 */
@Composable
fun CornerPill(
    label: String,
    onClick: () -> Unit,
    kind: Kind = Kind.SECONDARY,
    enabled: Boolean = true,
    focusRequester: FocusRequester? = null,
) {
    val corners = Corners.of(LocalFormFactor.current == FormFactor.PHONE)
    LauncherButton(
        label, onClick, Modifier.widthIn(min = if (kind == Kind.PRIMARY || !enabled) corners.wide else corners.narrow), kind, enabled,
        focusRequester, height = corners.pill, padding = 24.dp, floating = true,
    )
}

/**
 * Why the main action waits, under its cause: amber when it waits on the player (keys to type),
 * muted when it waits on the app (a check, the Launcher starting). With an error's banner, the
 * only words a flow screen adds to its content; a d-pad stop like the rows.
 */
@Composable
fun WaitNote(text: String, modifier: Modifier = Modifier, onPlayer: Boolean = false) {
    Text(
        text, style = Type.Hint, color = if (onPlayer) Palette.Amber else Palette.Muted,
        modifier = modifier.fillMaxWidth().readingStop().padding(horizontal = 4.dp, vertical = 8.dp),
    )
}

/** The wordmark and the step marks over the rule, which carries a running job's progress. */
@Composable
private fun Header(step: FlowStep?, failed: Boolean, progress: Int?, phone: Boolean) {
    if (!phone) {
        Row(Modifier.fillMaxWidth().heightIn(min = 56.dp).padding(horizontal = 20.dp), verticalAlignment = Alignment.CenterVertically) {
            // The name gives way to the marks where both do not fit (Wordmark).
            Wordmark(Modifier.weight(1f))
            if (step != null) {
                Spacer(Modifier.width(16.dp))
                FlowMarks(step, failed = failed, progress = progress)
            }
        }
    } else {
        Column(Modifier.padding(horizontal = 20.dp)) {
            Wordmark()
            if (step != null) {
                FlowMarks(step, compact = true, failed = failed, progress = progress)
                Spacer(Modifier.height(12.dp))
            }
        }
    }
    if (progress == null) {
        Rule()
    } else {
        Canvas(Modifier.fillMaxWidth().height(2.dp)) {
            drawRect(Palette.Rule)
            drawRect(Palette.Teal, size = Size(size.width * progress / 100f, size.height))
        }
    }
}

/**
 * One scrolling column and the corner pills over its bottom, [start] left and [end] right, on a
 * fade from the content into the background. With a pill the column keeps [Corners.phantom] below
 * its end, inside the scroll, and brings what takes focus into view clear of the pills.
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun Pane(
    start: (@Composable () -> Unit)?,
    end: (@Composable () -> Unit)?,
    corners: Corners,
    modifier: Modifier,
    horizontal: Dp,
    top: Dp,
    bottom: Dp,
    content: @Composable ColumnScope.() -> Unit,
) {
    val pills = start != null || end != null
    val density = LocalDensity.current
    Box(modifier) {
        val spec = if (pills) remember(corners, density) { ClearOfPills(with(density) { corners.phantom.toPx() }) } else LocalBringIntoViewSpec.current
        CompositionLocalProvider(LocalBringIntoViewSpec provides spec) {
            Column(
                Modifier.fillMaxSize().verticalScroll(rememberScrollState())
                    .padding(start = horizontal, end = horizontal, top = top, bottom = if (pills) corners.phantom else bottom),
                content = content,
            )
        }
        if (pills) {
            val band = corners.pill + corners.margin
            val height = Corners.FADE + band
            Box(
                Modifier.align(Alignment.BottomCenter).fillMaxWidth().height(height).background(
                    Brush.verticalGradient(
                        0f to Palette.Background.copy(alpha = 0f),
                        Corners.FADE / height to Palette.Background.copy(alpha = 0.86f),
                        (Corners.FADE + corners.pill / 2) / height to Palette.Background,
                    ),
                ),
            )
            Row(
                Modifier.align(Alignment.BottomCenter).fillMaxWidth().padding(start = horizontal, end = horizontal, bottom = corners.margin),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                start?.invoke()
                // The main action keeps the right corner; a label too long for what is left ends in an ellipsis.
                Box(Modifier.weight(1f).padding(start = if (start != null) 12.dp else 0.dp), contentAlignment = Alignment.CenterEnd) {
                    end?.invoke()
                }
            }
        }
    }
}

/** Brings what takes focus (or asks to be seen) into view with [band] at the bottom kept clear: the pills and their air. */
@OptIn(ExperimentalFoundationApi::class)
internal class ClearOfPills(private val band: Float) : BringIntoViewSpec {
    override fun calculateScrollDistance(offset: Float, size: Float, containerSize: Float): Float =
        super.calculateScrollDistance(offset, size, (containerSize - band).coerceAtLeast(size.coerceAtMost(containerSize)))
}

@Composable
private fun RefusalBanner(notice: RefusalNotice) {
    val reason = notice.refusal.reason
    val text = Refusals.text(reason)?.let { stringResource(it) } ?: stringResource(R.string.refusal_other, reason)
    Banner(text, Palette.Red, Modifier.padding(vertical = 8.dp)) {
        TextLink(stringResource(R.string.refusal_dismiss), notice.onDismiss)
    }
}

/**
 * The four step marks, Check · Download · Install · Play: ✓ done, a filled number for the
 * current step, a ring for the rest, ! when it stopped. Labels never wrap; [compact] (phones)
 * keeps only the current one's. [progress]: a running job's percent after the current label.
 */
@Composable
fun FlowMarks(current: FlowStep, modifier: Modifier = Modifier, compact: Boolean = false, failed: Boolean = false, progress: Int? = null) {
    val name = stringResource(current.label)
    val counter = stringResource(R.string.step_counter, current.ordinal + 1, FlowStep.entries.size)
    val percent = progress?.let { stringResource(R.string.percent, it) }
    Row(
        modifier.clearAndSetSemantics { contentDescription = listOfNotNull(counter, name, percent).joinToString(", ") },
        verticalAlignment = Alignment.CenterVertically,
    ) {
        FlowStep.entries.forEach { step ->
            val mark = stepMark(step, current, failed)
            if (step.ordinal > 0) {
                Box(
                    Modifier.padding(horizontal = 8.dp).width(if (compact) 12.dp else 16.dp).height(1.dp)
                        .background(if (step <= current) Palette.Teal.copy(alpha = 0.5f) else Palette.Rule),
                )
            }
            val now = mark == StepMark.CURRENT || mark == StepMark.FAILED
            Box(
                Modifier.size(20.dp).clip(CircleShape)
                    .background(
                        when (mark) {
                            StepMark.CURRENT -> Palette.Teal
                            StepMark.FAILED -> Palette.Red
                            else -> Color.Transparent
                        },
                    )
                    .border(1.5.dp, if (mark == StepMark.DONE) Palette.Teal else if (now) Color.Transparent else Palette.Rule, CircleShape),
                contentAlignment = Alignment.Center,
            ) {
                Text(
                    when (mark) {
                        StepMark.DONE -> "✓"
                        StepMark.FAILED -> "!"
                        else -> "${step.ordinal + 1}"
                    },
                    style = Type.Glyph,
                    color = if (now) Palette.Background else if (mark == StepMark.DONE) Palette.Teal else Palette.Faint,
                )
            }
            if (!compact || now) {
                Spacer(Modifier.width(6.dp))
                Text(
                    stringResource(step.label), style = Type.Hint, maxLines = 1, softWrap = false,
                    color = if (now) Palette.Text else if (mark == StepMark.DONE) Palette.Muted else Palette.Faint,
                )
                if (now && percent != null) {
                    Spacer(Modifier.width(6.dp))
                    Text("· $percent", style = Type.Hint, color = Palette.Muted, maxLines = 1, softWrap = false)
                }
            }
        }
    }
}

/**
 * The step's figure: a big percentage when known, with [beside] at its right (the installer
 * window's thumbnail), the bar, one caption.
 */
@Composable
fun Figure(
    percent: Int?,
    caption: String?,
    color: Color = Palette.Teal,
    dim: Boolean = false,
    moving: Boolean = false,
    beside: (@Composable () -> Unit)? = null,
) {
    if (percent != null) {
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.Bottom) {
            Text(stringResource(R.string.percent, percent), style = Type.Figure, color = if (dim) Palette.Muted else Palette.Text, modifier = Modifier.weight(1f))
            beside?.invoke()
        }
        Spacer(Modifier.height(10.dp))
    }
    Bar(percent?.let { it / 100f } ?: if (moving) null else 0f, color = color)
    if (caption != null) {
        Spacer(Modifier.height(8.dp))
        Text(caption, style = Type.Hint, color = Palette.Muted)
    }
}
