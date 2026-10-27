package com.pd2launcher.app.ui

import android.os.SystemClock
import androidx.annotation.StringRes
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.pd2launcher.app.Failures
import com.pd2launcher.app.Framebuffer
import com.pd2launcher.app.Line
import com.pd2launcher.app.Onboarding
import com.pd2launcher.app.R
import com.pd2launcher.app.Refusal
import com.pd2launcher.app.SessionStatus
import com.pd2launcher.app.SourceChoice
import com.pd2launcher.app.Tone
import com.pd2launcher.app.control.EnvironmentPart
import com.pd2launcher.app.control.EnvironmentSetup
import com.pd2launcher.app.control.Host
import com.pd2launcher.app.control.Installation
import com.pd2launcher.app.control.Job
import com.pd2launcher.app.control.Operation
import com.pd2launcher.app.control.OperationStatus
import com.pd2launcher.app.control.Phase
import com.pd2launcher.app.control.Progress
import com.pd2launcher.app.control.Snapshot
import com.pd2launcher.app.control.Step
import com.pd2launcher.app.control.Update
import kotlinx.coroutines.delay

/** [STARTING]: the player started the install and the game environment starts; no job runs yet. */
enum class InstallState { WAITING, READY, STARTING, RUNNING, STOPPING, STOPPED, FAILED, DONE }

/** Step 4, Install, as the screen shows it. */
data class InstallView(
    val state: InstallState,
    /** The Launcher step that runs: Install Diablo II or Install Project Diablo 2. */
    val step: Step?,
    /** What the running task does now, in plain words (the run's part, else the engine's phase). */
    val stage: Line?,
    /** 0..100 for the whole install while it runs (the run's weighted figure, else the engine's). */
    val percent: Int?,
    /** The running part's bytes or files, its speed and time left (the engine's summary, [ProgressCaption]). */
    val summary: Line?,
    /** The running part's bytes done of the total, when it reports bytes. */
    val amount: Line? = null,
    /** Why the last install did not succeed, in the app's words ([Failures]). */
    val problem: Line?,
    /**
     * The first task the engine's evidence does not show done, running or not; null once all
     * are ([InstallProgress.standingTask]).
     */
    val standing: InstallTask? = InstallTask.ENVIRONMENT,
    /** The phase and job of the last install operation, running or ended. */
    val phase: Phase? = null,
    val job: Job? = null,
    /**
     * Since when (the run's clock) the running part has run, while it reports no bytes or files
     * (an installer program runs, a part of the game environment is set up): the screen counts
     * its time so the work shows as moving.
     */
    val since: Long? = null,
) {
    /** Stop safely is offered only while install work runs and can still stop. */
    val canStop: Boolean get() = state == InstallState.RUNNING

    /** Install work runs or is about to: no Back, and the figure shows. */
    val working: Boolean get() = state == InstallState.STARTING || state == InstallState.RUNNING || state == InstallState.STOPPING

    val tone: Tone get() = when (state) {
        InstallState.DONE -> Tone.OK
        InstallState.FAILED -> Tone.ERROR
        InstallState.STOPPED -> Tone.WARNING
        InstallState.STARTING, InstallState.RUNNING, InstallState.STOPPING -> Tone.WORKING
        InstallState.WAITING, InstallState.READY -> Tone.PENDING
    }
}

/**
 * Install's tasks, in order: Diablo II and Lord of Destruction each from Blizzard (their
 * downloader, then their installer), or an installed Diablo II copied or adopted ([GAME] alone).
 */
enum class InstallTask { ENVIRONMENT, GAME, EXPANSION, PROJECT_DIABLO, VERIFY }

enum class TaskState { WAITING, RUNNING, DONE, STOPPED, FAILED }

/** One row of Install's task list. */
data class TaskRow(
    val task: InstallTask,
    @param:StringRes val label: Int,
    val state: TaskState,
    val detail: Line?,
)

/** One timed part of a task: the engine phases it covers and how long it typically takes. */
data class Segment(val task: InstallTask, val phases: Set<Phase>, val seconds: Int)

/**
 * What each source's install consists of and how long each part typically takes, for the
 * overall figure: the game environment's parts the engine runs ([PLANNED_PARTS]), Blizzard's Diablo
 * II downloader 1104 s and the Lord of Destruction one 318 s, their installers 23 s and 25 s,
 * Project Diablo 2 (manifest, client files, checks) 52 s, promotion and the final check 6 s,
 * importing an installed Diablo II 30 s. A part the engine reports bytes or files for fills by
 * those; one it reports nothing for fills by its typical time, up to 95 %, so the bar never
 * stands still. On Android each downloader runs right before its installer (the engine's
 * `install_base_game`).
 */
object InstallPlan {
    /**
     * The game environment's parts and their typical seconds, which the engine names as each
     * starts (`environment` in its operation): Windows (a copy of the app's prefix template,
     * with the XInput bridge, the Launcher's registry values and the link to the app's .NET)
     * 8 s, with the closing check; web components (Wine Gecko) 24 s, .NET (Wine Mono) 61 s and
     * the .NET Desktop Runtime the official PD2 launcher needs 68 s only from an engine that
     * installs those, which this app's does not.
     */
    val ENVIRONMENT_PARTS: Map<EnvironmentPart, Int> = mapOf(
        EnvironmentPart.WINDOWS to 8,
        EnvironmentPart.WEB_COMPONENTS to 24,
        EnvironmentPart.DOTNET to 61,
        EnvironmentPart.LAUNCHER_SUPPORT to 68,
    )

    /**
     * The parts the engine plans for the app's prefix: Windows only (the app provides the .NET
     * runtime, and the prefix has no Wine Gecko and no Wine Mono).
     */
    val PLANNED_PARTS: List<EnvironmentPart> = listOf(EnvironmentPart.WINDOWS)
    private val ENVIRONMENT = Segment(
        InstallTask.ENVIRONMENT,
        setOf(Phase.RUNTIME_ACQUISITION, Phase.PREFIX_INITIALIZATION),
        PLANNED_PARTS.sumOf { ENVIRONMENT_PARTS.getValue(it) },
    )
    private val BASE_DOWNLOAD = Segment(InstallTask.GAME, setOf(Phase.BASE_GAME_DOWNLOAD), 1104)
    private val BASE_INSTALL = Segment(InstallTask.GAME, setOf(Phase.BASE_GAME_INSTALL), 23)
    private val EXPANSION_DOWNLOAD = Segment(InstallTask.EXPANSION, setOf(Phase.EXPANSION_DOWNLOAD), 318)
    private val EXPANSION_INSTALL = Segment(InstallTask.EXPANSION, setOf(Phase.EXPANSION_INSTALL), 25)
    private val IMPORT = Segment(InstallTask.GAME, setOf(Phase.IMPORT_GAME), 30)
    /** The engine's client manifest and download, and the hashing between files (`verification`). */
    private val PROJECT_DIABLO = Segment(InstallTask.PROJECT_DIABLO, setOf(Phase.CLIENT_DOWNLOAD, Phase.VERIFICATION), 52)
    private val VERIFY = Segment(InstallTask.VERIFY, setOf(Phase.PROMOTION), 6)

    /** A part reported nothing for fills by time up to this much. */
    const val TIMED_LIMIT = 0.95f

    fun segments(source: SourceChoice): List<Segment> = listOf(ENVIRONMENT) + when (source) {
        SourceChoice.BLIZZARD -> listOf(BASE_DOWNLOAD, BASE_INSTALL, EXPANSION_DOWNLOAD, EXPANSION_INSTALL, PROJECT_DIABLO)
        // An import keeps the folder's Project Diablo 2 as it is: no task of its own (the owner's
        // rule of 2026-10-26).
        SourceChoice.EXISTING -> listOf(IMPORT)
    } + listOf(VERIFY)

    /** The source's tasks, in order: five from Blizzard, three for an installed Diablo II. */
    fun tasks(source: SourceChoice): List<InstallTask> = segments(source).map { it.task }.distinct()

    /**
     * The part [phase] of [job] belongs to; null when it names none. `verification` is the
     * client files' hashing only in Install Project Diablo 2; in Install Diablo II it is the
     * engine's closing audit.
     */
    fun segmentOf(segments: List<Segment>, phase: Phase, job: Job?): Int? {
        if (phase == Phase.VERIFICATION && job != Job.INSTALL_PROJECT_DIABLO) return null
        return segments.indexOfFirst { phase in it.phases }.takeIf { it >= 0 }
    }

    /** The first part of [task]. */
    fun first(segments: List<Segment>, task: InstallTask): Int = segments.indexOfFirst { it.task == task }

    /** How far the part is: the engine's bytes or files when it reports them, else the time it has taken. */
    fun fraction(operation: Operation?, elapsedMs: Long, segment: Segment): Float {
        val progress = operation?.progress
        val percent = progress?.percent
        // "stages" counts the whole install's parts, not this one's.
        if (percent != null && (progress.unit == "bytes" || progress.unit == "files")) return percent.coerceIn(0, 100) / 100f
        return (elapsedMs.coerceAtLeast(0) / (segment.seconds * 1000f)).coerceAtMost(TIMED_LIMIT)
    }

    /**
     * How far the game environment is: its parts before the running one done, and the running
     * one by the time it has taken ([partMs]) of its typical time, up to 95 %. Each part weighs
     * its typical time, so the bar moves at one pace through all of them.
     */
    fun environmentFraction(setup: EnvironmentSetup, partMs: Long): Float {
        val parts = setup.parts.ifEmpty { listOf(setup.part) }
        val total = parts.sumOf { ENVIRONMENT_PARTS.getValue(it) }
        val before = parts.takeWhile { it != setup.part }.sumOf { ENVIRONMENT_PARTS.getValue(it) }
        val running = ENVIRONMENT_PARTS.getValue(setup.part)
        val within = (partMs.coerceAtLeast(0) / (running * 1000f)).coerceAtMost(TIMED_LIMIT) * running
        return (before + within) / total
    }

    /** The whole install's percentage with the parts before [index] done and [fraction] of it. */
    fun percent(segments: List<Segment>, index: Int, fraction: Float): Int {
        val total = segments.sumOf { it.seconds }
        val done = segments.take(index).sumOf { it.seconds } + fraction.coerceIn(0f, 1f) * segments[index].seconds
        return (done * 100 / total).toInt().coerceIn(0, 100)
    }
}

/**
 * One install the player started (or found running), from the press of Install to its end:
 * which part runs and since when, and the figure so far. Kept by the activity, advanced on every
 * snapshot and every second by [InstallProgress.track].
 */
data class InstallRun(
    val source: SourceChoice,
    val startedAt: Long,
    /** The game environment's failure when the player pressed Install; only a new one ends the start. */
    val failureAtStart: String?,
    val segment: Int,
    val segmentSince: Long,
    /** An install job of this run was seen. */
    val sawJob: Boolean = false,
    /** The job's first operation: the engine's start label, which names no real phase yet. */
    val label: Operation? = null,
    /** A later part the engine names, more than one ahead, and since when (see [InstallProgress.track]). */
    val candidate: Int? = null,
    val candidateSince: Long = 0L,
    val percent: Int = 0,
    /** The install ended (done, stopped, failed, or it never started); the rows keep where. */
    val ended: Boolean = false,
    /** The engine's refusal shown when the player pressed Install; only a new one ends the start. */
    val refusalAtStart: Refusal? = null,
    /** The game environment's part the engine names as running, and since when (its own timer). */
    val part: EnvironmentPart? = null,
    val partSince: Long = 0L,
    /** The engine has named the run's part (its job's start label included) since the run got there. */
    val seen: Boolean = false,
)

/** Maps the engine's snapshot onto the install screen. Pure, so every state is unit tested. */
object InstallProgress {
    private val INSTALL_JOBS = setOf(Job.INSTALL_BASE_GAME, Job.INSTALL_PROJECT_DIABLO)
    /** The audit's verdicts that no install step can follow: only Repair. */
    private val DAMAGED = setOf(Installation.REPAIR_REQUIRED, Installation.INDETERMINATE)
    private val RUNNING = setOf(
        OperationStatus.QUEUED, OperationStatus.PLANNING, OperationStatus.RUNNING,
        OperationStatus.VERIFYING, OperationStatus.COMMITTING,
    )

    /** How long Install may take to start (the game environment first) before the screen gives up on it. */
    const val START_LIMIT_MS = 3 * 60_000L

    /** How long a phase more than one part ahead must hold before the run moves there. */
    const val JUMP_HOLD_MS = 3_000L

    fun view(session: SessionStatus, run: InstallRun? = null): InstallView {
        val snapshot = session.snapshot
        val operation = snapshot?.operation?.takeIf { it.job in INSTALL_JOBS }
        val engineState = when {
            // Until the engine's audit ends its snapshot is a placeholder (the app reopened
            // mid-install opens here, FlowNav.opening): nothing can start or resume yet.
            snapshot == null || session.engine != SessionStatus.Engine.CONNECTED || !Onboarding.audited(snapshot) -> InstallState.WAITING
            operation?.status == OperationStatus.SAFE_STOPPING -> InstallState.STOPPING
            operation != null && operation.status in RUNNING -> InstallState.RUNNING
            snapshot.installation == Installation.READY -> InstallState.DONE
            operation?.status == OperationStatus.CANCELLED || snapshot.stopResult != null -> InstallState.STOPPED
            operation?.status == OperationStatus.FAILED || operation?.status == OperationStatus.INDETERMINATE -> InstallState.FAILED
            // Install Diablo II got part way (the app restarted since): it is paused there.
            paused(snapshot) -> InstallState.STOPPED
            else -> InstallState.READY
        }
        // From the press of Install until its job runs, the last outcome does not show.
        val starting = run != null && !run.ended && !run.sawJob && engineState != InstallState.DONE &&
            snapshot?.activeJob !in INSTALL_JOBS
        val state = if (starting) InstallState.STARTING else engineState
        val working = state == InstallState.RUNNING || state == InstallState.STOPPING
        // What runs now: the engine's operation while it is the run's part; else (the start
        // label, a blip, Install Diablo II's closing audit) the run's part itself, without figures.
        val active = run?.takeUnless { it.ended }
        val live = operation?.let { op ->
            val segments = active?.let { InstallPlan.segments(it.source) }
            when {
                active == null || segments == null -> op
                op != active.label && InstallPlan.segmentOf(segments, op.phase, op.job) == active.segment -> op
                else -> op.copy(phase = segments[active.segment].phases.first(), progress = null, environment = null)
            }
        }
        val step = when (operation?.job) {
            Job.INSTALL_BASE_GAME -> Step.INSTALL_D2
            Job.INSTALL_PROJECT_DIABLO -> Step.INSTALL_PD2
            else -> null
        }
        // A part of the game environment counts its own time; any other part from when it began.
        val partSince = active?.takeIf { run -> run.part != null && run.part == live?.environment?.part }?.partSince
        return InstallView(
            state = state,
            step = step.takeIf { working },
            stage = live?.let { stage(it.phase, it.job, it.progress, it.environment) }?.takeIf { working },
            percent = when {
                state == InstallState.STARTING -> run?.percent
                !working -> null
                run != null && !run.ended -> run.percent
                else -> operation?.progress?.percent?.coerceIn(0, 100)
            },
            summary = live?.progress?.summary?.takeIf { working }?.let { ProgressCaption.of(it, live.job) },
            amount = live?.progress?.takeIf { working }?.let(::amount),
            problem = when {
                state == InstallState.FAILED -> Failures.line(operation?.failure, operation?.job)
                // The install was not sent: the game environment it runs in did not start.
                !working && state != InstallState.DONE && state != InstallState.STARTING ->
                    session.gameEnvironmentFailure?.let { Line(R.string.failure_detail, it) }
                else -> null
            },
            standing = standingTask(snapshot),
            phase = operation?.phase,
            job = operation?.job,
            since = (partSince ?: active?.segmentSince)?.takeIf { state == InstallState.RUNNING && live?.progress?.summary == null },
        )
    }

    /**
     * The figure's caption at [now] on the run's clock, where the engine gives no summary of its
     * own: the running part's bytes when it counts them, else how long it has run ("1:12
     * elapsed"). Never what the part does: its task row says that. Null unless an install runs.
     */
    fun caption(view: InstallView, now: Long): Line? {
        if (view.state != InstallState.RUNNING) return null
        return view.amount ?: view.since?.let { Line(R.string.install_elapsed, elapsed(now - it)) }
    }

    /** [ms] as m:ss, or h:mm:ss from an hour. */
    fun elapsed(ms: Long): String {
        val seconds = ms.coerceAtLeast(0) / 1000
        val h = seconds / 3600
        val m = seconds / 60 % 60
        val s = seconds % 60
        return if (h > 0) "%d:%02d:%02d".format(h, m, s) else "%d:%02d".format(m, s)
    }

    /**
     * A run the player starts (or resumes) now from [session], at the task the installation
     * stands at by the engine's evidence, with the figure of everything before it.
     */
    fun start(source: SourceChoice, session: SessionStatus, now: Long): InstallRun {
        val segments = InstallPlan.segments(source)
        val at = standingTask(session.snapshot) ?: InstallTask.ENVIRONMENT
        val segment = InstallPlan.first(segments, at).coerceAtLeast(0)
        return InstallRun(
            source, startedAt = now, failureAtStart = session.gameEnvironmentFailure, segment = segment, segmentSince = now,
            percent = InstallPlan.percent(segments, segment, 0f), refusalAtStart = session.refusal,
        )
    }

    /**
     * The first task the engine's evidence (its last audit) does not show done, whatever this
     * session saw: Project Diablo 2 once Diablo II and Lord of Destruction are in, Lord of
     * Destruction once Diablo II is, Diablo II once the game environment is set up. Null once
     * the game is installed.
     */
    fun standingTask(snapshot: Snapshot?): InstallTask? = when {
        snapshot == null -> InstallTask.ENVIRONMENT
        snapshot.step == Step.PLAY || snapshot.installation == Installation.READY -> null
        snapshot.step == Step.INSTALL_PD2 -> InstallTask.PROJECT_DIABLO
        snapshot.installation == Installation.NEEDS_EXPANSION -> InstallTask.EXPANSION
        snapshot.environmentReady -> InstallTask.GAME
        else -> InstallTask.ENVIRONMENT
    }

    /**
     * Install Diablo II stands part way with nothing running: the game environment is set up, or
     * Diablo II is in. Only a stop (or a failure) leaves it there, so it shows as paused, also
     * after the app restarted, and Resume goes on from there.
     */
    private fun paused(snapshot: Snapshot): Boolean =
        snapshot.step == Step.INSTALL_D2 && snapshot.activeJob == null &&
            (snapshot.environmentReady || snapshot.installation == Installation.NEEDS_EXPANSION)

    /**
     * [run] after [session] at [now]; a run is made for an install job found running (the app
     * opened while it runs). The part only moves forward: to the next once the engine has named
     * the run's own part, else (further, or a run resumed at a downloader) once the later phase
     * has held for [JUMP_HOLD_MS]: the engine names an install phase for a moment while it checks
     * a cached downloader, before that downloader runs. Install Diablo II going on to Install
     * Project Diablo 2 keeps the run; any other end marks it [InstallRun.ended].
     */
    fun track(run: InstallRun?, session: SessionStatus, source: SourceChoice, now: Long): InstallRun? {
        val snapshot = session.snapshot ?: return run
        val job = snapshot.activeJob?.takeIf { it in INSTALL_JOBS }
        val operation = snapshot.operation?.takeIf { it.job in INSTALL_JOBS }
        if (run?.ended == true && job == null) return run
        var current = run?.takeUnless { it.ended } ?: if (job != null) found(source, session, operation, now) else return null
        val segments = InstallPlan.segments(current.source)
        if (job != null) {
            if (!current.sawJob) {
                val named = operation?.let { InstallPlan.segmentOf(segments, it.phase, it.job) } == current.segment
                current = current.copy(sawJob = true, label = operation, seen = current.seen || named)
            }
            val candidate = operation?.takeIf { it != current.label }?.let { InstallPlan.segmentOf(segments, it.phase, it.job) }
            current = advance(current, candidate, now)
        } else if (!current.sawJob) {
            val failed = session.gameEnvironmentFailure.let { it != null && it != current.failureAtStart }
            // The engine refused the start (shown on the screen): nothing runs.
            val refused = session.refusal.let { it != null && it != current.refusalAtStart }
            if (failed || refused || now - current.startedAt > START_LIMIT_MS) return current.copy(ended = true)
        } else if (!settled(snapshot)) {
            // The job ended; its final status has not landed yet.
        } else if (operation?.job == Job.INSTALL_BASE_GAME && continuesToPd2(Job.INSTALL_BASE_GAME, snapshot)) {
            val pd2 = InstallPlan.first(segments, InstallTask.PROJECT_DIABLO)
            current = current.copy(
                sawJob = false, label = null, startedAt = now, failureAtStart = session.gameEnvironmentFailure, candidate = null,
                refusalAtStart = session.refusal, seen = false,
            ).let { if (it.segment < pd2) it.copy(segment = pd2, segmentSince = now) else it }
        } else {
            return current.copy(ended = true)
        }
        val live = operation?.takeIf { job != null && it != current.label && InstallPlan.segmentOf(segments, it.phase, it.job) == current.segment }
        // Each part of the game environment the engine names gets its own timer.
        val environment = live?.environment
        if (environment != null && environment.part != current.part) current = current.copy(part = environment.part, partSince = now)
        val fraction = environment?.let { InstallPlan.environmentFraction(it, now - current.partSince) }
            ?: InstallPlan.fraction(live, now - current.segmentSince, segments[current.segment])
        return current.copy(percent = maxOf(current.percent, InstallPlan.percent(segments, current.segment, fraction)))
    }

    private fun found(source: SourceChoice, session: SessionStatus, operation: Operation?, now: Long): InstallRun {
        val run = start(source, session, now)
        val segments = InstallPlan.segments(source)
        val at = operation?.let { InstallPlan.segmentOf(segments, it.phase, it.job) } ?: run.segment
        return run.copy(segment = maxOf(at, run.segment), sawJob = true, seen = at >= run.segment)
    }

    private fun advance(run: InstallRun, candidate: Int?, now: Long): InstallRun = when {
        candidate == null || candidate < run.segment -> run.copy(candidate = null)
        candidate == run.segment -> run.copy(candidate = null, seen = true)
        candidate == run.segment + 1 && run.seen -> run.copy(segment = candidate, segmentSince = now, candidate = null)
        run.candidate != candidate -> run.copy(candidate = candidate, candidateSince = now)
        now - run.candidateSince >= JUMP_HOLD_MS -> run.copy(segment = candidate, segmentSince = now, candidate = null, seen = true)
        else -> run
    }

    /**
     * The source's tasks and how each stands: done, running, stopped or failed where the install
     * is (the run's part, else the engine's last phase, and never before what the engine's
     * evidence shows done), waiting after it. Before the install starts nothing runs: what the
     * installation already has is done, the rest waits. Each row says only how its task stands
     * ("Downloading game files from Blizzard", "Paused", "Done"); amounts and times are the
     * figure's.
     */
    fun tasks(view: InstallView, run: InstallRun?, labels: TaskLabels): List<TaskRow> {
        val done = view.state == InstallState.DONE || view.standing == null
        val standing = view.standing
        val reached = run?.let { InstallPlan.segments(it.source)[it.segment].task }
            ?: view.phase?.let { phase -> InstallPlan.segmentOf(InstallPlan.segments(labels.source), phase, view.job) }
                ?.let { InstallPlan.segments(labels.source)[it].task }
        val at = listOfNotNull(reached, standing).maxOrNull()
        val here = when (view.state) {
            InstallState.STARTING, InstallState.RUNNING, InstallState.STOPPING -> TaskState.RUNNING
            InstallState.STOPPED -> TaskState.STOPPED
            InstallState.FAILED -> TaskState.FAILED
            // The start gave up (the game environment did not start): where it was to run.
            InstallState.READY, InstallState.WAITING -> if (run?.ended == true && view.problem != null) TaskState.FAILED else null
            InstallState.DONE -> null
        }
        return InstallPlan.tasks(labels.source).map { task ->
            val state = when {
                done -> TaskState.DONE
                here == null -> if (standing != null && task < standing) TaskState.DONE else TaskState.WAITING
                at == null || task < at -> TaskState.DONE
                task == at -> here
                else -> TaskState.WAITING
            }
            val detail = when (state) {
                TaskState.RUNNING -> when {
                    view.state == InstallState.STARTING -> Line(R.string.install_row_starting)
                    view.state == InstallState.STOPPING -> Line(R.string.install_title_stopping)
                    else -> view.stage ?: Line(R.string.install_row_working)
                }
                TaskState.DONE -> Line(R.string.install_row_done)
                TaskState.STOPPED -> Line(R.string.install_row_stopped)
                TaskState.FAILED -> Line(R.string.install_row_failed)
                TaskState.WAITING -> null
            }
            TaskRow(task, labels.label(task), state, detail)
        }
    }

    /**
     * The screen's title. While install work runs it names the task underway in plain words
     * ("Setting up the game environment", "Installing Diablo II", "Installing Lord of
     * Destruction", "Installing Project Diablo 2", "Checking the game files"); the running row
     * and the caption carry that task's details. Otherwise it names how the install stands.
     */
    @StringRes
    fun title(view: InstallView, tasks: List<TaskRow>): Int = when (view.state) {
        InstallState.WAITING, InstallState.READY -> R.string.install_title
        InstallState.STARTING -> R.string.install_title_starting
        InstallState.STOPPING -> R.string.install_title_stopping
        InstallState.STOPPED -> R.string.install_title_stopped
        InstallState.FAILED -> R.string.install_title_failed
        InstallState.DONE -> R.string.install_title_done
        InstallState.RUNNING ->
            tasks.firstOrNull { it.state == TaskState.RUNNING }?.task?.let(::runningTitle)
                ?: R.string.install_title_running
    }

    @StringRes
    private fun runningTitle(task: InstallTask): Int = when (task) {
        InstallTask.ENVIRONMENT -> R.string.install_title_environment
        InstallTask.GAME -> R.string.install_title_game
        InstallTask.EXPANSION -> R.string.install_title_lod
        InstallTask.PROJECT_DIABLO -> R.string.install_title_running
        InstallTask.VERIFY -> R.string.install_title_verify
    }

    /** The task names for the player's source: an installed Diablo II is copied or adopted. */
    data class TaskLabels(val source: SourceChoice, val inPlace: Boolean = false) {
        @StringRes
        fun label(task: InstallTask): Int = when (task) {
            InstallTask.ENVIRONMENT -> R.string.install_task_environment
            InstallTask.GAME -> when {
                source != SourceChoice.EXISTING -> R.string.install_step_d2
                inPlace -> R.string.install_task_adopt_d2
                else -> R.string.install_task_copy_d2
            }
            InstallTask.EXPANSION -> R.string.install_task_lod
            InstallTask.PROJECT_DIABLO -> R.string.install_step_pd2
            InstallTask.VERIFY -> R.string.install_task_verify
        }
    }

    /** What Install's button starts. */
    enum class StartAction {
        /** Install Project Diablo 2 (Diablo II is in). */
        PROJECT_DIABLO,
        /** An installed Diablo II is copied or adopted: no keys. */
        IMPORT,
        /** Resume of a paused Install Diablo II with the keys entered before the stop. */
        HELD_KEYS,
        /** Install Diablo II from the key page. */
        KEY_PAGE,
    }

    /**
     * What Install's button starts from [state]: Stop paused Install Diablo II, so its Resume
     * sends the keys still held in memory ([keysHeld]) and stays on Install. Only a first
     * install, a retry after a failure (the keys may be wrong), or a Resume after the app was
     * closed (the keys are gone) opens the key page.
     */
    fun startAction(snapshot: Snapshot, state: InstallState, importsExisting: Boolean, keysHeld: Boolean): StartAction = when {
        snapshot.step == Step.INSTALL_PD2 -> StartAction.PROJECT_DIABLO
        importsExisting -> StartAction.IMPORT
        state == InstallState.STOPPED && keysHeld -> StartAction.HELD_KEYS
        else -> StartAction.KEY_PAGE
    }

    /** Which page of step 4 shows once Download is done; null before Install. */
    enum class Page {
        /** Install Diablo II needs both CD keys and Blizzard's terms first. */
        KEYS,
        /** An install job runs, or Install Project Diablo 2 is next. */
        PROGRESS,
        /**
         * A stage was interrupted, or the engine's audit, with nothing running, finds the
         * installation damaged or not known: it takes no install until Repair (Check again, Start
         * over).
         */
        REPAIR,
    }

    /**
     * [imported]: the installation was imported (the engine's import record). An import never
     * installs or updates Project Diablo 2, so one that left it to install shows Repair (Check
     * again, Start over), never Install Project Diablo 2.
     */
    fun page(session: SessionStatus, imported: Boolean = false): Page? {
        val snapshot = session.snapshot ?: return null
        return when {
            snapshot.activeJob in INSTALL_JOBS -> Page.PROGRESS
            snapshot.installation == Installation.STAGE_INTERRUPTED -> Page.REPAIR
            snapshot.installation in DAMAGED && snapshot.activeJob == null && Onboarding.audited(snapshot) -> Page.REPAIR
            imported && snapshot.step == Step.INSTALL_PD2 && snapshot.activeJob == null && Onboarding.audited(snapshot) -> Page.REPAIR
            snapshot.step == Step.INSTALL_D2 -> Page.KEYS
            snapshot.step == Step.INSTALL_PD2 -> Page.PROGRESS
            else -> null
        }
    }

    /**
     * The install job still running in [next], or [ending] while its end has not settled: the
     * engine clears the active job before its fresh audit and final status land, so a job's
     * outcome is read from the first snapshot with no job and a final status.
     */
    fun endingInstallJob(ending: Job?, next: Snapshot): Job? = when {
        next.activeJob in INSTALL_JOBS -> next.activeJob
        settled(next) -> null
        else -> ending
    }

    /**
     * Install Diablo II ended and left Install Project Diablo 2 next: it follows without input. Not
     * after an import ([imported]): an import never installs or updates Project Diablo 2.
     */
    fun continuesToPd2(ending: Job?, next: Snapshot, imported: Boolean = false): Boolean =
        !imported && ending == Job.INSTALL_BASE_GAME && settled(next) && next.step == Step.INSTALL_PD2

    /** An install job ended and none follows: the game environment stops. */
    fun installEnded(ending: Job?, next: Snapshot?, imported: Boolean = false): Boolean =
        ending != null && (next == null || (settled(next) && !continuesToPd2(ending, next, imported)))

    /** No job runs and the last operation reached its final status. */
    private fun settled(snapshot: Snapshot): Boolean =
        snapshot.activeJob == null && snapshot.operation?.status.let {
            it !in RUNNING && it != OperationStatus.SAFE_STOPPING
        }

    /**
     * What a phase does, in the player's words. The game environment names its running part
     * once the engine reports it ([part]). Project Diablo 2's part downloads while the engine
     * counts bytes or files; before that it runs the PD2 installer.
     */
    private fun stage(phase: Phase, job: Job?, progress: Progress?, environment: EnvironmentSetup?): Line? = when (phase) {
        Phase.RUNTIME_ACQUISITION, Phase.PREFIX_INITIALIZATION -> environment?.let(::part) ?: Line(R.string.install_stage_environment)
        Phase.BASE_GAME_DOWNLOAD, Phase.EXPANSION_DOWNLOAD -> Line(R.string.install_stage_blizzard_download)
        Phase.BASE_GAME_INSTALL -> Line(R.string.install_stage_d2_installer)
        Phase.EXPANSION_INSTALL -> Line(R.string.install_stage_lod_installer)
        Phase.IMPORT_GAME -> Line(R.string.install_stage_import)
        Phase.CLIENT_DOWNLOAD -> Line(
            if (progress?.unit == "bytes" || progress?.unit == "files") R.string.install_stage_client_download else R.string.install_stage_pd2_installer,
        )
        Phase.VERIFICATION ->
            Line(if (job == Job.INSTALL_PROJECT_DIABLO) R.string.install_stage_client_check else R.string.install_stage_verification)
        Phase.PROMOTION -> Line(R.string.install_stage_promotion)
        else -> null
    }

    /** The game environment's running part, its place and how many this setup runs: "Setting up Windows (1 of 4)". */
    fun part(setup: EnvironmentSetup): Line {
        val parts = setup.parts.ifEmpty { listOf(setup.part) }
        val id = when (setup.part) {
            EnvironmentPart.WINDOWS -> R.string.install_part_windows
            EnvironmentPart.WEB_COMPONENTS -> R.string.install_part_web_components
            EnvironmentPart.DOTNET -> R.string.install_part_dotnet
            EnvironmentPart.LAUNCHER_SUPPORT -> R.string.install_part_launcher_support
        }
        return Line(id, (parts.indexOf(setup.part) + 1).coerceAtLeast(1), parts.size)
    }

    /** Bytes done of the total, in GB from a gigabyte up, else in MB; null unless bytes are counted. */
    private fun amount(progress: Progress): Line? {
        val completed = progress.completed ?: return null
        val total = progress.total ?: return null
        if (progress.unit != "bytes") return null
        return if (total >= 1L shl 30) {
            Line(R.string.install_amount_gb, completed / GIB, total / GIB)
        } else {
            Line(R.string.download_amount, completed shr 20, total shr 20)
        }
    }

    private const val GIB = 1_073_741_824.0
}

/**
 * Install: the tasks (set up the game environment, Diablo II, Lord of Destruction, Project
 * Diablo 2, verify) and how each stands (the running row says what it does now; the title names
 * the task underway, [InstallProgress.title]), the figure for the whole install while it runs with its
 * amounts or time and the live installer window's thumbnail beside it, and Stop, which pauses:
 * once pressed, the note that a stuck installer window clears only when the Launcher's sealed
 * helpers time out (up to ten minutes); then Resume goes on at the same step. Nothing starts
 * until [onInstall]. [onBack] goes back a step; not while install work runs or starts
 * ([backShown]). Under the tasks, what the page must say ([installNote]); under a problem, and while
 * it runs or is paused, Share logs and Save logs.
 */
@Composable
fun InstallScreen(
    view: InstallView,
    tasks: List<TaskRow>,
    onStopSafely: () -> Unit,
    onInstall: (() -> Unit)? = null,
    onBack: (() -> Unit)? = null,
    window: Framebuffer? = null,
) {
    val focus = remember { FocusRequester() }
    val canInstall = onInstall != null && view.state in setOf(InstallState.READY, InstallState.STOPPED, InstallState.FAILED)
    val working = view.working
    InitialFocus(focus, view.canStop, canInstall)
    // The run's clock, each second while the running part's time is counted.
    val clock by produceState(SystemClock.elapsedRealtime(), view.since) {
        while (view.since != null) {
            value = SystemClock.elapsedRealtime()
            delay(1_000)
        }
    }
    // The figure's caption: the engine's bytes, speed and time left, else the running part's
    // own time ("1:12 elapsed"). What the part does is its task row's.
    val caption = (view.summary ?: InstallProgress.caption(view, clock))?.let { text(it) }
    val title = stringResource(InstallProgress.title(view, tasks))
    val color = when (view.state) {
        InstallState.FAILED -> Palette.Red
        InstallState.STOPPED -> Palette.Amber
        else -> Palette.Teal
    }
    val percent = view.percent ?: 0
    FlowShell(
        step = FlowStep.INSTALL,
        failed = view.state == InstallState.FAILED,
        title = title,
        intro = stringResource(if (working) R.string.install_intro_running else R.string.install_intro),
        onBack = onBack.takeUnless { working },
        backButton = backShown(onBack != null, working),
        progress = stepProgress(working, percent),
        // The whole install's figure; its caption is the running part's (bytes, speed, time left).
        // Beside it, the installer windows as they work.
        lead = if (working) ({
            Figure(percent, caption, color = color, beside = window?.let { { InstallWindow(it) } })
        }) else null,
        action = if (working || canInstall) ({
            if (working) {
                CornerPill(
                    stringResource(if (view.state == InstallState.STOPPING) R.string.action_stopping else R.string.action_stop), onStopSafely,
                    enabled = view.canStop, focusRequester = focus,
                )
            } else if (onInstall != null) {
                val label = when (view.state) {
                    InstallState.STOPPED -> R.string.action_resume
                    InstallState.FAILED -> R.string.action_try_again
                    else -> R.string.install_action_install
                }
                CornerPill(stringResource(label), onInstall, Kind.PRIMARY, focusRequester = focus)
            }
        }) else null,
    ) {
        tasks.forEachIndexed { index, row ->
            if (index > 0) Rule()
            val tone = when (row.state) {
                TaskState.WAITING -> Tone.PENDING
                TaskState.RUNNING -> Tone.WORKING
                TaskState.DONE -> Tone.OK
                TaskState.STOPPED -> Tone.WARNING
                TaskState.FAILED -> Tone.ERROR
            }
            StatusLine(tone, stringResource(row.label), listOfNotNull(row.detail?.let { text(it) }), dim = row.state == TaskState.WAITING)
        }
        // Once Stop is pressed: why it may take a while.
        if (view.state == InstallState.STOPPING) {
            Spacer(Modifier.height(12.dp))
            Banner(stringResource(R.string.install_stop_note), Palette.Muted)
        }
        val note = installNote(view)
        when (note) {
            Note.PROBLEM -> {
                Spacer(Modifier.height(12.dp))
                Banner(view.problem?.let { text(it) }.orEmpty(), Palette.Red)
                // A failure the network can cause: what a new check finds, and the app's settings when Android blocks it.
                NetworkNote(Modifier.padding(top = 8.dp))
            }
            Note.WAITS -> WaitNote(stringResource(R.string.install_hint_waiting))
            Note.FOUND, null -> {}
        }
        // Under a problem, and while it runs or is paused, so a step that looks stuck can be reported
        // before it fails; one place, so the links and their note stay when the install fails.
        if (note == Note.PROBLEM || working || view.state == InstallState.STOPPED) {
            ShareLogsLink(Modifier.padding(top = 4.dp))
        }
    }
}

/**
 * What Install says under its tasks: why the last install did not succeed (the engine's words),
 * or why nothing can start yet (the Launcher is getting ready).
 */
fun installNote(view: InstallView): Note? = when {
    view.problem != null -> Note.PROBLEM
    view.state == InstallState.WAITING -> Note.WAITS
    else -> null
}


/**
 * Repair, from Play (and during setup for an interrupted stage): the installation as the engine
 * last checked it, Check again (the engine audits the installation again and repairs recorded
 * work), and Start over, which removes the installation after an in-place confirmation that says
 * what goes and what is kept. From the press of Start over until its reset ends or is refused,
 * nothing can be pressed again. [onImport]: after an update in place whose moved game the engine
 * rejected, Import of that game comes first (the corner Check again takes otherwise; Check again
 * moves under the rows), and Start over second. [onSettings]: on every Repair of an installed game
 * ([Onboarding.repairLinks]: held on screen, where its Back leaves the app, opened from Play, or
 * through Check again), Settings and Share logs and Save logs under the rows.
 */
@Composable
fun RepairScreen(
    session: SessionStatus,
    onCheckAgain: () -> Unit,
    onStartOver: () -> Unit,
    onBack: () -> Unit,
    onImport: (() -> Unit)? = null,
    onSettings: (() -> Unit)? = null,
) {
    val snapshot = session.snapshot
    val focus = remember { FocusRequester() }
    var confirm by remember { mutableStateOf(false) }
    var pressed by remember { mutableStateOf(false) }
    var sawReset by remember { mutableStateOf(false) }
    val resetJob = snapshot?.activeJob == Job.RESET
    LaunchedEffect(resetJob) {
        if (resetJob) {
            sawReset = true
        } else if (sawReset) {
            // The reset ended (a success leaves this screen; a failure shows on it).
            sawReset = false
            pressed = false
        }
    }
    // A new refusal of Start over: nothing runs.
    LaunchedEffect(session.refusal) { if (session.refusal != null && !sawReset) pressed = false }
    val resetting = pressed || resetJob
    InitialFocus(focus, confirm)
    val checking = snapshot == null || snapshot.host == Host.CHECKING || snapshot.activeJob != null
    FlowShell(
        step = null,
        title = stringResource(R.string.repair_title),
        intro = stringResource(R.string.repair_intro),
        onBack = { if (confirm) confirm = false else onBack() },
        // Its two actions take the corners: Start over (then Keep) left, Check again (then the
        // confirmed Start over) right.
        secondary = {
            if (!confirm) {
                CornerPill(stringResource(R.string.repair_start_over), { confirm = true }, Kind.DANGER, enabled = !checking)
            } else {
                CornerPill(stringResource(R.string.repair_keep), { confirm = false }, enabled = !resetting, focusRequester = focus)
            }
        },
        action = {
            if (!confirm && onImport != null) {
                CornerPill(stringResource(R.string.repair_import), onImport, Kind.PRIMARY, enabled = !checking, focusRequester = focus)
            } else if (!confirm) {
                CornerPill(stringResource(R.string.action_check_again), onCheckAgain, Kind.PRIMARY, enabled = !checking, focusRequester = focus)
            } else {
                CornerPill(
                    stringResource(R.string.repair_confirm),
                    {
                        pressed = true
                        onStartOver()
                    },
                    Kind.DANGER, enabled = !resetting,
                )
            }
        },
    ) {
        if (confirm) {
            Banner(stringResource(R.string.repair_confirm_body), Palette.Red, Modifier.padding(vertical = 8.dp))
        } else if (onImport != null) {
            Banner(stringResource(R.string.repair_import_body), Palette.Teal, Modifier.padding(vertical = 8.dp))
            LauncherButton(stringResource(R.string.action_check_again), onCheckAgain, Modifier.padding(bottom = 8.dp), enabled = !checking)
        }
        val installation = snapshot?.installation
        // The folder it uses in place is gone: only Start over goes on, and nothing else can be checked.
        val folderGone = snapshot?.gameFolder?.available == false
        StatusLine(
            if (installation == Installation.READY) Tone.OK else if (installation == null) Tone.PENDING else Tone.WARNING,
            stringResource(R.string.repair_row_installation),
            listOf(stringResource(if (folderGone) R.string.repair_installation_folder_gone else installationText(installation))),
        )
        Rule()
        val update = snapshot?.update ?: Update.UNKNOWN
        // Settings → Game → Check for game updates is off: the engine checks nothing.
        val checksOff = snapshot?.settings?.updateChecks == false
        StatusLine(
            if (checksOff) {
                Tone.PENDING
            } else {
                when (update) {
                    Update.UP_TO_DATE -> Tone.OK
                    Update.AVAILABLE, Update.CHECK_FAILED -> Tone.WARNING
                    Update.CHECKING -> Tone.WORKING
                    Update.UNKNOWN -> Tone.PENDING
                }
            },
            stringResource(R.string.repair_row_update),
            listOf(stringResource(if (checksOff) R.string.update_checks_off else updateText(update))),
            dim = checksOff || update == Update.UNKNOWN,
        )
        Rule()
        val host = snapshot?.host
        StatusLine(
            when (host) {
                Host.SUPPORTED -> Tone.OK
                Host.UNSUPPORTED -> Tone.ERROR
                else -> Tone.WORKING
            },
            stringResource(R.string.row_host),
            listOf(
                stringResource(
                    when {
                        folderGone -> R.string.host_folder_gone_short
                        host == Host.UNSUPPORTED -> R.string.host_missing_short
                        host == Host.SUPPORTED -> R.string.host_supported
                        else -> R.string.host_checking
                    },
                ),
            ),
        )
        repairWaits(resetting, checking)?.let { WaitNote(stringResource(it)) }
        if (onSettings != null) {
            Rule()
            TextLink(stringResource(R.string.play_settings), onSettings, Modifier.padding(top = 4.dp))
            ShareLogsLink()
        }
    }
}

/**
 * Why Repair's actions wait: Start over runs, or the engine checks. How the installation stands
 * is its rows' alone.
 */
@StringRes
fun repairWaits(resetting: Boolean, checking: Boolean): Int? = when {
    resetting -> R.string.repair_status_resetting
    checking -> R.string.repair_status_checking
    else -> null
}

@StringRes
private fun installationText(installation: Installation?): Int = when (installation) {
    Installation.READY -> R.string.repair_installation_ready
    Installation.REPAIR_REQUIRED, Installation.INDETERMINATE -> R.string.repair_installation_repair
    Installation.STAGE_INTERRUPTED -> R.string.repair_installation_interrupted
    null -> R.string.repair_status_checking
    else -> R.string.repair_installation_incomplete
}

@StringRes
fun updateText(update: Update): Int = when (update) {
    Update.UP_TO_DATE -> R.string.update_up_to_date
    Update.AVAILABLE -> R.string.update_available
    Update.CHECKING -> R.string.update_checking
    Update.CHECK_FAILED -> R.string.update_check_failed
    Update.UNKNOWN -> R.string.update_unknown
}
