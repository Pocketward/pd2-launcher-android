package com.pd2launcher.app.ui

import androidx.annotation.StringRes
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.layout.Layout
import androidx.compose.ui.layout.layoutId
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.pd2launcher.app.Line
import com.pd2launcher.app.Onboarding
import com.pd2launcher.app.R
import com.pd2launcher.app.SessionStatus
import com.pd2launcher.app.control.Command
import com.pd2launcher.app.control.Game
import com.pd2launcher.app.control.Host
import com.pd2launcher.app.control.Installation
import com.pd2launcher.app.control.Job
import com.pd2launcher.app.control.OperationStatus
import com.pd2launcher.app.control.Phase
import com.pd2launcher.app.control.Renderer
import com.pd2launcher.app.control.Snapshot
import com.pd2launcher.app.control.Update

/** Play: what the app shows once installed. */
object Play {
    enum class Mode {
        /** The Play screen: the installation is Ready and no Game client runs. */
        PLAY,
        /** The Game view, full screen: the Game client or the official PD2 launcher runs. */
        GAME,
    }

    /** The Play screen or the Game view from the engine's snapshot; null before Ready. */
    fun mode(status: SessionStatus): Mode? {
        val snapshot = status.snapshot ?: return null
        return when {
            snapshot.game == Game.RUNNING || snapshot.activeJob == Job.OFFICIAL_LAUNCHER -> Mode.GAME
            snapshot.installation == Installation.READY -> Mode.PLAY
            else -> null
        }
    }

    /** The official PD2 launcher can open: the game is installed and nothing runs (no install, game or check). */
    fun officialLauncherEnabled(status: SessionStatus): Boolean {
        val snapshot = status.snapshot ?: return false
        return status.engine == SessionStatus.Engine.CONNECTED && snapshot.installation == Installation.READY &&
            snapshot.activeJob == null && snapshot.game != Game.RUNNING
    }

    /** The official PD2 launcher session was refused or ended without opening it; null: nothing to say. */
    fun officialLauncherProblem(status: SessionStatus): Int? {
        if (status.refusal?.reason == "official-launcher-not-permitted") return R.string.refusal_official_launcher
        val operation = status.snapshot?.operation?.takeIf { it.job == Job.OFFICIAL_LAUNCHER } ?: return null
        return R.string.play_official_launcher_failed.takeIf {
            operation.status == OperationStatus.FAILED || operation.status == OperationStatus.INDETERMINATE
        }
    }

    /** The Game view shows the official PD2 launcher (a mouse/touch window), not the game. */
    fun officialLauncherRunning(status: SessionStatus): Boolean = status.snapshot?.activeJob == Job.OFFICIAL_LAUNCHER

    /** The overlay's status line: what runs. */
    fun overlayStatus(launcher: Boolean): Int = if (launcher) R.string.launcher_running else R.string.game_running

    /** The overlay's Stop button label. */
    fun overlayStopLabel(launcher: Boolean): Int = if (launcher) R.string.launcher_view_stop else R.string.game_view_stop

    /** The overlay's answer that keeps what runs. */
    fun overlayKeepLabel(launcher: Boolean): Int = if (launcher) R.string.launcher_menu_keep_open else R.string.game_menu_keep_playing

    /** The overlay's question before stopping: the game warns about unsaved progress, the launcher does not. */
    fun overlayStopBody(launcher: Boolean): Int = if (launcher) R.string.launcher_menu_stop_body else R.string.game_menu_stop_body

    /** The official PD2 launcher session was running and is not any more. */
    fun officialLauncherEnded(previous: Job?, next: Job?): Boolean =
        previous == Job.OFFICIAL_LAUNCHER && next != Job.OFFICIAL_LAUNCHER

    /** The Game client was running and is not any more (or the engine went away). */
    fun gameEnded(previous: Game?, next: Game?): Boolean = previous == Game.RUNNING && next != Game.RUNNING

    /** The Game client was not running and is now. */
    fun gameStarted(previous: Game?, next: Game?): Boolean = previous != Game.RUNNING && next == Game.RUNNING

    /**
     * How long a press of Play or the official PD2 launcher waits for the engine to show the game
     * or its session: the game environment starts, then the engine audits and launches.
     */
    const val LAUNCH_PENDING_MS = 15_000L

    /**
     * The session service takes no Play or official PD2 launcher now: the Game client runs, a job
     * runs (the official PD2 launcher's session is one), or a launch sent at [sentAt] has not
     * shown yet. The game environment is then never stopped or started again at another size
     * under what runs in it.
     */
    fun launchBusy(snapshot: Snapshot?, sentAt: Long?, now: Long): Boolean =
        snapshot?.game == Game.RUNNING || snapshot?.activeJob != null || (sentAt != null && now - sentAt < LAUNCH_PENDING_MS)

    /** A press of Play or the official PD2 launcher at [at], and the game environment's failure shown then. */
    data class LaunchPress(val at: Long, val environmentFailure: String?)

    /**
     * Play's two buttons stay disabled from [press] until the engine shows the game or a job
     * running, refuses the command, or the game environment fails to start (a failure other than
     * the one shown at the press); at most [LAUNCH_PENDING_MS].
     */
    fun launchPending(press: LaunchPress?, status: SessionStatus, now: Long): Boolean {
        if (press == null || now - press.at >= LAUNCH_PENDING_MS) return false
        val snapshot = status.snapshot ?: return false
        val failure = status.gameEnvironmentFailure
        return snapshot.game != Game.RUNNING && snapshot.activeJob == null && status.refusal == null &&
            (failure == null || failure == press.environmentFailure)
    }

    /** Where Project Diablo 2's Available update stands on Play. */
    enum class UpdateState {
        /** Nothing to offer: up to date, not checked yet, checking, or the check failed. Play stays Play. */
        NONE,
        /** An Available update: the main button reads Update. */
        OFFERED,
        /** The update runs: its figure above the main button, which reads Stop. */
        RUNNING,
        /** Stop was pressed: the update stops at its next safe point. */
        STOPPING,
        /** The update was stopped: the main button reads Resume. */
        STOPPED,
        /** The update failed or the Launcher refused it: a red banner, and the main button reads Try again. */
        FAILED,
        /**
         * The update finished and the engine checks again ([Recheck]): "Checking for updates…" where
         * "Ready to play" goes, and the main button (Play) disabled until the check says.
         */
        RECHECKING,
    }

    /** Where the engine's check after a finished update stands ([recheck]). */
    enum class Recheck {
        /** The update finished; the check after it has not shown yet (the update still says Available). */
        WAITING,
        /** The check after it runs. */
        CHECKING,
    }

    /**
     * The check after a finished update, from [current] (where it stood after [previous]) and the
     * engine's [next] snapshot: it begins when an update's success lands, runs while the check
     * says Checking, and ends once the check says anything else (Up to date, or Available again
     * after it ran: a newer update), or another operation replaces the update's. Until then the
     * snapshot's Available is the one from before the update, which Play must not offer again.
     */
    fun recheck(current: Recheck?, previous: Snapshot?, next: Snapshot): Recheck? {
        val succeeded = next.operation?.let { it.job == Job.UPDATE && it.status == OperationStatus.SUCCEEDED } == true
        if (!succeeded) return null
        val stands = if (previous?.operation != next.operation) Recheck.WAITING else current
        return when {
            stands == null -> null
            next.update == Update.CHECKING -> Recheck.CHECKING
            stands == Recheck.WAITING && next.update == Update.AVAILABLE -> Recheck.WAITING
            else -> null
        }
    }

    /**
     * The update on Play. [canStart]: the engine takes Update (Resume, Try again) now: no other job
     * runs. While it runs, [percent] of the file downloading now and [summary] (its amount, speed
     * and time left) are the engine's, each when known; [step] says what runs when there is no
     * summary. [problem]: why it failed.
     */
    data class UpdateView(
        val state: UpdateState = UpdateState.NONE,
        val canStart: Boolean = false,
        val percent: Int? = null,
        val summary: String? = null,
        @param:StringRes val step: Int? = null,
        @param:StringRes val problem: Int? = null,
    ) {
        /** The update's job runs (or stops): its figure shows and Play waits. */
        val working: Boolean get() = state == UpdateState.RUNNING || state == UpdateState.STOPPING

        /** Play stays possible beside the update: [PlayScreen]'s "Play without updating". */
        val playWithout: Boolean get() = state == UpdateState.OFFERED || state == UpdateState.STOPPED || state == UpdateState.FAILED
    }

    private val UPDATE_RUNNING = setOf(
        OperationStatus.QUEUED, OperationStatus.PLANNING, OperationStatus.RUNNING,
        OperationStatus.VERIFYING, OperationStatus.COMMITTING, OperationStatus.SAFE_STOPPING,
    )

    /**
     * The update on Play from the engine's audited snapshot. Offered only for a Ready installation
     * while neither the game nor the official PD2 launcher runs, and only once the check said
     * Available: a failed check never stands in the way of Play. Running from its job's start
     * until its operation ends; stopped or failed by that operation, until another replaces it.
     */
    fun update(status: SessionStatus): UpdateView {
        val snapshot = status.snapshot?.takeIf(Onboarding::audited) ?: return UpdateView()
        if (status.engine != SessionStatus.Engine.CONNECTED || snapshot.host != Host.SUPPORTED ||
            snapshot.installation != Installation.READY || snapshot.game == Game.RUNNING ||
            snapshot.activeJob == Job.OFFICIAL_LAUNCHER
        ) {
            return UpdateView()
        }
        val operation = snapshot.operation?.takeIf { it.job == Job.UPDATE }
        // The job's end lands a moment before its operation's outcome.
        if (snapshot.activeJob == Job.UPDATE || operation?.status in UPDATE_RUNNING) {
            val progress = operation?.progress
            return UpdateView(
                state = if (operation?.status == OperationStatus.SAFE_STOPPING) UpdateState.STOPPING else UpdateState.RUNNING,
                percent = progress?.percent?.coerceIn(0, 100),
                summary = progress?.summary,
                step = when {
                    operation?.status == OperationStatus.SAFE_STOPPING -> null
                    operation?.phase == Phase.PROMOTION -> R.string.update_step_installing
                    operation?.phase == Phase.VERIFICATION -> R.string.update_step_checking
                    else -> R.string.update_step_looking
                },
            )
        }
        // The update finished: until the check after it says, neither the update (its Available is
        // from before) nor Play.
        if (status.updateRecheck != null) return UpdateView(UpdateState.RECHECKING)
        if (snapshot.update != Update.AVAILABLE) return UpdateView()
        val canStart = snapshot.activeJob == null && !snapshot.unresolvedJournal
        return when {
            status.refusal?.reason == UPDATE_REFUSED ->
                UpdateView(UpdateState.FAILED, canStart, problem = R.string.update_refused)
            operation?.status == OperationStatus.FAILED || operation?.status == OperationStatus.INDETERMINATE ->
                UpdateView(UpdateState.FAILED, canStart, problem = R.string.update_failed)
            operation?.status == OperationStatus.CANCELLED -> UpdateView(UpdateState.STOPPED, canStart)
            else -> UpdateView(UpdateState.OFFERED, canStart)
        }
    }

    /** The engine's refusal of an update start (its guard: something runs, or nothing is Available). */
    const val UPDATE_REFUSED = "update-not-permitted"

    /** The main button's label: Play, or the update's next step. */
    @StringRes
    fun primaryLabel(update: UpdateView): Int = when (update.state) {
        UpdateState.NONE, UpdateState.RECHECKING -> R.string.action_play
        UpdateState.OFFERED -> R.string.action_update
        UpdateState.RUNNING -> R.string.action_stop
        UpdateState.STOPPING -> R.string.action_stopping
        UpdateState.STOPPED -> R.string.action_resume
        UpdateState.FAILED -> R.string.action_try_again
    }

    /** The engine command the main button sends; null for Play (its own action) and while the update stops. */
    fun primaryCommand(update: UpdateView): Command? = when (update.state) {
        UpdateState.NONE, UpdateState.STOPPING, UpdateState.RECHECKING -> null
        UpdateState.OFFERED, UpdateState.STOPPED, UpdateState.FAILED -> Command.ApplyUpdate
        UpdateState.RUNNING -> Command.RequestSafeStop
    }

    /**
     * Where Play's two buttons start in a stage [height] tall, with [above] standing on them and
     * [below] hanging from them ([PlayStage]): centred, unless either would not fit; then they
     * give way only as far as needed.
     */
    fun stageTop(height: Int, above: Int, middle: Int, below: Int): Int =
        ((height - middle) / 2).coerceAtMost(height - middle - below).coerceAtLeast(above)

    /** The main button takes a press (as far as the update goes; "Getting ready…" disables it too). */
    fun primaryEnabled(update: UpdateView): Boolean = when (update.state) {
        UpdateState.NONE, UpdateState.RUNNING -> true
        UpdateState.STOPPING, UpdateState.RECHECKING -> false
        UpdateState.OFFERED, UpdateState.STOPPED, UpdateState.FAILED -> update.canStart
    }

    /**
     * The one line of facts under Play: the renderer the next launch uses, the GPU it runs on,
     * and Project Diablo 2's update state once the engine has checked. Only what is known.
     */
    fun facts(snapshot: Snapshot?, gpu: String?): List<Line> = buildList {
        snapshot?.settings?.renderer?.let {
            add(Line(if (it == Renderer.GLIDE) R.string.play_fact_glide else R.string.play_fact_direct_draw))
        }
        if (!gpu.isNullOrBlank()) add(Line(R.string.play_fact_plain, gpu))
        when (snapshot?.update) {
            Update.UP_TO_DATE -> add(Line(R.string.update_up_to_date))
            Update.AVAILABLE -> add(Line(R.string.update_available))
            Update.CHECKING -> add(Line(R.string.update_checking))
            Update.CHECK_FAILED -> add(Line(R.string.update_check_failed))
            Update.UNKNOWN, null -> Unit
        }
    }
}

/** Where Play leads, along the foot of the stage. */
class PlayDestinations(
    val onSettings: () -> Unit,
    val onControls: () -> Unit,
    val onRepair: () -> Unit,
    val onAbout: () -> Unit,
)

/**
 * Play: alone in the middle of a quiet field, the official PD2 launcher as a quiet second button
 * under it, one line of facts beneath, the destinations (Settings, Controls, Repair, About) along
 * the foot. [problem]: why the last Play or official launcher did not start.
 * [onOfficialLauncher] null: its button is disabled (an install, the game, a check or the update runs).
 * [waiting]: why Play cannot start yet ("Getting ready…"), shown where "Ready to play" goes; both
 * buttons are disabled while it shows.
 * [update]: Project Diablo 2's Available update ([Play.update]): the main button then reads Update
 * and sends [onCommand]'s commands (Stop while it runs, Resume, Try again); where "Ready to play"
 * goes, its figure while it runs and a red banner once it failed; "Play without updating"
 * ([onPlay]) under the buttons while it does not run.
 * [launching]: Play or the official PD2 launcher was pressed and the engine has not shown it yet
 * ([Play.launchPending]): both buttons and "Play without updating" wait.
 */
@Composable
fun PlayScreen(
    facts: List<Line>,
    problem: String?,
    onPlay: () -> Unit,
    destinations: PlayDestinations,
    onOfficialLauncher: (() -> Unit)? = null,
    waiting: String? = null,
    update: Play.UpdateView = Play.UpdateView(),
    onCommand: (Command) -> Unit = {},
    launching: Boolean = false,
    /** The engine or the game support files failed: under the [problem], a Try again that starts them again. */
    onTryAgain: (() -> Unit)? = null,
) {
    val focus = remember { FocusRequester() }
    InitialFocus(focus, waiting == null)
    val phone = LocalFormFactor.current == FormFactor.PHONE
    val width = if (phone) 280.dp else 300.dp
    // The update's figure and banner: the buttons' width on a phone, which has height to spare;
    // up to 400 dp on a handheld, so the caption keeps to one line and the buttons keep their
    // place on a 480 dp screen with a 3-button bar.
    val wide = if (phone) Modifier.width(width) else Modifier.widthIn(max = 400.dp).fillMaxWidth()
    Box(Modifier.fillMaxSize()) {
        Lattice(Modifier.matchParentSize())
        Column(
            Modifier.fillMaxSize().windowInsetsPadding(WindowInsets.safeDrawing).padding(horizontal = 20.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                Wordmark(Modifier.weight(1f))
            }
            PlayStage(
                Modifier.weight(1f).fillMaxWidth(),
                above = {
                    // Where "Ready to play" goes: what stands, or the update's figure or failure.
                    Column(horizontalAlignment = Alignment.CenterHorizontally) {
                        val failure = update.problem?.takeIf { update.state == Play.UpdateState.FAILED }
                        if (update.working && waiting == null) {
                            UpdateFigure(update, wide)
                        } else if (failure != null && waiting == null) {
                            Banner(stringResource(failure), Palette.Red, wide)
                        } else {
                            val headline = when (update.state) {
                                Play.UpdateState.OFFERED -> stringResource(R.string.play_update_headline)
                                Play.UpdateState.STOPPED -> stringResource(R.string.play_update_stopped)
                                Play.UpdateState.RECHECKING -> stringResource(R.string.play_update_rechecking)
                                else -> stringResource(R.string.play_ready)
                            }
                            Text(waiting ?: headline, style = Type.Title.copy(fontSize = 15.sp), color = Palette.Muted, textAlign = TextAlign.Center)
                        }
                        Spacer(Modifier.height(16.dp))
                    }
                },
                middle = {
                    Column(horizontalAlignment = Alignment.CenterHorizontally) {
                        val command = Play.primaryCommand(update)
                        LauncherButton(
                            stringResource(Play.primaryLabel(update)),
                            { if (update.state == Play.UpdateState.NONE) onPlay() else command?.let(onCommand) },
                            Modifier.width(width),
                            if (update.working) Kind.SECONDARY else Kind.PRIMARY,
                            enabled = waiting == null && !launching && Play.primaryEnabled(update), focusRequester = focus, height = 68.dp, style = Type.Play,
                        )
                        Spacer(Modifier.height(10.dp))
                        LauncherButton(
                            stringResource(R.string.play_official_launcher), onOfficialLauncher ?: {}, Modifier.width(width),
                            Kind.SECONDARY, enabled = onOfficialLauncher != null && waiting == null && !launching, height = 48.dp,
                        )
                    }
                },
                below = {
                    Column(horizontalAlignment = Alignment.CenterHorizontally) {
                        Spacer(Modifier.height(10.dp))
                        if (problem != null) {
                            Text(problem, style = Type.Hint, color = Palette.Red, textAlign = TextAlign.Center)
                            if (onTryAgain != null) {
                                Spacer(Modifier.height(4.dp))
                                TextLink(stringResource(R.string.action_try_again), onTryAgain)
                            }
                        } else if (facts.isNotEmpty()) {
                            Text(facts.map { text(it) }.joinToString("  ·  "), style = Type.Hint, color = Palette.Muted, textAlign = TextAlign.Center)
                        }
                        // The engine runs an out-of-date client too (online play may need the update).
                        if (update.playWithout && waiting == null && !launching) {
                            Spacer(Modifier.height(4.dp))
                            TextLink(stringResource(R.string.play_without_updating), onPlay)
                        }
                    }
                },
            )
            Rule(Modifier.widthIn(max = 460.dp))
            val link = Type.Hint.copy(fontWeight = FontWeight.Medium)
            Row(Modifier.widthIn(max = 460.dp).fillMaxWidth().padding(vertical = 6.dp), horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                listOf(
                    R.string.play_settings to destinations.onSettings,
                    R.string.play_controls to destinations.onControls,
                    R.string.play_repair to destinations.onRepair,
                    R.string.play_about to destinations.onAbout,
                ).forEach { (label, go) ->
                    LauncherButton(stringResource(label), go, Modifier.weight(1f), Kind.QUIET, height = 48.dp, style = link, padding = 8.dp)
                }
            }
        }
    }
}

/**
 * The running update's figure, in Download's style: the percent of the file downloading now (when
 * the engine knows it) with what runs beside it, the bar (moving while no percent is known), and one
 * caption: the engine's amount, speed and time left, else what runs. The percent's line keeps its
 * height without a percent, so the figure does not jump.
 */
@Composable
private fun UpdateFigure(update: Play.UpdateView, modifier: Modifier = Modifier) {
    Column(modifier) {
        Row(Modifier.fillMaxWidth().heightIn(min = 44.dp), verticalAlignment = Alignment.Bottom) {
            Text(
                update.percent?.let { stringResource(R.string.percent, it) }.orEmpty(), style = Type.Figure, color = Palette.Text,
                modifier = Modifier.alignByBaseline(),
            )
            Spacer(Modifier.weight(1f).widthIn(min = 12.dp))
            Text(
                stringResource(if (update.state == Play.UpdateState.STOPPING) R.string.play_update_stopping else R.string.play_updating),
                style = Type.Hint, color = Palette.Muted, maxLines = 1, overflow = TextOverflow.Ellipsis,
                modifier = Modifier.alignByBaseline(),
            )
        }
        Spacer(Modifier.height(10.dp))
        Bar(update.percent?.let { it / 100f }, color = Palette.Teal)
        val caption = update.summary ?: update.step?.let { stringResource(it) }
        if (caption != null) {
            Spacer(Modifier.height(8.dp))
            Text(caption, style = Type.Hint, color = Palette.Muted, maxLines = 2, overflow = TextOverflow.Ellipsis)
        }
    }
}

/**
 * Play's middle: [middle] (the two buttons) centred in the room, [above] standing on it and [below]
 * hanging from it, so what shows above or below (the update's figure, a banner) never moves the
 * buttons while it fits; where it does not, the buttons give way only as far as needed.
 */
@Composable
private fun PlayStage(
    modifier: Modifier,
    above: @Composable () -> Unit,
    middle: @Composable () -> Unit,
    below: @Composable () -> Unit,
) {
    Layout(
        content = {
            Box(Modifier.layoutId(StageSlot.ABOVE)) { above() }
            Box(Modifier.layoutId(StageSlot.MIDDLE)) { middle() }
            Box(Modifier.layoutId(StageSlot.BELOW)) { below() }
        },
        modifier = modifier,
    ) { measurables, constraints ->
        val loose = constraints.copy(minWidth = 0, minHeight = 0)
        fun slot(slot: StageSlot, maxHeight: Int) =
            measurables.first { it.layoutId == slot }.measure(loose.copy(maxHeight = maxHeight.coerceAtLeast(0)))
        val height = constraints.maxHeight
        val middlePlaced = slot(StageSlot.MIDDLE, height)
        val belowPlaced = slot(StageSlot.BELOW, height - middlePlaced.height)
        val abovePlaced = slot(StageSlot.ABOVE, height - middlePlaced.height - belowPlaced.height)
        val top = Play.stageTop(height, abovePlaced.height, middlePlaced.height, belowPlaced.height)
        layout(constraints.maxWidth, height) {
            val centre = { width: Int -> (constraints.maxWidth - width) / 2 }
            abovePlaced.placeRelative(centre(abovePlaced.width), top - abovePlaced.height)
            middlePlaced.placeRelative(centre(middlePlaced.width), top)
            belowPlaced.placeRelative(centre(belowPlaced.width), top + middlePlaced.height)
        }
    }
}

private enum class StageSlot { ABOVE, MIDDLE, BELOW }
