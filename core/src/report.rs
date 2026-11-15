//! The problem report a player saves from Diagnostics.
//!
//! One `pd2-launcher-report-<UTC>.tar.xz` holds `summary.txt` (a one-screen
//! triage), `report.json` (everything the Launcher knows about this host and
//! installation) and `logs/` (bounded tails of the failure activity log and
//! of the Blizzard, Project Diablo 2 and game logs). Every text is redacted
//! ([`crate::redact`]); `.env.local` is never read. docs/DIAGNOSTICS.md
//! describes the contents.

use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use serde_json::{Value, json};

use crate::{
    InstallationStatus,
    activity::{self, ActivityEntry, ActivityEvent},
    audit::{
        AuditCheck, CheckStatus, GuestExecutionEvidence, HostEvidence, HostSupport,
        InstallationReport,
    },
    control_channel::HostFacts,
    diagnostics::{CustomSourceRecord, DiagnosticCode, JobKind, JobRecord, JobStatus},
    redact::{Redactor, mask_game_identities},
};

/// The most any one file contributes.
pub(crate) const FILE_CAP: usize = 256 * 1024;
/// The most all files contribute together, before compression.
pub(crate) const TOTAL_CAP: usize = 8 * 1024 * 1024;
pub(crate) const FILE_PREFIX: &str = "pd2-launcher-report-";
const PROJECT_D2: &str = "ProjectD2";
const REGISTRY_LIMIT: u64 = 64 * 1024 * 1024;
/// Registry values of the Diablo II keys a report may show; every other
/// value is listed by name only.
const REGISTRY_VALUES: [&str; 14] = [
    "InstallPath",
    "GamePath",
    "UninstallPath",
    "Save Path",
    "Program",
    "Version",
    "Resolution",
    "Language",
    "Gfx",
    "Render",
    "UseGlide",
    "VideoConfig",
    "BNETIP",
    "Preferred Realm",
];

/// The Launcher build that wrote the report.
#[derive(Clone, Debug, Default)]
pub(crate) struct BuildInfo {
    pub(crate) version: &'static str,
    pub(crate) commit: Option<&'static str>,
    pub(crate) artifact_identity: Option<&'static str>,
    pub(crate) debug: bool,
    pub(crate) x86_64_host_row: bool,
}

/// Where the Launcher keeps things, for this report.
#[derive(Clone, Debug, Default)]
pub(crate) struct ReportPaths {
    pub(crate) managed_root: PathBuf,
    pub(crate) diagnostics_root: PathBuf,
    pub(crate) default_location: bool,
    /// A Diablo II folder used in place.
    pub(crate) game_folder: Option<PathBuf>,
}

/// One download source as it is configured now: host only.
#[derive(Clone, Debug)]
pub(crate) struct SourceRow {
    pub(crate) artifact: &'static str,
    pub(crate) custom: bool,
    pub(crate) host: String,
}

/// Everything the Launcher's services know, gathered by the caller.
#[derive(Default)]
pub(crate) struct ReportInputs {
    pub(crate) now: u64,
    pub(crate) redactor: Redactor,
    pub(crate) build: BuildInfo,
    pub(crate) paths: ReportPaths,
    pub(crate) host: Option<HostEvidence>,
    /// The Android host facts the app handed to the Launcher engine.
    pub(crate) android: Option<HostFacts>,
    /// The audit taken for the report, or why none could be taken.
    pub(crate) audit: Option<Result<InstallationReport, String>>,
    pub(crate) downloads: Option<String>,
    /// The game's recorded OpenGL renderer, if the probe ran.
    pub(crate) renderer_probe: Option<Option<String>>,
    pub(crate) active_job: Option<JobRecord>,
    pub(crate) jobs: Vec<JobRecord>,
    pub(crate) events: Vec<DiagnosticCode>,
    pub(crate) custom_source_records: Vec<CustomSourceRecord>,
    pub(crate) sources: Vec<SourceRow>,
    pub(crate) settings: Option<Value>,
    pub(crate) installer_choice: Option<&'static str>,
}

/// The files of one report, redacted and bounded, in archive order.
pub(crate) struct ReportArchive {
    redactor: Redactor,
    entries: Vec<(String, Vec<u8>)>,
    total: usize,
    omitted: Vec<String>,
}

impl ReportArchive {
    pub(crate) fn new(redactor: Redactor) -> Self {
        Self {
            redactor,
            entries: Vec::new(),
            total: 0,
            omitted: Vec::new(),
        }
    }

    /// Add `text`, redacted, keeping its end when it is over [`FILE_CAP`].
    pub(crate) fn add_text(&mut self, name: &str, text: &str) {
        let redacted = self.redactor.text(text);
        let bytes = tail_bytes(redacted.as_bytes(), FILE_CAP);
        self.add_raw(name, bytes);
    }

    /// Add `value` as JSON with every key and string redacted first, so the
    /// JSON stays valid.
    pub(crate) fn add_json(&mut self, name: &str, value: &Value) {
        let redacted = self.redact_value(value);
        let text = serde_json::to_string_pretty(&redacted).unwrap_or_default();
        self.add_raw(name, text.into_bytes());
    }

    /// JSON lines, each redacted as [`Self::add_json`] does; the newest
    /// lines are kept within [`FILE_CAP`].
    fn add_json_lines(&mut self, name: &str, values: &[Value]) {
        let mut lines = Vec::new();
        let mut size = 0;
        for value in values.iter().rev() {
            let line = serde_json::to_string(&self.redact_value(value)).unwrap_or_default();
            size += line.len() + 1;
            if size > FILE_CAP {
                break;
            }
            lines.push(line);
        }
        lines.reverse();
        let mut text = lines.join("\n");
        text.push('\n');
        self.add_raw(name, text.into_bytes());
    }

    fn redact_value(&self, value: &Value) -> Value {
        match value {
            Value::String(text) => Value::String(self.redactor.text(text)),
            Value::Array(values) => Value::Array(
                values
                    .iter()
                    .map(|value| self.redact_value(value))
                    .collect(),
            ),
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(key, value)| (self.redactor.text(key), self.redact_value(value)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    fn add_raw(&mut self, name: &str, bytes: Vec<u8>) {
        if self.total + bytes.len() > TOTAL_CAP {
            self.omitted.push(name.to_owned());
            return;
        }
        self.total += bytes.len();
        self.entries.push((name.to_owned(), bytes));
    }

    /// Add the tail of a log file. Names and quoted strings in Battle.net
    /// and game logs are masked when `identities`, and a crash report's
    /// memory dump there keeps its hex words ([`Redactor::game_log_text`]).
    fn add_log(&mut self, name: &str, path: &Path, identities: bool) -> bool {
        let Ok(bytes) = read_tail(path, FILE_CAP as u64) else {
            return false;
        };
        let text = decode_log(&bytes);
        if identities {
            // Keys before names, as the engine's own copy does: a name rule
            // would else take a key's first group for a name.
            let redacted = mask_game_identities(&self.redactor.game_log_text(&text));
            let bytes = tail_bytes(redacted.as_bytes(), FILE_CAP);
            self.add_raw(name, bytes);
        } else {
            self.add_text(name, &text);
        }
        true
    }

    #[cfg(test)]
    pub(crate) fn entries(&self) -> &[(String, Vec<u8>)] {
        &self.entries
    }

    /// Write `pd2-launcher-report-<UTC stamp of now>.tar.xz` into
    /// `directory`, never replacing a file.
    pub(crate) fn write_into(&self, directory: &Path, now: u64) -> io::Result<PathBuf> {
        let (path, file) = create_unique(directory, &utc_stamp(now))?;
        let result = (|| {
            let encoder = xz2::write::XzEncoder::new(file, 6);
            let mut builder = tar::Builder::new(encoder);
            builder.mode(tar::HeaderMode::Deterministic);
            for (name, bytes) in &self.entries {
                let mut header = tar::Header::new_gnu();
                header.set_size(bytes.len() as u64);
                header.set_mode(0o644);
                header.set_mtime(now);
                header.set_entry_type(tar::EntryType::Regular);
                builder.append_data(&mut header, name, bytes.as_slice())?;
            }
            let encoder = builder.into_inner()?;
            let file = encoder.finish()?;
            file.sync_all()
        })();
        if let Err(error) = result {
            let _ = std::fs::remove_file(&path);
            return Err(error);
        }
        Ok(path)
    }
}

fn create_unique(directory: &Path, stamp: &str) -> io::Result<(PathBuf, File)> {
    for attempt in 0..100 {
        let name = if attempt == 0 {
            format!("{FILE_PREFIX}{stamp}.tar.xz")
        } else {
            format!("{FILE_PREFIX}{stamp}-{attempt}.tar.xz")
        };
        let path = directory.join(name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free report name",
    ))
}

fn tail_bytes(bytes: &[u8], limit: usize) -> Vec<u8> {
    if bytes.len() <= limit {
        return bytes.to_vec();
    }
    let mut start = bytes.len() - limit;
    // Start on a line, when one begins soon.
    if let Some(newline) = bytes[start..]
        .iter()
        .take(4096)
        .position(|byte| *byte == b'\n')
    {
        start += newline + 1;
    }
    let mut output = b"[earlier lines omitted]\n".to_vec();
    output.extend_from_slice(&bytes[start..]);
    output
}

/// The last `limit` bytes of a regular file, never following a symlink.
fn read_tail(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let file = File::from(rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )?);
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "not a file"));
    }
    let mut file = file;
    let start = metadata.len().saturating_sub(limit);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.take(limit).read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Text of a log: UTF-16 (Wine's registry, some Windows logs) or lossy UTF-8.
fn decode_log(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xff, 0xfe]) {
        let units = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect::<Vec<_>>();
        return String::from_utf16_lossy(&units);
    }
    String::from_utf8_lossy(bytes).into_owned()
}

/// `20260928T101500Z` for `seconds` since the epoch.
pub(crate) fn utc_stamp(seconds: u64) -> String {
    let (year, month, day, hour, minute, second) = civil(seconds);
    format!("{year:04}{month:02}{day:02}T{hour:02}{minute:02}{second:02}Z")
}

/// `2026-09-28 10:15:00 UTC`.
pub(crate) fn utc_time(seconds: u64) -> String {
    let (year, month, day, hour, minute, second) = civil(seconds);
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} UTC")
}

/// `2026-09-28 10:15:00.250 UTC` for `millis` since the epoch.
pub(crate) fn utc_time_millis(millis: u64) -> String {
    let (year, month, day, hour, minute, second) = civil(millis / 1000);
    format!(
        "{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}.{:03} UTC",
        millis % 1000
    )
}

fn civil(seconds: u64) -> (i64, u32, u32, u64, u64, u64) {
    let days = (seconds / 86_400) as i64;
    let rest = seconds % 86_400;
    // Howard Hinnant's days-to-civil.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day, rest / 3600, rest % 3600 / 60, rest % 60)
}

/// Keep the newest `keep` reports in `directory`; older ones are removed.
pub(crate) fn prune(directory: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut reports = entries
        .flatten()
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            name.starts_with(FILE_PREFIX)
                && name.ends_with(".tar.xz")
                && entry.file_type().is_ok_and(|kind| kind.is_file())
        })
        .map(|entry| {
            let modified = entry.metadata().map_or(0, |metadata| metadata.mtime());
            (modified, entry.file_name(), entry.path())
        })
        .collect::<Vec<_>>();
    reports.sort();
    let excess = reports.len().saturating_sub(keep);
    for (_, _, path) in reports.into_iter().take(excess) {
        let _ = std::fs::remove_file(path);
    }
}

/// Open the folder holding `file` in the user's file manager, with the file
/// selected: the freedesktop file manager's `ShowItems` D-Bus call, else
/// `xdg-open` on the folder itself (never its parent, which may be hidden).
/// Programs run by absolute path with explicit arguments, never a shell.
pub(crate) fn reveal(file: &Path) -> bool {
    let uri = file_uri(file);
    if let Some(dbus_send) = system_program("dbus-send")
        && run_briefly(
            Command::new(dbus_send).args([
                "--session",
                "--print-reply",
                "--reply-timeout=5000",
                "--dest=org.freedesktop.FileManager1",
                "--type=method_call",
                "/org/freedesktop/FileManager1",
                "org.freedesktop.FileManager1.ShowItems",
                &format!("array:string:{uri}"),
                "string:",
            ]),
            Duration::from_secs(6),
        )
    {
        return true;
    }
    let Some(folder) = file.parent() else {
        return false;
    };
    system_program("xdg-open").is_some_and(|xdg_open| {
        run_briefly(Command::new(xdg_open).arg(folder), Duration::from_secs(3))
    })
}

fn system_program(name: &str) -> Option<PathBuf> {
    ["/usr/bin", "/bin", "/usr/local/bin"]
        .into_iter()
        .map(|directory| Path::new(directory).join(name))
        .find(|path| path.is_file())
}

/// Run `command`: it succeeded when it exits 0, or is still running (a
/// file manager `xdg-open` waits for) after `grace`.
fn run_briefly(command: &mut Command, grace: Duration) -> bool {
    let Ok(mut child) = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let deadline = Instant::now() + grace;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(None) => {
                // Reaped in the background once it exits.
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                return true;
            }
            Err(_) => return false,
        }
    }
}

fn file_uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut uri = String::from("file://");
    for byte in path.as_os_str().as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~') {
            uri.push(*byte as char);
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri
}

/// A maintainer's reading of a stable failure code.
pub(crate) fn plain_reason(code: &str) -> &'static str {
    match code {
        "base-game-key-rejected" => "Diablo II's installer rejected the CD key",
        "expansion-key-rejected" => "Lord of Destruction's installer rejected the CD key",
        "base-game-key-wrong-length" | "expansion-key-wrong-length" => {
            "A CD key does not have 26 characters"
        }
        "base-game-key-16-characters" | "expansion-key-16-characters" => {
            "A 16-character CD key: Blizzard's installers take only 26-character keys"
        }
        "base-game-key-invalid-character" | "expansion-key-invalid-character" => {
            "A CD key has a character that keys do not use"
        }
        "base-game-key-is-expansion-key" => "The Lord of Destruction key is in the Diablo II field",
        "expansion-key-is-base-game-key" => "The Diablo II key is in the Lord of Destruction field",
        "base-game-key-not-valid" => "The Diablo II CD key failed the installer's own check",
        "expansion-key-not-valid" => {
            "The Lord of Destruction CD key failed the installer's own check"
        }
        "base-game-installer-error" => "Diablo II's installer showed an error dialog",
        "expansion-installer-error" => "Lord of Destruction's installer showed an error dialog",
        "base-game-installer-menu" | "expansion-installer-menu" => {
            "Blizzard's installer stayed at its menu instead of installing"
        }
        "base-game-install-failed" => "Diablo II's installer did not finish",
        "expansion-install-failed" => "Lord of Destruction's installer did not finish",
        "runtime-setup-failed" => "Setting up Wine or the game environment (prefix) failed",
        "builtin-fex-setup-failed" => "Unpacking the built-in FEX and its RootFS failed",
        "client-install-failed" => "Installing Project Diablo 2 failed",
        "installer-download-failed" => {
            "Blizzard's downloader program could not be downloaded from Blizzard"
        }
        "installer-download-stalled" => {
            "Blizzard's downloader got no new data for a minute (a lost or blocked connection)"
        }
        "installer-downloader-error" => {
            "Blizzard's downloader showed an error message (the downloader UI helper's output has its text)"
        }
        "installer-download-incomplete" => {
            "Blizzard's downloader ended before its download was complete (see payload-rejected)"
        }
        "installer-download-rejected" => {
            "Files Blizzard's downloader fetched differ from their pinned identity (see payload-rejected)"
        }
        "installer-payload-store-failed" => {
            "Blizzard's downloader finished, but its files could not be stored (for example no free space)"
        }
        "direct-download-refused" => {
            "Blizzard's piece host refused the direct download's access, also with a fresh downloader program"
        }
        "direct-download-incomplete" => {
            "The direct download still missed pieces after its retries (a lost or blocked connection)"
        }
        "direct-download-mismatch" => {
            "Blizzard's torrent or the direct download's files differ from their pins (see payload-rejected)"
        }
        "discard-downloads-failed" => {
            "The Launcher's own downloads could not all be removed for the import"
        }
        "installer-downloader-failed" => {
            "Blizzard's downloader did not run to the end (no window, could not be stopped, or exited)"
        }
        "download-failed" => "A download did not finish",
        "download-cache-unusable" => {
            "The download cache could not be used (unrecognised entries or write state, for example a folder copied mid-write)"
        }
        "client-download-failed" => "Project Diablo 2's files could not be downloaded",
        "downloads-required" => "A verified download was missing when a step needed it",
        "import-failed" => "Copying or linking an existing Diablo II failed",
        "existing-install-invalid" => "The chosen Diablo II folder is not usable",
        "game-still-running" => {
            "The game or the official launcher of an earlier session was still running"
        }
        "installation-in-use" => "Another launcher or program was using the installation",
        "stage-interrupted" | "launcher-stopped-during-job" => {
            "A step was interrupted (the Launcher was closed or stopped part way)"
        }
        "operation-cancelled" => "The player stopped the task",
        "operation-indeterminate" => "A stop could not be confirmed as safe",
        "managed-install-failed" => "The installation did not reach the expected state",
        "game-folder-unavailable" => "The Diablo II folder used in place is unavailable",
        "diagnostics-unavailable" => "The Launcher's own diagnostics could not be written",
        _ => "See report.json and logs/ for details",
    }
}

/// Build every file of the report from `inputs` and the files on disk.
pub(crate) fn build(inputs: &ReportInputs) -> ReportArchive {
    let activity = activity::read(&inputs.paths.diagnostics_root);
    let game = game_directory(&inputs.paths);
    let mut archive = ReportArchive::new(inputs.redactor.clone());
    let failure = latest_failure(inputs, &activity);
    let mut logs = ReportArchive::new(inputs.redactor.clone());
    let included = add_logs(&mut logs, inputs, &activity, game.as_deref());
    let summary = summary_text(inputs, &activity, failure.as_ref(), &included, &logs);
    archive.add_text("summary.txt", &summary);
    let report = report_json(inputs, &activity, failure.as_ref(), &included);
    archive.add_json("report.json", &report);
    for (name, bytes) in logs.entries {
        if archive.total + bytes.len() <= TOTAL_CAP {
            archive.total += bytes.len();
            archive.entries.push((name, bytes));
        } else {
            archive.omitted.push(name);
        }
    }
    archive.omitted.extend(logs.omitted);
    archive
}

fn game_directory(paths: &ReportPaths) -> Option<PathBuf> {
    paths.game_folder.clone().or_else(|| {
        let managed = paths
            .managed_root
            .join(crate::game_folder::managed_relative(crate::android_host()));
        managed.is_dir().then_some(managed)
    })
}

/// The most recent failed job, or an interrupted installation.
struct Failure {
    step: String,
    code: String,
    when: Option<u64>,
    job: Option<u64>,
    detail: Option<String>,
}

fn job_codes(activity: &[ActivityEntry]) -> BTreeMap<u64, String> {
    let mut codes = BTreeMap::new();
    for entry in activity {
        if let (Some(job), ActivityEvent::JobFailed { code }) = (entry.job, &entry.event) {
            codes.insert(job, code.clone());
        }
    }
    codes
}

fn job_times(activity: &[ActivityEntry], job: u64) -> (Option<u64>, Option<u64>) {
    let started = activity.iter().find_map(|entry| {
        (entry.job == Some(job) && matches!(entry.event, ActivityEvent::JobStarted { .. }))
            .then_some(entry.at)
    });
    let finished = activity.iter().rev().find_map(|entry| {
        (entry.job == Some(job) && matches!(entry.event, ActivityEvent::JobFinished { .. }))
            .then_some(entry.at)
    });
    (started, finished)
}

fn latest_failure(inputs: &ReportInputs, activity: &[ActivityEntry]) -> Option<Failure> {
    let codes = job_codes(activity);
    let job = inputs
        .jobs
        .iter()
        .rev()
        .find(|job| matches!(job.status(), JobStatus::Failed | JobStatus::Indeterminate))
        .map(|job| {
            let id = job.id().value();
            let detail = activity.iter().rev().find_map(|entry| match &entry.event {
                ActivityEvent::StageFailed { detail, .. } if entry.job == Some(id) => {
                    Some(detail.clone())
                }
                _ => None,
            });
            // The phase it failed in: the last one before its failure was
            // recorded (the job's own record moves on to verification).
            let mut phase = job.phase();
            for entry in activity.iter().filter(|entry| entry.job == Some(id)) {
                match &entry.event {
                    ActivityEvent::JobStarted { phase: started, .. } => phase = *started,
                    ActivityEvent::Phase { phase: changed } => phase = *changed,
                    ActivityEvent::StageFailed { .. } | ActivityEvent::JobFailed { .. } => break,
                    _ => {}
                }
            }
            Failure {
                step: format!(
                    "{} (job #{id}, phase: {})",
                    job.kind().message(),
                    phase.message()
                ),
                code: codes
                    .get(&id)
                    .cloned()
                    .unwrap_or_else(|| match job.status() {
                        JobStatus::Indeterminate => "operation-indeterminate".to_owned(),
                        _ => "unrecorded".to_owned(),
                    }),
                when: job_times(activity, id).1,
                job: Some(id),
                detail,
            }
        });
    let interrupted = match inputs.audit.as_ref() {
        Some(Ok(report)) => match report.installation_status() {
            InstallationStatus::StageInterrupted(stage) => Some(Failure {
                step: format!("Installation interrupted: {stage:?}"),
                code: "stage-interrupted".to_owned(),
                when: None,
                job: None,
                detail: Some(format!("{:?}", report.installation_status())),
            }),
            _ => None,
        },
        _ => None,
    };
    match (job, interrupted) {
        // The interruption is the standing problem; the job says how.
        (Some(job), Some(mut interrupted)) => {
            interrupted.detail = Some(format!(
                "{}; last failed job: {} ({})",
                interrupted.detail.unwrap_or_default(),
                job.step,
                job.code
            ));
            interrupted.job = job.job;
            interrupted.when = job.when;
            Some(interrupted)
        }
        (job, interrupted) => interrupted.or(job),
    }
}

/// Lines worth reading first: the failed job's process output and download
/// errors, else error lines from the installer logs.
fn excerpt(
    activity: &[ActivityEntry],
    failure: Option<&Failure>,
    included: &[(String, String)],
    logs: &ReportArchive,
) -> Vec<String> {
    let mut lines = Vec::new();
    let job = failure.and_then(|failure| failure.job);
    for entry in activity
        .iter()
        .filter(|entry| job.is_some() && entry.job == job)
    {
        match &entry.event {
            ActivityEvent::Process {
                label,
                outcome,
                exit_code,
                stdout,
                stderr,
            } => {
                lines.push(format!(
                    "{label}: {outcome}{}{}",
                    exit_code.map_or_else(String::new, |code| format!(" with code {code}")),
                    helper_verdict(label, *exit_code)
                ));
                for line in stderr
                    .lines()
                    .chain(stdout.lines())
                    .filter(|line| !line.trim().is_empty())
                    .rev()
                    .take(6)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                {
                    lines.push(format!(
                        "  | {}",
                        line.chars().take(160).collect::<String>()
                    ));
                }
            }
            ActivityEvent::Download {
                target,
                host,
                status,
                error,
            } => lines.push(format!(
                "download {target} from {host}: {}{}",
                status.map_or_else(|| error.clone(), |status| format!("HTTP {status}")),
                if entry.repeat > 1 {
                    format!(" ×{}", entry.repeat)
                } else {
                    String::new()
                }
            )),
            ActivityEvent::StageFailed { code, detail } => {
                lines.push(format!("stage failed: {code} ({detail})"));
            }
            ActivityEvent::PayloadRejected {
                payload,
                file,
                check,
                size,
            } => lines.push(format!(
                "payload {payload}: {file} failed the {check} check{}",
                size.map_or_else(String::new, |size| format!(" ({size} bytes)"))
            )),
            _ => {}
        }
    }
    if lines.len() < 4 {
        // Installer and game logs; the Launcher's own are summarized above.
        for (name, _) in included.iter().filter(|(name, _)| {
            !name.starts_with("logs/launcher-") && name != "logs/failed-processes.txt"
        }) {
            let Some((_, bytes)) = logs.entries.iter().find(|(entry, _)| entry == name) else {
                continue;
            };
            let text = String::from_utf8_lossy(bytes);
            for line in text.lines().filter(|line| {
                let line = line.to_ascii_lowercase();
                line.contains("error") || line.contains("fail") || line.contains("could not")
            }) {
                lines.push(format!(
                    "{name}: {}",
                    strip_tags(line).chars().take(160).collect::<String>()
                ));
            }
        }
    }
    let len = lines.len();
    lines.into_iter().skip(len.saturating_sub(14)).collect()
}

/// What the sealed installer UI helper's exit code says it saw.
fn helper_verdict(label: &str, code: Option<i32>) -> &'static str {
    match (label, code) {
        ("installer UI helper", Some(20)) => " (the installer showed an error dialog)",
        ("installer UI helper", Some(22)) => " (the installer rejected the CD key)",
        ("installer UI helper", Some(23)) => " (the installer stayed at its menu)",
        ("downloader UI helper", Some(21)) => " (no downloader window within ten minutes)",
        ("downloader UI helper", Some(22)) => " (the downloader asked for a folder)",
        ("downloader UI helper", Some(23)) => " (the downloader showed an error message)",
        ("downloader UI helper", Some(24..=26)) => " (the downloader could not be stopped)",
        _ => "",
    }
}

fn strip_tags(line: &str) -> String {
    let mut output = String::new();
    let mut in_tag = false;
    for character in line.chars() {
        match character {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => output.push(character),
            _ => {}
        }
    }
    output.trim().to_owned()
}

fn check_status(status: CheckStatus) -> &'static str {
    match status {
        CheckStatus::Satisfied => "ok",
        CheckStatus::Missing => "missing",
        CheckStatus::Invalid => "invalid",
        CheckStatus::Indeterminate => "indeterminate",
    }
}

/// Each setup step as Done, Failed, Interrupted, Running or Not started.
fn steps(inputs: &ReportInputs, activity: &[ActivityEntry]) -> Vec<(String, String)> {
    let codes = job_codes(activity);
    let last = |kinds: &[JobKind]| {
        inputs
            .active_job
            .iter()
            .chain(inputs.jobs.iter().rev())
            .find(|job| kinds.contains(&job.kind()))
    };
    let job_state = |kinds: &[JobKind]| -> Option<String> {
        let job = last(kinds)?;
        let code = codes
            .get(&job.id().value())
            .map_or_else(String::new, |code| format!(": {code}"));
        match job.status() {
            JobStatus::Running | JobStatus::SafeStopping => Some("Running".to_owned()),
            JobStatus::Failed => Some(format!("Failed (job #{}{code})", job.id().value())),
            JobStatus::Indeterminate => {
                Some(format!("Failed (job #{}, indeterminate)", job.id().value()))
            }
            JobStatus::Cancelled => {
                Some(format!("Stopped by the player (job #{})", job.id().value()))
            }
            JobStatus::Succeeded => None,
        }
    };
    let report = inputs.audit.as_ref().and_then(|audit| audit.as_ref().ok());
    let satisfied = |checks: &[AuditCheck]| {
        report.is_some_and(|report| {
            checks
                .iter()
                .all(|check| report.check(*check) == CheckStatus::Satisfied)
        })
    };
    let host = match report.map(InstallationReport::host_support) {
        Some(HostSupport::Supported) => "Done".to_owned(),
        Some(HostSupport::Unsupported) => {
            let reasons = report
                .map(|report| {
                    report
                        .findings()
                        .iter()
                        .filter(|finding| finding.check() == AuditCheck::Host)
                        .map(|finding| format!("{:?}", finding.reason()))
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            format!("Failed ({reasons})")
        }
        Some(HostSupport::Indeterminate) => "Unknown (host evidence indeterminate)".to_owned(),
        None => "Unknown (no audit)".to_owned(),
    };
    let location = if inputs.paths.managed_root.is_dir() {
        format!(
            "Done ({} location{})",
            if inputs.paths.default_location {
                "default"
            } else {
                "chosen"
            },
            inputs
                .installer_choice
                .map_or_else(String::new, |choice| format!(", installers: {choice}"))
        )
    } else {
        "Not started".to_owned()
    };
    let downloads_done = inputs
        .downloads
        .as_deref()
        .is_some_and(|downloads| downloads.starts_with("Complete"));
    let download = if downloads_done {
        "Done".to_owned()
    } else {
        job_state(&[JobKind::Download]).unwrap_or_else(|| "Not started".to_owned())
    };
    let base = if let Some(InstallationStatus::StageInterrupted(stage)) =
        report.map(InstallationReport::installation_status)
    {
        format!("Interrupted ({stage:?} stage; Start over continues)")
    } else if satisfied(&[AuditCheck::BaseGame, AuditCheck::Expansion]) {
        "Done".to_owned()
    } else {
        job_state(&[JobKind::InstallBaseGame, JobKind::Install])
            .unwrap_or_else(|| "Not started".to_owned())
    };
    let client = if satisfied(&[AuditCheck::Client]) {
        "Done".to_owned()
    } else {
        job_state(&[JobKind::InstallProjectDiablo]).unwrap_or_else(|| "Not started".to_owned())
    };
    let play = match report.map(|report| (report.installation_status(), report.launch_preflight()))
    {
        Some((InstallationStatus::Ready, preflight)) => {
            format!("Ready (launch preflight {preflight:?})")
        }
        Some((status, _)) => format!("Not ready ({status:?})"),
        None => "Unknown".to_owned(),
    };
    vec![
        ("1 Check your computer".to_owned(), host),
        ("2 Location & installers".to_owned(), location),
        ("3 Download".to_owned(), download),
        ("4 Install Diablo II".to_owned(), base),
        ("5 Install Project Diablo 2".to_owned(), client),
        ("6 Play".to_owned(), play),
    ]
}

fn os_release() -> BTreeMap<String, String> {
    let text = std::fs::read_to_string("/etc/os-release")
        .or_else(|_| std::fs::read_to_string("/usr/lib/os-release"))
        .unwrap_or_default();
    text.lines()
        .filter_map(|line| {
            let (name, value) = line.split_once('=')?;
            matches!(
                name,
                "ID" | "ID_LIKE" | "VERSION_ID" | "BUILD_ID" | "PRETTY_NAME" | "VARIANT_ID"
            )
            .then(|| (name.to_owned(), value.trim_matches('"').to_owned()))
        })
        .collect()
}

fn environment_shape(name: &str) -> Value {
    let Some(value) = std::env::var_os(name) else {
        return Value::Null;
    };
    let value = value.to_string_lossy();
    let display = value.strip_prefix(':').is_some_and(|rest| {
        !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit() || c == '.')
    });
    let wayland = value
        .strip_prefix("wayland-")
        .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()));
    if display || wayland {
        json!(value)
    } else {
        json!("set")
    }
}

fn plain_environment(name: &str) -> Value {
    std::env::var(name)
        .ok()
        .filter(|value| {
            value.len() <= 64
                && value
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '@'))
        })
        .map_or(Value::Null, |value| json!(value))
}

fn free_space(path: &Path) -> Value {
    let existing = path.ancestors().find(|ancestor| ancestor.exists());
    existing
        .and_then(|path| rustix::fs::statvfs(path).ok())
        .map_or(Value::Null, |stats| {
            json!({
                "available_bytes": stats.f_bavail.saturating_mul(stats.f_frsize),
                "total_bytes": stats.f_blocks.saturating_mul(stats.f_frsize),
            })
        })
}

fn kernel() -> (String, String) {
    let uname = rustix::system::uname();
    (
        uname.release().to_string_lossy().into_owned(),
        uname.machine().to_string_lossy().into_owned(),
    )
}

fn host_json(host: &HostEvidence) -> Value {
    let guest = match &host.guest_execution {
        GuestExecutionEvidence::Native {
            ia32_emulation,
            i386_loader_present,
            missing_i386_libraries,
            missing_wine_libraries,
            libunwind_present,
            x11_display_available,
        } => json!({
            "kind": "native",
            "ia32_emulation": format!("{ia32_emulation:?}"),
            "i386_loader_present": i386_loader_present,
            "missing_i386_libraries": missing_i386_libraries,
            "missing_wine_libraries": missing_wine_libraries
                .iter()
                .map(|library| json!({"library": format!("{library:?}"), "sonames": library.sonames()}))
                .collect::<Vec<_>>(),
            "libunwind_present": libunwind_present,
            "x11_display_available": x11_display_available,
        }),
        GuestExecutionEvidence::Translated {
            interpreter_present,
            page_size_4k,
            rootfs_ready,
            pending,
        } => json!({
            "kind": "translated",
            "fex_interpreter_present": interpreter_present,
            "page_size_4k": page_size_4k,
            "fex_rootfs_ready": rootfs_ready,
            "builtin_fex_pending": pending,
        }),
        GuestExecutionEvidence::AppProvided { page_size_4k } => json!({
            "kind": "app_provided",
            "page_size_4k": page_size_4k,
        }),
        GuestExecutionEvidence::Unavailable => json!({"kind": "unavailable"}),
    };
    json!({
        "os_id": host.os_id,
        "os_version": host.os_version,
        "os_variant": host.os_variant,
        "architecture": host.architecture,
        "kernel_machine": host.kernel_machine,
        "kernel_release": host.kernel_release,
        "display_session": format!("{:?}", host.display_session),
        "guest_execution": guest,
        "fex_package": host.fex_package,
        "vulkan_loader": host.vulkan_loader,
        "vulkan_driver": host.vulkan_driver,
        "graphics": host.graphics_execution.as_ref().map(|graphics| json!({
            "device": graphics.device_name(),
            "driver": graphics.driver_name(),
            "driver_info": graphics.driver_info(),
            "software": graphics.is_software_emulated(),
        })),
        "runtime_preflight": format!("{:?}", host.runtime_execution_preflight),
    })
}

fn audit_json(report: &InstallationReport) -> Value {
    let checks = [
        AuditCheck::Host,
        AuditCheck::Runtime,
        AuditCheck::Prefix,
        AuditCheck::BaseGame,
        AuditCheck::Expansion,
        AuditCheck::Client,
        AuditCheck::Transaction,
    ]
    .into_iter()
    .map(|check| {
        (
            format!("{check:?}"),
            json!(check_status(report.check(check))),
        )
    })
    .collect::<serde_json::Map<_, _>>();
    let components = report.prefix_components();
    json!({
        "installation_status": format!("{:?}", report.installation_status()),
        "host_support": format!("{:?}", report.host_support()),
        "launch_preflight": format!("{:?}", report.launch_preflight()),
        "checks": checks,
        "findings": report.findings().iter().map(|finding| json!({
            "check": format!("{:?}", finding.check()),
            "reason": format!("{:?}", finding.reason()),
            "path": finding.path().map(|path| path.as_str().to_owned()),
        })).collect::<Vec<_>>(),
        "protected_inventory": format!("{:?}", report.protected_inventory_status()),
        "protected_files": report.protected_files().len(),
        "custom_sources": report.custom_sources().iter().map(|source| json!({
            "component": format!("{:?}", source.component()),
            "host": source.host(),
        })).collect::<Vec<_>>(),
        "runtime": match report.installed_runtime() {
            crate::runtime::InstalledRuntime::Custom => json!({"kind": "custom"}),
            crate::runtime::InstalledRuntime::App => json!({
                "kind": "app",
                "identity": crate::runtime::APP_RUNTIME_DIRECTORY,
                "name": crate::runtime::APP_RUNTIME_DISPLAY_NAME,
            }),
            crate::runtime::InstalledRuntime::BuiltIn(runtime) => json!({
                "kind": "pinned",
                "identity": runtime.identity,
                "name": runtime.display_name,
            }),
        },
        "prefix_components": {
            "base": check_status(components.base),
            "gecko": check_status(components.gecko),
            "mono": check_status(components.mono),
            "dotnet": check_status(components.dotnet),
        },
        "official_launcher": format!("{:?}", report.official_launcher()),
    })
}

/// Every `.pd2launcher-*` record in the places the installer keeps them.
fn receipts(paths: &ReportPaths) -> Value {
    let mut records = serde_json::Map::new();
    let mut places = vec![
        (String::new(), paths.managed_root.clone()),
        ("prefix/".to_owned(), paths.managed_root.join("prefix")),
    ];
    if let Ok(runners) = std::fs::read_dir(paths.managed_root.join("runner")) {
        for runner in runners.flatten().take(8) {
            places.push((
                format!("runner/{}/", runner.file_name().to_string_lossy()),
                runner.path(),
            ));
        }
    }
    if let Some(folder) = &paths.game_folder {
        places.push(("game-folder/".to_owned(), folder.clone()));
    }
    for (prefix, directory) in places {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        let mut names = entries
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".pd2launcher-") && name != ".pd2launcher-retained")
            .collect::<Vec<_>>();
        names.sort();
        for name in names.into_iter().take(32) {
            let path = directory.join(&name);
            let value = match read_tail(&path, 4096) {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes).into_owned();
                    serde_json::from_str::<Value>(&text).unwrap_or(json!(text.trim()))
                }
                Err(_) if path.is_dir() => json!("(directory)"),
                Err(_) => json!("(unreadable)"),
            };
            records.insert(format!("{prefix}{name}"), value);
        }
    }
    Value::Object(records)
}

fn cache_summary(paths: &ReportPaths) -> Value {
    let artifacts = paths.managed_root.join("cache/artifacts");
    let recorded = std::fs::read(paths.managed_root.join("cache/downloads.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    let recorded_paths = recorded
        .as_ref()
        .and_then(|record| record.get("items"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("path").and_then(Value::as_str).map(str::to_owned))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut rows = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&artifacts) {
        let mut entries = entries.flatten().collect::<Vec<_>>();
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries.into_iter().take(64) {
            let name = entry.file_name().to_string_lossy().into_owned();
            let relative = format!("cache/artifacts/{name}");
            let size = tree_size(&entry.path(), 0);
            rows.push(json!({
                "name": name,
                "bytes": size,
                "verified": name.ends_with(".verified") || name.ends_with("-payload"),
                "recorded_by_download": recorded_paths
                    .iter()
                    .any(|path| path == &relative || path.starts_with(&format!("{relative}/"))),
            }));
        }
    }
    json!({
        "download_record": recorded.map(|record| json!({
            "plan": record.get("plan"),
            "reach": record.get("reach"),
            "prefix": record.get("prefix"),
            "total_bytes": record.get("total_bytes"),
            "items": recorded_paths.len(),
        })),
        "artifacts": rows,
    })
}

fn tree_size(path: &Path, depth: usize) -> u64 {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if metadata.is_file() {
        return metadata.size();
    }
    if !metadata.is_dir() || depth > 6 {
        return 0;
    }
    std::fs::read_dir(path).map_or(0, |entries| {
        entries
            .flatten()
            .take(4096)
            .map(|entry| tree_size(&entry.path(), depth + 1))
            .sum()
    })
}

fn job_json(job: &JobRecord, activity: &[ActivityEntry]) -> Value {
    let id = job.id().value();
    let mut value = serde_json::to_value(job).unwrap_or(Value::Null);
    let entries = activity
        .iter()
        .filter(|entry| entry.job == Some(id))
        .collect::<Vec<_>>();
    // Phase timings: each phase lasts until the next one or the end.
    let mut phases = Vec::new();
    let mut open: Option<(String, u64)> = None;
    let mut finished = None;
    for entry in &entries {
        let phase = match &entry.event {
            ActivityEvent::JobStarted { phase, .. } | ActivityEvent::Phase { phase } => {
                Some(format!("{phase:?}"))
            }
            ActivityEvent::JobFinished { .. } => {
                finished = Some(entry.at);
                None
            }
            _ => None,
        };
        if let Some(phase) = phase {
            if let Some((previous, started)) = open.take() {
                phases.push(json!({"phase": previous, "started": utc_time(started), "seconds": entry.at.saturating_sub(started)}));
            }
            open = Some((phase, entry.at));
        }
    }
    if let Some((previous, started)) = open {
        phases.push(json!({
            "phase": previous,
            "started": utc_time(started),
            "seconds": finished.map(|end: u64| end.saturating_sub(started)),
        }));
    }
    let (started, _) = job_times(activity, id);
    if let Value::Object(map) = &mut value {
        map.insert("kind_label".to_owned(), json!(job.kind().message()));
        map.insert("started".to_owned(), json!(started.map(utc_time)));
        map.insert("finished".to_owned(), json!(finished.map(utc_time)));
        map.insert("phases".to_owned(), json!(phases));
        map.insert(
            "failure_code".to_owned(),
            json!(job_codes(activity).get(&id)),
        );
        map.insert(
            "failures".to_owned(),
            json!(
                entries
                    .iter()
                    .filter(|entry| matches!(
                        entry.event,
                        ActivityEvent::StageFailed { .. }
                            | ActivityEvent::Process { .. }
                            | ActivityEvent::PayloadRejected { .. }
                            | ActivityEvent::Download { .. }
                    ))
                    .map(|entry| {
                        let mut value = serde_json::to_value(entry).unwrap_or(Value::Null);
                        if let Value::Object(map) = &mut value {
                            map.insert("time".to_owned(), json!(utc_time(entry.at)));
                        }
                        value
                    })
                    .collect::<Vec<_>>()
            ),
        );
    }
    value
}

fn report_json(
    inputs: &ReportInputs,
    activity: &[ActivityEntry],
    failure: Option<&Failure>,
    included: &[(String, String)],
) -> Value {
    let (kernel_release, kernel_machine) = kernel();
    let build = &inputs.build;
    let mut report = json!({
        "schema": 1,
        "created": utc_time(inputs.now),
        "build": {
            "version": build.version,
            "commit": build.commit,
            "artifact_identity": build.artifact_identity,
            "profile": if build.debug { "debug" } else { "release" },
            "x86_64_host_row": build.x86_64_host_row,
            "target_arch": std::env::consts::ARCH,
        },
        "system": {
            "os_release": os_release(),
            "kernel_release": kernel_release,
            "kernel_machine": kernel_machine,
            "arch": std::env::consts::ARCH,
            "session_type": plain_environment("XDG_SESSION_TYPE"),
            "desktop": plain_environment("XDG_CURRENT_DESKTOP"),
            "desktop_session": plain_environment("DESKTOP_SESSION"),
            "display": environment_shape("DISPLAY"),
            "wayland_display": environment_shape("WAYLAND_DISPLAY"),
            "locale": {
                "LANG": plain_environment("LANG"),
                "LC_ALL": plain_environment("LC_ALL"),
            },
            "install_location_free_space": free_space(&inputs.paths.managed_root),
        },
        "host_evidence": inputs.host.as_ref().map(host_json),
        "renderer_probe": inputs.renderer_probe.as_ref().map(|renderer| json!({
            "renderer": renderer,
            "software": renderer.as_deref().is_some_and(crate::process::software_renderer),
        })),
        "audit": match &inputs.audit {
            Some(Ok(report)) => audit_json(report),
            Some(Err(reason)) => json!({"unavailable": reason}),
            None => Value::Null,
        },
        "installation": {
            "location": inputs.redactor.path(&inputs.paths.managed_root),
            "default_location": inputs.paths.default_location,
            "game_folder_in_place": inputs.paths.game_folder.as_deref().map(|path| inputs.redactor.path(path)),
            "installer_choice": inputs.installer_choice,
            "downloads": inputs.downloads,
            "steps": steps(inputs, activity).into_iter().collect::<BTreeMap<_, _>>(),
            "receipts": receipts(&inputs.paths),
            "cache": cache_summary(&inputs.paths),
        },
        "latest_failure": failure.map(|failure| json!({
            "step": failure.step,
            "code": failure.code,
            "reason": plain_reason(&failure.code),
            "when": failure.when.map(utc_time),
            "job": failure.job,
            "detail": failure.detail,
        })),
        "jobs": {
            "active": inputs.active_job.as_ref().map(|job| job_json(job, activity)),
            "recent": inputs.jobs.iter().rev().map(|job| job_json(job, activity)).collect::<Vec<_>>(),
        },
        "download_sources": inputs.sources.iter().map(|source| json!({
            "artifact": source.artifact,
            "origin": if source.custom { "custom" } else { "built-in" },
            "host": source.host,
        })).collect::<Vec<_>>(),
        "custom_sources_used": inputs.custom_source_records.iter().map(|record| json!({
            "job": record.job().value(),
            "artifact": record.artifact().label(),
            "host": record.host(),
        })).collect::<Vec<_>>(),
        "settings": inputs.settings,
        "recent_events": inputs.events.iter().map(|event| json!({
            "code": event,
            "message": event.message(),
        })).collect::<Vec<_>>(),
        "logs": included.iter().map(|(name, source)| json!({"file": name, "source": source})).collect::<Vec<_>>(),
    });
    // Only an Android host has the section; a desktop report has none.
    if let Some(android) = &inputs.android {
        report["android"] = json!({
            "release": android.android_release,
            "sdk": android.sdk,
            "device": android.device_model,
            "gpu": android.gpu_name,
            "chip_id": format!("{:#010x}", android.chip_id),
            "kernel_release": android.kernel_release,
            "page_size": android.page_size,
        });
    }
    report
}

fn summary_text(
    inputs: &ReportInputs,
    activity: &[ActivityEntry],
    failure: Option<&Failure>,
    included: &[(String, String)],
    logs: &ReportArchive,
) -> String {
    let mut text = String::new();
    let mut line = |label: &str, value: String| {
        text.push_str(&format!("{label:<16}{value}\n"));
    };
    let build = &inputs.build;
    line(
        "Launcher",
        format!(
            "{} {}{}{}",
            build.version,
            build.commit.map_or_else(
                || "(commit unknown)".to_owned(),
                |commit| format!("commit {commit}")
            ),
            build
                .artifact_identity
                .map_or_else(String::new, |identity| format!(", {identity}")),
            if build.debug { ", debug build" } else { "" },
        ),
    );
    line("Created", utc_time(inputs.now));
    // The desktop's system, session, window GPU and Vulkan loader: on the
    // Android host the app runs no desktop session, and its own facts
    // (device.txt) and the Android line say what these would.
    let android = inputs.android.is_some();
    let release = os_release();
    let (kernel_release, _) = kernel();
    if !android {
        line(
            "System",
            format!(
                "{} (ID={} ID_LIKE={} VERSION_ID={}{}) kernel {kernel_release} {}",
                release.get("PRETTY_NAME").map_or("unknown", String::as_str),
                release.get("ID").map_or("-", String::as_str),
                release.get("ID_LIKE").map_or("-", String::as_str),
                release.get("VERSION_ID").map_or("-", String::as_str),
                release
                    .get("BUILD_ID")
                    .map_or_else(String::new, |id| format!(" BUILD_ID={id}")),
                std::env::consts::ARCH,
            ),
        );
    }
    if let Some(android) = &inputs.android {
        line("Android", android.summary());
    }
    let session = |name: &str| match plain_environment(name) {
        Value::String(value) => value,
        _ => "-".to_owned(),
    };
    let presence = |name: &str| match environment_shape(name) {
        Value::String(value) => value,
        _ => "no".to_owned(),
    };
    if !android {
        line(
            "Session",
            format!(
                "{} on {} (DISPLAY {}, WAYLAND_DISPLAY {})",
                session("XDG_SESSION_TYPE"),
                session("XDG_CURRENT_DESKTOP"),
                presence("DISPLAY"),
                presence("WAYLAND_DISPLAY"),
            ),
        );
    }
    let graphics = inputs
        .host
        .as_ref()
        .and_then(|host| host.graphics_execution.as_ref())
        .map_or_else(
            || "unknown (no window graphics evidence)".to_owned(),
            |graphics| {
                format!(
                    "{} · {} {}{}",
                    graphics.device_name(),
                    graphics.driver_name(),
                    graphics.driver_info(),
                    if graphics.is_software_emulated() {
                        " (software)"
                    } else {
                        ""
                    }
                )
            },
        );
    if !android {
        line("GPU", graphics);
    }
    line(
        "Game renderer",
        match &inputs.renderer_probe {
            Some(Some(renderer)) => format!(
                "{renderer}{}",
                if crate::process::software_renderer(renderer) {
                    " (software rendering)"
                } else {
                    ""
                }
            ),
            Some(None) => "probe found no OpenGL context".to_owned(),
            None => "not probed yet".to_owned(),
        },
    );
    if let Some(host) = &inputs.host {
        let mut notes = Vec::new();
        if !android {
            notes.push(format!(
                "Vulkan loader {}, driver {}",
                yes(host.vulkan_loader),
                yes(host.vulkan_driver)
            ));
        }
        notes.push(format!(
            "Wine preflight {:?}",
            host.runtime_execution_preflight
        ));
        if let GuestExecutionEvidence::Native {
            missing_wine_libraries,
            ia32_emulation,
            ..
        } = &host.guest_execution
        {
            if !missing_wine_libraries.is_empty() {
                notes.push(format!(
                    "MISSING host libraries: {}",
                    missing_wine_libraries
                        .iter()
                        .map(|library| format!("{library:?} ({})", library.sonames().join(" or ")))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            notes.push(format!("ia32 {ia32_emulation:?}"));
        }
        line("Host", notes.join("; "));
    }
    if let Some(Ok(report)) = &inputs.audit {
        let host_findings = report
            .findings()
            .iter()
            .filter(|finding| finding.check() == AuditCheck::Host)
            .map(|finding| format!("{:?}", finding.reason()))
            .collect::<Vec<_>>();
        if !host_findings.is_empty() {
            line("Step 1 findings", host_findings.join(", "));
        }
        line(
            "Status",
            format!(
                "{:?} (host {:?}, launch preflight {:?})",
                report.installation_status(),
                report.host_support(),
                report.launch_preflight()
            ),
        );
    } else if let Some(Err(reason)) = &inputs.audit {
        line("Status", format!("audit unavailable: {reason}"));
    }
    let custom = inputs
        .sources
        .iter()
        .filter(|source| source.custom)
        .map(|source| format!("{} from {}", source.artifact, source.host))
        .collect::<Vec<_>>();
    if !custom.is_empty() {
        line("Custom sources", custom.join(", "));
    }
    text.push_str("\nSetup steps\n");
    for (step, state) in steps(inputs, activity) {
        text.push_str(&format!("  {step:<28}{state}\n"));
    }
    text.push('\n');
    match failure {
        Some(failure) => {
            text.push_str("Most recent failure\n");
            text.push_str(&format!("  Step    {}\n", failure.step));
            text.push_str(&format!("  Reason  {}\n", plain_reason(&failure.code)));
            text.push_str(&format!("  Code    {}\n", failure.code));
            text.push_str(&format!(
                "  When    {}\n",
                failure.when.map_or_else(|| "unknown".to_owned(), utc_time)
            ));
            if let Some(detail) = &failure.detail {
                text.push_str(&format!("  Detail  {detail}\n"));
            }
        }
        None => text.push_str("Most recent failure\n  none recorded\n"),
    }
    let lines = excerpt(activity, failure, included, logs);
    if !lines.is_empty() {
        text.push_str("\nRelevant log lines\n");
        for excerpt in lines {
            text.push_str(&format!("  {excerpt}\n"));
        }
    }
    text.push_str(&format!(
        "\nFiles: summary.txt, report.json{}\n",
        included
            .iter()
            .map(|(name, _)| format!(", {name}"))
            .collect::<String>()
    ));
    text
}

fn yes(value: bool) -> &'static str {
    if value { "present" } else { "MISSING" }
}

/// Add every log the report carries; returns (archive name, source) pairs.
fn add_logs(
    archive: &mut ReportArchive,
    inputs: &ReportInputs,
    activity: &[ActivityEntry],
    game: Option<&Path>,
) -> Vec<(String, String)> {
    let mut included = Vec::new();
    // The failed processes' output, readable.
    let mut output = String::new();
    for entry in activity {
        if let ActivityEvent::Process {
            label,
            outcome,
            exit_code,
            stdout,
            stderr,
        } = &entry.event
        {
            output.push_str(&format!(
                "=== {} job #{} {label}: {outcome}{}\n",
                utc_time(entry.at),
                entry
                    .job
                    .map_or_else(|| "-".to_owned(), |job| job.to_string()),
                exit_code.map_or_else(String::new, |code| format!(" with code {code}"))
            ));
            if !stdout.is_empty() {
                output.push_str(&format!("--- stdout (tail)\n{stdout}\n"));
            }
            if !stderr.is_empty() {
                output.push_str(&format!("--- stderr (tail)\n{stderr}\n"));
            }
        }
    }
    if !output.is_empty() {
        archive.add_text("logs/failed-processes.txt", &output);
        included.push((
            "logs/failed-processes.txt".to_owned(),
            "activity log".to_owned(),
        ));
    }
    if !activity.is_empty() {
        let values = activity
            .iter()
            .map(|entry| {
                let mut value = serde_json::to_value(entry).unwrap_or(Value::Null);
                if let Value::Object(map) = &mut value {
                    map.insert("time".to_owned(), json!(utc_time(entry.at)));
                }
                value
            })
            .collect::<Vec<_>>();
        archive.add_json_lines("logs/launcher-activity.jsonl", &values);
        included.push((
            "logs/launcher-activity.jsonl".to_owned(),
            "logs/diagnostics/activity*.jsonl".to_owned(),
        ));
    }
    let Some(game) = game else {
        return included;
    };
    let mut candidates: Vec<(String, PathBuf, bool)> = Vec::new();
    // Blizzard's installer logs beside the game.
    if let Ok(entries) = std::fs::read_dir(game) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let lower = name.to_ascii_lowercase();
            if lower.contains("log")
                && (lower.ends_with(".html") || lower.ends_with(".htm") || lower.ends_with(".log"))
                || lower == "bnetlog.txt"
            {
                let identities = lower == "bnetlog.txt";
                candidates.push((format!("logs/game/{name}"), entry.path(), identities));
            }
        }
    }
    let project = game.join(PROJECT_D2);
    for name in ["update.log", "launcher-update-debug.log", "BnetLog.txt"] {
        let path = project.join(name);
        if path.is_file() {
            candidates.push((
                format!("logs/ProjectD2/{name}"),
                path,
                name == "BnetLog.txt",
            ));
        }
    }
    // The latest game log (`D2<date>.txt`) in either folder.
    for folder in [game.to_owned(), project] {
        let latest = std::fs::read_dir(&folder).ok().and_then(|entries| {
            entries
                .flatten()
                .filter(|entry| {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    name.starts_with("D2") && name.to_ascii_lowercase().ends_with(".txt")
                })
                .max_by_key(|entry| {
                    entry
                        .metadata()
                        .map(|metadata| metadata.mtime())
                        .unwrap_or(0)
                })
        });
        if let Some(entry) = latest {
            let relative = if folder == game { "game" } else { "ProjectD2" };
            candidates.push((
                format!("logs/{relative}/{}", entry.file_name().to_string_lossy()),
                entry.path(),
                true,
            ));
        }
    }
    candidates.sort_by(|left, right| left.0.cmp(&right.0));
    candidates.dedup_by(|left, right| left.0 == right.0);
    for (target, path, identities) in candidates {
        if archive.add_log(&target, &path, identities) {
            included.push((target, inputs.redactor.path(&path)));
        }
    }
    let registry = registry_excerpt(&inputs.paths.managed_root.join("prefix"));
    if !registry.is_empty() {
        archive.add_text("logs/registry-diablo-ii.txt", &registry);
        included.push((
            "logs/registry-diablo-ii.txt".to_owned(),
            "prefix/user.reg, prefix/system.reg (Diablo II keys only)".to_owned(),
        ));
    }
    included
}

/// The Diablo II keys of the prefix's registry, with only reviewed values.
fn registry_excerpt(prefix: &Path) -> String {
    let mut output = String::new();
    for file in ["system.reg", "user.reg"] {
        let Ok(bytes) =
            crate::persistence::read_bounded_regular(&prefix.join(file), REGISTRY_LIMIT)
        else {
            continue;
        };
        let text = decode_log(&bytes);
        let mut in_section = false;
        for line in text.lines() {
            if line.starts_with('[') {
                let lower = line.to_ascii_lowercase();
                in_section = lower.contains("blizzard entertainment\\\\diablo ii");
                if in_section {
                    let header = line.split(']').next().unwrap_or(line);
                    output.push_str(&format!("\n{file}: {header}]\n"));
                }
                continue;
            }
            if !in_section || !line.starts_with('"') {
                continue;
            }
            let Some((name, value)) = line.split_once("\"=") else {
                continue;
            };
            let name = name.trim_start_matches('"');
            if REGISTRY_VALUES
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(name))
            {
                output.push_str(&format!("\"{name}\"={value}\n"));
            } else {
                output.push_str(&format!("\"{name}\"=<not included>\n"));
            }
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn a_game_logs_copy_keeps_a_dump_rows_hex_and_masks_keys_before_names() {
        let fixture = TempDir::new().unwrap();
        let path = fixture.path().join("D226102.txt");
        std::fs::write(
            &path,
            "21:13:04.951  0201FF18: FFFFFFFF D87E244C AD144BBF F293C16F  TM.x..%.l......o\n\
             \x20             0201FF28: 2468BCDE  GHJKMNPR\n\
             name: 2468 BCDE 9F7E 6D4B\n",
        )
        .unwrap();
        let mut archive = ReportArchive::new(Redactor::default());
        assert!(archive.add_log("logs/ProjectD2/D226102.txt", &path, true));
        let text = String::from_utf8(archive.entries()[0].1.clone()).unwrap();
        let lines = text.lines().collect::<Vec<_>>();
        assert_eq!(
            lines[0],
            "21:13:04.951  0201FF18: FFFFFFFF D87E244C AD144BBF F293C16F  TM.x..%.l......o"
        );
        assert_eq!(
            lines[1],
            format!("              0201FF28: {}", crate::redact::KEY_MASK)
        );
        // The key goes whole before the name rule takes its first group.
        for group in ["BCDE", "9F7E", "6D4B"] {
            assert!(!lines[2].contains(group), "{}", lines[2]);
        }
    }

    #[test]
    fn each_helper_s_exit_codes_are_read_by_its_own_meaning() {
        assert_eq!(
            helper_verdict("installer UI helper", Some(23)),
            " (the installer stayed at its menu)"
        );
        assert_eq!(
            helper_verdict("downloader UI helper", Some(23)),
            " (the downloader showed an error message)"
        );
        assert_eq!(
            helper_verdict("downloader UI helper", Some(21)),
            " (no downloader window within ten minutes)"
        );
        assert_eq!(helper_verdict("downloader UI helper", Some(0)), "");
        assert_eq!(helper_verdict("wine BaseGameWrapper", Some(23)), "");
        assert!(plain_reason("installer-download-stalled").contains("connection"));
        assert!(!plain_reason("installer-download-rejected").contains("connection"));
    }

    #[test]
    fn utc_stamps_are_civil_dates() {
        assert_eq!(utc_stamp(0), "19700101T000000Z");
        assert_eq!(utc_stamp(1_790_590_141), "20260928T100901Z");
        assert_eq!(utc_time(951_782_400), "2000-02-29 00:00:00 UTC");
        assert_eq!(
            utc_time_millis(951_782_400_250),
            "2000-02-29 00:00:00.250 UTC"
        );
    }

    #[test]
    fn file_uris_escape_spaces_and_non_ascii() {
        assert_eq!(
            file_uri(Path::new("/home/a b/ü.tar.xz")),
            "file:///home/a%20b/%C3%BC.tar.xz"
        );
    }

    #[test]
    fn archives_never_replace_a_file_and_are_owner_only() {
        let fixture = TempDir::new().unwrap();
        let mut archive = ReportArchive::new(Redactor::default());
        archive.add_text("summary.txt", "hello");
        let first = archive.write_into(fixture.path(), 1_767_225_600).unwrap();
        let second = archive.write_into(fixture.path(), 1_767_225_600).unwrap();
        assert_ne!(first, second);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&first).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let names = read_archive(&first)
            .into_iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>();
        assert_eq!(names, ["summary.txt"]);
    }

    #[test]
    fn a_saved_report_has_an_android_section_only_on_an_android_host() {
        let fixture = TempDir::new().unwrap();
        let inputs = |android| ReportInputs {
            now: 1_790_000_100,
            paths: ReportPaths {
                managed_root: fixture.path().join("game"),
                diagnostics_root: fixture.path().join("logs"),
                ..ReportPaths::default()
            },
            android,
            ..ReportInputs::default()
        };
        let saved = |inputs: &ReportInputs| {
            let path = build(inputs)
                .write_into(fixture.path(), inputs.now)
                .unwrap();
            let entries = read_archive(&path);
            let report: Value = serde_json::from_str(&entries[1].1).unwrap();
            (entries[0].1.clone(), report)
        };

        let (summary, report) = saved(&inputs(Some(HostFacts {
            android_release: "13".to_owned(),
            sdk: 33,
            device_model: "Retroid Pocket Nova".to_owned(),
            gpu_name: "Adreno (TM) 740".to_owned(),
            chip_id: 0x4305_0a01,
            kernel_release: "5.15.153-android13-8".to_owned(),
            page_size: 4096,
            data_dir: "/data/data/com.pd2launcher.app".to_owned(),
        })));
        assert_eq!(
            report["android"],
            json!({
                "release": "13",
                "sdk": 33,
                "device": "Retroid Pocket Nova",
                "gpu": "Adreno (TM) 740",
                "chip_id": "0x43050a01",
                "kernel_release": "5.15.153-android13-8",
                "page_size": 4096,
            })
        );
        assert!(
            summary.contains("Android         Android 13 (SDK 33), Retroid Pocket Nova"),
            "{summary}"
        );
        for desktop in ["System ", "Session ", "GPU "] {
            assert!(
                !summary.lines().any(|line| line.starts_with(desktop)),
                "{desktop}in {summary}"
            );
        }

        let (summary, report) = saved(&inputs(None));
        assert!(report.get("android").is_none(), "{report}");
        assert!(!summary.contains("Android"), "{summary}");
        assert!(
            summary.lines().any(|line| line.starts_with("System ")),
            "{summary}"
        );
    }

    #[test]
    fn oversized_files_keep_their_end_within_the_caps() {
        let mut archive = ReportArchive::new(Redactor::default());
        let long = format!("{}\nTHE END", "line\n".repeat(FILE_CAP));
        archive.add_text("logs/a.txt", &long);
        let (_, bytes) = &archive.entries()[0];
        assert!(bytes.len() <= FILE_CAP + 64);
        assert!(bytes.ends_with(b"THE END"));
        for index in 0..64 {
            archive.add_text(&format!("logs/{index}.txt"), &long);
        }
        assert!(archive.total <= TOTAL_CAP);
        assert!(!archive.omitted.is_empty());
    }

    /// Every source a report reads is seeded with CD keys, the home path,
    /// the user name, account and character names, and URL tokens: none
    /// may reach the archive, and `.env.local` is never read.
    #[test]
    fn no_seeded_secret_survives_into_any_archive_entry() {
        const D2: &str = "7QK2PXRM4BVT9HCD";
        const LOD: &str = "C8WPD4NRJ7KX2MTFQ9HV6BZG3Y";
        const USER: &str = "canaryuser";
        const SECRETS: [&str; 7] = [
            "CanaryAcct",
            "CanaryChar",
            "CanaryGame",
            "SECRETTOKEN",
            "CanaryOwner",
            "hunter2pass",
            "ENVLOCALMARKER",
        ];
        let fixture = TempDir::new().unwrap();
        let home = fixture.path().join("home").join(USER);
        let launcher = home.join(".pd2launcher");
        let managed = launcher.join("game");
        let diagnostics = launcher.join("logs/diagnostics");
        let prefix = managed.join("prefix");
        let game = prefix.join("drive_c/Program Files (x86)/Diablo II");
        let project = game.join("ProjectD2");
        for directory in [&diagnostics, &project, &managed.join("cache/artifacts")] {
            std::fs::create_dir_all(directory).unwrap();
        }
        let home_text = home.to_string_lossy().into_owned();
        let grouped = |key: &str| {
            key.as_bytes()
                .chunks(4)
                .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
                .collect::<Vec<_>>()
                .join("-")
        };
        let everything = format!(
            "home {home_text}/x wine Z:{} key {D2} lod {} grouped {} lower {} \
             url https://{USER}:hunter2pass@mirror.example/wine.tar.xz?token=SECRETTOKEN#frag \
             C:\\users\\{USER}\\Saved Games D2_KEY={D2} --key1={LOD}\n",
            home_text.replace('/', "\\"),
            LOD,
            grouped(LOD),
            D2.to_ascii_lowercase(),
        );
        // The activity log as an older, unredacted Launcher might have left it.
        activity::append(
            &diagnostics,
            ActivityEntry {
                at: 1_790_000_000,
                job: Some(1),
                event: ActivityEvent::Process {
                    label: format!("wine {home_text}/Installer.exe"),
                    outcome: "exited".to_owned(),
                    exit_code: Some(1),
                    stdout: everything.clone(),
                    stderr: everything.clone(),
                },
                repeat: 1,
            },
        )
        .unwrap();
        activity::append(
            &diagnostics,
            ActivityEntry {
                at: 1_790_000_001,
                job: Some(1),
                event: ActivityEvent::StageFailed {
                    code: "base-game-key-rejected".to_owned(),
                    detail: everything.clone(),
                },
                repeat: 1,
            },
        )
        .unwrap();
        let identities = format!(
            "{everything}Logon Account: CanaryAcct\ncharacter=CanaryChar joined game CanaryGame\nname: \"CanaryChar\"\n"
        );
        for (path, text) in [
            (
                game.join("Diablo II Install Log.html"),
                format!("<p>{everything}</p>"),
            ),
            (game.join("BnetLog.txt"), identities.clone()),
            (project.join("update.log"), everything.clone()),
            (
                project.join("launcher-update-debug.log"),
                everything.clone(),
            ),
            (project.join("D220260928.txt"), identities.clone()),
            (
                prefix.join("user.reg"),
                format!(
                    "WINE REGISTRY Version 2\n[Software\\\\Blizzard Entertainment\\\\Diablo II] 1790000000\n\"InstallPath\"=\"C:\\\\users\\\\{USER}\\\\Diablo II\"\n\"Owner\"=\"CanaryOwner\"\n\"CDKey\"=\"{D2}\"\n\"Last BNet\"=\"CanaryAcct\"\n"
                ),
            ),
            (
                prefix.join(".pd2launcher-gecko-custom.json"),
                format!(
                    "{{\"url\":\"https://mirror.example/g.msi?token=SECRETTOKEN\",\"note\":\"{everything}\"}}"
                ),
            ),
            (managed.join(".pd2launcher-stage.json"), everything.clone()),
            (
                home.join(".env.local"),
                format!("D2_KEY={D2}\nENVLOCALMARKER=1\n"),
            ),
            (managed.join(".env.local"), "ENVLOCALMARKER=1\n".to_owned()),
        ] {
            std::fs::write(path, text).unwrap();
        }
        let mut store = crate::diagnostics::DiagnosticStore::open(
            diagnostics.join("recent.json"),
            launcher.join("logs/exports"),
        )
        .unwrap();
        let id = store
            .start_job(
                JobKind::InstallBaseGame,
                crate::operation::WorkPhase::BaseGameInstall,
            )
            .unwrap();
        store.finish_job(id, JobStatus::Failed).unwrap();
        let inputs = ReportInputs {
            now: 1_790_000_100,
            redactor: Redactor::new(Some(&home), Some(USER), [D2.to_owned(), LOD.to_owned()]),
            build: BuildInfo {
                version: "0.1.0",
                ..BuildInfo::default()
            },
            paths: ReportPaths {
                managed_root: managed.clone(),
                diagnostics_root: diagnostics.clone(),
                default_location: true,
                game_folder: None,
            },
            host: None,
            android: None,
            audit: Some(Err(format!("unavailable at {home_text}"))),
            downloads: Some(format!("Incomplete {everything}")),
            renderer_probe: Some(Some(format!("llvmpipe {everything}"))),
            active_job: None,
            jobs: store.recent_jobs().cloned().collect(),
            events: vec![DiagnosticCode::BaseGameKeyRejected],
            custom_source_records: Vec::new(),
            sources: vec![SourceRow {
                artifact: "Wine",
                custom: true,
                host: "mirror.example".to_owned(),
            }],
            settings: Some(json!({"note": everything})),
            installer_choice: Some("download from Blizzard"),
        };
        let mut archive = build(&inputs);
        archive.add_text("logs/launcher-history.json", &everything);
        let out = fixture.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let path = archive.write_into(&out, inputs.now).unwrap();
        let entries = read_archive(&path);
        assert_eq!(entries[0].0, "summary.txt");
        assert_eq!(entries[1].0, "report.json");
        let report: Value = serde_json::from_str(&entries[1].1).unwrap();
        assert_eq!(report["latest_failure"]["code"], "unrecorded");
        let names = entries
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>();
        for expected in [
            "logs/failed-processes.txt",
            "logs/launcher-activity.jsonl",
            "logs/game/Diablo II Install Log.html",
            "logs/game/BnetLog.txt",
            "logs/ProjectD2/update.log",
            "logs/ProjectD2/launcher-update-debug.log",
            "logs/ProjectD2/D220260928.txt",
            "logs/registry-diablo-ii.txt",
            "logs/launcher-history.json",
        ] {
            assert!(
                names.contains(&expected),
                "{expected} missing from {names:?}"
            );
        }
        let registry = &entries
            .iter()
            .find(|(name, _)| name == "logs/registry-diablo-ii.txt")
            .unwrap()
            .1;
        assert!(registry.contains("InstallPath"), "{registry}");
        for (name, text) in &entries {
            let lower = text.to_ascii_lowercase();
            for secret in [D2, LOD, &grouped(LOD), &grouped(D2), USER, &home_text]
                .into_iter()
                .chain(SECRETS)
            {
                assert!(
                    !lower.contains(&secret.to_ascii_lowercase()),
                    "{secret} survived in {name}:\n{text}"
                );
            }
        }
    }

    pub(crate) fn read_archive(path: &Path) -> Vec<(String, String)> {
        let file = File::open(path).unwrap();
        let mut archive = tar::Archive::new(xz2::read::XzDecoder::new(file));
        archive
            .entries()
            .unwrap()
            .map(|entry| {
                let mut entry = entry.unwrap();
                let name = entry.path().unwrap().to_string_lossy().into_owned();
                let mut text = String::new();
                entry.read_to_string(&mut text).unwrap();
                (name, text)
            })
            .collect()
    }
}
