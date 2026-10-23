package com.pd2launcher.app

import android.graphics.Rect as ScreenRect
import android.util.Log
import android.view.View
import android.view.ViewGroup
import android.view.inspector.WindowInspector
import android.widget.EditText
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.platform.ViewRootForTest
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.semantics.SemanticsNode
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.semantics.getOrNull
import androidx.compose.ui.text.TextLayoutResult
import androidx.compose.ui.unit.sp
import kotlin.math.ceil
import kotlin.math.max
import kotlin.math.min

/**
 * Debug builds only, for [OverlayPreviewActivity]: measures which texts of the overlay are cut
 * off, from the Compose semantics of every window of this process (the activity's and its
 * dialogs'), with no change to the composables. Main thread only.
 *
 * One `kind=TEXT` line per text, with its verdicts:
 * - ELLIPSIS: a line ends in "…" (its layout was ellipsized); `need` is the width in px the text
 *   takes on one line, `have` the width it got.
 * - BROKEN: a word is broken over two lines (its box is narrower than the word).
 * - OVERFLOW: with no ellipsis, one line is more than 1 px wider than its box, its lines reach
 *   more than 1 px below the box, or it has more lines than it may show (cut where clipped).
 * - CLIPPED / HIDDEN: a parent clips part / all of it (a panel's edge, or a scroll that hides it).
 * - OFFSCREEN: part of it lies outside the screen.
 * - COVERED: a labelled control drawn later (above it) overlaps it; such a control may be
 *   translucent, so this is "drawn under", not "unreadable". Controls that fill most of the
 *   screen (scrims, tap catchers) do not count.
 * - OK: none of these.
 * One `kind=SCROLL` line per scrolling container that has more content than it shows, one
 * `kind=SUMMARY` line, and `kind=READY` last.
 */
internal object OverlayProbe {
    const val TAG = "pd2-overlay-eval"

    /** The Compose roots of every window this process shows, the activity's first. */
    private fun roots(): List<ViewRootForTest> {
        val found = mutableListOf<ViewRootForTest>()
        fun visit(view: View) {
            if (view is ViewRootForTest) found += view
            if (view is ViewGroup) for (i in 0 until view.childCount) visit(view.getChildAt(i))
        }
        WindowInspector.getGlobalWindowViews().forEach(::visit)
        return found
    }

    /**
     * [node] and every node under it, in drawing order (a node before the ones over it); not what
     * only takes its room unseen (the menu's hidden tab, the settings under the Stop question).
     */
    private fun preorder(node: SemanticsNode, into: MutableList<SemanticsNode> = mutableListOf()): List<SemanticsNode> {
        if (node.config.getOrNull(SemanticsProperties.TestTag) == "unseen") return into
        into += node
        node.children.forEach { preorder(it, into) }
        return into
    }

    private fun texts(node: SemanticsNode): List<String> =
        (node.config.getOrNull(SemanticsProperties.Text).orEmpty() +
            listOfNotNull(node.config.getOrNull(SemanticsProperties.EditableText))).map { it.text }

    private fun descriptions(node: SemanticsNode): List<String> = node.config.getOrNull(SemanticsProperties.ContentDescription).orEmpty()

    /** What a control says: its texts and descriptions and those under it, shortened. */
    private fun label(node: SemanticsNode): String =
        preorder(node).flatMap { texts(it) + descriptions(it) }.joinToString(" ").take(40)

    /** Clicks the first node with the text or description [label] (or its clickable parent), as a tap would. */
    fun click(label: String): Boolean {
        for (root in roots()) {
            val hit = preorder(root.semanticsOwner.unmergedRootSemanticsNode)
                .firstOrNull { label in texts(it) || label in descriptions(it) } ?: continue
            var node: SemanticsNode? = hit
            while (node != null && node.config.getOrNull(SemanticsActions.OnClick) == null) node = node.parent
            val action = node?.config?.getOrNull(SemanticsActions.OnClick)?.action ?: continue
            action()
            return true
        }
        return false
    }

    /** The first Android text field in this process's windows (the Frame rate input's). */
    fun firstEditText(): EditText? {
        fun find(view: View): EditText? = when (view) {
            is EditText -> view
            is ViewGroup -> (0 until view.childCount).firstNotNullOfOrNull { find(view.getChildAt(it)) }
            else -> null
        }
        return WindowInspector.getGlobalWindowViews().firstNotNullOfOrNull(::find)
    }

    private fun Rect.area(): Float = max(0f, width) * max(0f, height)

    private fun Rect.text(): String = "${left.toInt()},${top.toInt()},${width.toInt()}x${height.toInt()}"

    private fun quote(text: String): String = "\"" + text.replace("\n", "⏎").replace("\"", "'") + "\""

    /** Logs every text of every window, with its verdicts, against the [screen] (px), then READY. */
    fun report(prefix: String, screen: ScreenRect) {
        val display = Rect(screen.left.toFloat(), screen.top.toFloat(), screen.right.toFloat(), screen.bottom.toFloat())
        val counts = linkedMapOf("OK" to 0, "ELLIPSIS" to 0, "OVERFLOW" to 0, "BROKEN" to 0, "CLIPPED" to 0, "HIDDEN" to 0, "OFFSCREEN" to 0, "COVERED" to 0)
        var total = 0
        roots().forEachIndexed { index, root ->
            val view = root.view
            val at = IntArray(2).also(view::getLocationOnScreen)
            val dx = at[0].toFloat()
            val dy = at[1].toFloat()
            val density = root.density
            Log.i(
                TAG,
                "$prefix kind=ROOT root=$index size=${view.width}x${view.height}px at=${at[0]},${at[1]} screen=${screen.width()}x${screen.height()}px " +
                    "density=${density.density} fontScale=${density.fontScale} sp14=${with(density) { 14.sp.toPx() }}px",
            )
            val nodes = preorder(root.semanticsOwner.unmergedRootSemanticsNode)
            val rootArea = view.width.toFloat() * view.height.toFloat()
            // Labelled controls, for COVERED: drawn after a text and over it.
            val controls = nodes.withIndex().filter { (_, node) ->
                node.config.getOrNull(SemanticsActions.OnClick) != null && node.boundsInRoot.area() < rootArea * 0.9f && label(node).isNotEmpty()
            }
            nodes.forEachIndexed { order, node ->
                // The menu's, the Controls screen's and the number inputs' panels: their size in dp.
                node.config.getOrNull(SemanticsProperties.TestTag)?.let { tag ->
                    val box = node.boundsInRoot.translate(dx, dy)
                    Log.i(
                        TAG,
                        "$prefix kind=PANEL root=$index tag=$tag box=${box.text()} " +
                            "dp=${(box.width / density.density).toInt()}x${(box.height / density.density).toInt()}",
                    )
                }
                node.config.getOrNull(SemanticsProperties.VerticalScrollAxisRange)?.let { range ->
                    val most = range.maxValue()
                    if (most > 0f) {
                        Log.i(
                            TAG,
                            "$prefix kind=SCROLL root=$index id=${node.id} box=${node.boundsInRoot.translate(dx, dy).text()} " +
                                "scrolled=${range.value().toInt()} more=${most.toInt()}px first=${quote(label(node))}",
                        )
                    }
                }
                val words = texts(node)
                if (words.isEmpty()) return@forEachIndexed
                total++
                val whole = Rect(node.positionInRoot, Size(node.size.width.toFloat(), node.size.height.toFloat())).translate(dx, dy)
                val shown = node.boundsInRoot.translate(dx, dy)
                val visible = if (whole.area() > 0f) shown.area() / whole.area() else 0f
                val layouts = mutableListOf<TextLayoutResult>()
                node.config.getOrNull(SemanticsActions.GetTextLayoutResult)?.action?.invoke(layouts)
                val layout = layouts.firstOrNull()
                val verdicts = mutableListOf<String>()
                val ellipsized = layout != null && (0 until layout.lineCount).any { layout.isLineEllipsized(it) }
                if (ellipsized) verdicts += "ELLIPSIS"
                // TextLayoutResult.hasVisualOverflow reports a text that exactly fits its box (through
                // semantics its paragraph is laid out again, as wide as the constraints): measured
                // here, with a pixel to spare: one line wider than the box, lines below its foot, or
                // lines left out with no ellipsis.
                val paragraph = layout?.multiParagraph
                val oneLine = paragraph?.intrinsics?.maxIntrinsicWidth ?: 0f
                val overflows = layout != null && paragraph != null && !ellipsized && (
                    (layout.lineCount == 1 && oneLine > layout.size.width + 1f) ||
                        layout.getLineBottom(layout.lineCount - 1) > layout.size.height + 1f ||
                        paragraph.didExceedMaxLines
                    )
                if (overflows) verdicts += "OVERFLOW"
                // A word broken over two lines: the box is narrower than the word.
                val text = layout?.layoutInput?.text?.text.orEmpty()
                val broken = layout != null && (0 until layout.lineCount - 1).any { line ->
                    val end = layout.getLineEnd(line)
                    end in 1 until text.length && text[end - 1].isLetterOrDigit() && text[end].isLetterOrDigit()
                }
                if (broken) verdicts += "BROKEN"
                if (visible <= 0.001f) verdicts += "HIDDEN" else if (visible < 0.98f) verdicts += "CLIPPED"
                if (whole.left < display.left - 0.5f || whole.top < display.top - 0.5f ||
                    whole.right > display.right + 0.5f || whole.bottom > display.bottom + 0.5f
                ) verdicts += "OFFSCREEN"
                var cover = ""
                if (visible > 0.001f) {
                    val over = controls.filter { (i, _) -> i > order }
                        .map { (_, control) -> control to control.boundsInRoot.translate(dx, dy).intersect(shown).area() / shown.area() }
                        .filter { it.second > 0.05f }
                        .maxByOrNull { it.second }
                    if (over != null) {
                        verdicts += "COVERED"
                        cover = " coveredBy=${quote(label(over.first))} coveredShare=${"%.2f".format(over.second)}"
                    }
                }
                if (verdicts.isEmpty()) verdicts += "OK"
                verdicts.forEach { counts[it] = (counts[it] ?: 0) + 1 }
                val need = layout?.multiParagraph?.intrinsics?.maxIntrinsicWidth?.let { ceil(it).toInt() }
                val have = layout?.size?.width
                val inScroll = generateSequence(node.parent) { it.parent }
                    .any { it.config.getOrNull(SemanticsProperties.VerticalScrollAxisRange) != null }
                Log.i(
                    TAG,
                    "$prefix kind=TEXT root=$index id=${node.id} verdict=${verdicts.joinToString("+")} text=${quote(words.joinToString(" "))} " +
                        "box=${whole.text()} visible=${"%.2f".format(min(1f, visible))} lines=${layout?.lineCount ?: "-"} " +
                        "need=${need ?: "-"} have=${have ?: "-"} " +
                        "textHeight=${layout?.let { ceil(it.getLineBottom(it.lineCount - 1)).toInt() } ?: "-"} boxHeight=${layout?.size?.height ?: "-"} " +
                        "inScroll=$inScroll$cover",
                )
            }
        }
        Log.i(TAG, "$prefix kind=SUMMARY texts=$total " + counts.entries.joinToString(" ") { "${it.key.lowercase()}=${it.value}" })
        Log.i(TAG, "$prefix kind=READY")
    }
}
