//! Bounded, redacted durable histories for Launcher jobs and non-job events.

use std::{
    collections::VecDeque,
    fs::File,
    io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    activity::ActivityEvent,
    operation::{ProgressDimension, ProgressUnit, WorkPhase, WorkProgress},
    persistence::{atomic_write, read_bounded_regular},
    safe_fs::Directory,
    sources::{DownloadArtifact, valid_diagnostic_host},
};

const HISTORY_SCHEMA: u32 = 3;
const PREVIOUS_HISTORY_SCHEMA: u32 = 2;
const LEGACY_HISTORY_SCHEMA: u32 = 1;
const COMPLETED_JOB_LIMIT: usize = 10;
const EVENT_LIMIT: usize = 64;
const JOB_EVIDENCE_LIMIT: usize = 32;
const CUSTOM_SOURCE_LIMIT: usize = 32;
const DIAGNOSTIC_LIMIT: u64 = 20 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum DiagnosticCode {
    LayoutOpened,
    AuditCompleted,
    /// An audit that found the installation Ready, so a headless run's
    /// history shows the outcome and not only that an audit ran.
    AuditReady,
    UpdateCheckFailed,
    GameLaunchFailed,
    /// Diablo II's installer rejected the CD key it was given.
    BaseGameKeyRejected,
    /// Lord of Destruction's installer rejected the CD key it was given.
    ExpansionKeyRejected,
    /// A Blizzard installer showed an error dialog that names no key.
    VendorInstallerError,
    /// A Blizzard installer stayed at its main menu instead of installing.
    VendorInstallerMenu,
    DiagnosticsExported,
    /// The Android host wrote D2GL's configuration, or its screen values or
    /// graphics profile, before a launch.
    DisplayConfigSeeded,
    /// The Android host could not read D2GL's configuration or set its
    /// values in it, so it stayed as it was.
    DisplayConfigSkipped,
    /// The official launcher left the client out of date and its update
    /// could not be accepted (the protected-file inventory was incomplete).
    OfficialClientNotReconciled,
    /// The XDG layout of an earlier release was moved into the home (kept so
    /// older records still read; the Launcher no longer moves that layout).
    LegacyLayoutMigrated,
    /// As above, but the installation stays where it was and the pointer
    /// names it (another filesystem, or a location the user chose).
    LegacyInstallationKeptInPlace,
    /// Both the home and an earlier release's XDG layout exist; the home is
    /// used and the legacy layout is left untouched (kept so older records
    /// still read).
    LegacyLayoutIgnored,
    /// Wine processes of an earlier session held the installation lease
    /// without their wineserver and were stopped.
    OrphanedWineStopped,
}

impl DiagnosticCode {
    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::LayoutOpened => "Launcher-owned storage opened",
            Self::AuditCompleted => "Installation evidence audited",
            Self::AuditReady => "Installation evidence audited: Ready",
            Self::UpdateCheckFailed => "Update check did not complete",
            Self::GameLaunchFailed => "Game client did not start",
            Self::BaseGameKeyRejected => "Diablo II's installer rejected the CD key",
            Self::ExpansionKeyRejected => "Lord of Destruction's installer rejected the CD key",
            Self::VendorInstallerError => "A Blizzard installer stopped with an error dialog",
            Self::VendorInstallerMenu => {
                "A Blizzard installer opened its menu instead of installing"
            }
            Self::DiagnosticsExported => "Redacted diagnostics exported",
            Self::DisplayConfigSeeded => "D2GL settings written for the Android app",
            Self::DisplayConfigSkipped => {
                "D2GL settings were left unchanged because its configuration could not be read or edited"
            }
            Self::OfficialClientNotReconciled => {
                "The official launcher's client update was not accepted: the protected-file inventory was incomplete"
            }
            Self::LegacyLayoutMigrated => "Launcher data moved into ~/.pd2launcher",
            Self::LegacyInstallationKeptInPlace => {
                "Launcher data moved into ~/.pd2launcher; the installation stays where it was"
            }
            Self::LegacyLayoutIgnored => {
                "An earlier Launcher's data in the XDG folders was left untouched because ~/.pd2launcher already exists"
            }
            Self::OrphanedWineStopped => {
                "Wine processes left from an earlier game session were stopped so the installation could be used"
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub(crate) struct JobId(u64);

impl JobId {
    pub(crate) const fn value(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum JobKind {
    /// An all-in-one install from launchers before the separate Download
    /// step (kept so older records still read).
    Install,
    /// Step 3: download and verify everything.
    Download,
    /// Step 4: install or import Diablo II.
    InstallBaseGame,
    /// Step 5: install Project Diablo 2.
    InstallProjectDiablo,
    Update,
    Repair,
    /// A session of the official PD2 launcher, the fallback when this
    /// Launcher cannot update or start the game.
    OfficialLauncher,
    /// A confirmed Start over.
    Reset,
    /// Choosing or changing the install location.
    Relocation,
    /// Removing what this Launcher's own downloads made, for an import.
    DiscardDownloads,
}

impl JobKind {
    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::Install => "Managed installation",
            Self::Download => "Download and verify",
            Self::InstallBaseGame => "Install Diablo II",
            Self::InstallProjectDiablo => "Install Project Diablo 2",
            Self::Update => "Verified update",
            Self::Repair => "Transaction repair",
            Self::OfficialLauncher => "Official PD2 launcher session",
            Self::Reset => "Start over",
            Self::Relocation => "Install location change",
            Self::DiscardDownloads => "Removal of the downloads for an import",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum JobStatus {
    Running,
    SafeStopping,
    Succeeded,
    Failed,
    Cancelled,
    Indeterminate,
}

impl JobStatus {
    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::SafeStopping => "Stopping at a transaction-safe boundary",
            Self::Succeeded => "Succeeded",
            Self::Failed => "Failed",
            Self::Cancelled => "Cancelled safely",
            Self::Indeterminate => "Operation outcome indeterminate; Repair is required",
        }
    }

    const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::Indeterminate
        )
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct JobProgress {
    unit: ProgressUnit,
    completed: u64,
    total: Option<u64>,
    #[serde(default)]
    file_bytes: Option<JobProgressDimension>,
    #[serde(default)]
    overall: Option<JobProgressDimension>,
    #[serde(default)]
    verification: Option<JobProgressDimension>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct JobProgressDimension {
    unit: ProgressUnit,
    completed: u64,
    total: Option<u64>,
}

impl JobProgress {
    fn new(unit: ProgressUnit, completed: u64, total: Option<u64>) -> Self {
        let total = total.map(|total| total.max(1));
        Self {
            unit,
            completed: total.map_or(completed, |total| completed.min(total)),
            total,
            file_bytes: None,
            overall: None,
            verification: None,
        }
    }

    fn from_work(progress: WorkProgress) -> Option<Self> {
        let primary = progress
            .file_bytes()
            .or(progress.overall())
            .or(progress.verification())?;
        Some(Self {
            unit: primary.unit(),
            completed: primary.completed(),
            total: primary.total(),
            file_bytes: progress.file_bytes().map(JobProgressDimension::from),
            overall: progress.overall().map(JobProgressDimension::from),
            verification: progress.verification().map(JobProgressDimension::from),
        })
    }

    pub(crate) const fn unit(self) -> ProgressUnit {
        self.unit
    }

    pub(crate) const fn completed(self) -> u64 {
        self.completed
    }

    pub(crate) const fn total(self) -> Option<u64> {
        self.total
    }

    pub(crate) const fn file_bytes(self) -> Option<JobProgressDimension> {
        self.file_bytes
    }

    pub(crate) const fn overall(self) -> Option<JobProgressDimension> {
        self.overall
    }

    pub(crate) const fn verification(self) -> Option<JobProgressDimension> {
        self.verification
    }
}

impl From<ProgressDimension> for JobProgressDimension {
    fn from(progress: ProgressDimension) -> Self {
        Self {
            unit: progress.unit(),
            completed: progress.completed(),
            total: progress.total(),
        }
    }
}

impl JobProgressDimension {
    pub(crate) const fn unit(self) -> ProgressUnit {
        self.unit
    }

    pub(crate) const fn completed(self) -> u64 {
        self.completed
    }

    pub(crate) const fn total(self) -> Option<u64> {
        self.total
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum JobEvidence {
    Started,
    PhaseChanged,
    ProgressObserved,
    SafeStopRequested,
    Succeeded,
    Failed,
    Cancelled,
    Indeterminate,
    Interrupted,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct JobRecord {
    id: JobId,
    kind: JobKind,
    status: JobStatus,
    phase: WorkPhase,
    progress: Option<JobProgress>,
    evidence: VecDeque<JobEvidence>,
}

impl JobRecord {
    pub(crate) const fn id(&self) -> JobId {
        self.id
    }

    pub(crate) const fn kind(&self) -> JobKind {
        self.kind
    }

    pub(crate) const fn status(&self) -> JobStatus {
        self.status
    }

    pub(crate) const fn phase(&self) -> WorkPhase {
        self.phase
    }

    pub(crate) const fn progress(&self) -> Option<JobProgress> {
        self.progress
    }

    fn push_evidence(&mut self, evidence: JobEvidence) {
        if self.evidence.back() != Some(&evidence) {
            self.evidence.push_back(evidence);
        }
        while self.evidence.len() > JOB_EVIDENCE_LIMIT {
            self.evidence.pop_front();
        }
    }
}

/// One job's use of a custom, unverified download source. Only the
/// artifact and a validated host name are kept: never a path, query string,
/// token, or credential.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CustomSourceRecord {
    job: JobId,
    artifact: DownloadArtifact,
    host: String,
}

impl CustomSourceRecord {
    pub(crate) const fn job(&self) -> JobId {
        self.job
    }

    pub(crate) const fn artifact(&self) -> DownloadArtifact {
        self.artifact
    }

    pub(crate) fn host(&self) -> &str {
        &self.host
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct DiagnosticHistory {
    schema: u32,
    next_job_id: u64,
    active_job: Option<JobRecord>,
    completed_jobs: VecDeque<JobRecord>,
    events: VecDeque<DiagnosticCode>,
    #[serde(default, skip_serializing_if = "VecDeque::is_empty")]
    custom_sources: VecDeque<CustomSourceRecord>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreviousDiagnosticHistory {
    schema: u32,
    next_job_id: u64,
    active_job: Option<PreviousJobRecord>,
    completed_jobs: VecDeque<PreviousJobRecord>,
    events: VecDeque<DiagnosticCode>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreviousJobRecord {
    id: JobId,
    kind: JobKind,
    status: JobStatus,
    phase: WorkPhase,
    progress: Option<PreviousJobProgress>,
    evidence: VecDeque<JobEvidence>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreviousJobProgress {
    completed: u64,
    total: u64,
}

impl Default for DiagnosticHistory {
    fn default() -> Self {
        Self {
            schema: HISTORY_SCHEMA,
            next_job_id: 1,
            active_job: None,
            completed_jobs: VecDeque::new(),
            events: VecDeque::new(),
            custom_sources: VecDeque::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyHistory {
    schema: u32,
    active: Option<LegacyDiagnosticCode>,
    completed: VecDeque<LegacyDiagnosticCode>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum LegacyDiagnosticCode {
    LayoutOpened,
    AuditCompleted,
    InstallationActive,
    UpdateActive,
    RepairActive,
    InterruptedActivity,
    RepairCompleted,
    UpdateCheckFailed,
    InstallFailed,
    UpdateFailed,
    GameLaunchFailed,
    DiagnosticsExported,
}

impl LegacyDiagnosticCode {
    fn event(self) -> Option<DiagnosticCode> {
        match self {
            Self::LayoutOpened => Some(DiagnosticCode::LayoutOpened),
            Self::AuditCompleted => Some(DiagnosticCode::AuditCompleted),
            Self::UpdateCheckFailed => Some(DiagnosticCode::UpdateCheckFailed),
            Self::GameLaunchFailed => Some(DiagnosticCode::GameLaunchFailed),
            Self::DiagnosticsExported => Some(DiagnosticCode::DiagnosticsExported),
            Self::InstallationActive
            | Self::UpdateActive
            | Self::RepairActive
            | Self::InterruptedActivity
            | Self::RepairCompleted
            | Self::InstallFailed
            | Self::UpdateFailed => None,
        }
    }

    fn active_job(self) -> Option<(JobKind, WorkPhase)> {
        match self {
            Self::InstallationActive => Some((JobKind::Install, WorkPhase::RuntimeAcquisition)),
            Self::UpdateActive => Some((JobKind::Update, WorkPhase::ClientManifest)),
            Self::RepairActive => Some((JobKind::Repair, WorkPhase::Repair)),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub(crate) struct DiagnosticStore {
    history_file: PathBuf,
    export_root: PathBuf,
    history_lock: File,
    owner_lock: File,
    owns_active_job: bool,
    history: DiagnosticHistory,
}

impl DiagnosticStore {
    pub(crate) fn open(
        history_file: impl Into<PathBuf>,
        export_root: impl Into<PathBuf>,
    ) -> io::Result<Self> {
        let history_file = history_file.into();
        let parent = validated_parent(&history_file)?;
        let history_lock = open_lock(&parent.join("recent.lock"))?;
        let owner_lock = open_lock(&parent.join("active-job-owner.lock"))?;
        File::lock(&history_lock)?;
        let loaded = (|| {
            let history = match read_bounded_regular(&history_file, DIAGNOSTIC_LIMIT) {
                Ok(bytes) => decode_history(&bytes)?,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    DiagnosticHistory::default()
                }
                Err(error) => return Err(error),
            };
            validate_history(&history)?;
            Ok(history)
        })();
        let unlock = File::unlock(&history_lock);
        let history = loaded.and_then(|history| unlock.map(|()| history))?;
        Ok(Self {
            history_file,
            export_root: export_root.into(),
            history_lock,
            owner_lock,
            owns_active_job: false,
            history,
        })
    }

    pub(crate) fn record_event(&mut self, code: DiagnosticCode) -> io::Result<()> {
        self.lock_and_reload()?;
        self.history.events.push_back(code);
        while self.history.events.len() > EVENT_LIMIT {
            self.history.events.pop_front();
        }
        self.persist_and_unlock()
    }

    /// Record that job `id` downloads with custom sources. Hosts that are not
    /// a bounded DNS name or IP literal are stored as `unknown-host`.
    pub(crate) fn record_custom_sources(
        &mut self,
        id: JobId,
        sources: impl IntoIterator<Item = (DownloadArtifact, String)>,
    ) -> io::Result<()> {
        self.lock_and_reload()?;
        for (artifact, host) in sources {
            let host = if valid_diagnostic_host(&host) {
                host
            } else {
                "unknown-host".to_owned()
            };
            self.history.custom_sources.push_back(CustomSourceRecord {
                job: id,
                artifact,
                host,
            });
        }
        while self.history.custom_sources.len() > CUSTOM_SOURCE_LIMIT {
            self.history.custom_sources.pop_front();
        }
        self.persist_and_unlock()
    }

    pub(crate) fn recent_custom_sources(
        &self,
    ) -> impl DoubleEndedIterator<Item = &CustomSourceRecord> {
        self.history.custom_sources.iter()
    }

    pub(crate) fn start_job(&mut self, kind: JobKind, phase: WorkPhase) -> io::Result<JobId> {
        self.lock_and_reload()?;
        if self.history.active_job.is_some() {
            self.unlock_history()?;
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "a durable job is already active",
            ));
        }
        match File::try_lock(&self.owner_lock) {
            Ok(()) => self.owns_active_job = true,
            Err(std::fs::TryLockError::WouldBlock) => {
                self.unlock_history()?;
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "another Launcher owns the durable job",
                ));
            }
            Err(std::fs::TryLockError::Error(error)) => {
                self.unlock_history()?;
                return Err(error);
            }
        }
        let id = JobId(self.history.next_job_id);
        let Some(next_job_id) = self.history.next_job_id.checked_add(1) else {
            self.unlock_history()?;
            self.owns_active_job = false;
            File::unlock(&self.owner_lock)?;
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "diagnostic job id exhausted",
            ));
        };
        self.history.next_job_id = next_job_id;
        self.history.active_job = Some(JobRecord {
            id,
            kind,
            status: JobStatus::Running,
            phase,
            progress: None,
            evidence: VecDeque::from([JobEvidence::Started]),
        });
        if let Err(error) = self.persist_and_unlock() {
            self.owns_active_job = false;
            let _ = File::unlock(&self.owner_lock);
            return Err(error);
        }
        crate::activity::set_job(Some(id.value()));
        crate::activity::record(ActivityEvent::JobStarted { kind, phase });
        Ok(id)
    }

    pub(crate) fn update_job(
        &mut self,
        id: JobId,
        phase: WorkPhase,
        unit: Option<ProgressUnit>,
        completed: Option<u64>,
        total: Option<u64>,
    ) -> io::Result<()> {
        self.lock_and_reload()?;
        if !self.owns_active_job {
            self.unlock_history()?;
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "durable job owner lease was not held",
            ));
        }
        let result = (|| {
            let job = self.active_job_mut(id)?;
            if job.phase != phase {
                job.phase = phase;
                job.push_evidence(JobEvidence::PhaseChanged);
                crate::activity::record(ActivityEvent::Phase { phase });
            }
            job.progress = match (unit, completed) {
                (Some(unit), Some(completed)) => Some(JobProgress::new(unit, completed, total)),
                _ => None,
            };
            if job.progress.is_some() {
                job.push_evidence(JobEvidence::ProgressObserved);
            }
            self.persist_locked()
        })();
        let unlock = self.unlock_history();
        result.and(unlock)
    }

    pub(crate) fn update_job_progress(
        &mut self,
        id: JobId,
        progress: WorkProgress,
    ) -> io::Result<()> {
        self.lock_and_reload()?;
        if !self.owns_active_job {
            self.unlock_history()?;
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "durable job owner lease was not held",
            ));
        }
        let result = (|| {
            let job = self.active_job_mut(id)?;
            if job.phase != progress.phase() {
                job.phase = progress.phase();
                job.push_evidence(JobEvidence::PhaseChanged);
                crate::activity::record(ActivityEvent::Phase {
                    phase: progress.phase(),
                });
            }
            job.progress = JobProgress::from_work(progress);
            if job.progress.is_some() {
                job.push_evidence(JobEvidence::ProgressObserved);
            }
            self.persist_locked()
        })();
        let unlock = self.unlock_history();
        result.and(unlock)
    }

    pub(crate) fn request_safe_stop(&mut self, id: JobId) -> io::Result<()> {
        self.lock_and_reload()?;
        if !self.owns_active_job {
            self.unlock_history()?;
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "durable job owner lease was not held",
            ));
        }
        let result = (|| {
            let job = self.active_job_mut(id)?;
            job.status = JobStatus::SafeStopping;
            job.push_evidence(JobEvidence::SafeStopRequested);
            self.persist_locked()
        })();
        let unlock = self.unlock_history();
        result.and(unlock)
    }

    pub(crate) fn finish_job(&mut self, id: JobId, status: JobStatus) -> io::Result<()> {
        self.lock_and_reload()?;
        if !self.owns_active_job {
            self.unlock_history()?;
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "durable job owner lease was not held",
            ));
        }
        if !status.is_terminal() {
            self.unlock_history()?;
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "job completion status was not terminal",
            ));
        }
        let Some(mut job) = self.history.active_job.take() else {
            self.unlock_history()?;
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "durable job was not active",
            ));
        };
        if job.id != id {
            self.history.active_job = Some(job);
            self.unlock_history()?;
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "durable job id did not match",
            ));
        }
        job.status = status;
        crate::activity::record_for(
            job.id.value(),
            ActivityEvent::JobFinished {
                kind: job.kind,
                status,
            },
        );
        crate::activity::set_job(None);
        job.push_evidence(match status {
            JobStatus::Succeeded => JobEvidence::Succeeded,
            JobStatus::Failed => JobEvidence::Failed,
            JobStatus::Cancelled => JobEvidence::Cancelled,
            JobStatus::Indeterminate => JobEvidence::Indeterminate,
            JobStatus::Running | JobStatus::SafeStopping => unreachable!(),
        });
        self.history.completed_jobs.push_back(job);
        self.retain_completed();
        let result = self.persist_and_unlock();
        self.owns_active_job = false;
        let unlock = File::unlock(&self.owner_lock);
        result.and(unlock)
    }

    pub(crate) fn recover_interrupted(&mut self) -> io::Result<()> {
        self.lock_and_reload()?;
        if self.history.active_job.is_none() || self.owns_active_job {
            return self.unlock_history();
        }
        match File::try_lock(&self.owner_lock) {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return self.unlock_history(),
            Err(std::fs::TryLockError::Error(error)) => {
                self.unlock_history()?;
                return Err(error);
            }
        }
        if let Some(mut job) = self.history.active_job.take() {
            job.status = JobStatus::Failed;
            job.push_evidence(JobEvidence::Interrupted);
            crate::activity::record_for(
                job.id.value(),
                ActivityEvent::JobFailed {
                    code: "launcher-stopped-during-job".to_owned(),
                },
            );
            crate::activity::record_for(
                job.id.value(),
                ActivityEvent::JobFinished {
                    kind: job.kind,
                    status: JobStatus::Failed,
                },
            );
            self.history.completed_jobs.push_back(job);
            self.retain_completed();
            let result = self.persist_and_unlock();
            let unlock = File::unlock(&self.owner_lock);
            return result.and(unlock);
        }
        self.unlock_history()
    }

    pub(crate) fn active_job(&self) -> Option<&JobRecord> {
        self.history.active_job.as_ref()
    }

    pub(crate) fn recent_jobs(&self) -> impl DoubleEndedIterator<Item = &JobRecord> {
        self.history.completed_jobs.iter()
    }

    pub(crate) fn recent_events(&self) -> impl DoubleEndedIterator<Item = DiagnosticCode> + '_ {
        self.history.events.iter().copied()
    }

    pub(crate) fn export(&mut self) -> io::Result<PathBuf> {
        let result: io::Result<PathBuf> = (|| {
            Directory::ensure_absolute(&self.export_root)?;
            let export = self.export_root.join("pd2-launcher-diagnostics.json");
            atomic_write(&export, &self.encoded()?)?;
            Ok(export)
        })();
        let export = result?;
        self.record_event(DiagnosticCode::DiagnosticsExported)?;
        Ok(export)
    }

    fn active_job_mut(&mut self, id: JobId) -> io::Result<&mut JobRecord> {
        let job =
            self.history.active_job.as_mut().ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, "durable job was not active")
            })?;
        if job.id != id || job.status.is_terminal() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "durable job transition was invalid",
            ));
        }
        Ok(job)
    }

    fn retain_completed(&mut self) {
        while self.history.completed_jobs.len() > COMPLETED_JOB_LIMIT {
            self.history.completed_jobs.pop_front();
        }
    }

    fn lock_and_reload(&mut self) -> io::Result<()> {
        File::lock(&self.history_lock)?;
        let result = (|| {
            self.history = match read_bounded_regular(&self.history_file, DIAGNOSTIC_LIMIT) {
                Ok(bytes) => decode_history(&bytes)?,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    DiagnosticHistory::default()
                }
                Err(error) => return Err(error),
            };
            validate_history(&self.history)
        })();
        if result.is_err() {
            let _ = File::unlock(&self.history_lock);
        }
        result
    }

    fn unlock_history(&self) -> io::Result<()> {
        File::unlock(&self.history_lock)
    }

    fn persist_and_unlock(&self) -> io::Result<()> {
        let result = self.persist_locked();
        let unlock = self.unlock_history();
        result.and(unlock)
    }

    fn persist_locked(&self) -> io::Result<()> {
        let parent = self.history_file.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "diagnostic parent unavailable")
        })?;
        Directory::ensure_absolute(parent)?;
        atomic_write(&self.history_file, &self.encoded()?)
    }

    fn encoded(&self) -> io::Result<Vec<u8>> {
        let bytes = serde_json::to_vec(&self.history)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "diagnostic encode failed"))?;
        if bytes.len() as u64 > DIAGNOSTIC_LIMIT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "diagnostic history exceeded its cap",
            ));
        }
        Ok(bytes)
    }
}

fn decode_history(bytes: &[u8]) -> io::Result<DiagnosticHistory> {
    let mut value = serde_json::from_slice::<serde_json::Value>(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "diagnostic schema missing"))?;
    let schema = value
        .get("schema")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "diagnostic schema missing"))?;
    forget_retired_patch_stage(&mut value);
    match schema as u32 {
        HISTORY_SCHEMA => serde_json::from_value(value).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "diagnostic history was invalid")
        }),
        PREVIOUS_HISTORY_SCHEMA => {
            let previous: PreviousDiagnosticHistory =
                serde_json::from_value(value).map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "previous diagnostic history was invalid",
                    )
                })?;
            migrate_previous(previous)
        }
        LEGACY_HISTORY_SCHEMA => {
            let legacy: LegacyHistory = serde_json::from_slice(bytes).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "legacy diagnostic history was invalid",
                )
            })?;
            migrate_legacy(legacy)
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "diagnostic history schema was unsupported",
        )),
    }
}

/// History written while the 1.14d patch was a stage still reads: a job's
/// patch phase is shown as the step it belonged to, and a record of a custom
/// patch source is dropped.
fn forget_retired_patch_stage(history: &mut serde_json::Value) {
    for key in ["active_job", "completed_jobs"] {
        let jobs: Vec<&mut serde_json::Value> = match history.get_mut(key) {
            Some(serde_json::Value::Array(jobs)) => jobs.iter_mut().collect(),
            Some(job) => vec![job],
            None => Vec::new(),
        };
        for job in jobs {
            if let Some(phase) = job.get_mut("phase") {
                match phase.as_str() {
                    Some("patch-download") => *phase = "base-game-download".into(),
                    Some("patch") => *phase = "base-game-install".into(),
                    _ => {}
                }
            }
        }
    }
    if let Some(serde_json::Value::Array(sources)) = history.get_mut("custom_sources") {
        sources.retain(|source| {
            source.get("artifact").and_then(serde_json::Value::as_str) != Some("patch")
        });
    }
}

fn migrate_previous(previous: PreviousDiagnosticHistory) -> io::Result<DiagnosticHistory> {
    if previous.schema != PREVIOUS_HISTORY_SCHEMA {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "previous diagnostic schema was invalid",
        ));
    }
    let migrate_job = |job: PreviousJobRecord| JobRecord {
        id: job.id,
        kind: job.kind,
        status: job.status,
        phase: job.phase,
        progress: job.progress.map(|progress| JobProgress {
            unit: ProgressUnit::Stages,
            completed: progress.completed,
            total: Some(progress.total),
            file_bytes: None,
            overall: None,
            verification: None,
        }),
        evidence: job.evidence,
    };
    let history = DiagnosticHistory {
        schema: HISTORY_SCHEMA,
        next_job_id: previous.next_job_id,
        active_job: previous.active_job.map(migrate_job),
        completed_jobs: previous
            .completed_jobs
            .into_iter()
            .map(migrate_job)
            .collect(),
        events: previous.events,
        custom_sources: VecDeque::new(),
    };
    validate_history(&history)?;
    Ok(history)
}

fn migrate_legacy(legacy: LegacyHistory) -> io::Result<DiagnosticHistory> {
    if legacy.schema != LEGACY_HISTORY_SCHEMA || legacy.completed.len() > COMPLETED_JOB_LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "legacy diagnostic retention was invalid",
        ));
    }
    let mut history = DiagnosticHistory::default();
    for code in legacy.completed {
        if let Some(event) = code.event() {
            history.events.push_back(event);
        }
    }
    if let Some((kind, phase)) = legacy.active.and_then(LegacyDiagnosticCode::active_job) {
        history.active_job = Some(JobRecord {
            id: JobId(1),
            kind,
            status: JobStatus::Running,
            phase,
            progress: None,
            evidence: VecDeque::from([JobEvidence::Started]),
        });
        history.next_job_id = 2;
    } else if let Some(event) = legacy.active.and_then(LegacyDiagnosticCode::event) {
        history.events.push_back(event);
    }
    while history.events.len() > EVENT_LIMIT {
        history.events.pop_front();
    }
    Ok(history)
}

fn validate_history(history: &DiagnosticHistory) -> io::Result<()> {
    let valid_dimension = |progress: JobProgressDimension| {
        progress
            .total()
            .is_none_or(|total| total > 0 && progress.completed() <= total)
    };
    let valid_job = |job: &JobRecord| {
        job.id.value() > 0
            && job.id.value() < history.next_job_id
            && !job.evidence.is_empty()
            && job.evidence.len() <= JOB_EVIDENCE_LIMIT
            && job.progress.is_none_or(|progress| {
                progress
                    .total()
                    .is_none_or(|total| total > 0 && progress.completed() <= total)
                    && progress.file_bytes().is_none_or(valid_dimension)
                    && progress.overall().is_none_or(valid_dimension)
                    && progress.verification().is_none_or(valid_dimension)
            })
    };
    let valid = history.schema == HISTORY_SCHEMA
        && history.next_job_id > 0
        && history.completed_jobs.len() <= COMPLETED_JOB_LIMIT
        && history.events.len() <= EVENT_LIMIT
        && history.custom_sources.len() <= CUSTOM_SOURCE_LIMIT
        && history.custom_sources.iter().all(|record| {
            record.job.value() > 0
                && record.job.value() < history.next_job_id
                && valid_diagnostic_host(&record.host)
        })
        && history
            .active_job
            .as_ref()
            .is_none_or(|job| !job.status.is_terminal() && valid_job(job))
        && history
            .completed_jobs
            .iter()
            .all(|job| job.status.is_terminal() && valid_job(job));
    valid.then_some(()).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "diagnostic history schema or transitions were invalid",
        )
    })
}

fn validated_parent(path: &Path) -> io::Result<PathBuf> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "diagnostic parent unavailable")
    })?;
    Directory::ensure_absolute(parent)?;
    Ok(parent.to_owned())
}

fn open_lock(path: &Path) -> io::Result<File> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "diagnostic lock parent unavailable",
        )
    })?;
    let name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "diagnostic lock name unavailable",
        )
    })?;
    let file = Directory::open(parent)?.open_or_create_file(name)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "diagnostic lock was not a regular file",
        ));
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
        fs::create_dir_all(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_tree(&entry.path(), &target);
            } else {
                fs::copy(entry.path(), &target).unwrap();
            }
        }
    }

    /// A launcher folder copied to another place or computer gives every file
    /// a new identity; its history still opens and keeps recording.
    #[test]
    fn copied_logs_open_recover_and_keep_their_history() {
        let fixture = TempDir::new().unwrap();
        let original = fixture.path().join("original");
        let mut store = DiagnosticStore::open(
            original.join("diagnostics/recent.json"),
            original.join("exports"),
        )
        .unwrap();
        let id = store
            .start_job(JobKind::Update, WorkPhase::ClientManifest)
            .unwrap();
        store.finish_job(id, JobStatus::Succeeded).unwrap();
        store.record_event(DiagnosticCode::AuditReady).unwrap();
        drop(store);

        let copy = fixture.path().join("copy");
        copy_tree(&original, &copy);
        let mut copied =
            DiagnosticStore::open(copy.join("diagnostics/recent.json"), copy.join("exports"))
                .unwrap();
        copied.recover_interrupted().unwrap();
        assert_eq!(copied.recent_jobs().count(), 1);
        copied.record_event(DiagnosticCode::LayoutOpened).unwrap();
        let reopened =
            DiagnosticStore::open(copy.join("diagnostics/recent.json"), copy.join("exports"))
                .unwrap();
        assert_eq!(reopened.recent_jobs().count(), 1);
    }

    #[test]
    fn jobs_persist_typed_transitions_progress_and_only_ten_terminal_records() {
        let fixture = TempDir::new().unwrap();
        let history = fixture.path().join("diagnostics/recent.json");
        let exports = fixture.path().join("exports");
        let mut store = DiagnosticStore::open(&history, &exports).unwrap();
        for index in 0..12 {
            let id = store
                .start_job(JobKind::Update, WorkPhase::ClientManifest)
                .unwrap();
            store
                .update_job(
                    id,
                    WorkPhase::ClientDownload,
                    Some(ProgressUnit::Files),
                    Some(index),
                    Some(12),
                )
                .unwrap();
            store.finish_job(id, JobStatus::Succeeded).unwrap();
        }
        store
            .record_event(DiagnosticCode::ExpansionKeyRejected)
            .unwrap();
        store.record_event(DiagnosticCode::AuditReady).unwrap();
        store.record_event(DiagnosticCode::AuditCompleted).unwrap();

        let reopened = DiagnosticStore::open(&history, &exports).unwrap();
        assert!(reopened.active_job().is_none());
        assert_eq!(reopened.recent_jobs().count(), 10);
        let latest = reopened.recent_jobs().last().unwrap();
        assert_eq!(latest.kind(), JobKind::Update);
        assert_eq!(latest.status(), JobStatus::Succeeded);
        assert_eq!(latest.phase(), WorkPhase::ClientDownload);
        assert_eq!(latest.progress().unwrap().total(), Some(12));
        assert_eq!(
            reopened.recent_events().last(),
            Some(DiagnosticCode::AuditCompleted)
        );
        assert_eq!(
            reopened.recent_events().rev().nth(1),
            Some(DiagnosticCode::AuditReady)
        );
        let encoded = fs::read_to_string(history).unwrap();
        assert!(encoded.contains("\"audit-ready\""));
        assert!(encoded.contains("\"expansion-key-rejected\""));
        assert!(!encoded.contains("environment"));
        assert!(!encoded.contains("command"));
        assert!(!encoded.contains("product"));
    }

    #[test]
    fn indeterminate_job_outcome_round_trips_distinct_from_failure_and_cancellation() {
        let fixture = TempDir::new().unwrap();
        let history = fixture.path().join("diagnostics/recent.json");
        let exports = fixture.path().join("exports");
        let mut store = DiagnosticStore::open(&history, &exports).unwrap();
        let id = store
            .start_job(JobKind::Update, WorkPhase::ClientDownload)
            .unwrap();
        store.finish_job(id, JobStatus::Indeterminate).unwrap();
        drop(store);

        let reopened = DiagnosticStore::open(&history, &exports).unwrap();
        let outcome = reopened.recent_jobs().last().unwrap();
        assert_eq!(outcome.status(), JobStatus::Indeterminate);
        assert_eq!(outcome.evidence.back(), Some(&JobEvidence::Indeterminate));
        assert_ne!(outcome.status(), JobStatus::Failed);
        assert_ne!(outcome.status(), JobStatus::Cancelled);
    }

    #[test]
    fn restart_recovery_terminalizes_an_active_job_as_interrupted() {
        let fixture = TempDir::new().unwrap();
        let history = fixture.path().join("diagnostics/recent.json");
        let exports = fixture.path().join("exports");
        let mut store = DiagnosticStore::open(&history, &exports).unwrap();
        store
            .start_job(JobKind::Install, WorkPhase::BaseGameInstall)
            .unwrap();

        let mut reopened = DiagnosticStore::open(&history, &exports).unwrap();
        reopened.recover_interrupted().unwrap();
        assert!(reopened.active_job().is_some());
        drop(store);
        reopened.recover_interrupted().unwrap();
        assert!(reopened.active_job().is_none());
        let recovered = reopened.recent_jobs().last().unwrap();
        assert_eq!(recovered.kind(), JobKind::Install);
        assert_eq!(recovered.status(), JobStatus::Failed);
        assert_eq!(recovered.evidence.back(), Some(&JobEvidence::Interrupted));
    }

    #[test]
    fn independent_stores_cannot_overwrite_live_jobs_or_reuse_ids() {
        let fixture = TempDir::new().unwrap();
        let history = fixture.path().join("diagnostics/recent.json");
        let exports = fixture.path().join("exports");
        let mut owner = DiagnosticStore::open(&history, &exports).unwrap();
        let first = owner
            .start_job(JobKind::Install, WorkPhase::BaseGameInstall)
            .unwrap();
        let mut observer = DiagnosticStore::open(&history, &exports).unwrap();

        observer.recover_interrupted().unwrap();
        assert_eq!(observer.active_job().unwrap().id(), first);
        assert!(
            observer
                .start_job(JobKind::Update, WorkPhase::ClientManifest)
                .is_err()
        );
        assert!(
            observer
                .update_job(
                    first,
                    WorkPhase::ClientDownload,
                    Some(ProgressUnit::Files),
                    Some(1),
                    Some(2),
                )
                .is_err()
        );

        owner.finish_job(first, JobStatus::Succeeded).unwrap();
        let second = observer
            .start_job(JobKind::Update, WorkPhase::ClientManifest)
            .unwrap();
        assert!(second.value() > first.value());
    }

    #[test]
    fn unknown_total_byte_progress_round_trips_without_fabricating_completion() {
        let fixture = TempDir::new().unwrap();
        let history = fixture.path().join("diagnostics/recent.json");
        let exports = fixture.path().join("exports");
        let mut store = DiagnosticStore::open(&history, &exports).unwrap();
        let id = store
            .start_job(JobKind::Update, WorkPhase::ClientDownload)
            .unwrap();
        store
            .update_job(
                id,
                WorkPhase::ClientDownload,
                Some(ProgressUnit::Bytes),
                Some(8192),
                None,
            )
            .unwrap();

        let reopened = DiagnosticStore::open(&history, &exports).unwrap();
        let progress = reopened.active_job().unwrap().progress().unwrap();
        assert_eq!(progress.unit(), ProgressUnit::Bytes);
        assert_eq!(progress.completed(), 8192);
        assert_eq!(progress.total(), None);
    }

    #[test]
    fn safe_stopping_is_durable_before_the_mutator_finishes() {
        let fixture = TempDir::new().unwrap();
        let history = fixture.path().join("diagnostics/recent.json");
        let exports = fixture.path().join("exports");
        let mut owner = DiagnosticStore::open(&history, &exports).unwrap();
        let id = owner
            .start_job(JobKind::Install, WorkPhase::BaseGameInstall)
            .unwrap();
        owner.request_safe_stop(id).unwrap();

        let observer = DiagnosticStore::open(&history, &exports).unwrap();
        assert_eq!(
            observer.active_job().unwrap().status(),
            JobStatus::SafeStopping
        );
    }

    #[test]
    fn composite_progress_round_trips_all_dimensions_in_one_snapshot() {
        let fixture = TempDir::new().unwrap();
        let history = fixture.path().join("diagnostics/recent.json");
        let exports = fixture.path().join("exports");
        let mut store = DiagnosticStore::open(&history, &exports).unwrap();
        let id = store
            .start_job(JobKind::Update, WorkPhase::ClientDownload)
            .unwrap();
        store
            .update_job_progress(
                id,
                WorkProgress::for_phase(WorkPhase::ClientDownload)
                    .with_file_bytes(8192, None)
                    .with_overall(ProgressUnit::Files, 3, Some(10))
                    .with_verification(2, Some(10)),
            )
            .unwrap();

        let reopened = DiagnosticStore::open(&history, &exports).unwrap();
        let progress = reopened.active_job().unwrap().progress().unwrap();
        assert_eq!(progress.file_bytes().unwrap().total(), None);
        assert_eq!(progress.overall().unwrap().completed(), 3);
        assert_eq!(progress.verification().unwrap().completed(), 2);
    }

    #[test]
    fn legacy_schema_migrates_without_turning_untyped_events_into_jobs() {
        let fixture = TempDir::new().unwrap();
        let history = fixture.path().join("diagnostics/recent.json");
        fs::create_dir_all(history.parent().unwrap()).unwrap();
        fs::write(
            &history,
            br#"{"schema":1,"active":"update-active","completed":["layout-opened","install-failed"]}"#,
        )
        .unwrap();
        let mut store = DiagnosticStore::open(&history, fixture.path().join("exports")).unwrap();
        assert_eq!(store.active_job().unwrap().kind(), JobKind::Update);
        assert_eq!(store.recent_jobs().count(), 0);
        assert_eq!(store.recent_events().count(), 1);
        store.recover_interrupted().unwrap();
        assert_eq!(store.recent_jobs().count(), 1);
    }

    #[test]
    fn explicit_export_is_capped_and_contains_no_free_form_secret_channel() {
        let fixture = TempDir::new().unwrap();
        let mut store = DiagnosticStore::open(
            fixture.path().join("diagnostics/recent.json"),
            fixture.path().join("exports"),
        )
        .unwrap();
        let id = store.start_job(JobKind::Repair, WorkPhase::Repair).unwrap();
        store.request_safe_stop(id).unwrap();
        store.finish_job(id, JobStatus::Cancelled).unwrap();

        let export = store.export().unwrap();
        let bytes = fs::read(export).unwrap();
        assert!(bytes.len() as u64 <= DIAGNOSTIC_LIMIT);
        let text = String::from_utf8(bytes).unwrap();
        assert!(!text.contains("key"));
        assert!(!text.contains("credential"));
        assert!(!text.contains("argv"));
    }

    #[test]
    fn custom_sources_are_recorded_by_host_only_and_exported() {
        let fixture = TempDir::new().unwrap();
        let history = fixture.path().join("diagnostics/recent.json");
        let mut store = DiagnosticStore::open(&history, fixture.path().join("exports")).unwrap();
        let id = store
            .start_job(JobKind::Install, WorkPhase::RuntimeAcquisition)
            .unwrap();
        store
            .record_custom_sources(
                id,
                [
                    (DownloadArtifact::Wine, "mirror.example".to_owned()),
                    (
                        DownloadArtifact::Pd2Installer,
                        "evil/path?token=secret".to_owned(),
                    ),
                ],
            )
            .unwrap();
        store.finish_job(id, JobStatus::Succeeded).unwrap();

        let reopened = DiagnosticStore::open(&history, fixture.path().join("exports")).unwrap();
        let recorded: Vec<_> = reopened
            .recent_custom_sources()
            .map(|record| (record.job(), record.artifact(), record.host().to_owned()))
            .collect();
        assert_eq!(
            recorded,
            [
                (id, DownloadArtifact::Wine, "mirror.example".to_owned()),
                (
                    id,
                    DownloadArtifact::Pd2Installer,
                    "unknown-host".to_owned()
                ),
            ]
        );
        let mut store = reopened;
        let export = fs::read_to_string(store.export().unwrap()).unwrap();
        assert!(export.contains("mirror.example"));
        assert!(!export.contains("token"));
        assert!(!export.contains("secret"));

        // Histories written before custom sources existed still load.
        let bare =
            br#"{"schema":3,"next_job_id":1,"active_job":null,"completed_jobs":[],"events":[]}"#;
        fs::write(&history, bare).unwrap();
        let store = DiagnosticStore::open(&history, fixture.path().join("exports")).unwrap();
        assert_eq!(store.recent_custom_sources().count(), 0);

        // Histories from when the 1.14d patch was a stage still load: its
        // phase reads as the step it belonged to, its custom source is gone.
        let patched = br#"{"schema":3,"next_job_id":3,"active_job":null,"completed_jobs":[{"id":1,"kind":"download","status":"succeeded","phase":"patch-download","progress":null,"evidence":["started"]},{"id":2,"kind":"install-base-game","status":"failed","phase":"patch","progress":null,"evidence":["started"]}],"events":[],"custom_sources":[{"job":1,"artifact":"patch","host":"mirror.example"},{"job":1,"artifact":"wine","host":"mirror.example"}]}"#;
        fs::write(&history, patched).unwrap();
        let store = DiagnosticStore::open(&history, fixture.path().join("exports")).unwrap();
        let phases: Vec<_> = store.recent_jobs().map(JobRecord::phase).collect();
        assert!(phases.contains(&WorkPhase::BaseGameDownload), "{phases:?}");
        assert!(phases.contains(&WorkPhase::BaseGameInstall), "{phases:?}");
        assert_eq!(
            store
                .recent_custom_sources()
                .map(CustomSourceRecord::artifact)
                .collect::<Vec<_>>(),
            [DownloadArtifact::Wine]
        );
    }

    #[test]
    fn invalid_transition_and_oversized_history_fail_closed() {
        let fixture = TempDir::new().unwrap();
        let history = fixture.path().join("diagnostics/recent.json");
        let mut store = DiagnosticStore::open(&history, fixture.path().join("exports")).unwrap();
        let id = store
            .start_job(JobKind::Install, WorkPhase::RuntimeAcquisition)
            .unwrap();
        assert!(
            store
                .finish_job(JobId(id.value() + 1), JobStatus::Succeeded)
                .is_err()
        );
        assert!(store.finish_job(id, JobStatus::Running).is_err());

        fs::write(&history, vec![b' '; DIAGNOSTIC_LIMIT as usize + 1]).unwrap();
        assert!(DiagnosticStore::open(&history, fixture.path().join("exports")).is_err());
    }
}
