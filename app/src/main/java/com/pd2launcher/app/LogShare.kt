package com.pd2launcher.app

import android.content.ClipData
import android.content.Context
import android.content.Intent
import androidx.core.content.FileProvider
import com.pd2launcher.app.control.Report
import java.io.File
import java.io.IOException
import java.io.OutputStream
import java.nio.file.Files
import java.nio.file.LinkOption
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import java.util.TimeZone
import java.util.zip.ZipEntry
import java.util.zip.ZipOutputStream
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.withTimeoutOrNull

/**
 * Share logs (Settings → App, and beside a failure on Check, Download and Install): one zip with
 * `device.txt` (what the app knows about this device and the installation), the logs of the
 * processes the app starts (`files/logs`, under `app/`), the engine's diagnostics and the report
 * it saves for the share (under `engine/`), handed to Android's share sheet. None holds a CD key:
 * the app never logs one, and the engine's diagnostics and report leave out process arguments.
 * The app's own logs are not redacted: they can name the player's characters and network
 * addresses, which the row says. The zips are in [folder] (`res/xml/shared_logs.xml`); the next
 * share or save removes those older than [KEEP_MS]. Save logs packs the same zip and copies it where
 * the player chooses on Android's "Save as" screen ([saveTo]): the share sheet lists only apps that
 * take a zip, and many phones have none that keeps it on the device.
 */
object LogShare {
    /** How long a zip stays in [folder]: an app it was shared with may read it only when it sends it. */
    const val KEEP_MS = 60 * 60 * 1000L

    /**
     * Where Save logs' "Save as" screen opens: the shared storage's Download folder (a document of
     * Android's own provider, [SourceImport.SHARED_STORAGE]). Without it the screen opens in the folder
     * Android's picker showed this app last: often the folder of an import.
     */
    const val SAVE_FOLDER_DOCUMENT = "primary:Download"

    /** One file in the zip: [name] in it, read from [file]. */
    data class Entry(val name: String, val file: File)

    /** What Share logs follows while it waits for the engine's report: whether the engine is connected, and its report. */
    data class ReportWatch(val connected: Boolean, val report: Report?)

    /** How the wait for the engine's report ended; [describe] says it in `device.txt`. */
    sealed interface ReportOutcome {
        data class Saved(val file: File) : ReportOutcome
        data class Failed(val reason: String) : ReportOutcome
        data object NotConnected : ReportOutcome
        data object EngineLeft : ReportOutcome
        data class TimedOut(val waitedMs: Long) : ReportOutcome
    }

    /**
     * Asks the engine for a report ([request]) and follows [watch] until it is saved (a path
     * other than one saved before), fails, or the engine is gone, for at most [timeoutMs]; then
     * [dismiss]es it, as the app shows no report. A failure counts once the report started, or at
     * once when none had failed before (the engine's start and failure can reach the app as one
     * update). Nothing is asked of an engine that is not connected.
     */
    suspend fun awaitReport(watch: Flow<ReportWatch>, request: () -> Unit, dismiss: () -> Unit, timeoutMs: Long): ReportOutcome {
        val before = watch.first()
        if (!before.connected) return ReportOutcome.NotConnected
        val previous = (before.report as? Report.Saved)?.path
        val failedBefore = before.report is Report.Failed
        request()
        var started = false
        val end = try {
            withTimeoutOrNull(timeoutMs) {
                watch.first { now ->
                    val report = now.report
                    if (report == Report.Creating) started = true
                    !now.connected ||
                        (report is Report.Saved && report.path != previous) ||
                        (report is Report.Failed && (started || !failedBefore))
                }
            }
        } finally {
            dismiss()
        }
        val report = end?.report
        return when {
            end == null -> ReportOutcome.TimedOut(timeoutMs)
            !end.connected -> ReportOutcome.EngineLeft
            report is Report.Saved -> ReportOutcome.Saved(File(report.path))
            report is Report.Failed -> ReportOutcome.Failed(report.reason)
            else -> ReportOutcome.EngineLeft
        }
    }

    /** The `device.txt` line for [outcome]. */
    fun describe(outcome: ReportOutcome): String = when (outcome) {
        is ReportOutcome.Saved -> "saved (${outcome.file.name})"
        is ReportOutcome.Failed -> "failed: ${outcome.reason}"
        ReportOutcome.NotConnected -> "none: the engine was not connected"
        ReportOutcome.EngineLeft -> "none: the engine stopped during the wait"
        is ReportOutcome.TimedOut -> "none within ${outcome.waitedMs} ms"
    }

    /** The zip's folder in the app's cache, the one the share sheet's provider serves. */
    fun folder(cacheDir: File): File = File(cacheDir, "shared-logs")

    /**
     * The files to pack: every regular file directly in [logs] (rotated ones too) as `app/<name>`,
     * every one in the engine's [diagnostics] but its lock files as `engine/<name>`, and the
     * engine's [report] as `engine/<name>`. Links are left out, so nothing outside these folders
     * can be packed through one.
     */
    fun entries(logs: File, diagnostics: File, report: File?): List<Entry> {
        val app = regularFiles(logs).map { Entry("app/${it.name}", it) }
        val engine = regularFiles(diagnostics).filterNot { it.name.endsWith(".lock") }.map { Entry("engine/${it.name}", it) }
        val saved = listOfNotNull(report?.takeIf(::isRegularFile)).map { Entry("engine/${it.name}", it) }
        return app + engine + saved
    }

    /**
     * Writes the zip to [target]: [about] as `device.txt`, then each of [entries], opened without
     * following a link. One that can no longer be opened (rotated away since it was listed) is
     * left out; one that fails part way ends with a line that says so; a log still being written
     * is packed as far as it is when read. Only a failure to write the zip fails it.
     */
    fun pack(target: File, about: String, entries: List<Entry>) {
        ZipOutputStream(target.outputStream().buffered()).use { zip ->
            zip.putNextEntry(ZipEntry("device.txt"))
            zip.write(about.toByteArray())
            zip.closeEntry()
            for (entry in entries) {
                val input = try {
                    Files.newInputStream(entry.file.toPath(), LinkOption.NOFOLLOW_LINKS)
                } catch (_: IOException) {
                    continue
                }
                input.use {
                    zip.putNextEntry(ZipEntry(entry.name))
                    try {
                        it.copyTo(zip)
                    } catch (error: IOException) {
                        // A read failed; a failed write of the zip throws again here and ends the share.
                        zip.write("\n[PD2 Launcher: the rest of this file could not be read: $error]\n".toByteArray())
                    }
                    zip.closeEntry()
                }
            }
        }
    }

    /** `device.txt`: one "label: value" line for each of [facts]. */
    fun about(facts: List<Pair<String, String>>): String = facts.joinToString("") { (label, value) -> "$label: $value\n" }

    /** The zip's name, from the UTC time [now] (milliseconds since the epoch). */
    fun fileName(now: Long): String {
        val format = SimpleDateFormat("yyyyMMdd-HHmmss", Locale.ROOT).apply { timeZone = TimeZone.getTimeZone("UTC") }
        return "pd2-launcher-logs-${format.format(Date(now))}.zip"
    }

    /**
     * Packs a new zip into [folder] after removing those earlier shares and saves left more than
     * [KEEP_MS] before [now]: [about] and the files of [entries] (from [paths] and the engine's
     * [report]). Returns the zip.
     */
    fun write(cacheDir: File, paths: SessionPaths, about: String, report: File?, now: Long = System.currentTimeMillis()): File {
        val folder = folder(cacheDir)
        folder.mkdirs()
        folder.listFiles()?.filter { now - it.lastModified() >= KEEP_MS }?.forEach { it.delete() }
        val target = File(folder, fileName(now))
        try {
            pack(target, about, entries(paths.logs, paths.engineDiagnostics, report))
        } catch (error: IOException) {
            target.delete()
            throw error
        }
        return target
    }

    /** The zip the last share or save packed into [folder], if it is still there. */
    fun latest(cacheDir: File): File? =
        regularFiles(folder(cacheDir)).filter { it.name.endsWith(".zip") }.maxByOrNull { it.name }

    /**
     * Save logs' copy of [zip] (from [latest]) into the document the "Save as" screen made, which
     * [open] opens. A copy that cannot start or fails part way [delete]s that document, so no empty
     * or cut file stays where the player chose; then it throws what went wrong.
     */
    fun saveTo(zip: File?, open: () -> OutputStream?, delete: () -> Unit) {
        try {
            if (zip == null) throw IOException("no zip to save")
            val output = open() ?: throw IOException("no stream for the document")
            output.use { zip.inputStream().use { input -> input.copyTo(it) } }
        } catch (error: Exception) {
            runCatching(delete)
            throw error
        }
    }

    /** Android's share sheet for [zip]; the app the player picks reads it through the URI the share grants. */
    fun chooser(context: Context, zip: File, subject: String, title: String): Intent {
        val uri = FileProvider.getUriForFile(context, "${context.packageName}.logs", zip)
        val send = Intent(Intent.ACTION_SEND).apply {
            type = "application/zip"
            putExtra(Intent.EXTRA_STREAM, uri)
            putExtra(Intent.EXTRA_SUBJECT, subject)
            clipData = ClipData.newRawUri(subject, uri)
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        }
        return Intent.createChooser(send, title)
    }

    private fun regularFiles(folder: File): List<File> =
        folder.listFiles()?.filter(::isRegularFile)?.sortedBy { it.name }.orEmpty()

    private fun isRegularFile(file: File): Boolean = Files.isRegularFile(file.toPath(), LinkOption.NOFOLLOW_LINKS)
}
