package com.pd2launcher.app.ui

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.LocalIndication
import androidx.compose.foundation.background
import androidx.compose.foundation.Image
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.focusable
import androidx.compose.foundation.indication
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsFocusedAsState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalInputModeManager
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.disabled
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.rememberTextMeasurer
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import com.pd2launcher.app.Line
import com.pd2launcher.app.R
import com.pd2launcher.app.Tone
import kotlin.math.max

/** The radius family: 8 dp controls, 12 dp rows and panels, pills for buttons. */
val Small = RoundedCornerShape(8.dp)
val RowShape = RoundedCornerShape(12.dp)
internal val Pill = RoundedCornerShape(percent = 50)

/** A [Line] as text. */
@Composable
fun text(line: Line): String = stringResource(line.id, *line.args.toTypedArray())

fun toneColor(tone: Tone): Color = when (tone) {
    Tone.OK, Tone.WORKING, Tone.WAITING -> Palette.Teal
    Tone.WARNING -> Palette.Amber
    Tone.ERROR -> Palette.Red
    Tone.PENDING -> Palette.Muted
}

/**
 * Requests focus on [requester] once the screen shows and whenever [keys] change, and again when
 * the first d-pad or controller press leaves touch mode (Compose does not focus while in touch
 * mode, so a request made at launch is lost). Not attached on some first frames: then focus
 * stays where it was.
 */
@Composable
fun InitialFocus(requester: FocusRequester, vararg keys: Any?) {
    val mode = LocalInputModeManager.current.inputMode
    LaunchedEffect(mode, *keys) { runCatching { requester.requestFocus() } }
}

// ---------------------------------------------------------------- surface and type

@Composable
fun Rule(modifier: Modifier = Modifier, vertical: Boolean = false) {
    if (vertical) {
        Box(modifier.width(1.dp).background(Palette.Rule))
    } else {
        Box(modifier.fillMaxWidth().height(1.dp).background(Palette.Rule))
    }
}

/** The one card level: a Surface panel with a hairline edge. Never nested. */
@Composable
fun Panel(modifier: Modifier = Modifier, padding: Dp = 16.dp, alpha: Float = 1f, content: @Composable ColumnScope.() -> Unit) {
    Column(
        modifier.clip(RowShape).background(Palette.Surface.copy(alpha = alpha)).border(1.dp, Palette.Rule, RowShape).padding(padding),
        content = content,
    )
}

/**
 * The quiet texture behind Play: a dot lattice at 4 % and a low teal glow, so the surface reads
 * as the launcher's own rather than a flat Android background.
 */
@Composable
fun Lattice(modifier: Modifier = Modifier) {
    Canvas(modifier) {
        drawRect(Palette.Background)
        drawRect(
            Brush.radialGradient(
                listOf(Palette.Teal.copy(alpha = 0.14f), Color.Transparent),
                center = Offset(size.width / 2, size.height / 2), radius = max(size.width, size.height) * 0.7f,
            ),
        )
        val step = 18f * density
        var y = step / 2
        while (y < size.height) {
            var x = step / 2
            while (x < size.width) {
                drawCircle(Color.White.copy(alpha = 0.04f), 0.9f * density, Offset(x, y))
                x += step
            }
            y += step
        }
    }
}

@Composable
fun ScreenTitle(text: String, modifier: Modifier = Modifier, style: TextStyle = Type.Title) {
    Text(text, style = style, color = Palette.Text, modifier = modifier.semantics { heading() })
}

/** A teal overline that starts a group, [top] under what comes before it. */
@Composable
fun GroupLabel(text: String, modifier: Modifier = Modifier, top: Dp = 16.dp) {
    Text(text.uppercase(), style = Type.Label, color = Palette.Teal, modifier = modifier.padding(top = top, bottom = 6.dp).semantics { heading() })
}

/**
 * The ring-and-II mark and the name, as every launcher screen's header shows them. Where the
 * name does not fit beside the mark (a narrow header beside the step marks, a large font) only
 * the mark shows, so it never runs into what follows.
 */
@Composable
fun Wordmark(modifier: Modifier = Modifier) {
    val name = stringResource(R.string.brand_name)
    val measurer = rememberTextMeasurer()
    BoxWithConstraints(
        modifier.heightIn(min = 48.dp).semantics(mergeDescendants = true) { contentDescription = name },
        contentAlignment = Alignment.CenterStart,
    ) {
        val nameWidth = with(LocalDensity.current) {
            measurer.measure(name.uppercase(), Type.Brand, maxLines = 1, softWrap = false).size.width.toDp()
        }
        val fits = MARK_SIZE + MARK_GAP + nameWidth <= maxWidth
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(MARK_GAP)) {
            Image(painterResource(R.drawable.ic_brand_mark), contentDescription = null, modifier = Modifier.size(MARK_SIZE))
            if (fits) {
                Text(name.uppercase(), style = Type.Brand, color = Palette.Text, maxLines = 1, softWrap = false)
            }
        }
    }
}

private val MARK_SIZE = 40.dp
private val MARK_GAP = 10.dp

/** A status dot: the save state's, "Game running". */
@Composable
fun Dot(color: Color, modifier: Modifier = Modifier) {
    Box(modifier.size(DOT_SIZE).clip(CircleShape).background(color))
}

// ---------------------------------------------------------------- controls

/** [TONAL]: a tool beside the main action (the official PD2 launcher under Play), a filled pill. */
enum class Kind { PRIMARY, SECONDARY, DANGER, QUIET, TONAL }

/**
 * A pill at least 52 dp tall for thumbs, a ring where the d-pad is, a danger style for Stop.
 * A disabled button stays focusable so the controller can land on it and the page says why
 * beside the cause. Labels never wrap; one too long for the room left ends in an ellipsis.
 * [floating]: a corner pill over scrolling content, opaque, with a clearer edge when quiet and a
 * soft shadow when it is the enabled main action.
 */
@Composable
fun LauncherButton(
    label: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    kind: Kind = Kind.SECONDARY,
    enabled: Boolean = true,
    focusRequester: FocusRequester? = null,
    height: Dp = 52.dp,
    style: TextStyle = Type.Button,
    padding: Dp = 22.dp,
    floating: Boolean = false,
    /**
     * The app's attention dot ([Dot]: 8 dp, [Palette.Amber]; the owner, 2026-11-03: "we want the same
     * color dot everywhere in our app") at the label's top-right corner, as mobile games mark an icon
     * that has news (Play's Version while a new version is out), and these words for TalkBack after
     * the label. Null: none. Only drawn: the layout and the focus stay as they are.
     */
    dot: String? = null,
) {
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    val fill = when {
        kind == Kind.PRIMARY && enabled -> Palette.Teal
        kind == Kind.DANGER && focused && enabled -> Palette.Red
        kind == Kind.TONAL -> if (!enabled) Palette.Surface else if (focused) Palette.TonalFocus else Palette.Tonal
        focused -> Palette.Raised
        floating -> Palette.Background
        else -> Color.Transparent
    }
    val edge = when {
        focused && (kind == Kind.PRIMARY || kind == Kind.DANGER) && enabled -> Palette.Text
        focused -> Palette.Teal
        kind == Kind.PRIMARY && enabled -> Palette.Teal
        kind == Kind.DANGER && enabled -> Palette.Red.copy(alpha = 0.7f)
        kind == Kind.QUIET -> Color.Transparent
        kind == Kind.TONAL && enabled -> Palette.TonalEdge
        floating && enabled -> Palette.Edge
        else -> Palette.Rule
    }
    val ink = when {
        !enabled -> Palette.Faint
        kind == Kind.PRIMARY -> Palette.OnTeal
        kind == Kind.DANGER -> if (focused) Palette.Background else Palette.Red
        else -> Palette.Text
    }
    val action = if (enabled) {
        Modifier.clickable(interactionSource = interaction, indication = LocalIndication.current, role = Role.Button, onClick = onClick)
    } else {
        Modifier.semantics { disabled(); role = Role.Button }.focusable(interactionSource = interaction)
    }
    Box(
        modifier
            .focusOn(focusRequester)
            .heightIn(min = height)
            .widthIn(min = 48.dp)
            .then(if (floating && kind == Kind.PRIMARY && enabled) Modifier.shadow(6.dp, Pill) else Modifier)
            .border(if (focused) 2.dp else 1.dp, edge, Pill)
            .clip(Pill)
            .background(fill)
            .then(if (dot != null) Modifier.semantics { stateDescription = dot } else Modifier)
            .then(action)
            .padding(horizontal = padding),
        contentAlignment = Alignment.Center,
    ) {
        val mark = if (dot != null) Modifier.drawBehind { drawDot() } else Modifier
        Text(label, mark, style = style, color = ink, textAlign = TextAlign.Center, maxLines = 1, softWrap = false, overflow = TextOverflow.Ellipsis)
    }
}

/** [LauncherButton]'s dot, the same as [Dot] (size and colour): [DOT_SIZE] across, [DOT_GAP] right of the label, its top on the label's top. */
private fun DrawScope.drawDot() {
    val radius = DOT_SIZE.toPx() / 2
    drawCircle(Palette.Amber, radius, Offset(size.width + DOT_GAP.toPx() + radius, radius))
}

/** [Dot]'s size. */
private val DOT_SIZE = 8.dp
private val DOT_GAP = 2.dp

/** [requester] on this element, when there is one. */
internal fun Modifier.focusOn(requester: FocusRequester?): Modifier = if (requester != null) focusRequester(requester) else this

/** The ring and fill every focusable row shares. */
@Composable
internal fun Modifier.focusMark(focused: Boolean, selected: Boolean = false, shape: RoundedCornerShape = RowShape): Modifier =
    clip(shape)
        .background(if (selected) Palette.Teal.copy(alpha = 0.16f) else if (focused) Palette.Raised else Color.Transparent)
        .border(if (focused) 2.dp else 0.dp, if (focused) Palette.Teal else Color.Transparent, shape)

/** A quiet text action with a 48 dp target and the same focus ring. */
@Composable
fun TextLink(label: String, onClick: () -> Unit, modifier: Modifier = Modifier) {
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    Box(
        modifier.heightIn(min = 48.dp).focusMark(focused, shape = Small)
            .clickable(interactionSource = interaction, indication = LocalIndication.current, role = Role.Button, onClick = onClick)
            .padding(horizontal = 10.dp),
        contentAlignment = Alignment.CenterStart,
    ) {
        Text(label, style = Type.Body, color = Palette.Teal)
    }
}

/** A focusable full-width row: destinations, bindings, list choices. */
@Composable
fun FocusRow(
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    focusRequester: FocusRequester? = null,
    minHeight: Dp = 56.dp,
    content: @Composable RowScope.() -> Unit,
) {
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    Row(
        modifier
            .focusOn(focusRequester)
            .fillMaxWidth().heightIn(min = minHeight).focusMark(focused)
            .clickable(interactionSource = interaction, indication = LocalIndication.current, onClick = onClick)
            .padding(horizontal = 14.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(14.dp),
        content = content,
    )
}

/** The launcher's toggle: a slim round pill, an outline when off, teal when on. */
@Composable
fun Toggle(checked: Boolean, modifier: Modifier = Modifier) {
    Canvas(modifier.size(width = 36.dp, height = 20.dp)) {
        val r = CornerRadius(size.height / 2)
        if (checked) {
            drawRoundRect(Palette.Teal, cornerRadius = r)
        } else {
            val line = 1.5f * density
            drawRoundRect(
                Palette.Edge, Offset(line / 2, line / 2), Size(size.width - line, size.height - line),
                CornerRadius(r.x - line / 2), style = Stroke(line),
            )
        }
        val k = 6f * density
        val inset = 4f * density
        val x = if (checked) size.width - inset - k else inset + k
        drawCircle(if (checked) Palette.OnTeal else Palette.Muted, k, Offset(x, size.height / 2))
    }
}

/**
 * A row with a title, a supporting line and the toggle; the whole row toggles. Disabled rows
 * stay focusable so the controller can reach them and read why. [prominent]: the row stands apart
 * on a raised surface, its title in bold.
 */
@Composable
fun ToggleRow(
    title: String,
    supporting: String,
    checked: Boolean,
    onChange: (Boolean) -> Unit,
    enabled: Boolean = true,
    focusRequester: FocusRequester? = null,
    prominent: Boolean = false,
) {
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    Row(
        Modifier
            .then(
                if (prominent) {
                    // An outer border draws over the ring of focusMark, so it goes while the ring shows.
                    Modifier.padding(top = 6.dp, bottom = 4.dp).clip(RowShape).background(Palette.Surface)
                        .border(1.dp, if (focused) Color.Transparent else Palette.Rule, RowShape)
                } else {
                    Modifier
                },
            )
            .focusOn(focusRequester)
            .fillMaxWidth().heightIn(min = 60.dp).focusMark(focused)
            .then(
                if (enabled) {
                    Modifier.toggleable(checked, interaction, LocalIndication.current, role = Role.Switch, onValueChange = onChange)
                } else {
                    Modifier.semantics { disabled() }.focusable(interactionSource = interaction)
                },
            )
            .padding(horizontal = 14.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(
                title, style = Type.Body.copy(fontWeight = if (prominent) FontWeight.SemiBold else null),
                color = if (enabled) Palette.Text else Palette.Faint,
            )
            if (supporting.isNotEmpty()) Text(supporting, style = Type.Hint, color = if (enabled) Palette.Muted else Palette.Faint)
        }
        Toggle(checked)
    }
}

/** A radio button's mark: a teal ring and dot. [ring]: the ring when it is not teal (the GPU page's is clearer). */
@Composable
fun RadioMark(selected: Boolean, modifier: Modifier = Modifier, enabled: Boolean = true, ring: Color = Palette.Rule) {
    Box(
        modifier.size(20.dp).clip(CircleShape).border(1.5.dp, if (selected && enabled) Palette.Teal else ring, CircleShape),
        contentAlignment = Alignment.Center,
    ) {
        if (selected) Box(Modifier.size(10.dp).clip(CircleShape).background(if (enabled) Palette.Teal else Palette.Faint))
    }
}

/**
 * One of a single choice, with its explanation: a teal ring and dot; the whole row selects. A grey
 * row ([enabled] false) stays focusable so the controller can reach it, unless [focusable] is false.
 * [tinted]: the selected row also gets the teal tint, and the rows are tighter, so three fit a
 * handheld's screen at a large font (the Diablo II source page).
 */
@Composable
fun OptionRow(
    title: String,
    body: String?,
    selected: Boolean,
    onSelect: () -> Unit,
    enabled: Boolean = true,
    focusRequester: FocusRequester? = null,
    tinted: Boolean = false,
    focusable: Boolean = true,
    modifier: Modifier = Modifier,
) {
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    Row(
        modifier
            .focusOn(focusRequester)
            .fillMaxWidth().heightIn(min = if (tinted) 52.dp else 60.dp).focusMark(focused, selected = tinted && selected)
            .then(
                when {
                    enabled -> Modifier.selectable(selected, interaction, LocalIndication.current, role = Role.RadioButton, onClick = onSelect)
                    // A grey row that takes no focus either, as Loot filters ([focusable] false).
                    !focusable -> Modifier.semantics { disabled() }
                    else -> Modifier.semantics { disabled() }.focusable(interactionSource = interaction)
                },
            )
            .padding(horizontal = 14.dp, vertical = if (tinted) 3.dp else 10.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        RadioMark(selected, enabled = enabled)
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(if (tinted) 0.dp else 2.dp)) {
            Text(title, style = Type.Body, color = if (enabled) Palette.Text else Palette.Faint)
            if (body != null) Text(body, style = Type.Hint, color = if (enabled) Palette.Muted else Palette.Faint)
        }
    }
}

/**
 * A slim button of a row: 30 dp to see and 48 dp to touch, so the row stays lean. A disabled one
 * stays focusable, as [LauncherButton], unless its [modifier] says `canFocus = false` (Check now
 * during a check, Manage): that node's change loses the focus it holds, so the caller hands it on.
 */
@Composable
fun SlimButton(
    label: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
    focusRequester: FocusRequester? = null,
) {
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    val action = if (enabled) {
        Modifier.clickable(interactionSource = interaction, indication = null, role = Role.Button, onClick = onClick)
    } else {
        Modifier.semantics { disabled(); role = Role.Button }.focusable(interactionSource = interaction)
    }
    Box(modifier.focusOn(focusRequester).heightIn(min = 48.dp).widthIn(min = 48.dp).then(action), contentAlignment = Alignment.Center) {
        Box(
            Modifier.height(30.dp).border(if (focused) 2.dp else 1.dp, if (focused) Palette.Teal else if (enabled) Palette.Edge else Palette.Rule, Pill)
                .clip(Pill).background(if (focused) Palette.Raised else Color.Transparent)
                .indication(interaction, LocalIndication.current).padding(horizontal = 14.dp),
            contentAlignment = Alignment.Center,
        ) {
            Text(
                label, style = Type.Hint.copy(fontWeight = FontWeight.SemiBold), color = if (enabled) Palette.Text else Palette.Faint,
                maxLines = 1, softWrap = false, overflow = TextOverflow.Ellipsis,
            )
        }
    }
}

/**
 * A slim switch between [options]: one line in a pill, 34 dp to see and 48 dp to touch, the
 * chosen option filled. Each option is its own focus stop; [selectedFocus] is the chosen one's, so
 * a row above can send the d-pad there.
 */
@Composable
fun SlimSwitch(options: List<String>, selected: Int, onSelect: (Int) -> Unit, modifier: Modifier = Modifier, selectedFocus: FocusRequester? = null) {
    Row(
        modifier.height(48.dp).drawBehind {
            val high = SLIM_SWITCH_HIGH.toPx()
            val line = 1.dp.toPx()
            drawRoundRect(
                Palette.Edge, Offset(line / 2, (size.height - high + line) / 2), Size(size.width - line, high - line),
                CornerRadius(high / 2), style = Stroke(line),
            )
        }.padding(horizontal = 3.dp),
        horizontalArrangement = Arrangement.spacedBy(2.dp),
    ) {
        options.forEachIndexed { i, label ->
            val interaction = remember { MutableInteractionSource() }
            val focused by interaction.collectIsFocusedAsState()
            val on = i == selected
            Box(
                Modifier.fillMaxHeight()
                    .then(if (on && selectedFocus != null) Modifier.focusRequester(selectedFocus) else Modifier)
                    .selectable(on, interaction, null, role = Role.RadioButton) { onSelect(i) },
                contentAlignment = Alignment.Center,
            ) {
                Box(
                    Modifier.height(28.dp).clip(Pill).background(if (on || focused) Palette.Raised else Color.Transparent)
                        .border(if (focused) 2.dp else 0.dp, if (focused) Palette.Teal else Color.Transparent, Pill)
                        .indication(interaction, LocalIndication.current).padding(horizontal = 12.dp),
                    contentAlignment = Alignment.Center,
                ) {
                    Text(
                        label, style = Type.Hint.copy(fontWeight = if (on) FontWeight.SemiBold else FontWeight.Medium),
                        color = if (on) Palette.Teal else Palette.Muted, maxLines = 1, softWrap = false,
                    )
                }
            }
        }
    }
}

private val SLIM_SWITCH_HIGH = 34.dp

/**
 * A segmented choice: the options in one hairline slot, the chosen one teal-tinted. Each option
 * is its own focus stop, at least 48 dp high. [compact]: the control of a row, beside the row's
 * label (Touch): each option only as wide as its word, at least 48 dp.
 */
@Composable
fun Segmented(options: List<String>, selected: Int, onSelect: (Int) -> Unit, modifier: Modifier = Modifier, compact: Boolean = false) {
    val gap = if (compact) 2.dp else 3.dp
    Row(
        modifier.clip(RowShape).border(1.dp, Palette.Rule, RowShape).padding(gap),
        horizontalArrangement = Arrangement.spacedBy(gap),
    ) {
        options.forEachIndexed { i, label ->
            val interaction = remember { MutableInteractionSource() }
            val focused by interaction.collectIsFocusedAsState()
            val on = i == selected
            Box(
                (if (compact) Modifier.widthIn(min = 48.dp) else Modifier.weight(1f)).heightIn(min = 48.dp)
                    .focusMark(focused, selected = on, shape = Small)
                    .selectable(on, interaction, LocalIndication.current, role = Role.RadioButton) { onSelect(i) }
                    .padding(horizontal = 8.dp),
                contentAlignment = Alignment.Center,
            ) {
                Text(
                    label, style = Type.Hint.copy(fontWeight = if (on) FontWeight.SemiBold else null),
                    color = if (on) Palette.Teal else Palette.Muted, textAlign = TextAlign.Center, maxLines = 1, overflow = TextOverflow.Ellipsis,
                )
            }
        }
    }
}

/** A rounded bar; [progress] null moves indeterminately. */
@Composable
fun Bar(progress: Float?, modifier: Modifier = Modifier, height: Dp = 6.dp, color: Color = Palette.Teal) {
    if (progress == null) {
        LinearProgressIndicator(modifier.fillMaxWidth().height(height).clip(Pill), color = color, trackColor = Palette.Rule, gapSize = 0.dp)
        return
    }
    Canvas(modifier.fillMaxWidth().height(height)) {
        val r = CornerRadius(size.height / 2)
        drawRoundRect(Palette.Rule, cornerRadius = r)
        val w = size.width * progress.coerceIn(0f, 1f)
        if (w > 0f) drawRoundRect(color, size = Size(w, size.height), cornerRadius = r)
    }
}

/**
 * A state's small mark, with the state in words for TalkBack: a dot of 8 dp (the size of Settings'
 * save-state [Dot]) in teal (OK), amber (a warning) or red (an error), a small spinner (10 dp)
 * while work runs, a teal ring with a dot when it waits for the player ([Tone.WAITING]), or a faint
 * empty ring not reached yet ([dim]). The owner, 2026-11-03: "we don't have to use such
 * huge yellow warning icons" ("yes all of it"): no large mark anywhere.
 */
@Composable
fun StatusMark(tone: Tone, modifier: Modifier = Modifier, dim: Boolean = false) {
    val words = stringResource(
        when (tone) {
            Tone.OK -> R.string.status_ok
            Tone.WARNING -> R.string.status_warning
            Tone.ERROR -> R.string.status_error
            Tone.PENDING, Tone.WORKING -> R.string.status_pending
            Tone.WAITING -> R.string.status_waiting
        },
    )
    Box(modifier.size(10.dp).semantics { contentDescription = words }, contentAlignment = Alignment.Center) {
        when {
            tone == Tone.WORKING || (tone == Tone.PENDING && !dim) ->
                CircularProgressIndicator(Modifier.size(10.dp), strokeWidth = 1.5.dp, color = Palette.Teal, trackColor = Palette.Rule)
            tone == Tone.PENDING -> Box(Modifier.size(8.dp).clip(CircleShape).border(1.dp, Palette.Faint, CircleShape))
            // Waiting for the player: a teal ring with a small teal dot, not the filled dot (done) or the empty ring (ahead).
            tone == Tone.WAITING -> Box(Modifier.size(8.dp).clip(CircleShape).border(1.dp, Palette.Teal, CircleShape), contentAlignment = Alignment.Center) {
                Box(Modifier.size(4.dp).clip(CircleShape).background(Palette.Teal))
            }
            tone == Tone.OK -> Dot(Palette.Teal)
            tone == Tone.WARNING -> Dot(Palette.Amber)
            else -> Dot(Palette.Red)
        }
    }
}

/**
 * One line of evidence: a status mark, a title, detail lines, an optional value or [action] at the end.
 * Problem details stay in the text colour; rows not reached yet are dimmed. A line without an
 * action is still a d-pad stop, so a controller can scroll a long list to every row.
 */
@Composable
fun StatusLine(
    tone: Tone,
    title: String,
    details: List<String>,
    value: String? = null,
    dim: Boolean = false,
    action: (@Composable () -> Unit)? = null,
) {
    Row(
        Modifier.fillMaxWidth().then(if (action == null) Modifier.readingStop() else Modifier)
            .heightIn(min = 56.dp).padding(horizontal = 4.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        StatusMark(tone, dim = dim)
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(title, style = Type.Body, color = if (dim) Palette.Faint else Palette.Text)
            val detailColor = when {
                tone == Tone.ERROR || tone == Tone.WARNING -> Palette.Text
                dim -> Palette.Faint
                else -> Palette.Muted
            }
            details.forEach { Text(it, style = Type.Hint, color = detailColor) }
        }
        if (value != null) Text(value, style = Type.Hint, color = if (dim) Palette.Faint else Palette.Muted)
        action?.invoke()
    }
}

/**
 * A focus stop that does nothing when pressed: the d-pad lands on it so the scroll brings it into
 * view, with the same ring as every focusable row.
 */
@Composable
internal fun Modifier.readingStop(): Modifier {
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    return focusMark(focused).focusable(interactionSource = interaction)
}

/**
 * A quiet note for warnings and errors: a coloured edge, the line, an optional action. Without
 * an action it is a d-pad stop of its own (the action is one otherwise).
 */
@Composable
fun Banner(text: String, color: Color, modifier: Modifier = Modifier, action: (@Composable () -> Unit)? = null) {
    Row(
        modifier.fillMaxWidth().then(if (action == null) Modifier.readingStop() else Modifier).clip(RowShape).background(Palette.Raised)
            .drawBehind { drawRect(color, size = Size(3f * density, size.height)) }
            .padding(start = 14.dp, end = 6.dp, top = 8.dp, bottom = 8.dp).heightIn(min = 40.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Text(text, style = Type.Hint, color = Palette.Text, modifier = Modifier.weight(1f))
        action?.invoke()
    }
}

/** A key cap: a controller button or a keyboard key. */
@Composable
fun KeyCap(text: String, modifier: Modifier = Modifier, selected: Boolean = false) {
    Box(
        modifier.widthIn(min = 40.dp).height(30.dp).clip(Small)
            .background(if (selected) Palette.Teal else Palette.Raised)
            .border(1.dp, if (selected) Palette.Teal else Palette.Rule, Small).padding(horizontal = 8.dp),
        contentAlignment = Alignment.Center,
    ) { Text(text, style = Type.Glyph, color = if (selected) Palette.OnTeal else Palette.Text, maxLines = 1) }
}
