//! Application coordination for the zero-to-Ready and Play journey.
//!
//! The coordinator owns authorization and operation policy. Adapters return
//! evidence; they never publish presentation state directly.

use std::{
    path::PathBuf,
    sync::{
        Arc,
        mpsc::{Receiver, Sender, SyncSender, TryRecvError, TrySendError, channel},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::{
    InstallationStatus, LaunchPreflight, UpdateStatus,
    audit::ActivityEvidence,
    game_language::GameLanguage,
    install::InstallationAuthorization,
    operation::{OperationControl, ProgressReceiver, WorkPhase, progress_channel},
    persistence::SetupRecord,
    presentation::{
        ActiveJobView, AuthorizationView, Destination, DomainEvent, DownloadSourceView,
        DownloadsView, EvidenceTone, EvidenceView, FexSettingView, FolderOpenView, GameFolderView,
        GameNoticeView, GameView, GraphicsDriver, GraphicsProfile, HostView, ImportModeView,
        InstallerMediaView, Intent, LaunchSettingsView, LocationView, OperationPhase,
        OperationStatusView, OperationView, REQUEST_NOT_STARTED, ReportView, SavesPutAsideView,
        SequencedEvent, ServiceEndpoint, SettingsChange, SetupStep, StepAffordance, StopResultView,
        SystemContractView, UiSnapshot, human_bytes, human_duration, step_affordance,
    },
    sources::DownloadArtifact,
};

const SERVICE_TICK: Duration = Duration::from_millis(20);
const OFFICIAL_LAUNCHER_TITLE: &str = "The official PD2 launcher is open";

pub(crate) type CancellationToken = OperationControl;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BackendAudit {
    pub(crate) host: HostView,
    pub(crate) installation: InstallationStatus,
    pub(crate) launch_preflight: LaunchPreflight,
    pub(crate) system: SystemContractView,
    pub(crate) evidence: Vec<EvidenceView>,
    /// The official launcher binary is neither the manifest's nor the copy
    /// recorded after its last session; opening it asks first.
    pub(crate) official_launcher_unverified: bool,
    /// The game has never been launched with D2GL (it has not written its
    /// configuration yet), so its first launch opens at the wrong size.
    pub(crate) first_launch_pending: bool,
    /// The Download step's durable result.
    pub(crate) downloads: DownloadsView,
    /// The installed game's OpenGL renderer draws in software; `None` when
    /// this audit could not tell (the last answer stands).
    pub(crate) game_software_rendering: Option<bool>,
    /// The language of the installed copy (`data\\local\\use`); `None` when
    /// it is not installed or this audit could not read it.
    pub(crate) game_language: Option<GameLanguage>,
    /// The Managed runtime and prefix were both accepted.
    pub(crate) environment_ready: bool,
}

impl BackendAudit {
    pub(crate) fn new(
        host: HostView,
        installation: InstallationStatus,
        launch_preflight: LaunchPreflight,
        system: SystemContractView,
        evidence: Vec<EvidenceView>,
    ) -> Self {
        Self {
            host,
            installation,
            launch_preflight,
            system,
            evidence,
            official_launcher_unverified: false,
            first_launch_pending: false,
            downloads: DownloadsView::default(),
            game_software_rendering: Some(false),
            game_language: None,
            environment_ready: false,
        }
    }

    pub(crate) fn with_game_language(mut self, language: Option<GameLanguage>) -> Self {
        self.game_language = language;
        self
    }

    pub(crate) fn with_game_software_rendering(mut self, software: Option<bool>) -> Self {
        self.game_software_rendering = software;
        self
    }

    pub(crate) fn with_environment_ready(mut self, ready: bool) -> Self {
        self.environment_ready = ready;
        self
    }

    pub(crate) fn with_downloads(mut self, downloads: DownloadsView) -> Self {
        self.downloads = downloads;
        self
    }

    pub(crate) fn with_first_launch_pending(mut self, pending: bool) -> Self {
        self.first_launch_pending = pending;
        self
    }

    pub(crate) fn with_official_launcher_unverified(mut self, unverified: bool) -> Self {
        self.official_launcher_unverified = unverified;
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BackendFailure {
    code: &'static str,
}

impl BackendFailure {
    pub(crate) const fn new(code: &'static str) -> Self {
        Self { code }
    }

    pub(crate) const fn code(self) -> &'static str {
        self.code
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BackendGameState {
    Idle,
    Running,
    Exited,
    SupervisionFailed,
}

/// Whole-system seam. Production composition implements this with the real
/// auditor, installer, updater, transaction manager, supervisor, and stores.
/// Tests replace only this external boundary.
pub(crate) trait CoordinatorBackend: Send + Sync + 'static {
    fn open_managed_layout(&self) -> Result<(), BackendFailure>;
    fn audit(&self, activity: ActivityEvidence) -> Result<BackendAudit, BackendFailure>;
    fn repair(&self, cancellation: &OperationControl) -> Result<(), BackendFailure>;
    fn check_for_update(&self) -> Result<UpdateStatus, BackendFailure>;
    fn select_installer_media(
        &self,
        path: std::path::PathBuf,
    ) -> Result<InstallerMediaView, BackendFailure>;
    /// Choose the official Blizzard downloader wrappers from their built-in
    /// URLs instead of a local folder (session-only, like a folder choice).
    fn select_installer_download(&self) -> Result<InstallerMediaView, BackendFailure> {
        Ok(InstallerMediaView::BlizzardDownload)
    }
    /// Where the managed root is, and whether it resolved.
    fn install_location(&self) -> Option<LocationView> {
        None
    }
    /// Effective download source per artifact.
    fn download_sources(&self) -> Result<Vec<DownloadSourceView>, BackendFailure> {
        Ok(Vec::new())
    }
    /// Persist (`Some`) or remove (`None`) the GUI override for one artifact.
    fn update_download_source(
        &self,
        _artifact: DownloadArtifact,
        _url: Option<String>,
    ) -> Result<Vec<DownloadSourceView>, BackendFailure> {
        Err(BackendFailure::new("download-sources-unavailable"))
    }
    /// Choose a Diablo II folder the user already has, of any version. A
    /// folder that can't be used is returned with the reason.
    fn select_existing_install(
        &self,
        _path: std::path::PathBuf,
    ) -> Result<InstallerMediaView, BackendFailure> {
        Err(BackendFailure::new("existing-install-unavailable"))
    }
    /// Settings → FEX on ARM64 builds; `None` elsewhere.
    fn fex_setting(&self) -> Option<FexSettingView> {
        None
    }
    /// Save where FEX is (`None`: find it automatically). A path that isn't
    /// an executable file is returned with the problem and not saved.
    fn save_fex(&self, _fex: Option<std::path::PathBuf>) -> Result<FexSettingView, BackendFailure> {
        Err(BackendFailure::new("fex-setting-unavailable"))
    }
    /// Use the built-in FEX or a device FEX; refused while this build has
    /// no built-in FEX.
    fn choose_fex_source(
        &self,
        _source: crate::presentation::FexSourceView,
    ) -> Result<FexSettingView, BackendFailure> {
        Err(BackendFailure::new("fex-setting-unavailable"))
    }
    /// The installer choice the last Download was made for, when it still
    /// holds (an existing installation chosen before a restart).
    fn restored_installer_choice(&self) -> Option<InstallerMediaView> {
        None
    }
    /// A game folder used in place, and whether it is reachable.
    fn game_folder(&self) -> Option<GameFolderView> {
        None
    }
    /// Step 3: download and verify everything the remaining steps need.
    fn download_all(&self, cancellation: &OperationControl) -> Result<(), BackendFailure>;
    /// Step 4: install Diablo II with the CD keys, or import the existing
    /// installation. The game is never patched.
    fn install_base_game(
        &self,
        authorization: Option<InstallationAuthorization>,
        import: Option<ImportModeView>,
        cancellation: &OperationControl,
    ) -> Result<(), BackendFailure>;
    /// Step 5: install Project Diablo 2.
    fn install_project_diablo(&self, cancellation: &OperationControl)
    -> Result<(), BackendFailure>;
    /// Remove what this Launcher's own downloads made, for a player who
    /// imports the game instead.
    fn discard_downloads(&self, _cancellation: &OperationControl) -> Result<(), BackendFailure> {
        Err(BackendFailure::new("discard-downloads-unavailable"))
    }
    fn apply_update(&self, cancellation: &OperationControl) -> Result<(), BackendFailure>;
    /// Run the official PD2 launcher under the exclusive installation lease
    /// until it and everything it started exit, then reconcile a client
    /// update it made. Cancellation closes it.
    /// `confirmed`: the user agreed to run a binary that matches neither
    /// the manifest nor the copy recorded after its last session.
    /// `strict_memory_order`: a game that it starts gets Box64's strict x86
    /// memory order (the Android app's Strict CPU emulation);
    /// `detailed_logging`: its Wine, and the game it starts, log in detail
    /// (the Android app's Detailed logging). Neither from the desktop.
    fn open_official_launcher(
        &self,
        _confirmed: bool,
        _strict_memory_order: bool,
        _detailed_logging: bool,
        _cancellation: &OperationControl,
    ) -> Result<(), BackendFailure> {
        Err(BackendFailure::new("official-launcher-unavailable"))
    }
    fn request_mutation_safe_stop(&self) -> Result<(), BackendFailure> {
        Ok(())
    }
    /// Start over: under the exclusive installation lease,
    /// remove the Managed prefix and the interrupted-stage marker, keeping
    /// the Wine runtime and downloads.
    fn reset_installation(&self, _cancellation: &OperationControl) -> Result<(), BackendFailure> {
        Err(BackendFailure::new("reset-unavailable"))
    }
    /// Install into `<parent>/pd2-launcher`: adopt a fresh root,
    /// or move the current installation there.
    /// `fresh_only` refuses to move a root that holds an installation.
    fn relocate(
        &self,
        _parent: std::path::PathBuf,
        _fresh_only: bool,
        _cancellation: &OperationControl,
    ) -> Result<(), BackendFailure> {
        Err(BackendFailure::new("location-change-unavailable"))
    }
    fn remove_previous_installation(
        &self,
        _cancellation: &OperationControl,
    ) -> Result<(), BackendFailure> {
        Err(BackendFailure::new("location-previous-unavailable"))
    }
    /// `driver` and `profile` are the Android app's graphics driver and D2GL
    /// graphics profile for this launch (no profile from the desktop);
    /// `strict_memory_order` whether the game gets Box64's strict x86 memory
    /// order (the Android app's Strict CPU emulation), `detailed_logging`
    /// whether its Wine logs in detail (the Android app's Detailed logging);
    /// neither from the desktop. A returned notice is shown for this launch
    /// only.
    fn launch_game(
        &self,
        driver: GraphicsDriver,
        profile: Option<GraphicsProfile>,
        strict_memory_order: bool,
        detailed_logging: bool,
    ) -> Result<Option<GameNoticeView>, BackendFailure>;
    fn poll_game(&self) -> BackendGameState;
    fn request_game_stop(&self) -> Result<(), BackendFailure>;
    fn load_settings(&self) -> Result<LaunchSettingsView, BackendFailure>;
    fn update_settings(&self, change: SettingsChange)
    -> Result<LaunchSettingsView, BackendFailure>;
    fn diagnostics_evidence(&self) -> Result<Vec<EvidenceView>, BackendFailure>;
    /// Write the redacted report archive; its path.
    fn save_report(&self) -> Result<PathBuf, BackendFailure>;
    /// Show a saved report in the file manager; whether that worked.
    fn reveal_report(&self, _path: &std::path::Path) -> bool {
        false
    }
    /// Open the game folder in the file manager; whether that worked.
    fn open_game_folder(&self, _folder: &std::path::Path) -> bool {
        false
    }
    /// What keeping the characters put in a backup since this was last
    /// asked, for the player to be told once.
    fn take_saves_put_aside(&self) -> Option<SavesPutAsideView> {
        None
    }
    /// Show a backup of the characters in the file manager; whether that
    /// worked.
    fn open_backups(&self, _folder: &std::path::Path) -> bool {
        false
    }
    /// How far the setup got, by the Launcher's record.
    fn setup_record(&self) -> SetupRecord {
        SetupRecord::None
    }
    /// Replace the setup record.
    fn save_setup_record(&self, _record: SetupRecord) -> Result<(), BackendFailure> {
        Ok(())
    }
}

enum Control {
    Shutdown,
}

enum Completion {
    UpdateChecked(Result<UpdateStatus, BackendFailure>),
    ReportSaved(Result<PathBuf, BackendFailure>),
    ReportRevealed {
        path: PathBuf,
        opened: bool,
    },
    GameFolderOpened(bool),
    BackupOpened(bool),
    MutationFinished {
        kind: MutationKind,
        result: Result<(), BackendFailure>,
        audit: Box<Result<BackendAudit, BackendFailure>>,
        location: Option<LocationView>,
        game_folder: Option<GameFolderView>,
        /// After Download: whether the built-in FEX is now installed.
        fex_setting: Option<FexSettingView>,
        /// What keeping the characters put in a backup meanwhile.
        saves_put_aside: Option<Box<SavesPutAsideView>>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MutationKind {
    /// Step 3.
    Download,
    /// Step 4.
    InstallBaseGame,
    /// Step 5.
    InstallProjectDiablo,
    Update,
    Repair,
    /// The official PD2 launcher: an external mutator of ProjectD2 that runs
    /// under the same exclusive lease as the Launcher's own mutations.
    OfficialLauncher,
    /// A confirmed Start over.
    Reset,
    /// Choosing or changing the install location.
    Relocate,
    /// Removing the installation a move to another filesystem left behind.
    RemovePrevious,
    /// Removing what this Launcher's own downloads made, for an import.
    DiscardDownloads,
}

impl MutationKind {
    const fn is_setup(self) -> bool {
        matches!(
            self,
            Self::Download | Self::InstallBaseGame | Self::InstallProjectDiablo
        )
    }

    const fn job(self) -> ActiveJobView {
        match self {
            Self::Download => ActiveJobView::Download,
            Self::InstallBaseGame => ActiveJobView::InstallBaseGame,
            Self::InstallProjectDiablo => ActiveJobView::InstallProjectDiablo,
            Self::Update => ActiveJobView::Update,
            Self::Repair => ActiveJobView::Repair,
            Self::OfficialLauncher => ActiveJobView::OfficialLauncher,
            Self::Reset => ActiveJobView::Reset,
            Self::Relocate | Self::RemovePrevious => ActiveJobView::Relocation,
            Self::DiscardDownloads => ActiveJobView::DiscardDownloads,
        }
    }
}

struct ActiveMutation {
    kind: MutationKind,
    cancellation: OperationControl,
    progress: ProgressReceiver,
}

pub(crate) struct ServiceRuntime {
    control: Sender<Control>,
    stopped: Receiver<()>,
    worker: Option<JoinHandle<()>>,
}

impl ServiceRuntime {
    pub(crate) fn start<B: CoordinatorBackend>(
        endpoint: ServiceEndpoint,
        backend: Arc<B>,
    ) -> Result<Self, BackendFailure> {
        let (control, control_receiver) = channel();
        let (stopped_sender, stopped) = channel();
        let worker = thread::Builder::new()
            .name("pd2-service-runtime".to_owned())
            .spawn(move || service_loop(endpoint, backend, control_receiver, stopped_sender))
            .map_err(|_| BackendFailure::new("service-runtime-unavailable"))?;
        Ok(Self {
            control,
            stopped,
            worker: Some(worker),
        })
    }

    #[cfg(test)]
    pub(crate) fn shutdown(self) {}
}

impl Drop for ServiceRuntime {
    fn drop(&mut self) {
        let _ = self.control.send(Control::Shutdown);
        // The process must not detach a mutator or an owned child on shutdown.
        // Cooperative cancellation makes the worker converge at its next safe
        // boundary; we retain and join it even when that takes longer than a UI
        // convenience timeout.
        let _ = self.stopped.recv();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn service_loop<B: CoordinatorBackend>(
    endpoint: ServiceEndpoint,
    backend: Arc<B>,
    control: Receiver<Control>,
    stopped: Sender<()>,
) {
    let (completion_sender, completion_receiver) = channel();
    let mut coordinator = Coordinator::new(endpoint.event_sender);
    coordinator.startup(backend.clone(), completion_sender.clone(), false);
    let mut shutdown_requested = false;

    loop {
        if control.try_recv().is_ok() && !shutdown_requested {
            coordinator.request_safe_stop(backend.as_ref());
            shutdown_requested = true;
        }

        for _ in 0..32 {
            match endpoint.intent_receiver.try_recv() {
                Ok(intent) => {
                    coordinator.handle_intent(intent, backend.clone(), completion_sender.clone())
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }

        for completion in completion_receiver.try_iter() {
            if coordinator.handle_completion(completion) {
                coordinator.start_update_check(backend.clone(), completion_sender.clone());
            }
            coordinator.start_pending_work(backend.clone(), completion_sender.clone());
        }
        coordinator.poll_progress();
        coordinator.poll_game(backend.as_ref());
        coordinator.follow_setup_record(backend.as_ref());

        if shutdown_requested && coordinator.can_shutdown() {
            break;
        }
        thread::sleep(SERVICE_TICK);
    }
    let _ = stopped.send(());
}

struct Coordinator {
    events: SyncSender<SequencedEvent>,
    snapshot: UiSnapshot,
    sequence: u64,
    active_mutation: Option<ActiveMutation>,
    update_check_active: bool,
    update_check_pending: bool,
    /// The step whose editor opens once a "Stop and change" stop is proven.
    pending_return: Option<SetupStep>,
    /// The parent folder the next relocation job installs into, and
    /// whether it may only replace a root that holds nothing.
    relocation_target: Option<(std::path::PathBuf, bool)>,
    /// The next official-launcher job was confirmed for an unverified binary.
    official_launcher_confirmed: bool,
    /// A game that the next official-launcher job starts gets Box64's strict
    /// x86 memory order (the Android app's Strict CPU emulation).
    official_launcher_strict_memory_order: bool,
    /// The next official-launcher job's Wine logs in detail (the Android
    /// app's Detailed logging).
    official_launcher_detailed_logging: bool,
    /// How the next Install Diablo II job imports the existing installation.
    import_mode: Option<ImportModeView>,
    /// Keys given while the Download job ran: Install Diablo II starts with
    /// them once a Download ends well.
    pending_install: Option<InstallationAuthorization>,
    /// A Download that ended well found keys waiting.
    install_after_download: bool,
    /// The running Download stops, then the Launcher's downloads are
    /// removed.
    discard_after_stop: bool,
    /// A stop was asked of the running Download: even if it ends well,
    /// Install Diablo II does not start (the keys keep waiting).
    download_stop_requested: bool,
    /// The last Download failed or stopped: new keys wait for its Try again.
    download_unfinished: bool,
    /// Download speed of the running job's byte progress.
    byte_rate: ByteRate,
    /// The Launcher engine on the Android host: an update's progress is
    /// shown by bytes.
    android_host: bool,
}

/// A smoothed bytes-per-second estimate for one file's byte progress in one
/// phase.
#[derive(Default)]
struct ByteRate {
    /// The phase and the file's size, when known: another one is another
    /// file.
    file: Option<(WorkPhase, Option<u64>)>,
    last: Option<(Instant, u64)>,
    rate: Option<f64>,
}

impl ByteRate {
    /// Samples closer than a second apart keep the current estimate.
    const MIN_INTERVAL: Duration = Duration::from_secs(1);

    fn sample(
        &mut self,
        phase: WorkPhase,
        completed: u64,
        total: Option<u64>,
        now: Instant,
    ) -> Option<u64> {
        // A new phase or a new file starts a new estimate: the time between
        // two files (the check of the one done, the entries already in
        // place) is no download time, so a new file has no speed and no
        // time left until a second of its own bytes is measured.
        if self.file != Some((phase, total)) {
            *self = Self {
                file: Some((phase, total)),
                ..Self::default()
            };
        }
        match self.last {
            Some((at, bytes)) if completed >= bytes => {
                let elapsed = now.saturating_duration_since(at);
                if elapsed >= Self::MIN_INTERVAL {
                    let instant = (completed - bytes) as f64 / elapsed.as_secs_f64();
                    self.rate = Some(self.rate.map_or(instant, |rate| rate * 0.7 + instant * 0.3));
                    self.last = Some((now, completed));
                }
            }
            // A restarted or first sample starts a new estimate.
            _ => {
                self.last = Some((now, completed));
                self.rate = None;
            }
        }
        self.rate
            .filter(|rate| *rate >= 1.0)
            .map(|rate| rate as u64)
    }
}

impl Coordinator {
    fn new(events: SyncSender<SequencedEvent>) -> Self {
        Self {
            events,
            snapshot: UiSnapshot::initial(),
            sequence: 0,
            active_mutation: None,
            update_check_active: false,
            update_check_pending: false,
            pending_return: None,
            relocation_target: None,
            official_launcher_confirmed: false,
            official_launcher_strict_memory_order: false,
            official_launcher_detailed_logging: false,
            import_mode: None,
            pending_install: None,
            install_after_download: false,
            discard_after_stop: false,
            download_stop_requested: false,
            download_unfinished: false,
            byte_rate: ByteRate::default(),
            android_host: crate::android_host(),
        }
    }

    /// The host audit, then the update check. `asked`: Check again. On the
    /// Android host the update check runs only then, and only for a game
    /// that is installed: the app asks when the player presses Play, the
    /// only time Project Diablo 2's update is checked there.
    fn startup<B: CoordinatorBackend>(
        &mut self,
        backend: Arc<B>,
        completions: Sender<Completion>,
        asked: bool,
    ) {
        // The Android app keeps its own record.
        if !self.android_host {
            self.change_setup_record(backend.setup_record());
        }
        self.change_operation(Some(OperationView::read_only(
            OperationPhase::HostAudit,
            OperationStatusView::Running,
            "Checking this device",
        )));
        let opened = backend.open_managed_layout();
        self.change_location(backend.install_location());
        self.change_game_folder(backend.game_folder());
        if self.snapshot.installer_media() == &InstallerMediaView::NotSelected
            && let Some(choice) = backend.restored_installer_choice()
        {
            self.change_installer_media(choice);
        }
        if let Err(error) = opened {
            self.fail_closed(error);
            return;
        }
        self.refresh_settings(backend.as_ref());
        self.refresh_download_sources(backend.as_ref());

        let initial = backend.audit(ActivityEvidence::Idle);
        match initial {
            Ok(audit) if audit.installation == InstallationStatus::RepairRequired => {
                self.apply_audit(audit);
                self.change_journal(true);
                self.start_mutation(
                    backend,
                    completions,
                    MutationKind::Repair,
                    None,
                    OperationPhase::Repair,
                    "Repairing recorded work",
                );
            }
            Ok(audit) => {
                self.apply_audit(audit);
                self.change_operation(None);
                if !self.android_host
                    || (asked && self.snapshot.installation() == InstallationStatus::Ready)
                {
                    self.start_update_check(backend, completions);
                }
            }
            Err(error) => self.fail_closed(error),
        }
    }

    fn handle_intent<B: CoordinatorBackend>(
        &mut self,
        intent: Intent,
        backend: Arc<B>,
        completions: Sender<Completion>,
    ) {
        let fresh_only = matches!(intent, Intent::UseFreshInstallLocation(_));
        let official_launcher_confirmed =
            matches!(intent, Intent::OpenOfficialLauncherConfirmed(..));
        let (official_launcher_strict_memory_order, official_launcher_detailed_logging) =
            match intent {
                Intent::OpenOfficialLauncherConfirmed(strict, detailed) => (strict, detailed),
                _ => (false, false),
            };
        match intent {
            Intent::SelectInstallerMedia(path) => {
                if self.media_guard() {
                    let result = backend.select_installer_media(path);
                    self.apply_installer_media(result);
                }
            }
            Intent::UseBlizzardDownload => {
                if self.media_guard() {
                    let result = backend.select_installer_download();
                    self.apply_installer_media(result);
                }
            }
            Intent::UseDefaultInstallerSource => {
                // A choice restored from the last Download is kept.
                if self.snapshot.installer_media() == &InstallerMediaView::NotSelected
                    && self.media_guard()
                {
                    let result = backend.select_installer_download();
                    self.apply_installer_media(result);
                }
            }
            Intent::SelectExistingInstall(path) => {
                if self.media_guard() {
                    let result = backend.select_existing_install(path);
                    self.apply_installer_media(result);
                }
            }
            Intent::SaveFex(fex) => match backend.save_fex(fex) {
                Ok(setting) => self.change_fex_setting(Some(setting)),
                Err(error) => self.publish_guard_failure(error.code()),
            },
            Intent::ChooseFexSource(source) => match backend.choose_fex_source(source) {
                Ok(setting) => {
                    self.change_fex_setting(Some(setting));
                    // The other FEX is checked at once, as at startup.
                    if self.active_mutation.is_none() && self.snapshot.game() == GameView::Idle {
                        self.startup(backend, completions, false);
                    }
                }
                Err(error) => self.publish_guard_failure(error.code()),
            },
            Intent::DownloadAll => {
                if !self.download_guard() {
                    self.publish_guard_failure("download-not-permitted");
                    return;
                }
                self.start_mutation(
                    backend,
                    completions,
                    MutationKind::Download,
                    None,
                    OperationPhase::RuntimeAcquisition,
                    "Downloading",
                );
            }
            Intent::ForgetWaitingKeys => self.drop_pending_install(),
            Intent::DiscardDownloads => match self.active_kind() {
                Some(MutationKind::Download) => {
                    // Stop first; the downloads go once it stopped.
                    self.discard_after_stop = true;
                    self.drop_pending_install();
                    self.request_safe_stop(backend.as_ref());
                }
                None if self.setup_idle() => {
                    self.drop_pending_install();
                    self.start_mutation(
                        backend,
                        completions,
                        MutationKind::DiscardDownloads,
                        None,
                        OperationPhase::Verification,
                        "Removing the downloads",
                    );
                }
                _ => self.publish_guard_failure("discard-downloads-not-permitted"),
            },
            Intent::ImportExistingInstall(mode) => {
                if !self.import_guard() {
                    self.publish_guard_failure("import-not-permitted");
                    return;
                }
                self.import_mode = Some(mode);
                self.start_mutation(
                    backend,
                    completions,
                    MutationKind::InstallBaseGame,
                    None,
                    OperationPhase::ImportGame,
                    "Setting up your Diablo II",
                );
            }
            Intent::InstallProjectDiablo => {
                if !self.project_diablo_guard() {
                    self.publish_guard_failure("install-not-permitted");
                    return;
                }
                self.start_mutation(
                    backend,
                    completions,
                    MutationKind::InstallProjectDiablo,
                    None,
                    OperationPhase::ClientDownload,
                    "Installing Project Diablo 2",
                );
            }
            Intent::EditStep(step) => {
                if step_affordance(&self.snapshot, step) == StepAffordance::Change {
                    self.change_editing(Some(step));
                } else {
                    self.publish_guard_failure("step-change-not-permitted");
                }
            }
            Intent::CloseEditor => self.change_editing(None),
            Intent::StopAndChange(step) => {
                if step_affordance(&self.snapshot, step) == StepAffordance::StopAndChange {
                    self.pending_return = Some(step);
                    self.request_safe_stop(backend.as_ref());
                } else {
                    self.publish_guard_failure("stop-and-change-not-permitted");
                }
            }
            Intent::DismissStopResult => self.change_stop_result(None),
            Intent::DismissResult => {
                if self.active_mutation.is_none()
                    && self
                        .snapshot
                        .operation()
                        .is_some_and(|operation| operation.status().is_terminal())
                {
                    self.change_operation(None);
                }
            }
            Intent::CheckAgain => {
                // Missing prerequisites may have been installed, or a
                // transient service failure may have passed: check again
                // exactly as at startup, without a restart.
                if self.active_mutation.is_none() && self.snapshot.game() == GameView::Idle {
                    self.startup(backend, completions, true);
                } else {
                    self.publish_guard_failure("check-again-not-permitted");
                }
            }
            Intent::SaveDownloadSource(artifact, url) => {
                let result = backend.update_download_source(artifact, Some(url));
                self.apply_download_sources(artifact, result);
            }
            Intent::ResetDownloadSource(artifact) => {
                let result = backend.update_download_source(artifact, None);
                self.apply_download_sources(artifact, result);
            }
            Intent::AuthorizeAndInstall(input)
                if self.active_kind() == Some(MutationKind::Download) =>
            {
                // Agree and install during the Download job: the keys
                // wait, and Install Diablo II starts with them once the
                // Download ends well.
                if !self.installers_wanted() || self.discard_after_stop {
                    self.publish_guard_failure("install-not-permitted");
                    return;
                }
                let (base, expansion, consent) = input.into_parts();
                match InstallationAuthorization::new(base, expansion, consent) {
                    Ok(authorization) => {
                        self.pending_install = Some(authorization);
                        self.change_authorization(AuthorizationView::Authorized);
                    }
                    Err(_) => self.publish_guard_failure("authorization-invalid"),
                }
            }
            Intent::AuthorizeAndInstall(input) => {
                // Keys given now replace any that waited.
                self.drop_pending_install();
                if !self.base_game_guard(true) {
                    let downloads_missing = !self.snapshot.downloads_ready();
                    // After a failed or stopped Download, the new keys wait
                    // for its Try again; the refusal still says why nothing
                    // started now.
                    if downloads_missing
                        && self.download_unfinished
                        && self.setup_idle()
                        && self.installers_wanted()
                    {
                        let (base, expansion, consent) = input.into_parts();
                        if let Ok(authorization) =
                            InstallationAuthorization::new(base, expansion, consent)
                        {
                            self.pending_install = Some(authorization);
                            self.change_authorization(AuthorizationView::Authorized);
                        }
                    }
                    self.publish_guard_failure(if downloads_missing {
                        "downloads-required"
                    } else {
                        "install-not-permitted"
                    });
                    return;
                }
                let (base, expansion, consent) = input.into_parts();
                let authorization = match InstallationAuthorization::new(base, expansion, consent) {
                    Ok(authorization) => authorization,
                    Err(_) => {
                        self.publish_guard_failure("authorization-invalid");
                        return;
                    }
                };
                self.change_authorization(AuthorizationView::Authorized);
                self.start_mutation(
                    backend,
                    completions,
                    MutationKind::InstallBaseGame,
                    Some(authorization),
                    OperationPhase::BaseGameInstall,
                    "Installing Diablo II",
                );
            }
            Intent::ContinueInstallation => {
                // The step the installation stands at, without keys.
                let (kind, phase, title) = match self.continue_kind() {
                    Some(MutationKind::Download) => (
                        MutationKind::Download,
                        OperationPhase::RuntimeAcquisition,
                        "Downloading",
                    ),
                    Some(MutationKind::InstallProjectDiablo) => (
                        MutationKind::InstallProjectDiablo,
                        OperationPhase::ClientDownload,
                        "Installing Project Diablo 2",
                    ),
                    _ => {
                        self.publish_guard_failure("install-not-permitted");
                        return;
                    }
                };
                self.start_mutation(backend, completions, kind, None, phase, title);
            }
            Intent::ApplyUpdate => {
                if !self.update_guard() {
                    self.publish_guard_failure("update-not-permitted");
                    return;
                }
                self.start_mutation(
                    backend,
                    completions,
                    MutationKind::Update,
                    None,
                    OperationPhase::ClientDownload,
                    "Applying the Available update",
                );
            }
            Intent::LaunchGame(driver, profile, strict_memory_order, detailed_logging) => {
                self.launch_game(
                    backend.as_ref(),
                    driver,
                    profile,
                    strict_memory_order,
                    detailed_logging,
                );
            }
            Intent::OpenOfficialLauncher | Intent::OpenOfficialLauncherConfirmed(..) => {
                if !self.official_launcher_guard() {
                    self.publish_guard_failure("official-launcher-not-permitted");
                    return;
                }
                self.official_launcher_confirmed = official_launcher_confirmed;
                self.official_launcher_strict_memory_order = official_launcher_strict_memory_order;
                self.official_launcher_detailed_logging = official_launcher_detailed_logging;
                self.start_mutation(
                    backend,
                    completions,
                    MutationKind::OfficialLauncher,
                    None,
                    OperationPhase::OfficialLauncher,
                    OFFICIAL_LAUNCHER_TITLE,
                );
            }
            Intent::ChangeSettings(change) => match backend.update_settings(change.clone()) {
                Ok(settings) => {
                    if settings.install_source() != self.snapshot.settings().install_source() {
                        self.drop_pending_install();
                    }
                    self.change_settings(settings);
                    if let SettingsChange::UpdateChecks(_) = change {
                        self.update_checks_changed(backend, completions);
                    }
                }
                // A program path whose file is not in the game folder is
                // refused (`command_error`); nothing was saved.
                Err(error) if error.code() == "program-path-missing" => {
                    self.publish_guard_failure(error.code());
                }
                Err(error) => {
                    self.change_settings(self.snapshot.settings().failed(error.code()));
                }
            },
            Intent::RequestSafeStop => self.request_safe_stop(backend.as_ref()),
            Intent::ChooseInstallLocation(parent) | Intent::UseFreshInstallLocation(parent) => {
                if !self.relocation_guard() {
                    self.publish_guard_failure("location-change-not-permitted");
                    return;
                }
                self.change_location_error(None);
                self.relocation_target = Some((parent, fresh_only));
                self.start_mutation(
                    backend,
                    completions,
                    MutationKind::Relocate,
                    None,
                    OperationPhase::Relocation,
                    "Changing the install location",
                );
            }
            Intent::RemovePreviousInstallation => {
                if !self.remove_previous_guard() {
                    self.publish_guard_failure("remove-previous-not-permitted");
                    return;
                }
                self.change_location_error(None);
                self.start_mutation(
                    backend,
                    completions,
                    MutationKind::RemovePrevious,
                    None,
                    OperationPhase::Relocation,
                    "Removing the old installation",
                );
            }
            Intent::RetryLocation => {
                if self.active_mutation.is_none() && self.snapshot.location_unavailable() {
                    self.startup(backend, completions, false);
                } else {
                    self.publish_guard_failure("location-retry-not-permitted");
                }
            }
            Intent::StartOver => {
                if !self.reset_guard() {
                    self.publish_guard_failure("start-over-not-permitted");
                    return;
                }
                self.start_mutation(
                    backend,
                    completions,
                    MutationKind::Reset,
                    None,
                    OperationPhase::Reset,
                    "Starting over",
                );
            }
            // About and Repair read nothing more than the snapshot holds.
            Intent::Navigate(Destination::About | Destination::Repair) => {}
            Intent::Navigate(Destination::Settings) => {
                self.refresh_settings(backend.as_ref());
            }
            Intent::Navigate(Destination::Diagnostics) => {
                self.refresh_download_sources(backend.as_ref());
                self.replace_evidence(backend.diagnostics_evidence())
            }
            Intent::Navigate(Destination::Operations) => {
                // Page-specific evidence must not replace the Operations audit
                // after navigation. Active work already renders its retained
                // OperationView evidence and receives a fresh audit at its
                // completion boundary.
                if self.active_mutation.is_none() && self.snapshot.game() == GameView::Idle {
                    match backend.audit(ActivityEvidence::Idle) {
                        Ok(audit) => self.apply_audit(audit),
                        Err(error) => self.fail_closed(error),
                    }
                }
            }
            Intent::ExportDiagnostics => {
                if self.snapshot.report() == Some(&ReportView::Creating) {
                    return;
                }
                self.change_report(Some(ReportView::Creating));
                let worker_backend = backend.clone();
                let started = spawn_named("pd2-report", move || {
                    let saved = worker_backend.save_report();
                    let path = saved.as_ref().ok().cloned();
                    let _ = completions.send(Completion::ReportSaved(saved));
                    // The folder opens right away: that is where the report is.
                    if let Some(path) = path {
                        let opened = worker_backend.reveal_report(&path);
                        let _ = completions.send(Completion::ReportRevealed { path, opened });
                    }
                });
                if started.is_err() {
                    self.change_report(Some(ReportView::Failed("background-worker-unavailable")));
                }
            }
            Intent::RevealReport => {
                if let Some(ReportView::Saved { path, .. }) = self.snapshot.report().cloned() {
                    self.change_report(Some(ReportView::Saved {
                        path: path.clone(),
                        opened: None,
                    }));
                    let worker_backend = backend.clone();
                    let _ = spawn_named("pd2-report-reveal", move || {
                        let opened = worker_backend.reveal_report(&path);
                        let _ = completions.send(Completion::ReportRevealed { path, opened });
                    });
                }
            }
            Intent::DismissReport => self.change_report(None),
            Intent::OpenGameFolder => {
                let Some(folder) = crate::presentation::game_files_folder(&self.snapshot) else {
                    self.publish_guard_failure("game-folder-not-installed");
                    return;
                };
                if self.snapshot.game_files_open() == Some(FolderOpenView::Opening) {
                    return;
                }
                self.change_game_files_open(Some(FolderOpenView::Opening));
                let worker_backend = backend.clone();
                if spawn_named("pd2-game-folder", move || {
                    let opened = worker_backend.open_game_folder(&folder);
                    let _ = completions.send(Completion::GameFolderOpened(opened));
                })
                .is_err()
                {
                    self.change_game_files_open(Some(FolderOpenView::Failed));
                }
            }
            Intent::OpenSavesBackup => {
                let Some(notice) = self.snapshot.saves_put_aside().cloned() else {
                    return;
                };
                if notice.open == Some(FolderOpenView::Opening) {
                    return;
                }
                let folder = notice.folder.clone();
                self.change_saves_put_aside(Some(SavesPutAsideView {
                    open: Some(FolderOpenView::Opening),
                    ..notice
                }));
                let worker_backend = backend.clone();
                if spawn_named("pd2-backup-folder", move || {
                    let opened = worker_backend.open_backups(&folder);
                    let _ = completions.send(Completion::BackupOpened(opened));
                })
                .is_err()
                {
                    self.backup_opened(false);
                }
            }
            Intent::DismissSavesPutAside => self.change_saves_put_aside(None),
            Intent::RequestFullSnapshot => self.publish_full_snapshot(),
        }
    }

    fn handle_completion(&mut self, completion: Completion) -> bool {
        match completion {
            Completion::ReportSaved(result) => {
                self.change_report(Some(match result {
                    Ok(path) => ReportView::Saved { path, opened: None },
                    Err(error) => ReportView::Failed(error.code()),
                }));
                false
            }
            Completion::ReportRevealed { path, opened } => {
                if matches!(self.snapshot.report(), Some(ReportView::Saved { path: saved, .. }) if *saved == path)
                {
                    self.change_report(Some(ReportView::Saved {
                        path,
                        opened: Some(opened),
                    }));
                }
                false
            }
            Completion::GameFolderOpened(opened) => {
                self.change_game_files_open(Some(if opened {
                    FolderOpenView::Opened
                } else {
                    FolderOpenView::Failed
                }));
                false
            }
            Completion::BackupOpened(opened) => {
                self.backup_opened(opened);
                false
            }
            Completion::UpdateChecked(result) => {
                self.update_check_active = false;
                // A check that was running when the checks were turned off
                // says nothing.
                if !self.snapshot.settings().update_checks() {
                    self.update_check_pending = false;
                    self.change_update(UpdateStatus::Unknown);
                    return false;
                }
                self.change_update(result.unwrap_or(UpdateStatus::CheckFailed));
                std::mem::take(&mut self.update_check_pending)
            }
            Completion::MutationFinished {
                kind,
                result,
                audit,
                location,
                game_folder,
                fex_setting,
                saves_put_aside,
            } => {
                self.note_saves_put_aside(saves_put_aside.map(|put_aside| *put_aside));
                if self.active_mutation.as_ref().map(|active| active.kind) != Some(kind) {
                    return false;
                }
                self.active_mutation = None;
                self.change_active_job(None);
                if kind == MutationKind::InstallBaseGame {
                    // The backend's copy of the keys is gone with its job;
                    // the view keeps its own masked inputs for a retry.
                    self.change_authorization(AuthorizationView::Absent);
                }
                self.change_location(location);
                self.change_game_folder(game_folder);
                self.change_fex_setting(fex_setting);
                if matches!(kind, MutationKind::Relocate | MutationKind::RemovePrevious) {
                    self.change_location_error(result.as_ref().err().map(|error| error.code()));
                }
                match *audit {
                    Ok(audit) => self.apply_audit(audit),
                    Err(error) => self.fail_closed(error),
                }
                let operation_succeeded = result.is_ok();
                let failure = result.as_ref().err().map(|error| error.code());
                let stopped = failure == Some("operation-cancelled");
                let pending_return = self.pending_return.take();
                let (status, detail) = match result {
                    Ok(()) if kind == MutationKind::OfficialLauncher => (
                        OperationStatusView::Succeeded,
                        "The official launcher closed. A fresh audit re-checked the installation and accepted any client update it made.",
                    ),
                    Ok(()) if kind == MutationKind::RemovePrevious => (
                        OperationStatusView::Succeeded,
                        "The old installation was removed.",
                    ),
                    Ok(()) if kind == MutationKind::Relocate => (
                        OperationStatusView::Succeeded,
                        "The install location changed. A fresh audit established the installation state there.",
                    ),
                    Ok(()) if kind == MutationKind::Download => {
                        (OperationStatusView::Succeeded, "All downloads verified.")
                    }
                    Ok(()) if kind == MutationKind::DiscardDownloads => (
                        OperationStatusView::Succeeded,
                        "The Launcher's own downloads were removed; imported files were kept.",
                    ),
                    Ok(()) if kind == MutationKind::InstallBaseGame => (
                        OperationStatusView::Succeeded,
                        "Diablo II and Lord of Destruction are installed and checked.",
                    ),
                    Ok(()) if kind == MutationKind::InstallProjectDiablo => (
                        OperationStatusView::Succeeded,
                        "Project Diablo 2 is installed and checked.",
                    ),
                    Ok(()) if kind == MutationKind::Reset => (
                        OperationStatusView::Succeeded,
                        "The Managed prefix was removed. Downloads and the current Wine runtime were kept; install Diablo II again to continue.",
                    ),
                    Ok(()) => (
                        OperationStatusView::Succeeded,
                        "A fresh audit established the durable installation state.",
                    ),
                    Err(error) if error.code() == "official-launcher-confirmation-required" => (
                        OperationStatusView::Failed,
                        "The official launcher was not opened: PD2Launcher.exe is not verified, so running it needs your confirmation. Nothing was changed.",
                    ),
                    Err(error) if error.code() == "official-client-not-reconciled" => (
                        OperationStatusView::Failed,
                        "The official launcher closed, but the client update it made was not accepted because the protected-file inventory was incomplete. The displayed state comes from a fresh audit.",
                    ),
                    Err(error) if error.code() == "operation-cancelled" => (
                        OperationStatusView::Cancelled,
                        "The operation reached a safe stopped state; the displayed state comes from a fresh audit.",
                    ),
                    Err(error) if error.code() == "operation-indeterminate" => (
                        OperationStatusView::Indeterminate,
                        "The stop boundary could not be proven safe. Mutation stays excluded until fresh audit or Repair evidence resolves it.",
                    ),
                    Err(_) => (
                        OperationStatusView::Failed,
                        "The operation failed at a safe boundary; the displayed state comes from a fresh audit.",
                    ),
                };
                let phase = match kind {
                    MutationKind::Download
                    | MutationKind::InstallBaseGame
                    | MutationKind::InstallProjectDiablo
                    | MutationKind::DiscardDownloads => OperationPhase::Verification,
                    MutationKind::Update => OperationPhase::Verification,
                    MutationKind::Repair => OperationPhase::Repair,
                    MutationKind::OfficialLauncher => OperationPhase::OfficialLauncher,
                    MutationKind::Reset => OperationPhase::Reset,
                    MutationKind::Relocate | MutationKind::RemovePrevious => {
                        OperationPhase::Relocation
                    }
                };
                let mut outcome = OperationView::mutating(phase, status, "Operation complete")
                    .with_detail(detail)
                    .with_job(kind.job());
                if let Some(code) = failure {
                    outcome = outcome.with_failure(code);
                }
                self.change_operation(Some(outcome));
                let mut stop_asked = false;
                if kind == MutationKind::Download {
                    // A stop asked at its very end counts as a stop.
                    stop_asked = std::mem::take(&mut self.download_stop_requested);
                    self.download_unfinished = !operation_succeeded || stop_asked;
                    if operation_succeeded && !stop_asked && self.pending_install.is_some() {
                        // One flow: Install Diablo II starts with the waiting
                        // keys.
                        self.install_after_download = true;
                    }
                }
                // A Download asked to stop shows its stop even when it ended
                // well (the stop came at its very end): the app then knows no
                // install follows.
                if !self.install_after_download && kind.is_setup() && (stopped || stop_asked) {
                    self.finish_install_stop(pending_return);
                }
                // A new installation's first launch opens D2GL at 800x600:
                // say so before Play, not only once the game runs.
                if kind.is_setup()
                    && operation_succeeded
                    && self.snapshot.installation() == InstallationStatus::Ready
                {
                    self.change_game_notice(Some(GameNoticeView::FirstD2glLaunch));
                }
                if kind == MutationKind::Repair {
                    self.change_journal(
                        self.snapshot.installation() == InstallationStatus::RepairRequired,
                    );
                }
                // A stop of the official launcher can accept the update it
                // made, so the update check runs again, as after it closes.
                let check = (operation_succeeded
                    || (kind == MutationKind::OfficialLauncher && stopped))
                    && self.snapshot.installation() != InstallationStatus::RepairRequired;
                // On the Android host no check follows a job: the client may
                // have changed, so the update is unknown until Play asks.
                if self.android_host {
                    if check {
                        self.change_update(UpdateStatus::Unknown);
                    }
                    return false;
                }
                check
            }
        }
    }

    /// The read-only update check, unless the player turned the checks off:
    /// then none runs and the update stays unknown.
    fn start_update_check<B: CoordinatorBackend>(
        &mut self,
        backend: Arc<B>,
        completions: Sender<Completion>,
    ) {
        if !self.snapshot.settings().update_checks() {
            self.change_update(UpdateStatus::Unknown);
            return;
        }
        if self.snapshot.host() != HostView::Supported {
            return;
        }
        if self.update_check_active {
            self.update_check_pending = true;
            return;
        }
        self.update_check_active = true;
        self.change_update(UpdateStatus::Checking);
        if spawn_named("pd2-update-check", move || {
            let _ = completions.send(Completion::UpdateChecked(backend.check_for_update()));
        })
        .is_err()
        {
            self.update_check_active = false;
            self.change_update(UpdateStatus::CheckFailed);
        }
    }

    /// The update checks were turned on or off. Off, the update is unknown
    /// at once. The client is held to another rule, so an idle Launcher
    /// audits again at once (as after a navigation to Operations), then
    /// checks for an update when they are on (on the Android host only Play
    /// checks). While the game or a job runs, the next audit applies the
    /// rule; turned on then, the next check is a job's end, Check again or
    /// the next start.
    fn update_checks_changed<B: CoordinatorBackend>(
        &mut self,
        backend: Arc<B>,
        completions: Sender<Completion>,
    ) {
        if !self.snapshot.settings().update_checks() {
            self.change_update(UpdateStatus::Unknown);
        }
        let idle = self.active_mutation.is_none() && self.snapshot.game() == GameView::Idle;
        if idle {
            match backend.audit(ActivityEvidence::Idle) {
                Ok(audit) => self.apply_audit(audit),
                Err(error) => {
                    self.fail_closed(error);
                    return;
                }
            }
        }
        if (idle || !self.snapshot.settings().update_checks()) && !self.android_host {
            self.start_update_check(backend, completions);
        }
    }

    fn start_mutation<B: CoordinatorBackend>(
        &mut self,
        backend: Arc<B>,
        completions: Sender<Completion>,
        kind: MutationKind,
        authorization: Option<InstallationAuthorization>,
        phase: OperationPhase,
        title: &'static str,
    ) {
        if self.active_mutation.is_some() {
            self.publish_guard_failure("mutation-already-active");
            return;
        }
        if kind != MutationKind::Download {
            // Keys wait only for a Download (the install that takes them has
            // taken them already).
            self.drop_pending_install();
        }
        self.download_stop_requested = false;
        let relocation_target = self.relocation_target.take();
        let official_launcher_confirmed = std::mem::take(&mut self.official_launcher_confirmed);
        let official_launcher_strict_memory_order =
            std::mem::take(&mut self.official_launcher_strict_memory_order);
        let official_launcher_detailed_logging =
            std::mem::take(&mut self.official_launcher_detailed_logging);
        let import_mode = self.import_mode.take();
        let (progress, progress_receiver) = progress_channel();
        let cancellation = OperationControl::with_progress(progress);
        self.byte_rate = ByteRate::default();
        self.active_mutation = Some(ActiveMutation {
            kind,
            cancellation: cancellation.clone(),
            progress: progress_receiver,
        });
        self.change_active_job(Some(kind.job()));
        self.change_editing(None);
        self.change_stop_result(None);
        self.change_operation(Some(OperationView::mutating(
            phase,
            OperationStatusView::Running,
            title,
        )));
        if spawn_named("pd2-mutation", move || {
            let result = match kind {
                MutationKind::Download => backend.download_all(&cancellation),
                MutationKind::DiscardDownloads => backend.discard_downloads(&cancellation),
                MutationKind::InstallBaseGame => {
                    backend.install_base_game(authorization, import_mode, &cancellation)
                }
                MutationKind::InstallProjectDiablo => backend.install_project_diablo(&cancellation),
                MutationKind::Update => backend.apply_update(&cancellation),
                MutationKind::Repair => backend.repair(&cancellation),
                MutationKind::OfficialLauncher => backend.open_official_launcher(
                    official_launcher_confirmed,
                    official_launcher_strict_memory_order,
                    official_launcher_detailed_logging,
                    &cancellation,
                ),
                MutationKind::Reset => backend.reset_installation(&cancellation),
                MutationKind::RemovePrevious => backend.remove_previous_installation(&cancellation),
                MutationKind::Relocate => match relocation_target {
                    Some((parent, fresh_only)) => {
                        backend.relocate(parent, fresh_only, &cancellation)
                    }
                    None => Err(BackendFailure::new("location-change-failed")),
                },
            };
            if matches!(kind, MutationKind::Relocate | MutationKind::Reset) {
                // Re-resolve the (new) root: its selection hold, layout, and
                // diagnostics, exactly as at startup. After Start over, a
                // game folder used in place is forgotten with the prefix.
                let _ = backend.open_managed_layout();
            }
            cancellation.report(crate::operation::WorkProgress::for_phase(
                WorkPhase::Verification,
            ));
            let audit = Box::new(backend.audit(ActivityEvidence::Idle));
            let location = backend.install_location();
            let game_folder = backend.game_folder();
            let fex_setting = backend.fex_setting();
            let saves_put_aside = backend.take_saves_put_aside().map(Box::new);
            let _ = completions.send(Completion::MutationFinished {
                kind,
                result,
                audit,
                location,
                game_folder,
                fex_setting,
                saves_put_aside,
            });
        })
        .is_err()
        {
            self.active_mutation = None;
            self.change_active_job(None);
            if kind == MutationKind::InstallBaseGame {
                self.change_authorization(AuthorizationView::Absent);
            }
            self.change_operation(Some(
                OperationView::mutating(
                    phase,
                    OperationStatusView::Failed,
                    "Operation could not start",
                )
                .with_detail("The background worker was unavailable; no mutation was started."),
            ));
        }
    }

    fn launch_game(
        &mut self,
        backend: &dyn CoordinatorBackend,
        driver: GraphicsDriver,
        profile: Option<GraphicsProfile>,
        strict_memory_order: bool,
        detailed_logging: bool,
    ) {
        if !self.play_guard() {
            self.publish_guard_failure("play-not-permitted");
            return;
        }
        let fresh = match backend.audit(ActivityEvidence::Idle) {
            Ok(audit) => audit,
            Err(error) => {
                self.fail_closed(error);
                return;
            }
        };
        self.apply_audit(fresh);
        if !self.play_guard() {
            self.publish_guard_failure("play-preflight-rejected");
            self.play_failed("play-preflight-rejected");
            return;
        }
        let launched = backend.launch_game(driver, profile, strict_memory_order, detailed_logging);
        // The keep before it may have put characters aside, even when the
        // game then did not start.
        self.note_saves_put_aside(backend.take_saves_put_aside());
        match launched {
            Ok(notice) => {
                self.change_game_notice(notice);
                self.change_game(GameView::Running);
                self.change_operation(Some(OperationView::read_only(
                    OperationPhase::GameExecution,
                    OperationStatusView::Running,
                    "Game client is running",
                )));
            }
            Err(error) => {
                self.publish_guard_failure(error.code());
                self.play_failed(error.code());
            }
        }
    }

    /// Play did not start: the result card says so in plain words.
    fn play_failed(&mut self, code: &'static str) {
        self.change_operation(Some(
            OperationView::read_only(
                OperationPhase::GameExecution,
                OperationStatusView::Failed,
                "The game didn't start",
            )
            .with_failure(code),
        ));
    }

    fn poll_game(&mut self, backend: &dyn CoordinatorBackend) {
        if self.snapshot.game() != GameView::Running {
            return;
        }
        match backend.poll_game() {
            BackendGameState::Running => {}
            BackendGameState::Idle | BackendGameState::Exited => {
                // A first-launch tip stays after the game exits; the next
                // launch replaces it with that launch's own notice.
                self.change_game(GameView::Idle);
                self.change_operation(Some(OperationView::read_only(
                    OperationPhase::GameExecution,
                    OperationStatusView::Succeeded,
                    "Game client exited",
                )));
            }
            BackendGameState::SupervisionFailed => {
                self.change_operation(Some(
                    OperationView::read_only(
                        OperationPhase::GameExecution,
                        OperationStatusView::Failed,
                        "Game supervision needs attention",
                    )
                    .with_failure("game-supervision-failed"),
                ));
            }
        }
    }

    fn request_safe_stop(&mut self, backend: &dyn CoordinatorBackend) {
        if let Some(active) = &self.active_mutation {
            let cancellation = active.cancellation.clone();
            let kind = active.kind;
            if backend.request_mutation_safe_stop().is_err() {
                self.change_operation(Some(OperationView::mutating(
                    match kind {
                        MutationKind::Download
                        | MutationKind::InstallBaseGame
                        | MutationKind::InstallProjectDiablo
                        | MutationKind::DiscardDownloads => OperationPhase::Verification,
                        MutationKind::Update => OperationPhase::Promotion,
                        MutationKind::Repair => OperationPhase::Repair,
                        MutationKind::OfficialLauncher => OperationPhase::OfficialLauncher,
                        MutationKind::Reset => OperationPhase::Reset,
                        MutationKind::Relocate | MutationKind::RemovePrevious => {
                            OperationPhase::Relocation
                        }
                    },
                    OperationStatusView::Failed,
                    "The safe-stop request could not be recorded durably; the running mutation was not told to stop.",
                ).with_failure("safe-stop-unrecorded")));
                return;
            }
            cancellation.request_cancel();
            if kind == MutationKind::Download {
                self.download_stop_requested = true;
            }
            self.change_operation(Some(OperationView::mutating(
                match kind {
                    MutationKind::Download
                    | MutationKind::InstallBaseGame
                    | MutationKind::InstallProjectDiablo
                    | MutationKind::DiscardDownloads => OperationPhase::Verification,
                    MutationKind::Update => OperationPhase::Promotion,
                    MutationKind::Repair => OperationPhase::Repair,
                    MutationKind::OfficialLauncher => OperationPhase::OfficialLauncher,
                    MutationKind::Reset => OperationPhase::Reset,
                    MutationKind::Relocate | MutationKind::RemovePrevious => {
                        OperationPhase::Relocation
                    }
                },
                OperationStatusView::SafeStopping,
                "Stopping at a safe point",
            )));
        }
        if self.snapshot.game() == GameView::Running {
            let _ = backend.request_game_stop();
        }
    }

    fn poll_progress(&mut self) {
        let safe_stopping = self
            .active_mutation
            .as_ref()
            .is_some_and(|active| active.cancellation.is_cancelled());
        let Some(progress) = self
            .active_mutation
            .as_ref()
            .and_then(|active| active.progress.take_latest())
        else {
            return;
        };
        let phase = operation_phase(progress.phase());
        let status = if safe_stopping {
            OperationStatusView::SafeStopping
        } else {
            match progress.phase() {
                WorkPhase::Hashing | WorkPhase::Verification => OperationStatusView::Verifying,
                WorkPhase::Promotion => OperationStatusView::Committing,
                _ => OperationStatusView::Running,
            }
        };
        let mut operation = OperationView::mutating(phase, status, work_title(progress.phase()))
            .with_job(
                self.active_mutation
                    .as_ref()
                    .map_or(ActiveJobView::Download, |active| active.kind.job()),
            );
        // On Android an update's bar is the file it downloads, with its time
        // left: its file counts take in every manifest entry, most of them
        // already in place, so they say nothing of the update.
        let by_bytes = self.android_host
            && self
                .active_mutation
                .as_ref()
                .is_some_and(|active| active.kind == MutationKind::Update);
        let bytes = progress.file_bytes();
        if by_bytes && bytes.and_then(|value| value.total()).is_none() {
            // Between two files (the one done is checked, the entries
            // already in place are counted, the next has not begun) the
            // last file's figure and summary stay rather than going blank;
            // the manifest and the swap have none.
            let last = matches!(
                progress.phase(),
                WorkPhase::ClientDownload | WorkPhase::Hashing
            )
            .then(|| self.snapshot.operation())
            .flatten()
            .filter(|last| {
                last.job() == Some(ActiveJobView::Update) && last.progress_unit() == Some("bytes")
            })
            .and_then(|last| {
                Some((
                    last.progress_completed()?,
                    last.progress_total()?,
                    last.progress_summary().map(str::to_owned),
                ))
            });
            if let Some((completed, total, summary)) = last {
                operation = operation.with_typed_progress(completed, Some(total), "bytes");
                if let Some(summary) = summary {
                    operation = operation.with_progress_summary(summary);
                }
            }
            self.change_operation(Some(operation));
            return;
        }
        let files = progress
            .overall()
            .filter(|value| value.unit() == crate::operation::ProgressUnit::Files && !by_bytes);
        // One big download fills the bar by bytes; a many-file download by
        // files; anything else by whatever it reports.
        let primary = if by_bytes {
            bytes.filter(|value| value.total().is_some())
        } else {
            bytes
                .filter(|value| value.total().is_some() && files.is_none())
                .or(files)
                .or(progress.overall())
                .or(bytes)
                .or(progress.verification())
        };
        if let Some(primary) = primary {
            let label = match primary.unit() {
                crate::operation::ProgressUnit::Bytes => "bytes",
                crate::operation::ProgressUnit::Files => "files",
                crate::operation::ProgressUnit::Stages => "stages",
            };
            operation = operation.with_typed_progress(primary.completed(), primary.total(), label);
        }
        let mut summary = Vec::new();
        if let Some(bytes) = bytes {
            let rate = self.byte_rate.sample(
                progress.phase(),
                bytes.completed(),
                bytes.total(),
                Instant::now(),
            );
            summary.push(match bytes.total() {
                Some(total) => format!(
                    "{} of {}",
                    human_bytes(bytes.completed()),
                    human_bytes(total)
                ),
                None => format!("{} downloaded", human_bytes(bytes.completed())),
            });
            if let Some(rate) = rate {
                summary.push(format!("{}/s", human_bytes(rate)));
                if let Some(total) = bytes.total().filter(|_| files.is_none()) {
                    let left = total.saturating_sub(bytes.completed()) / rate.max(1);
                    summary.push(time_left(left, by_bytes));
                }
            }
        }
        if let Some(files) = files {
            summary.push(match files.total() {
                Some(total) => format!("{} of {total} files", files.completed()),
                None => format!("{} files", files.completed()),
            });
        }
        if let Some(checked) = progress.verification().filter(|_| files.is_some()) {
            summary.push(match checked.total() {
                Some(total) => format!("{} of {total} checked", checked.completed()),
                None => format!("{} checked", checked.completed()),
            });
        }
        if !summary.is_empty() {
            operation = operation.with_progress_summary(summary.join(" · "));
        }
        if let Some(environment) = progress.environment() {
            operation = operation.with_environment(environment);
        }
        self.change_operation(Some(operation));
    }

    /// Nothing else runs and nothing blocks the setup steps.
    fn setup_idle(&self) -> bool {
        self.snapshot.host() == HostView::Supported
            && self.active_mutation.is_none()
            && self.snapshot.game() == GameView::Idle
            && !self.snapshot.unresolved_journal()
            && !self.snapshot.location_unavailable()
            && !self.snapshot.game_folder_unavailable()
    }

    /// Step 3 needs a usable installer choice.
    fn download_guard(&self) -> bool {
        if !self.setup_idle() {
            return false;
        }
        // The built-in FEX chosen once the game was installed downloads on
        // its own.
        if self
            .snapshot
            .fex_setting()
            .is_some_and(FexSettingView::builtin_pending)
            && matches!(
                self.snapshot.installation(),
                InstallationStatus::Ready | InstallationStatus::NeedsRuntime
            )
        {
            return true;
        }
        self.snapshot.installer_media().is_ready()
            && matches!(
                self.snapshot.installation(),
                InstallationStatus::NeedsBaseGame
                    | InstallationStatus::NeedsExpansion
                    | InstallationStatus::NeedsClient
            )
    }

    /// Step 4 runs only from verified downloads, with both CD keys for
    /// Blizzard's installers.
    fn base_game_guard(&self, has_authorization: bool) -> bool {
        if !self.setup_idle() || !self.snapshot.downloads_ready() {
            return false;
        }
        match self.snapshot.installation() {
            InstallationStatus::NeedsBaseGame | InstallationStatus::NeedsExpansion => {
                has_authorization
                    && self.snapshot.installer_media().is_ready()
                    && self.snapshot.installer_media().existing().is_none()
            }
            _ => false,
        }
    }

    /// Blizzard's installers are what the install still needs: Diablo II or
    /// Lord of Destruction to install, from the installers' choice (not an
    /// existing installation).
    fn installers_wanted(&self) -> bool {
        matches!(
            self.snapshot.installation(),
            InstallationStatus::NeedsBaseGame | InstallationStatus::NeedsExpansion
        ) && self.snapshot.installer_media().is_ready()
            && self.snapshot.installer_media().existing().is_none()
    }

    fn active_kind(&self) -> Option<MutationKind> {
        self.active_mutation.as_ref().map(|active| active.kind)
    }

    fn drop_pending_install(&mut self) {
        self.install_after_download = false;
        if self.pending_install.take().is_some() {
            self.change_authorization(AuthorizationView::Absent);
        }
    }

    /// What waits for the job that just ended: the removal after a stopped
    /// Download, or Install Diablo II with the keys given during it.
    fn start_pending_work<B: CoordinatorBackend>(
        &mut self,
        backend: Arc<B>,
        completions: Sender<Completion>,
    ) {
        if self.active_mutation.is_some() {
            return;
        }
        if std::mem::take(&mut self.discard_after_stop) {
            if self.setup_idle() {
                self.start_mutation(
                    backend,
                    completions,
                    MutationKind::DiscardDownloads,
                    None,
                    OperationPhase::Verification,
                    "Removing the downloads",
                );
            } else {
                self.publish_guard_failure("discard-downloads-not-permitted");
            }
            return;
        }
        if !std::mem::take(&mut self.install_after_download) {
            return;
        }
        let Some(authorization) = self.pending_install.take() else {
            return;
        };
        if !self.base_game_guard(true) {
            self.change_authorization(AuthorizationView::Absent);
            self.publish_guard_failure(if self.snapshot.downloads_ready() {
                "install-not-permitted"
            } else {
                "downloads-required"
            });
            return;
        }
        self.start_mutation(
            backend,
            completions,
            MutationKind::InstallBaseGame,
            Some(authorization),
            OperationPhase::BaseGameInstall,
            "Installing Diablo II",
        );
    }

    /// Step 4 for an existing installation: a usable folder, verified
    /// downloads, and nothing of Diablo II installed yet.
    fn import_guard(&self) -> bool {
        self.setup_idle()
            && self.snapshot.downloads_ready()
            && self.snapshot.installation() == InstallationStatus::NeedsBaseGame
            && self
                .snapshot
                .installer_media()
                .existing()
                .is_some_and(|existing| existing.problem.is_none())
    }

    /// Step 5 needs Diablo II installed.
    fn project_diablo_guard(&self) -> bool {
        self.setup_idle()
            && matches!(
                self.snapshot.installation(),
                InstallationStatus::NeedsClient | InstallationStatus::NeedsRuntime
            )
    }

    /// The job Continue runs: the step the installation stands at.
    fn continue_kind(&self) -> Option<MutationKind> {
        if !self.setup_idle() {
            return None;
        }
        match self.snapshot.installation() {
            InstallationStatus::NeedsBaseGame | InstallationStatus::NeedsExpansion
                if !self.snapshot.downloads_ready() =>
            {
                self.download_guard().then_some(MutationKind::Download)
            }
            InstallationStatus::NeedsClient | InstallationStatus::NeedsRuntime => {
                Some(MutationKind::InstallProjectDiablo)
            }
            _ => None,
        }
    }

    fn update_guard(&self) -> bool {
        self.snapshot.host() == HostView::Supported
            && self.snapshot.installation() == InstallationStatus::Ready
            && self.snapshot.update() == UpdateStatus::Available
            && self.snapshot.game() == GameView::Idle
            && self.active_mutation.is_none()
            && !self.snapshot.unresolved_journal()
    }

    /// The official launcher is a fallback for an installed game whose
    /// Launcher-side update or start is broken: Ready, or only the client is
    /// out of date. Nothing else may run.
    fn official_launcher_guard(&self) -> bool {
        self.snapshot.host() == HostView::Supported
            && matches!(
                self.snapshot.installation(),
                InstallationStatus::Ready | InstallationStatus::NeedsClient
            )
            && self.snapshot.game() == GameView::Idle
            && self.active_mutation.is_none()
            && !self.snapshot.unresolved_journal()
    }

    /// A location is chosen or changed only while nothing runs.
    /// An unavailable location may always be replaced by a fresh one.
    fn relocation_guard(&self) -> bool {
        self.active_mutation.is_none()
            && self.snapshot.game() == GameView::Idle
            && (self.snapshot.location_unavailable()
                || (self.snapshot.host() == HostView::Supported
                    && !self.snapshot.unresolved_journal()
                    && self.snapshot.installation() != InstallationStatus::RepairRequired))
    }

    /// The old installation is removed only once the new one is Ready and
    /// nothing runs.
    fn remove_previous_guard(&self) -> bool {
        self.active_mutation.is_none()
            && self.snapshot.game() == GameView::Idle
            && self.snapshot.installation() == InstallationStatus::Ready
            && self
                .snapshot
                .location()
                .is_some_and(|location| location.available && location.previous.is_some())
    }

    /// Start over is a confirmed, explicit user action on an idle, supported
    /// installation without an unresolved journal.
    fn reset_guard(&self) -> bool {
        (self.snapshot.host() == HostView::Supported || self.snapshot.game_folder_unavailable())
            && self.snapshot.game() == GameView::Idle
            && self.active_mutation.is_none()
            && !self.snapshot.unresolved_journal()
            && self.snapshot.installation() != InstallationStatus::RepairRequired
    }

    fn play_guard(&self) -> bool {
        self.snapshot.host() == HostView::Supported
            && self.snapshot.installation() == InstallationStatus::Ready
            && self.snapshot.launch_preflight_passed()
            && self.snapshot.game() == GameView::Idle
            && self.active_mutation.is_none()
            && !self.snapshot.unresolved_journal()
    }

    fn can_shutdown(&self) -> bool {
        self.active_mutation.is_none() && self.snapshot.game() == GameView::Idle
    }

    fn apply_audit(&mut self, audit: BackendAudit) {
        self.change_host(audit.host);
        self.change_installation(audit.installation);
        if self.snapshot.downloads() != audit.downloads {
            self.change_downloads(audit.downloads);
        }
        // Warn before the first launch too, not only after an install in
        // this session. A notice already shown stays until the next start.
        if audit.first_launch_pending
            && audit.installation == InstallationStatus::Ready
            && self.snapshot.game() == GameView::Idle
            && self.snapshot.game_notice().is_none()
        {
            self.change_game_notice(Some(GameNoticeView::FirstD2glLaunch));
        }
        self.change_journal(audit.installation == InstallationStatus::RepairRequired);
        self.change_launch_preflight(audit.launch_preflight == LaunchPreflight::Passed);
        self.change_system(audit.system);
        self.change_evidence(audit.evidence);
        if let Some(software) = audit.game_software_rendering
            && self.snapshot.game_software_rendering() != software
        {
            self.snapshot = self.snapshot.clone().with_game_software_rendering(software);
            self.publish(DomainEvent::GameSoftwareRenderingChanged(software));
        }
        if self.snapshot.game_language() != audit.game_language {
            self.snapshot = self
                .snapshot
                .clone()
                .with_game_language(audit.game_language);
            self.publish(DomainEvent::GameLanguageChanged(audit.game_language));
        }
        if self.snapshot.environment_ready() != audit.environment_ready {
            self.snapshot = self
                .snapshot
                .clone()
                .with_environment_ready(audit.environment_ready);
            self.publish(DomainEvent::EnvironmentReadyChanged(
                audit.environment_ready,
            ));
        }
        if self.snapshot.official_launcher_unverified() != audit.official_launcher_unverified {
            self.snapshot = self
                .snapshot
                .clone()
                .with_official_launcher_unverified(audit.official_launcher_unverified);
            self.publish(DomainEvent::OfficialLauncherTrustChanged(
                audit.official_launcher_unverified,
            ));
        }
    }

    fn fail_closed(&mut self, error: BackendFailure) {
        self.change_host(HostView::Unsupported);
        self.change_installation(InstallationStatus::Indeterminate);
        self.change_launch_preflight(false);
        self.change_operation(Some(
            OperationView::read_only(
                OperationPhase::HostAudit,
                OperationStatusView::Failed,
                "Launcher service could not establish safe evidence",
            )
            .with_evidence(vec![error.code().to_owned()])
            .with_failure(error.code()),
        ));
    }

    fn publish_guard_failure(&mut self, code: &'static str) {
        self.change_evidence(vec![EvidenceView::new(
            REQUEST_NOT_STARTED,
            code,
            EvidenceTone::Advisory,
        )]);
    }

    fn replace_evidence(&mut self, evidence: Result<Vec<EvidenceView>, BackendFailure>) {
        match evidence {
            Ok(evidence) => self.change_evidence(evidence),
            Err(error) => self.publish_guard_failure(error.code()),
        }
    }

    fn change_fex_setting(&mut self, value: Option<FexSettingView>) {
        if self.snapshot.fex_setting() == value.as_ref() {
            return;
        }
        self.snapshot = self.snapshot.clone().with_fex_setting(value.clone());
        self.publish(DomainEvent::FexSettingChanged(value));
    }

    fn refresh_settings(&mut self, backend: &dyn CoordinatorBackend) {
        self.change_fex_setting(backend.fex_setting());
        match backend.load_settings() {
            Ok(settings) => self.change_settings(settings),
            Err(error) => {
                self.change_settings(self.snapshot.settings().failed(error.code()));
            }
        }
    }

    fn refresh_download_sources(&mut self, backend: &dyn CoordinatorBackend) {
        if let Ok(sources) = backend.download_sources() {
            self.change_download_sources(sources);
        }
    }

    /// A rejected Save leaves every source unchanged and marks only its row.
    fn apply_download_sources(
        &mut self,
        artifact: DownloadArtifact,
        result: Result<Vec<DownloadSourceView>, BackendFailure>,
    ) {
        match result {
            Ok(sources) => self.change_download_sources(sources),
            Err(error) => {
                let sources = self
                    .snapshot
                    .download_sources()
                    .iter()
                    .cloned()
                    .map(|mut source| {
                        source.error = (source.artifact == artifact).then_some(error.code());
                        source
                    })
                    .collect();
                self.change_download_sources(sources);
            }
        }
    }

    fn change_download_sources(&mut self, value: Vec<DownloadSourceView>) {
        self.snapshot = self.snapshot.clone().with_download_sources(value.clone());
        self.publish(DomainEvent::DownloadSourcesChanged(value));
    }

    /// A user-requested install stop was proven safe and a fresh audit is
    /// applied: report what was kept, and open the editor the user stopped
    /// for when the audit leaves that step changeable. An audit that cannot
    /// establish one outcome shows the recovery card instead.
    fn finish_install_stop(&mut self, pending_return: Option<SetupStep>) {
        let installation = self.snapshot.installation();
        let recovery = matches!(
            installation,
            InstallationStatus::Indeterminate | InstallationStatus::RepairRequired
        );
        self.change_stop_result(Some(StopResultView::new(installation, recovery)));
        if let Some(step) = pending_return
            && !recovery
            && step_affordance(&self.snapshot, step) == StepAffordance::Change
        {
            self.change_editing(Some(step));
        }
    }

    fn change_stop_result(&mut self, value: Option<StopResultView>) {
        if self.snapshot.stop_result() == value {
            return;
        }
        self.snapshot = self.snapshot.clone().with_stop_result(value);
        self.publish(DomainEvent::StopResultChanged(value));
    }

    fn change_location_error(&mut self, value: Option<&'static str>) {
        if self.snapshot.location_error() == value {
            return;
        }
        self.snapshot = self.snapshot.clone().with_location_error(value);
        self.publish(DomainEvent::LocationErrorChanged(value));
    }

    fn change_downloads(&mut self, value: DownloadsView) {
        self.snapshot = self.snapshot.clone().with_downloads(value);
        self.publish(DomainEvent::DownloadsChanged(value));
    }

    fn change_game_folder(&mut self, value: Option<GameFolderView>) {
        if self.snapshot.game_folder() == value.as_ref() {
            return;
        }
        self.snapshot = self.snapshot.clone().with_game_folder(value.clone());
        self.publish(DomainEvent::GameFolderChanged(value));
    }

    fn change_location(&mut self, value: Option<LocationView>) {
        if self.snapshot.location() == value.as_ref() {
            return;
        }
        self.snapshot = self.snapshot.clone().with_location(value.clone());
        self.publish(DomainEvent::LocationChanged(value));
    }

    fn change_editing(&mut self, value: Option<SetupStep>) {
        if self.snapshot.editing() == value {
            return;
        }
        self.snapshot = self.snapshot.clone().with_editing(value);
        self.publish(DomainEvent::EditingChanged(value));
    }

    fn change_active_job(&mut self, value: Option<ActiveJobView>) {
        self.snapshot = self.snapshot.clone().with_active_job(value);
        self.publish(DomainEvent::ActiveJobChanged(value));
    }

    fn change_host(&mut self, value: HostView) {
        self.snapshot = self.snapshot.clone().with_host(value);
        self.publish(DomainEvent::HostChanged(value));
    }

    fn change_authorization(&mut self, value: AuthorizationView) {
        self.snapshot = self.snapshot.clone().with_authorization(value);
        self.publish(DomainEvent::AuthorizationChanged(value));
    }

    /// A running job resolved its installer media when it started. A new
    /// choice now would show one source while the job uses another, so it is
    /// rejected; the user stops the install first ("Stop and change").
    fn media_guard(&mut self) -> bool {
        if self.active_mutation.is_some() {
            self.publish_guard_failure("installer-media-locked-while-installing");
            return false;
        }
        true
    }

    /// A rejected choice keeps the prior valid one, which the backend also
    /// still holds, and reports why the new one was not accepted.
    fn apply_installer_media(&mut self, result: Result<InstallerMediaView, BackendFailure>) {
        match result {
            Ok(media) => {
                if media != *self.snapshot.installer_media() {
                    self.drop_pending_install();
                }
                self.change_installer_media(media);
                self.change_installer_media_rejection(None);
            }
            Err(error) if self.snapshot.installer_media().is_ready() => {
                self.change_installer_media_rejection(Some(error.code()));
            }
            Err(error) => {
                self.change_installer_media(InstallerMediaView::Invalid(error.code()));
                self.change_installer_media_rejection(None);
            }
        }
    }

    fn change_installer_media_rejection(&mut self, value: Option<&'static str>) {
        if self.snapshot.installer_media_rejection() == value {
            return;
        }
        self.snapshot = self.snapshot.clone().with_installer_media_rejection(value);
        self.publish(DomainEvent::InstallerMediaRejected(value));
    }

    fn change_installer_media(&mut self, value: InstallerMediaView) {
        self.snapshot = self.snapshot.clone().with_installer_media(value.clone());
        self.publish(DomainEvent::InstallerMediaChanged(value));
    }

    fn change_installation(&mut self, value: InstallationStatus) {
        self.snapshot = self.snapshot.clone().with_installation(value);
        self.publish(DomainEvent::InstallationChanged(value));
    }

    fn change_update(&mut self, value: UpdateStatus) {
        self.snapshot = self.snapshot.clone().with_update(value);
        self.publish(DomainEvent::UpdateChanged(value));
    }

    fn change_operation(&mut self, value: Option<OperationView>) {
        self.snapshot = self.snapshot.clone().with_operation(value.clone());
        self.publish(DomainEvent::OperationChanged(value));
    }

    fn change_game(&mut self, value: GameView) {
        self.snapshot = self.snapshot.clone().with_game(value);
        self.publish(DomainEvent::GameChanged(value));
    }

    fn change_game_notice(&mut self, value: Option<GameNoticeView>) {
        if self.snapshot.game_notice() == value {
            return;
        }
        self.snapshot = self.snapshot.clone().with_game_notice(value);
        self.publish(DomainEvent::GameNoticeChanged(value));
    }

    fn change_journal(&mut self, value: bool) {
        self.snapshot = self.snapshot.clone().with_journal(value);
        self.publish(DomainEvent::JournalChanged(value));
    }

    fn change_launch_preflight(&mut self, value: bool) {
        self.snapshot = self.snapshot.clone().with_launch_preflight(value);
        self.publish(DomainEvent::LaunchPreflightChanged(value));
    }

    fn change_system(&mut self, value: SystemContractView) {
        self.snapshot = self.snapshot.clone().with_system(value.clone());
        self.publish(DomainEvent::SystemContractChanged(value));
    }

    fn change_settings(&mut self, value: LaunchSettingsView) {
        self.snapshot = self.snapshot.clone().with_settings(value.clone());
        self.publish(DomainEvent::SettingsChanged(value));
    }

    fn change_report(&mut self, value: Option<ReportView>) {
        if self.snapshot.report() != value.as_ref() {
            self.snapshot = self.snapshot.clone().with_report(value.clone());
            self.publish(DomainEvent::ReportChanged(value));
        }
    }

    /// Write the setup record as the setup progresses ([`next_setup_record`]).
    /// It only chooses the page the next start opens on, so a record that
    /// can't be written costs nothing but that: the check still decides.
    fn follow_setup_record(&mut self, backend: &dyn CoordinatorBackend) {
        let current = self.snapshot.setup_record();
        let next = next_setup_record(current, &self.snapshot);
        if self.android_host || next == current {
            return;
        }
        if let Err(error) = backend.save_setup_record(next) {
            eprintln!(
                "pd2-launcher: the setup record could not be saved ({})",
                error.code()
            );
        }
        self.change_setup_record(next);
    }

    fn change_game_files_open(&mut self, value: Option<FolderOpenView>) {
        if self.snapshot.game_files_open() == value {
            return;
        }
        self.snapshot = self.snapshot.clone().with_game_files_open(value);
        self.publish(DomainEvent::GameFilesOpenChanged(value));
    }

    fn change_saves_put_aside(&mut self, value: Option<SavesPutAsideView>) {
        if self.snapshot.saves_put_aside() == value.as_ref() {
            return;
        }
        self.snapshot = self.snapshot.clone().with_saves_put_aside(value.clone());
        self.publish(DomainEvent::SavesPutAsideChanged(value));
    }

    /// Characters a keep put aside join Play's notice until it is dismissed.
    fn note_saves_put_aside(&mut self, put_aside: Option<SavesPutAsideView>) {
        if let Some(put_aside) = put_aside {
            let notice = self.snapshot.saves_put_aside_with(put_aside);
            self.change_saves_put_aside(Some(notice));
        }
    }

    fn backup_opened(&mut self, opened: bool) {
        if let Some(notice) = self.snapshot.saves_put_aside().cloned() {
            self.change_saves_put_aside(Some(SavesPutAsideView {
                open: Some(if opened {
                    FolderOpenView::Opened
                } else {
                    FolderOpenView::Failed
                }),
                ..notice
            }));
        }
    }

    fn change_setup_record(&mut self, value: SetupRecord) {
        if self.snapshot.setup_record() == value {
            return;
        }
        self.snapshot = self.snapshot.clone().with_setup_record(value);
        self.publish(DomainEvent::SetupRecordChanged(value));
    }

    fn change_evidence(&mut self, value: Vec<EvidenceView>) {
        self.snapshot = self.snapshot.clone().with_evidence(value.clone());
        self.publish(DomainEvent::EvidenceChanged(value));
    }

    fn publish(&mut self, event: DomainEvent) {
        self.sequence += 1;
        self.snapshot = self.snapshot.clone().with_sequence(self.sequence);
        match self
            .events
            .try_send(SequencedEvent::new(self.sequence, event))
        {
            Ok(()) | Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {}
        }
    }

    fn publish_full_snapshot(&mut self) {
        self.sequence += 1;
        self.snapshot = self.snapshot.clone().with_sequence(self.sequence);
        let event = SequencedEvent::new(
            self.sequence,
            DomainEvent::FullSnapshot(Box::new(self.snapshot.clone())),
        );
        let _ = self.events.try_send(event);
    }
}

/// The setup record after `snapshot`: none once Start over succeeded;
/// installed once Play is reachable (a supported computer and a Ready
/// installation); started once an install step runs, from none. Nothing
/// else changes it: an installed game a later check finds broken stays
/// installed, and that check sends it to Continue installation or Start
/// over.
fn next_setup_record(record: SetupRecord, snapshot: &UiSnapshot) -> SetupRecord {
    let reset = snapshot.operation().is_some_and(|operation| {
        operation.phase() == OperationPhase::Reset
            && operation.status() == OperationStatusView::Succeeded
    });
    let installing = matches!(
        snapshot.active_job(),
        Some(ActiveJobView::InstallBaseGame | ActiveJobView::InstallProjectDiablo)
    );
    if reset {
        SetupRecord::None
    } else if snapshot.host() == HostView::Supported
        && snapshot.installation() == InstallationStatus::Ready
    {
        SetupRecord::Installed
    } else if record == SetupRecord::None && installing {
        SetupRecord::Started
    } else {
        record
    }
}

fn spawn_named(
    name: &'static str,
    run: impl FnOnce() + Send + 'static,
) -> Result<(), BackendFailure> {
    thread::Builder::new()
        .name(name.to_owned())
        .spawn(run)
        .map(|_| ())
        .map_err(|_| BackendFailure::new("background-worker-unavailable"))
}

const fn operation_phase(phase: WorkPhase) -> OperationPhase {
    match phase {
        WorkPhase::RuntimeAcquisition => OperationPhase::RuntimeAcquisition,
        WorkPhase::PrefixInitialization => OperationPhase::PrefixInitialization,
        WorkPhase::BaseGameInstall => OperationPhase::BaseGameInstall,
        WorkPhase::ExpansionInstall => OperationPhase::ExpansionInstall,
        WorkPhase::BaseGameDownload => OperationPhase::BaseGameDownload,
        WorkPhase::ExpansionDownload => OperationPhase::ExpansionDownload,
        WorkPhase::ImportGame => OperationPhase::ImportGame,
        WorkPhase::ClientManifest | WorkPhase::ClientDownload => OperationPhase::ClientDownload,
        WorkPhase::Hashing | WorkPhase::Verification => OperationPhase::Verification,
        WorkPhase::Promotion => OperationPhase::Promotion,
        WorkPhase::Repair => OperationPhase::Repair,
        WorkPhase::OfficialLauncher => OperationPhase::OfficialLauncher,
        WorkPhase::Reset => OperationPhase::Reset,
        WorkPhase::Relocation => OperationPhase::Relocation,
    }
}

/// The time left in a progress summary: "less than a minute left" under a
/// minute, else "about 3 minutes left". [`this_file`]: the figure is the
/// one file downloading now (an update on Android), so the time left says
/// it covers that file.
fn time_left(seconds: u64, this_file: bool) -> String {
    let left = if seconds < 60 {
        "less than a minute left".to_owned()
    } else {
        format!("about {} left", human_duration(seconds))
    };
    if this_file {
        format!("{left} for this file")
    } else {
        left
    }
}

const fn work_title(phase: WorkPhase) -> &'static str {
    match phase {
        WorkPhase::RuntimeAcquisition => "Downloading the Windows compatibility layer (Wine)",
        WorkPhase::PrefixInitialization => "Setting up the game environment",
        WorkPhase::BaseGameInstall => "Installing Diablo II",
        WorkPhase::ExpansionInstall => "Installing Lord of Destruction",
        WorkPhase::BaseGameDownload => "Downloading Diablo II from Blizzard",
        WorkPhase::ExpansionDownload => "Downloading Lord of Destruction from Blizzard",
        WorkPhase::ImportGame => "Setting up your Diablo II",
        WorkPhase::ClientManifest => "Installing Project Diablo 2",
        WorkPhase::ClientDownload => "Downloading Project Diablo 2 files",
        WorkPhase::Hashing => "Checking the downloaded files",
        WorkPhase::Promotion => "Installing Project Diablo 2 files",
        WorkPhase::Repair => "Repairing the last update",
        WorkPhase::OfficialLauncher => OFFICIAL_LAUNCHER_TITLE,
        WorkPhase::Reset => "Starting over",
        WorkPhase::Relocation => "Changing the install location",
        WorkPhase::Verification => "Checking the installation",
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        path::{Path, PathBuf},
        sync::{Condvar, Mutex, mpsc::sync_channel},
        time::{Duration, Instant},
    };

    use super::*;
    use crate::presentation::{
        ActionId, ActionView, AuthorizationInput, DeckPresentation, ReduceOutcome,
        RendererPreferenceView, SettingsPersistenceView, UiReducer,
    };

    // A limit for the waits below, not a delay: a test goes on as soon as its condition holds.
    // CI's small runners took more than 3 s for some events (2026-10-30).
    const WAIT: Duration = Duration::from_secs(30);

    /// Waits until the backend has made `call` `count` times, then checks that it made it
    /// exactly that often. The coordinator records a call on its own thread, so the call can
    /// come a moment after the snapshot that the test waited for.
    fn wait_for_calls(backend: &FakeBackend, call: &str, count: usize) {
        let deadline = Instant::now() + WAIT;
        loop {
            let made = backend.calls().iter().filter(|made| **made == call).count();
            if made >= count || Instant::now() >= deadline {
                assert_eq!(made, count, "{call} calls: {:?}", backend.calls());
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_chosen_built_in_fex_downloads_for_an_installed_game() {
        let (events, _received) = std::sync::mpsc::sync_channel(64);
        let mut coordinator = Coordinator::new(events);
        let fex = |installed| FexSettingView {
            source: crate::presentation::FexSourceView::Builtin,
            builtin_available: true,
            builtin_installed: installed,
            ..FexSettingView::default()
        };
        let ready = |fex_setting| {
            UiSnapshot::initial()
                .with_host(HostView::Supported)
                .with_installation(InstallationStatus::Ready)
                .with_fex_setting(fex_setting)
        };
        coordinator.snapshot = ready(None);
        assert!(
            !coordinator.download_guard(),
            "an installed game has nothing to download"
        );
        coordinator.snapshot = ready(Some(fex(false)));
        assert!(coordinator.download_guard(), "the built-in FEX it now uses");
        coordinator.snapshot = ready(Some(fex(true)));
        assert!(!coordinator.download_guard(), "once it is installed");
    }

    #[derive(Clone)]
    struct Gate {
        state: Arc<(Mutex<bool>, Condvar)>,
    }

    impl Gate {
        fn open() -> Self {
            Self {
                state: Arc::new((Mutex::new(true), Condvar::new())),
            }
        }

        fn closed() -> Self {
            Self {
                state: Arc::new((Mutex::new(false), Condvar::new())),
            }
        }

        fn release(&self) {
            let (released, changed) = &*self.state;
            *released.lock().expect("gate") = true;
            changed.notify_all();
        }

        fn wait(&self, cancellation: Option<&CancellationToken>) -> bool {
            let (released, changed) = &*self.state;
            let mut released = released.lock().expect("gate");
            while !*released {
                if cancellation.is_some_and(CancellationToken::is_requested) {
                    return false;
                }
                let (next, _) = changed
                    .wait_timeout(released, Duration::from_millis(10))
                    .expect("gate wait");
                released = next;
            }
            true
        }
    }

    struct FakeState {
        audits: VecDeque<BackendAudit>,
        latest_audit: BackendAudit,
        calls: Vec<&'static str>,
        update_status: UpdateStatus,
        update_check_failure: Option<BackendFailure>,
        game: BackendGameState,
        install_calls: usize,
        authorized_installs: usize,
        update_calls: usize,
        launch_calls: usize,
        launch_driver: GraphicsDriver,
        launch_profile: Option<GraphicsProfile>,
        launch_strict_memory_order: bool,
        launch_detailed_logging: bool,
        launch_notice: Option<GameNoticeView>,
        cancellation_observed: bool,
        settings: LaunchSettingsView,
        settings_load_failure: Option<BackendFailure>,
        settings_save_failure: Option<BackendFailure>,
        settings_updates: usize,
        install_failure: Option<BackendFailure>,
        official_launcher_calls: usize,
        official_launcher_confirmed: bool,
        official_launcher_strict_memory_order: bool,
        official_launcher_detailed_logging: bool,
        official_launcher_failure: Option<BackendFailure>,
        /// Locations reported by successive opens; the last one repeats.
        locations: VecDeque<Option<LocationView>>,
        location: Option<LocationView>,
        /// The next audit fails with this code (once).
        audit_failure: Option<BackendFailure>,
        launch_failure: Option<BackendFailure>,
        /// How each Install Diablo II job imported, in order.
        imports: Vec<ImportModeView>,
        /// A game folder used in place; Start over forgets it.
        game_folder: Option<GameFolderView>,
        /// The last authorization a job was given, dead once dropped.
        authorization: Option<std::sync::Weak<()>>,
        /// The setup record on disk, and every record saved, in order.
        setup_record: SetupRecord,
        saved_records: Vec<SetupRecord>,
        /// The game folders Open game folder opened, in order.
        opened_folders: Vec<PathBuf>,
        /// What the next keep puts aside, taken once.
        saves_put_aside: Option<SavesPutAsideView>,
        /// The setup job ends well even when a stop was asked (a stop that
        /// comes at its very end).
        stop_too_late: bool,
    }

    struct FakeBackend {
        state: Mutex<FakeState>,
        check_gate: Gate,
        install_gate: Gate,
        update_gate: Gate,
        official_launcher_gate: Gate,
        changed: Condvar,
    }

    impl FakeBackend {
        fn new(audits: impl IntoIterator<Item = BackendAudit>, update: UpdateStatus) -> Self {
            let audits = audits.into_iter().collect::<VecDeque<_>>();
            let latest_audit = audits.front().cloned().expect("at least one audit");
            Self {
                state: Mutex::new(FakeState {
                    audits,
                    latest_audit,
                    calls: Vec::new(),
                    update_status: update,
                    update_check_failure: None,
                    game: BackendGameState::Idle,
                    install_calls: 0,
                    authorized_installs: 0,
                    update_calls: 0,
                    launch_calls: 0,
                    launch_driver: GraphicsDriver::Standard,
                    launch_profile: None,
                    launch_strict_memory_order: false,
                    launch_detailed_logging: false,
                    launch_notice: None,
                    cancellation_observed: false,
                    settings: LaunchSettingsView::new(RendererPreferenceView::Glide, true, false),
                    settings_load_failure: None,
                    settings_save_failure: None,
                    settings_updates: 0,
                    install_failure: None,
                    official_launcher_calls: 0,
                    official_launcher_confirmed: false,
                    official_launcher_strict_memory_order: false,
                    official_launcher_detailed_logging: false,
                    official_launcher_failure: None,
                    locations: VecDeque::new(),
                    location: None,
                    audit_failure: None,
                    launch_failure: None,
                    imports: Vec::new(),
                    game_folder: None,
                    authorization: None,
                    setup_record: SetupRecord::None,
                    saved_records: Vec::new(),
                    opened_folders: Vec::new(),
                    saves_put_aside: None,
                    stop_too_late: false,
                }),
                check_gate: Gate::open(),
                install_gate: Gate::open(),
                update_gate: Gate::open(),
                official_launcher_gate: Gate::closed(),
                changed: Condvar::new(),
            }
        }

        fn with_check_blocked(mut self) -> Self {
            self.check_gate = Gate::closed();
            self
        }

        fn with_check_failure(self, failure: BackendFailure) -> Self {
            self.state.lock().expect("state").update_check_failure = Some(failure);
            self
        }

        fn with_install_blocked(mut self) -> Self {
            self.install_gate = Gate::closed();
            self
        }

        fn with_install_failure(self, failure: BackendFailure) -> Self {
            self.state.lock().expect("state").install_failure = Some(failure);
            self
        }

        fn with_settings_load_failure(self, failure: BackendFailure) -> Self {
            self.state.lock().expect("state").settings_load_failure = Some(failure);
            self
        }

        fn with_settings_save_failure(self, failure: BackendFailure) -> Self {
            self.state.lock().expect("state").settings_save_failure = Some(failure);
            self
        }

        fn with_update_blocked(mut self) -> Self {
            self.update_gate = Gate::closed();
            self
        }

        fn calls(&self) -> Vec<&'static str> {
            self.state.lock().expect("state").calls.clone()
        }

        fn wait_for(&self, predicate: impl Fn(&FakeState) -> bool) {
            let deadline = Instant::now() + WAIT;
            let mut state = self.state.lock().expect("state");
            while !predicate(&state) {
                let now = Instant::now();
                assert!(
                    now < deadline,
                    "backend condition timed out: {:?}",
                    state.calls
                );
                let (next, _) = self
                    .changed
                    .wait_timeout(state, deadline - now)
                    .expect("backend wait");
                state = next;
            }
        }

        fn notify(&self, state: std::sync::MutexGuard<'_, FakeState>) {
            drop(state);
            self.changed.notify_all();
        }
    }

    impl CoordinatorBackend for FakeBackend {
        fn open_managed_layout(&self) -> Result<(), BackendFailure> {
            let mut state = self.state.lock().expect("state");
            state.calls.push("open");
            if let Some(location) = state.locations.pop_front() {
                state.location = location;
            }
            let available = state
                .location
                .as_ref()
                .is_none_or(|location| location.available);
            let game_folder_available = state
                .game_folder
                .as_ref()
                .is_none_or(|folder| folder.available);
            self.notify(state);
            if !available {
                Err(BackendFailure::new("install-location-unavailable"))
            } else if !game_folder_available {
                Err(BackendFailure::new("game-folder-unavailable"))
            } else {
                Ok(())
            }
        }

        fn install_location(&self) -> Option<LocationView> {
            self.state.lock().expect("state").location.clone()
        }

        fn game_folder(&self) -> Option<GameFolderView> {
            self.state.lock().expect("state").game_folder.clone()
        }

        fn audit(&self, _activity: ActivityEvidence) -> Result<BackendAudit, BackendFailure> {
            let mut state = self.state.lock().expect("state");
            state.calls.push("audit");
            if let Some(failure) = state.audit_failure.take() {
                self.notify(state);
                return Err(failure);
            }
            if let Some(audit) = state.audits.pop_front() {
                state.latest_audit = audit;
            }
            let audit = state.latest_audit.clone();
            self.notify(state);
            Ok(audit)
        }

        fn repair(&self, cancellation: &CancellationToken) -> Result<(), BackendFailure> {
            let mut state = self.state.lock().expect("state");
            state.calls.push("repair");
            state.cancellation_observed = cancellation.is_requested();
            self.notify(state);
            Ok(())
        }

        fn check_for_update(&self) -> Result<UpdateStatus, BackendFailure> {
            {
                let mut state = self.state.lock().expect("state");
                state.calls.push("check-update");
                self.notify(state);
            }
            self.check_gate.wait(None);
            let state = self.state.lock().expect("state");
            match state.update_check_failure {
                Some(failure) => Err(failure),
                None => Ok(state.update_status),
            }
        }

        fn select_installer_media(
            &self,
            path: std::path::PathBuf,
        ) -> Result<InstallerMediaView, BackendFailure> {
            let mut state = self.state.lock().expect("state");
            state.calls.push("select-installer-media");
            self.notify(state);
            if path.ends_with("invalid") {
                return Err(BackendFailure::new(
                    "installer-folder-must-contain-both-official-files",
                ));
            }
            Ok(InstallerMediaView::Validated(path))
        }

        fn select_existing_install(
            &self,
            path: std::path::PathBuf,
        ) -> Result<InstallerMediaView, BackendFailure> {
            let mut state = self.state.lock().expect("state");
            state.calls.push("select-existing-install");
            self.notify(state);
            Ok(InstallerMediaView::Existing(
                crate::presentation::ExistingInstallView {
                    problem: path
                        .ends_with("classic")
                        .then_some(crate::presentation::ExistingProblemView::NoExpansion),
                    path,
                    copy_bytes: 1_900_000_000,
                    in_place_possible: true,
                    project_diablo: false,
                },
            ))
        }

        fn download_all(&self, cancellation: &CancellationToken) -> Result<(), BackendFailure> {
            self.run_setup_job("download", None, None, cancellation)
        }

        fn install_base_game(
            &self,
            authorization: Option<InstallationAuthorization>,
            import: Option<ImportModeView>,
            cancellation: &CancellationToken,
        ) -> Result<(), BackendFailure> {
            self.run_setup_job("install-base-game", authorization, import, cancellation)
        }

        fn install_project_diablo(
            &self,
            cancellation: &CancellationToken,
        ) -> Result<(), BackendFailure> {
            self.run_setup_job("install-project-diablo", None, None, cancellation)
        }

        fn discard_downloads(
            &self,
            _cancellation: &CancellationToken,
        ) -> Result<(), BackendFailure> {
            let mut state = self.state.lock().expect("state");
            state.calls.push("discard-downloads");
            self.notify(state);
            Ok(())
        }

        fn request_mutation_safe_stop(&self) -> Result<(), BackendFailure> {
            let mut state = self.state.lock().expect("state");
            state.calls.push("safe-stop");
            self.notify(state);
            Ok(())
        }

        fn relocate(
            &self,
            parent: PathBuf,
            fresh_only: bool,
            _cancellation: &CancellationToken,
        ) -> Result<(), BackendFailure> {
            let mut state = self.state.lock().expect("state");
            state.calls.push(if fresh_only {
                "relocate-fresh"
            } else {
                "relocate"
            });
            let result = if parent.ends_with("fat32") {
                Err(BackendFailure::new("location-not-writable"))
            } else {
                state.locations = VecDeque::from([Some(LocationView {
                    path: parent.join("pd2-launcher"),
                    default: false,
                    available: true,
                    previous: None,
                })]);
                Ok(())
            };
            self.notify(state);
            result
        }

        fn remove_previous_installation(
            &self,
            _cancellation: &CancellationToken,
        ) -> Result<(), BackendFailure> {
            let mut state = self.state.lock().expect("state");
            state.calls.push("remove-previous");
            if let Some(location) = state.location.as_mut() {
                location.previous = None;
            }
            self.notify(state);
            Ok(())
        }

        fn reset_installation(
            &self,
            _cancellation: &CancellationToken,
        ) -> Result<(), BackendFailure> {
            let mut state = self.state.lock().expect("state");
            state.calls.push("reset");
            state.game_folder = None;
            self.notify(state);
            Ok(())
        }

        fn apply_update(&self, cancellation: &CancellationToken) -> Result<(), BackendFailure> {
            {
                let mut state = self.state.lock().expect("state");
                state.calls.push("apply-update");
                state.update_calls += 1;
                self.notify(state);
            }
            if self.update_gate.wait(Some(cancellation)) {
                Ok(())
            } else {
                Err(BackendFailure::new("operation-cancelled"))
            }
        }

        fn open_official_launcher(
            &self,
            confirmed: bool,
            strict_memory_order: bool,
            detailed_logging: bool,
            cancellation: &CancellationToken,
        ) -> Result<(), BackendFailure> {
            {
                let mut state = self.state.lock().expect("state");
                state.official_launcher_confirmed = confirmed;
                state.official_launcher_strict_memory_order = strict_memory_order;
                state.official_launcher_detailed_logging = detailed_logging;
                state.calls.push("official-launcher");
                state.official_launcher_calls += 1;
                self.notify(state);
            }
            // Open until the user closes it (the gate) or a safe stop.
            if self.official_launcher_gate.wait(Some(cancellation)) {
                self.state
                    .lock()
                    .expect("state")
                    .official_launcher_failure
                    .map_or(Ok(()), Err)
            } else {
                Err(BackendFailure::new("operation-cancelled"))
            }
        }

        fn launch_game(
            &self,
            driver: GraphicsDriver,
            profile: Option<GraphicsProfile>,
            strict_memory_order: bool,
            detailed_logging: bool,
        ) -> Result<Option<GameNoticeView>, BackendFailure> {
            let mut state = self.state.lock().expect("state");
            state.calls.push("launch-game");
            state.launch_calls += 1;
            if let Some(failure) = state.launch_failure {
                self.notify(state);
                return Err(failure);
            }
            state.launch_driver = driver;
            state.launch_profile = profile;
            state.launch_strict_memory_order = strict_memory_order;
            state.launch_detailed_logging = detailed_logging;
            state.game = BackendGameState::Running;
            let notice = state.launch_notice;
            self.notify(state);
            Ok(notice)
        }

        fn poll_game(&self) -> BackendGameState {
            self.state.lock().expect("state").game
        }

        fn request_game_stop(&self) -> Result<(), BackendFailure> {
            let mut state = self.state.lock().expect("state");
            state.calls.push("stop-game");
            state.game = BackendGameState::Idle;
            self.notify(state);
            Ok(())
        }

        fn load_settings(&self) -> Result<LaunchSettingsView, BackendFailure> {
            let mut state = self.state.lock().expect("state");
            state.calls.push("load-settings");
            let result = state
                .settings_load_failure
                .map_or_else(|| Ok(state.settings.clone()), Err);
            self.notify(state);
            result
        }

        fn update_settings(
            &self,
            change: SettingsChange,
        ) -> Result<LaunchSettingsView, BackendFailure> {
            let mut state = self.state.lock().expect("state");
            state.calls.push("save-settings");
            if let Some(failure) = state.settings_save_failure {
                self.notify(state);
                return Err(failure);
            }
            let current = state.settings.clone();
            state.settings = match change {
                SettingsChange::Renderer(renderer) => LaunchSettingsView::new(
                    renderer,
                    current.skip_to_bnet(),
                    current.sound_in_background(),
                )
                .with_official_launcher_software_rendering(
                    current.official_launcher_software_rendering(),
                ),
                SettingsChange::SkipToBnet(enabled) => LaunchSettingsView::new(
                    current.renderer(),
                    enabled,
                    current.sound_in_background(),
                )
                .with_official_launcher_software_rendering(
                    current.official_launcher_software_rendering(),
                ),
                SettingsChange::SoundInBackground(enabled) => {
                    LaunchSettingsView::new(current.renderer(), current.skip_to_bnet(), enabled)
                        .with_official_launcher_software_rendering(
                            current.official_launcher_software_rendering(),
                        )
                }
                SettingsChange::OfficialLauncherSoftwareRendering(enabled) => {
                    current.with_official_launcher_software_rendering(enabled)
                }
                SettingsChange::DirectDrawRenderer(renderer) => {
                    current.with_direct_draw_renderer(renderer)
                }
                SettingsChange::AppMenuEntry(enabled) => current.with_app_menu_entry(enabled),
                SettingsChange::UpdateChecks(enabled) => current.with_update_checks(enabled),
                SettingsChange::InstallSource(source) => current.with_install_source(source),
                SettingsChange::CustomArguments(arguments) => {
                    current.with_custom_arguments(arguments)
                }
                SettingsChange::GameProgram(path) => current.with_game_program(path),
                SettingsChange::LauncherProgram(path) => current.with_launcher_program(path),
                SettingsChange::LauncherCustomArguments(arguments) => {
                    current.with_launcher_custom_arguments(arguments)
                }
            };
            state.settings_updates += 1;
            let settings = state.settings.clone();
            self.notify(state);
            Ok(settings)
        }

        fn diagnostics_evidence(&self) -> Result<Vec<EvidenceView>, BackendFailure> {
            Ok(Vec::new())
        }

        fn save_report(&self) -> Result<PathBuf, BackendFailure> {
            Ok(PathBuf::from(
                "/reports/pd2-launcher-report-19700101T000000Z.tar.xz",
            ))
        }

        fn open_game_folder(&self, folder: &Path) -> bool {
            let mut state = self.state.lock().expect("state");
            state.calls.push("open-game-folder");
            state.opened_folders.push(folder.to_owned());
            let opened = !folder.starts_with("/unopenable");
            self.notify(state);
            opened
        }

        fn take_saves_put_aside(&self) -> Option<SavesPutAsideView> {
            self.state.lock().expect("state").saves_put_aside.take()
        }

        fn open_backups(&self, folder: &Path) -> bool {
            let mut state = self.state.lock().expect("state");
            state.calls.push("open-backups");
            state.opened_folders.push(folder.to_owned());
            let opened = !folder.starts_with("/unopenable");
            self.notify(state);
            opened
        }

        fn setup_record(&self) -> SetupRecord {
            self.state.lock().expect("state").setup_record
        }

        fn save_setup_record(&self, record: SetupRecord) -> Result<(), BackendFailure> {
            let mut state = self.state.lock().expect("state");
            state.setup_record = record;
            state.saved_records.push(record);
            self.notify(state);
            Ok(())
        }
    }

    impl FakeBackend {
        fn run_setup_job(
            &self,
            call: &'static str,
            authorization: Option<InstallationAuthorization>,
            import: Option<ImportModeView>,
            cancellation: &CancellationToken,
        ) -> Result<(), BackendFailure> {
            {
                let mut state = self.state.lock().expect("state");
                state.calls.push(call);
                state.install_calls += 1;
                state.authorized_installs += usize::from(authorization.is_some());
                if let Some(authorization) = &authorization {
                    state.authorization = Some(authorization.liveness());
                }
                state.imports.extend(import);
                self.notify(state);
            }
            cancellation.report(
                crate::operation::WorkProgress::for_phase(WorkPhase::ClientDownload)
                    .with_file_bytes(4096, None)
                    .with_overall(crate::operation::ProgressUnit::Files, 3, Some(8))
                    .with_verification(2, Some(8)),
            );
            let too_late = self.state.lock().expect("state").stop_too_late;
            let completed = self.install_gate.wait((!too_late).then_some(cancellation));
            drop(authorization);
            let mut state = self.state.lock().expect("state");
            state.cancellation_observed = !completed;
            let failure = state.install_failure;
            self.notify(state);
            if completed {
                failure.map_or(Ok(()), Err)
            } else {
                Err(BackendFailure::new("operation-cancelled"))
            }
        }
    }

    struct Harness {
        intents: SyncSender<Intent>,
        events: Receiver<SequencedEvent>,
        runtime: ServiceRuntime,
        reducer: UiReducer,
    }

    impl Harness {
        fn start(backend: Arc<FakeBackend>) -> Self {
            let (intents, intent_receiver) = sync_channel(32);
            let (event_sender, events) = sync_channel(256);
            let runtime = ServiceRuntime::start(
                ServiceEndpoint {
                    intent_receiver,
                    event_sender,
                },
                backend,
            )
            .expect("service runtime");
            Self {
                intents,
                events,
                runtime,
                reducer: UiReducer::new(UiSnapshot::initial()),
            }
        }

        // The caller's line in a panic, and what the coordinator showed last: a wait that
        // fails on CI only must say which wait it was and where the coordinator stood.
        #[track_caller]
        fn wait_for(&mut self, predicate: impl Fn(&UiSnapshot) -> bool) -> UiSnapshot {
            let deadline = Instant::now() + WAIT;
            while !predicate(self.reducer.snapshot()) {
                let now = Instant::now();
                let left = deadline.saturating_duration_since(now);
                let event = match self.events.recv_timeout(left) {
                    Ok(event) if now < deadline => event,
                    result => {
                        let snapshot = self.reducer.snapshot();
                        panic!(
                            "no snapshot met the condition in {WAIT:?} ({}); the last one: \
                             installation {:?}, operation {:?}, setup record {:?}, update {:?}",
                            if result.is_ok() {
                                "events came"
                            } else {
                                "no event"
                            },
                            snapshot.installation(),
                            snapshot.operation(),
                            snapshot.setup_record(),
                            snapshot.update(),
                        )
                    }
                };
                assert_ne!(
                    self.reducer.reduce(event),
                    ReduceOutcome::SnapshotRequired,
                    "the normal test channel should not drop events"
                );
            }
            self.reducer.snapshot().clone()
        }

        /// The reason of the next refusal event, reducing every event up to it.
        fn next_refusal(&mut self) -> String {
            let deadline = Instant::now() + WAIT;
            loop {
                let now = Instant::now();
                assert!(now < deadline, "refusal timed out");
                let event = self
                    .events
                    .recv_timeout(deadline - now)
                    .expect("service event");
                let refusal = event.refusal().map(str::to_owned);
                self.reducer.reduce(event);
                if let Some(refusal) = refusal {
                    return refusal;
                }
            }
        }

        fn select_installers(&mut self) {
            self.intents
                .send(Intent::SelectInstallerMedia(PathBuf::from(
                    "/tmp/pd2-launcher-test-installers",
                )))
                .expect("installer media intent");
            self.wait_for(|snapshot| snapshot.installer_media().is_ready());
        }
    }

    /// A fresh audit whose downloads are complete for Blizzard's installers.
    fn audit(installation: InstallationStatus) -> BackendAudit {
        audit_without_downloads(installation).with_downloads(DownloadsView {
            complete: true,
            installers: true,
            total_bytes: 2_600_000_000,
        })
    }

    fn audit_without_downloads(installation: InstallationStatus) -> BackendAudit {
        BackendAudit::new(
            HostView::Supported,
            installation,
            if installation == InstallationStatus::Ready {
                LaunchPreflight::Passed
            } else {
                LaunchPreflight::Failed
            },
            SystemContractView {
                host: "Supported test host".to_owned(),
                runtime: "Verified".to_owned(),
                prefix: "Verified".to_owned(),
                artifact_health: "Test evidence".to_owned(),
                missing: Vec::new(),
                distro: crate::presentation::DistroFamily::Unknown,
            },
            vec![EvidenceView::new(
                "Audit",
                "Fresh test evidence",
                EvidenceTone::Verified,
            )],
        )
    }

    #[test]
    fn clean_authorized_journey_reaches_ready_only_from_the_post_install_audit() {
        let backend = Arc::new(FakeBackend::new(
            [
                audit(InstallationStatus::NeedsBaseGame),
                audit(InstallationStatus::Ready),
            ],
            UpdateStatus::UpToDate,
        ));
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| {
            snapshot.host() == HostView::Supported
                && snapshot.installation() == InstallationStatus::NeedsBaseGame
        });
        harness.select_installers();

        harness
            .intents
            .send(Intent::AuthorizeAndInstall(
                AuthorizationInput::new(
                    "AAAA-BBBB-CCCC-DDDD".to_owned(),
                    "EEEE-FFFF-GGGG-HHHH".to_owned(),
                    true,
                )
                .expect("authorization"),
            ))
            .expect("install intent");

        let snapshot = harness.wait_for(|snapshot| {
            snapshot.installation() == InstallationStatus::Ready
                && snapshot.authorization() == AuthorizationView::Absent
                && snapshot.launch_preflight_passed()
        });
        assert!(snapshot.launch_preflight_passed());
        assert_eq!(snapshot.system().host, "Supported test host");
        assert_eq!(snapshot.system().artifact_health, "Test evidence");
        backend.wait_for(|state| state.install_calls == 1);
        let state = backend.state.lock().expect("state");
        assert_eq!(state.authorized_installs, 1);
        assert_eq!(
            state.calls.iter().filter(|call| **call == "audit").count(),
            2
        );
    }

    #[test]
    fn an_interrupted_stage_refuses_authorize_with_a_refusal_event() {
        let interrupted = InstallationStatus::StageInterrupted(crate::InterruptedStage::BaseGame);
        let backend = Arc::new(FakeBackend::new(
            [audit(interrupted)],
            UpdateStatus::UpToDate,
        ));
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.installation() == interrupted);
        harness.select_installers();

        harness
            .intents
            .send(Intent::AuthorizeAndInstall(
                AuthorizationInput::new(
                    "AAAA-BBBB-CCCC-DDDD".to_owned(),
                    "EEEE-FFFF-GGGG-HHHH".to_owned(),
                    true,
                )
                .unwrap(),
            ))
            .unwrap();
        assert_eq!(harness.next_refusal(), "install-not-permitted");
        assert_eq!(backend.state.lock().unwrap().install_calls, 0);
    }

    #[test]
    fn fresh_install_cannot_start_before_installer_media_is_selected() {
        let backend = Arc::new(FakeBackend::new(
            [audit(InstallationStatus::NeedsBaseGame)],
            UpdateStatus::UpToDate,
        ));
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.installation() == InstallationStatus::NeedsBaseGame);

        harness
            .intents
            .send(Intent::AuthorizeAndInstall(
                AuthorizationInput::new(
                    "AAAA-BBBB-CCCC-DDDD".to_owned(),
                    "EEEE-FFFF-GGGG-HHHH".to_owned(),
                    true,
                )
                .unwrap(),
            ))
            .unwrap();
        let blocked = harness.wait_for(|snapshot| {
            snapshot.evidence().iter().any(|item| {
                item.label == "Request not started" && item.detail == "install-not-permitted"
            })
        });
        assert_eq!(blocked.installer_media(), &InstallerMediaView::NotSelected);
        assert_eq!(backend.state.lock().unwrap().install_calls, 0);

        harness.select_installers();
        assert_eq!(
            harness.reducer.snapshot().installer_media(),
            &InstallerMediaView::Validated(PathBuf::from("/tmp/pd2-launcher-test-installers"))
        );
    }

    #[test]
    fn blizzard_download_satisfies_the_installer_media_step_and_starts_install() {
        let backend = Arc::new(FakeBackend::new(
            [audit(InstallationStatus::NeedsBaseGame)],
            UpdateStatus::UpToDate,
        ));
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.installation() == InstallationStatus::NeedsBaseGame);

        harness.intents.send(Intent::UseBlizzardDownload).unwrap();
        let chosen = harness.wait_for(|snapshot| snapshot.installer_media().is_ready());
        assert_eq!(
            chosen.installer_media(),
            &InstallerMediaView::BlizzardDownload
        );

        harness
            .intents
            .send(Intent::AuthorizeAndInstall(
                AuthorizationInput::new(
                    "AAAA-BBBB-CCCC-DDDD".to_owned(),
                    "EEEE-FFFF-GGGG-HHHH".to_owned(),
                    true,
                )
                .unwrap(),
            ))
            .unwrap();
        backend.wait_for(|state| state.install_calls == 1);
    }

    #[test]
    fn installer_media_cannot_change_under_a_running_install() {
        let backend = Arc::new(
            FakeBackend::new(
                [
                    audit(InstallationStatus::NeedsBaseGame),
                    audit(InstallationStatus::NeedsBaseGame),
                ],
                UpdateStatus::UpToDate,
            )
            .with_install_blocked(),
        );
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.installation() == InstallationStatus::NeedsBaseGame);
        harness.select_installers();
        harness
            .intents
            .send(Intent::AuthorizeAndInstall(
                AuthorizationInput::new(
                    "AAAA-BBBB-CCCC-DDDD".to_owned(),
                    "EEEE-FFFF-GGGG-HHHH".to_owned(),
                    true,
                )
                .unwrap(),
            ))
            .unwrap();
        backend.wait_for(|state| state.install_calls == 1);
        let selections = |backend: &FakeBackend| {
            backend
                .calls()
                .iter()
                .filter(|call| **call == "select-installer-media")
                .count()
        };
        assert_eq!(selections(&backend), 1);

        for intent in [
            Intent::SelectInstallerMedia(PathBuf::from("/tmp/other-installers")),
            Intent::UseBlizzardDownload,
        ] {
            let before = harness.reducer.snapshot().last_event_sequence();
            harness.intents.send(intent).unwrap();
            let refused = harness.wait_for(|snapshot| {
                snapshot.last_event_sequence() > before
                    && snapshot
                        .evidence()
                        .iter()
                        .any(|item| item.detail == "installer-media-locked-while-installing")
            });
            // The job keeps the choice it started with, and so does the view.
            assert_eq!(
                refused.installer_media(),
                &InstallerMediaView::Validated(PathBuf::from("/tmp/pd2-launcher-test-installers"))
            );
        }
        thread::sleep(Duration::from_millis(80));
        assert_eq!(
            selections(&backend),
            1,
            "the backend never saw the new choice"
        );
        backend.install_gate.release();
    }

    #[test]
    fn a_rejected_installer_folder_keeps_the_prior_valid_choice() {
        let backend = Arc::new(FakeBackend::new(
            [audit(InstallationStatus::NeedsBaseGame)],
            UpdateStatus::UpToDate,
        ));
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.installation() == InstallationStatus::NeedsBaseGame);

        // Without a prior choice a rejection is shown as the choice itself.
        harness
            .intents
            .send(Intent::SelectInstallerMedia(PathBuf::from("/tmp/invalid")))
            .unwrap();
        let invalid = harness.wait_for(|snapshot| {
            matches!(snapshot.installer_media(), InstallerMediaView::Invalid(_))
        });
        assert_eq!(invalid.installer_media_rejection(), None);

        harness.select_installers();
        harness
            .intents
            .send(Intent::SelectInstallerMedia(PathBuf::from("/tmp/invalid")))
            .unwrap();
        let kept = harness.wait_for(|snapshot| snapshot.installer_media_rejection().is_some());
        assert_eq!(
            kept.installer_media(),
            &InstallerMediaView::Validated(PathBuf::from("/tmp/pd2-launcher-test-installers"))
        );
        assert_eq!(
            kept.installer_media_rejection(),
            Some("installer-folder-must-contain-both-official-files")
        );

        harness.intents.send(Intent::UseBlizzardDownload).unwrap();
        harness.wait_for(|snapshot| {
            snapshot.installer_media() == &InstallerMediaView::BlizzardDownload
                && snapshot.installer_media_rejection().is_none()
        });
    }

    #[test]
    fn a_step_editor_opens_only_where_the_step_allows_a_change() {
        let backend = Arc::new(
            FakeBackend::new(
                [
                    audit(InstallationStatus::NeedsBaseGame),
                    audit(InstallationStatus::NeedsBaseGame),
                ],
                UpdateStatus::UpToDate,
            )
            .with_install_blocked(),
        );
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.installation() == InstallationStatus::NeedsBaseGame);

        harness
            .intents
            .send(Intent::EditStep(SetupStep::Setup))
            .unwrap();
        harness.wait_for(|snapshot| snapshot.editing() == Some(SetupStep::Setup));
        harness.intents.send(Intent::CloseEditor).unwrap();
        harness.wait_for(|snapshot| snapshot.editing().is_none());
        harness
            .intents
            .send(Intent::EditStep(SetupStep::Play))
            .unwrap();
        harness.wait_for(|snapshot| {
            snapshot
                .evidence()
                .iter()
                .any(|item| item.detail == "step-change-not-permitted")
        });

        // Starting work closes an open editor.
        harness
            .intents
            .send(Intent::EditStep(SetupStep::Setup))
            .unwrap();
        harness.wait_for(|snapshot| snapshot.editing() == Some(SetupStep::Setup));
        harness.select_installers();
        harness
            .intents
            .send(Intent::AuthorizeAndInstall(
                AuthorizationInput::new(
                    "AAAA-BBBB-CCCC-DDDD".to_owned(),
                    "EEEE-FFFF-GGGG-HHHH".to_owned(),
                    true,
                )
                .unwrap(),
            ))
            .unwrap();
        harness
            .wait_for(|snapshot| snapshot.active_job().is_some() && snapshot.editing().is_none());
        backend.install_gate.release();
    }

    fn start_blocked_install(final_audit: InstallationStatus) -> (Arc<FakeBackend>, Harness) {
        let backend = Arc::new(
            FakeBackend::new(
                [audit(InstallationStatus::NeedsBaseGame), audit(final_audit)],
                UpdateStatus::UpToDate,
            )
            .with_install_blocked(),
        );
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.installation() == InstallationStatus::NeedsBaseGame);
        harness.select_installers();
        harness
            .intents
            .send(Intent::AuthorizeAndInstall(
                AuthorizationInput::new(
                    "AAAA-BBBB-CCCC-DDDD".to_owned(),
                    "EEEE-FFFF-GGGG-HHHH".to_owned(),
                    true,
                )
                .unwrap(),
            ))
            .unwrap();
        backend.wait_for(|state| state.install_calls == 1);
        harness.wait_for(|snapshot| snapshot.active_job() == Some(ActiveJobView::InstallBaseGame));
        (backend, harness)
    }

    #[test]
    fn stop_and_change_opens_the_editor_after_the_proven_stop_and_audit() {
        let (backend, mut harness) = start_blocked_install(InstallationStatus::NeedsBaseGame);
        harness
            .intents
            .send(Intent::StopAndChange(SetupStep::Setup))
            .unwrap();
        backend.wait_for(|state| state.calls.contains(&"safe-stop"));
        let changed = harness.wait_for(|snapshot| {
            snapshot.editing() == Some(SetupStep::Setup) && snapshot.stop_result().is_some()
        });
        assert_eq!(changed.active_job(), None);
        assert_eq!(changed.authorization(), AuthorizationView::Absent);
        let result = changed.stop_result().unwrap();
        assert!(!result.needs_recovery());
        // The installer choice is still the one the job started with.
        assert!(changed.installer_media().is_ready());

        // Dismissing the result leaves the editor open.
        harness.intents.send(Intent::DismissStopResult).unwrap();
        harness.wait_for(|snapshot| {
            snapshot.stop_result().is_none() && snapshot.editing() == Some(SetupStep::Setup)
        });
    }

    #[test]
    fn stop_and_change_shows_recovery_when_the_audit_cannot_decide() {
        let (backend, mut harness) = start_blocked_install(InstallationStatus::Indeterminate);
        harness
            .intents
            .send(Intent::StopAndChange(SetupStep::Setup))
            .unwrap();
        backend.wait_for(|state| state.calls.contains(&"safe-stop"));
        let stopped = harness.wait_for(|snapshot| snapshot.stop_result().is_some());
        assert!(stopped.stop_result().unwrap().needs_recovery());
        assert_eq!(stopped.editing(), None);
        assert_eq!(stopped.installation(), InstallationStatus::Indeterminate);
    }

    #[test]
    fn stop_and_change_is_refused_without_a_running_install() {
        let backend = Arc::new(FakeBackend::new(
            [audit(InstallationStatus::NeedsBaseGame)],
            UpdateStatus::UpToDate,
        ));
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.installation() == InstallationStatus::NeedsBaseGame);
        harness
            .intents
            .send(Intent::StopAndChange(SetupStep::Setup))
            .unwrap();
        harness.wait_for(|snapshot| {
            snapshot
                .evidence()
                .iter()
                .any(|item| item.detail == "stop-and-change-not-permitted")
        });
        assert!(!backend.calls().contains(&"safe-stop"));
    }

    #[test]
    fn a_plain_safe_stop_reports_what_was_kept_without_opening_an_editor() {
        let (backend, mut harness) = start_blocked_install(InstallationStatus::NeedsBaseGame);
        harness.intents.send(Intent::RequestSafeStop).unwrap();
        backend.wait_for(|state| state.calls.contains(&"safe-stop"));
        let stopped = harness.wait_for(|snapshot| snapshot.stop_result().is_some());
        assert_eq!(stopped.editing(), None);
    }

    /// The authorization the running Install Diablo II job holds; it must be
    /// alive while the job runs.
    fn held_authorization(backend: &FakeBackend) -> std::sync::Weak<()> {
        let held = backend
            .state
            .lock()
            .unwrap()
            .authorization
            .clone()
            .expect("the job got an authorization");
        assert!(held.upgrade().is_some(), "alive while the job runs");
        held
    }

    #[test]
    fn installation_authorization_ends_when_the_install_diablo_ii_stage_succeeds_or_fails() {
        for failure in [None, Some(BackendFailure::new("base-game-install-failed"))] {
            let backend = Arc::new(
                FakeBackend::new(
                    [
                        audit(InstallationStatus::NeedsBaseGame),
                        audit(InstallationStatus::NeedsExpansion),
                    ],
                    UpdateStatus::UpToDate,
                )
                .with_install_blocked(),
            );
            backend.state.lock().unwrap().install_failure = failure;
            let mut harness = Harness::start(backend.clone());
            harness
                .wait_for(|snapshot| snapshot.installation() == InstallationStatus::NeedsBaseGame);
            harness.select_installers();
            authorize(&harness);
            harness.wait_for(|snapshot| snapshot.authorization() == AuthorizationView::Authorized);
            backend.wait_for(|state| state.install_calls == 1);
            let held = held_authorization(&backend);

            backend.install_gate.release();
            harness.wait_for(|snapshot| {
                snapshot.active_job().is_none()
                    && snapshot.authorization() == AuthorizationView::Absent
            });
            assert!(held.upgrade().is_none(), "dropped when the stage ended");
        }
    }

    #[test]
    fn installation_authorization_ends_when_the_install_diablo_ii_stage_stops() {
        let (backend, mut harness) = start_blocked_install(InstallationStatus::NeedsBaseGame);
        let held = held_authorization(&backend);
        harness.intents.send(Intent::RequestSafeStop).unwrap();
        harness.wait_for(|snapshot| {
            snapshot.stop_result().is_some()
                && snapshot.authorization() == AuthorizationView::Absent
        });
        assert!(held.upgrade().is_none(), "dropped when the stage stopped");
    }

    #[test]
    fn installation_authorization_ends_when_the_engine_exits() {
        // The engine's exit drops the service runtime with the job running.
        let (backend, harness) = start_blocked_install(InstallationStatus::NeedsBaseGame);
        let held = held_authorization(&backend);
        drop(harness);
        assert!(held.upgrade().is_none(), "dropped before the engine exits");
    }

    #[test]
    fn start_over_runs_only_as_an_idle_confirmed_job_and_is_followed_by_an_audit() {
        let interrupted = InstallationStatus::StageInterrupted(crate::InterruptedStage::BaseGame);
        let backend = Arc::new(FakeBackend::new(
            [audit(interrupted), audit(InstallationStatus::NeedsBaseGame)],
            UpdateStatus::UpToDate,
        ));
        let mut harness = Harness::start(backend.clone());
        let snapshot = harness.wait_for(|snapshot| snapshot.installation() == interrupted);
        let deck = DeckPresentation::project(&snapshot, false);
        assert_eq!(
            deck.primary,
            ActionView::enabled(ActionId::StartOver, "Start over")
        );
        assert_eq!(
            step_affordance(&snapshot, SetupStep::InstallD2),
            StepAffordance::ResetAvailable
        );
        // An interrupted installation never continues by itself.
        harness.intents.send(Intent::ContinueInstallation).unwrap();
        harness.wait_for(|snapshot| {
            snapshot
                .evidence()
                .iter()
                .any(|item| item.detail == "install-not-permitted")
        });

        harness.intents.send(Intent::StartOver).unwrap();
        let reset = harness.wait_for(|snapshot| {
            snapshot.installation() == InstallationStatus::NeedsBaseGame
                && snapshot.operation().is_some_and(|operation| {
                    operation.phase() == OperationPhase::Reset
                        && operation.status() == OperationStatusView::Succeeded
                })
        });
        assert_eq!(reset.active_job(), None);
        let calls = backend.calls();
        let reset_call = calls.iter().position(|call| *call == "reset").unwrap();
        assert!(calls[reset_call..].contains(&"audit"), "{calls:?}");
        assert_eq!(backend.state.lock().unwrap().install_calls, 0);
    }

    #[test]
    fn start_over_forgets_a_game_folder_that_is_unavailable() {
        let backend = Arc::new(FakeBackend::new(
            [audit(InstallationStatus::NeedsBaseGame)],
            UpdateStatus::UpToDate,
        ));
        backend.state.lock().unwrap().game_folder = Some(GameFolderView {
            path: PathBuf::from("/games/Diablo II"),
            available: false,
        });
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| {
            snapshot.game_folder_unavailable()
                && snapshot
                    .operation()
                    .is_some_and(|operation| operation.status() == OperationStatusView::Failed)
        });
        assert!(!backend.calls().contains(&"audit"));

        harness.intents.send(Intent::StartOver).unwrap();
        let reset = harness.wait_for(|snapshot| {
            snapshot.operation().is_some_and(|operation| {
                operation.phase() == OperationPhase::Reset
                    && operation.status() == OperationStatusView::Succeeded
            })
        });
        assert!(!reset.game_folder_unavailable());
        assert_eq!(reset.game_folder(), None);
        assert_eq!(reset.installation(), InstallationStatus::NeedsBaseGame);
        assert_eq!(reset.host(), HostView::Supported);
        let calls = backend.calls();
        let reset_call = calls.iter().position(|call| *call == "reset").unwrap();
        assert_eq!(&calls[reset_call..reset_call + 2], ["reset", "open"]);
    }

    /// An Indeterminate installation lets Start over through to the backend:
    /// on the Android host a stop inside a Start over's removal of the update
    /// records can leave the transaction check Indeterminate, and the
    /// backend's own preflight then goes on while the started base-game
    /// marker stands (`composition::production` `start_over_on`).
    #[test]
    fn an_indeterminate_installation_lets_start_over_reach_the_backend() {
        let backend = Arc::new(FakeBackend::new(
            [audit(InstallationStatus::Indeterminate)],
            UpdateStatus::UpToDate,
        ));
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.installation() == InstallationStatus::Indeterminate);
        harness.intents.send(Intent::StartOver).unwrap();
        harness.wait_for(|snapshot| {
            snapshot.operation().is_some_and(|operation| {
                operation.phase() == OperationPhase::Reset
                    && operation.status() == OperationStatusView::Succeeded
            })
        });
        assert!(backend.calls().contains(&"reset"));
    }

    #[test]
    fn start_over_is_refused_while_anything_runs_or_a_journal_is_open() {
        let backend = Arc::new(FakeBackend::new(
            [audit(InstallationStatus::Ready)],
            UpdateStatus::UpToDate,
        ));
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.update() == UpdateStatus::UpToDate);
        harness
            .intents
            .send(Intent::LaunchGame(
                GraphicsDriver::Standard,
                None,
                false,
                false,
            ))
            .unwrap();
        harness.wait_for(|snapshot| snapshot.game() == GameView::Running);
        harness.intents.send(Intent::StartOver).unwrap();
        harness.wait_for(|snapshot| {
            snapshot
                .evidence()
                .iter()
                .any(|item| item.detail == "start-over-not-permitted")
        });
        assert!(!backend.calls().contains(&"reset"));

        let backend = Arc::new(FakeBackend::new(
            [
                audit(InstallationStatus::RepairRequired),
                audit(InstallationStatus::RepairRequired),
            ],
            UpdateStatus::UpToDate,
        ));
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| {
            snapshot.installation() == InstallationStatus::RepairRequired
                && snapshot
                    .operation()
                    .is_some_and(|operation| operation.status() == OperationStatusView::Succeeded)
        });
        harness.intents.send(Intent::StartOver).unwrap();
        harness.wait_for(|snapshot| {
            snapshot
                .evidence()
                .iter()
                .any(|item| item.detail == "start-over-not-permitted")
        });
        assert!(!backend.calls().contains(&"reset"));
    }

    #[test]
    fn an_unavailable_location_blocks_everything_until_it_is_back() {
        let card = |available| LocationView {
            path: PathBuf::from("/run/media/deck/card/pd2-launcher"),
            default: false,
            available,
            previous: None,
        };
        let backend = Arc::new(FakeBackend::new(
            [audit(InstallationStatus::Ready)],
            UpdateStatus::UpToDate,
        ));
        backend.state.lock().unwrap().locations =
            VecDeque::from([Some(card(false)), Some(card(true))]);
        let mut harness = Harness::start(backend.clone());
        let unavailable = harness.wait_for(|snapshot| snapshot.location_unavailable());
        let deck = DeckPresentation::project(&unavailable, false);
        assert_eq!(deck.title, "Install location unavailable");
        assert_eq!(
            deck.primary,
            ActionView::enabled(ActionId::RetryLocation, "Check again")
        );
        assert!(
            !backend.calls().contains(&"audit"),
            "nothing is audited there"
        );
        for intent in [
            Intent::LaunchGame(GraphicsDriver::Standard, None, false, false),
            Intent::ContinueInstallation,
            Intent::StartOver,
        ] {
            harness.intents.send(intent).unwrap();
        }
        thread::sleep(Duration::from_millis(80));
        assert!(!backend.calls().contains(&"reset"));
        assert_eq!(backend.state.lock().unwrap().launch_calls, 0);

        // Reconnected: checking again resolves it and audits as at startup.
        harness.intents.send(Intent::RetryLocation).unwrap();
        let back = harness.wait_for(|snapshot| {
            snapshot
                .location()
                .is_some_and(|location| location.available)
                && snapshot.installation() == InstallationStatus::Ready
                && snapshot.host() == HostView::Supported
                && snapshot.launch_preflight_passed()
        });
        assert!(DeckPresentation::project(&back, false).primary.enabled);
    }

    #[test]
    fn a_new_location_is_a_job_that_re_resolves_the_root_before_its_audit() {
        let card = LocationView {
            path: PathBuf::from("/run/media/deck/card/pd2-launcher"),
            default: false,
            available: false,
            previous: None,
        };
        let backend = Arc::new(FakeBackend::new(
            [audit(InstallationStatus::NeedsBaseGame)],
            UpdateStatus::UpToDate,
        ));
        backend.state.lock().unwrap().locations = VecDeque::from([Some(card)]);
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.location_unavailable());

        // A refused location is reported, and nothing else changes.
        harness
            .intents
            .send(Intent::ChooseInstallLocation(PathBuf::from(
                "/run/media/fat32",
            )))
            .unwrap();
        let refused =
            harness.wait_for(|snapshot| snapshot.location_error() == Some("location-not-writable"));
        assert!(refused.location_unavailable());

        harness
            .intents
            .send(Intent::ChooseInstallLocation(PathBuf::from(
                "/home/deck/Games",
            )))
            .unwrap();
        let moved = harness.wait_for(|snapshot| {
            snapshot.location().is_some_and(|location| {
                location.available && location.path == Path::new("/home/deck/Games/pd2-launcher")
            }) && snapshot.installation() == InstallationStatus::NeedsBaseGame
                && snapshot.host() == HostView::Supported
        });
        assert_eq!(moved.location_error(), None);
        let calls = backend.calls();
        let relocate = calls.iter().rposition(|call| *call == "relocate").unwrap();
        let reopen = calls.iter().rposition(|call| *call == "open").unwrap();
        let audit = calls.iter().rposition(|call| *call == "audit").unwrap();
        assert!(relocate < reopen && reopen < audit, "{calls:?}");
    }

    #[test]
    fn the_location_cannot_change_while_a_game_or_job_runs() {
        let backend = Arc::new(FakeBackend::new(
            [audit(InstallationStatus::Ready)],
            UpdateStatus::UpToDate,
        ));
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.update() == UpdateStatus::UpToDate);
        harness
            .intents
            .send(Intent::LaunchGame(
                GraphicsDriver::Standard,
                None,
                false,
                false,
            ))
            .unwrap();
        harness.wait_for(|snapshot| snapshot.game() == GameView::Running);
        harness
            .intents
            .send(Intent::ChooseInstallLocation(PathBuf::from(
                "/home/deck/Games",
            )))
            .unwrap();
        harness.wait_for(|snapshot| {
            snapshot
                .evidence()
                .iter()
                .any(|item| item.detail == "location-change-not-permitted")
        });
        assert!(!backend.calls().contains(&"relocate"));
    }

    #[test]
    fn the_old_installation_is_removed_only_after_the_new_one_is_ready() {
        let moved = |previous: bool| LocationView {
            path: PathBuf::from("/run/media/deck/card/pd2-launcher"),
            default: false,
            available: true,
            previous: previous
                .then(|| PathBuf::from("/home/deck/.local/share/pd2-launcher/managed")),
        };
        let backend = Arc::new(FakeBackend::new(
            [audit(InstallationStatus::NeedsBaseGame)],
            UpdateStatus::UpToDate,
        ));
        backend.state.lock().unwrap().locations = VecDeque::from([Some(moved(true))]);
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.installation() == InstallationStatus::NeedsBaseGame);
        harness
            .intents
            .send(Intent::RemovePreviousInstallation)
            .unwrap();
        harness.wait_for(|snapshot| {
            snapshot
                .evidence()
                .iter()
                .any(|item| item.detail == "remove-previous-not-permitted")
        });
        assert!(!backend.calls().contains(&"remove-previous"));

        let backend = Arc::new(FakeBackend::new(
            [audit(InstallationStatus::Ready)],
            UpdateStatus::UpToDate,
        ));
        backend.state.lock().unwrap().locations = VecDeque::from([Some(moved(true))]);
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.update() == UpdateStatus::UpToDate);
        harness
            .intents
            .send(Intent::RemovePreviousInstallation)
            .unwrap();
        harness.wait_for(|snapshot| {
            snapshot
                .location()
                .is_some_and(|location| location.previous.is_none())
        });
        assert!(backend.calls().contains(&"remove-previous"));
    }

    #[test]
    fn an_unattended_install_dir_asks_for_a_fresh_root_only() {
        let backend = Arc::new(FakeBackend::new(
            [audit(InstallationStatus::NeedsBaseGame)],
            UpdateStatus::UpToDate,
        ));
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.installation() == InstallationStatus::NeedsBaseGame);
        harness
            .intents
            .send(Intent::UseFreshInstallLocation(PathBuf::from(
                "/run/media/card",
            )))
            .unwrap();
        harness.wait_for(|snapshot| {
            snapshot
                .location()
                .is_some_and(|location| location.path == Path::new("/run/media/card/pd2-launcher"))
        });
        assert!(backend.calls().contains(&"relocate-fresh"));
        assert!(!backend.calls().contains(&"relocate"));
    }

    #[test]
    fn only_a_confirmed_request_runs_an_unverified_official_launcher() {
        let unverified = audit(InstallationStatus::Ready).with_official_launcher_unverified(true);
        let backend = Arc::new(FakeBackend::new(
            [unverified.clone(), unverified],
            UpdateStatus::UpToDate,
        ));
        let mut harness = Harness::start(backend.clone());
        let snapshot = harness.wait_for(|snapshot| {
            snapshot.official_launcher_unverified() && snapshot.update() == UpdateStatus::UpToDate
        });
        assert!(crate::presentation::official_launcher_available(&snapshot));
        harness
            .intents
            .send(Intent::OpenOfficialLauncherConfirmed(false, false))
            .unwrap();
        backend.wait_for(|state| state.official_launcher_calls == 1);
        assert!(backend.state.lock().unwrap().official_launcher_confirmed);
        assert!(
            !backend
                .state
                .lock()
                .unwrap()
                .official_launcher_strict_memory_order
        );
        backend.official_launcher_gate.release();
        harness.wait_for(|snapshot| {
            snapshot.operation().is_some_and(|operation| {
                operation.phase() == OperationPhase::OfficialLauncher
                    && operation.status() == OperationStatusView::Succeeded
            })
        });
        harness.intents.send(Intent::OpenOfficialLauncher).unwrap();
        backend.wait_for(|state| state.official_launcher_calls == 2);
        assert!(!backend.state.lock().unwrap().official_launcher_confirmed);
    }

    #[test]
    fn a_game_that_the_official_launcher_starts_gets_strict_memory_order_only_when_asked() {
        let backend = Arc::new(FakeBackend::new(
            [audit(InstallationStatus::Ready)],
            UpdateStatus::UpToDate,
        ));
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| {
            crate::presentation::official_launcher_available(snapshot)
                && snapshot.update() == UpdateStatus::UpToDate
        });
        harness
            .intents
            .send(Intent::OpenOfficialLauncherConfirmed(true, true))
            .unwrap();
        backend.wait_for(|state| state.official_launcher_calls == 1);
        assert!(
            backend
                .state
                .lock()
                .unwrap()
                .official_launcher_strict_memory_order
        );
        backend.official_launcher_gate.release();
        harness.wait_for(|snapshot| {
            snapshot.operation().is_some_and(|operation| {
                operation.phase() == OperationPhase::OfficialLauncher
                    && operation.status() == OperationStatusView::Succeeded
            })
        });
        assert!(
            backend
                .state
                .lock()
                .unwrap()
                .official_launcher_detailed_logging
        );
        // The next job asks again: nothing is kept from the last one.
        harness.intents.send(Intent::OpenOfficialLauncher).unwrap();
        backend.wait_for(|state| state.official_launcher_calls == 2);
        let state = backend.state.lock().unwrap();
        assert!(!state.official_launcher_strict_memory_order);
        assert!(!state.official_launcher_detailed_logging);
    }

    #[test]
    fn an_official_launcher_update_that_was_not_accepted_is_its_own_outcome() {
        let backend = Arc::new(FakeBackend::new(
            [
                audit(InstallationStatus::Ready),
                audit(InstallationStatus::NeedsClient),
            ],
            UpdateStatus::UpToDate,
        ));
        backend.state.lock().unwrap().official_launcher_failure =
            Some(BackendFailure::new("official-client-not-reconciled"));
        backend.official_launcher_gate.release();
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.update() == UpdateStatus::UpToDate);
        harness.intents.send(Intent::OpenOfficialLauncher).unwrap();
        let closed = harness.wait_for(|snapshot| {
            snapshot.operation().is_some_and(|operation| {
                operation.phase() == OperationPhase::OfficialLauncher
                    && operation.status() == OperationStatusView::Failed
            })
        });
        assert_eq!(closed.installation(), InstallationStatus::NeedsClient);
        assert!(
            closed
                .operation()
                .unwrap()
                .detail()
                .contains("was not accepted")
        );
    }

    /// Software rendering is information for the Play screen, never a
    /// gate; an audit that could not tell keeps the last answer.
    #[test]
    fn software_rendering_follows_the_audits_that_could_tell() {
        let (events, _receiver) = std::sync::mpsc::sync_channel(64);
        let mut coordinator = Coordinator::new(events);
        let ready =
            |software| audit(InstallationStatus::Ready).with_game_software_rendering(software);
        coordinator.apply_audit(ready(Some(true)));
        assert!(coordinator.snapshot.game_software_rendering());
        assert!(coordinator.play_guard(), "Play stays available");
        coordinator.apply_audit(ready(None));
        assert!(coordinator.snapshot.game_software_rendering());
        coordinator.apply_audit(ready(Some(false)));
        assert!(!coordinator.snapshot.game_software_rendering());
    }

    /// The Android app shows which part of the game environment is being
    /// set up, and marks the environment done from the audit alone.
    #[test]
    fn the_environment_part_reaches_the_operation_and_its_audit_the_snapshot() {
        use crate::operation::{EnvironmentPart, EnvironmentProgress, WorkProgress};
        let (events, _receiver) = std::sync::mpsc::sync_channel(64);
        let mut coordinator = Coordinator::new(events);
        let (publisher, progress) = progress_channel();
        let control = OperationControl::with_progress(publisher);
        coordinator.active_mutation = Some(ActiveMutation {
            kind: MutationKind::InstallBaseGame,
            cancellation: OperationControl::default(),
            progress,
        });
        let part = EnvironmentProgress::new(
            EnvironmentPart::Dotnet,
            &[EnvironmentPart::WebComponents, EnvironmentPart::Dotnet],
        );
        control.report(
            WorkProgress::for_phase(WorkPhase::PrefixInitialization).with_environment(part),
        );
        coordinator.poll_progress();
        let operation = coordinator.snapshot.operation().expect("operation");
        assert_eq!(operation.phase(), OperationPhase::PrefixInitialization);
        assert_eq!(operation.environment(), Some(part));

        assert!(!coordinator.snapshot.environment_ready());
        coordinator
            .apply_audit(audit(InstallationStatus::NeedsBaseGame).with_environment_ready(true));
        assert!(coordinator.snapshot.environment_ready());
        coordinator.apply_audit(audit(InstallationStatus::NeedsBaseGame));
        assert!(!coordinator.snapshot.environment_ready());
    }

    #[test]
    fn a_rejected_download_source_save_marks_only_its_row() {
        let (events, _receiver) = std::sync::mpsc::sync_channel(64);
        let mut coordinator = Coordinator::new(events);
        let row = |artifact| DownloadSourceView {
            artifact,
            effective_url: artifact.builtin_url(),
            badge: crate::presentation::SourceBadgeView::BuiltInVerified,
            error: None,
            installed: None,
        };
        coordinator.change_download_sources(vec![
            row(DownloadArtifact::Wine),
            row(DownloadArtifact::Gecko),
        ]);
        coordinator.apply_download_sources(
            DownloadArtifact::Wine,
            Err(BackendFailure::new("source-url-invalid")),
        );
        let sources = coordinator.snapshot.download_sources();
        assert_eq!(sources[0].error, Some("source-url-invalid"));
        assert_eq!(sources[1].error, None);
        assert_eq!(
            sources[0].effective_url,
            DownloadArtifact::Wine.builtin_url()
        );

        coordinator.apply_download_sources(
            DownloadArtifact::Wine,
            Ok(vec![
                row(DownloadArtifact::Wine),
                row(DownloadArtifact::Gecko),
            ]),
        );
        assert!(
            coordinator
                .snapshot
                .download_sources()
                .iter()
                .all(|source| source.error.is_none())
        );
    }

    #[test]
    fn settings_navigation_and_changes_publish_persisted_snapshot_state() {
        let backend = Arc::new(FakeBackend::new(
            [audit(InstallationStatus::Ready)],
            UpdateStatus::UpToDate,
        ));
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.host() == HostView::Supported);

        harness
            .intents
            .send(Intent::Navigate(Destination::Settings))
            .expect("settings intent");
        let snapshot = harness.wait_for(|snapshot| {
            snapshot.settings().persistence() == SettingsPersistenceView::Saved
        });
        assert_eq!(
            snapshot.settings().renderer(),
            RendererPreferenceView::Glide
        );

        for change in [
            SettingsChange::Renderer(RendererPreferenceView::DirectDraw),
            SettingsChange::SkipToBnet(false),
            SettingsChange::SoundInBackground(true),
        ] {
            harness
                .intents
                .send(Intent::ChangeSettings(change))
                .expect("settings change");
        }
        let changed = harness.wait_for(|snapshot| {
            snapshot.settings().renderer() == RendererPreferenceView::DirectDraw
                && !snapshot.settings().skip_to_bnet()
                && snapshot.settings().sound_in_background()
        });
        assert_eq!(
            changed.settings().persistence(),
            SettingsPersistenceView::Saved
        );
        backend.wait_for(|state| state.settings_updates == 3);

        harness
            .intents
            .send(Intent::Navigate(Destination::Operations))
            .expect("operations intent");
        let restored = harness
            .wait_for(|snapshot| snapshot.evidence().iter().any(|item| item.label == "Audit"));
        assert_eq!(restored.evidence()[0].detail, "Fresh test evidence");
    }

    #[test]
    fn save_report_shows_creating_then_where_the_file_is_and_reopens_its_folder() {
        let backend = Arc::new(FakeBackend::new(
            [audit(InstallationStatus::Ready)],
            UpdateStatus::UpToDate,
        ));
        let mut harness = Harness::start(backend);
        harness.wait_for(|snapshot| snapshot.host() == HostView::Supported);
        harness
            .intents
            .send(Intent::ExportDiagnostics)
            .expect("save report");
        let saved = harness.wait_for(|snapshot| {
            matches!(
                snapshot.report(),
                Some(ReportView::Saved {
                    opened: Some(_),
                    ..
                })
            )
        });
        let Some(ReportView::Saved { path, opened }) = saved.report() else {
            unreachable!("waited for a saved report");
        };
        assert!(path.ends_with("pd2-launcher-report-19700101T000000Z.tar.xz"));
        // The fake backend cannot open a file manager.
        assert_eq!(*opened, Some(false));

        harness
            .intents
            .send(Intent::RevealReport)
            .expect("open folder");
        harness.wait_for(|snapshot| {
            matches!(
                snapshot.report(),
                Some(ReportView::Saved {
                    opened: Some(false),
                    ..
                })
            )
        });
        harness
            .intents
            .send(Intent::DismissReport)
            .expect("dismiss");
        harness.wait_for(|snapshot| snapshot.report().is_none());
    }

    #[test]
    fn settings_load_and_save_failures_remain_stable_in_the_snapshot() {
        let backend = Arc::new(
            FakeBackend::new([audit(InstallationStatus::Ready)], UpdateStatus::UpToDate)
                .with_settings_load_failure(BackendFailure::new("settings-load-failed")),
        );
        let mut harness = Harness::start(backend);
        let failed = harness.wait_for(|snapshot| {
            snapshot.settings().persistence()
                == SettingsPersistenceView::Failed("settings-load-failed")
        });
        assert_eq!(failed.settings().renderer(), RendererPreferenceView::Glide);

        let backend = Arc::new(
            FakeBackend::new([audit(InstallationStatus::Ready)], UpdateStatus::UpToDate)
                .with_settings_save_failure(BackendFailure::new("settings-save-failed")),
        );
        let mut harness = Harness::start(backend);
        harness.wait_for(|snapshot| {
            snapshot.settings().persistence() == SettingsPersistenceView::Saved
        });
        harness
            .intents
            .send(Intent::ChangeSettings(SettingsChange::SkipToBnet(false)))
            .expect("settings change");
        let failed = harness.wait_for(|snapshot| {
            snapshot.settings().persistence()
                == SettingsPersistenceView::Failed("settings-save-failed")
        });
        assert!(failed.settings().skip_to_bnet());
    }

    #[test]
    fn startup_repairs_before_the_audit_that_can_restore_ready() {
        let backend = Arc::new(FakeBackend::new(
            [
                audit(InstallationStatus::RepairRequired),
                audit(InstallationStatus::Ready),
            ],
            UpdateStatus::UpToDate,
        ));
        let mut harness = Harness::start(backend.clone());

        let snapshot = harness.wait_for(|snapshot| {
            snapshot.installation() == InstallationStatus::Ready
                && !snapshot.unresolved_journal()
                && snapshot.launch_preflight_passed()
        });
        assert!(snapshot.launch_preflight_passed());
        let calls = backend.calls();
        let first_audit = calls.iter().position(|call| *call == "audit").unwrap();
        let repair = calls.iter().position(|call| *call == "repair").unwrap();
        let second_audit = calls.iter().rposition(|call| *call == "audit").unwrap();
        assert!(first_audit < repair && repair < second_audit, "{calls:?}");
    }

    #[test]
    fn typed_progress_is_coalesced_into_operation_state_without_replacing_audit_evidence() {
        let backend = Arc::new(
            FakeBackend::new(
                [
                    audit(InstallationStatus::NeedsBaseGame),
                    audit(InstallationStatus::Ready),
                ],
                UpdateStatus::UpToDate,
            )
            .with_install_blocked(),
        );
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.installation() == InstallationStatus::NeedsBaseGame);
        harness.select_installers();
        harness
            .intents
            .send(Intent::AuthorizeAndInstall(
                AuthorizationInput::new(
                    "AAAA-BBBB-CCCC-DDDD".to_owned(),
                    "EEEE-FFFF-GGGG-HHHH".to_owned(),
                    true,
                )
                .unwrap(),
            ))
            .unwrap();

        let progressing = harness.wait_for(|snapshot| {
            snapshot.operation().is_some_and(|operation| {
                operation.phase() == OperationPhase::ClientDownload
                    && operation.progress() == Some((3, 8))
            })
        });
        assert_eq!(progressing.evidence()[0].label, "Audit");
        assert_eq!(progressing.evidence()[0].detail, "Fresh test evidence");
        let summary = progressing
            .operation()
            .and_then(OperationView::progress_summary)
            .expect("composite progress summary");
        // Plain sizes and counts, never raw byte numbers.
        assert_eq!(summary, "4.1 KB downloaded · 3 of 8 files · 2 of 8 checked");
        assert_eq!(
            progressing
                .operation()
                .map(OperationView::phase)
                .map(|phase| phase == OperationPhase::ClientDownload),
            Some(true)
        );

        backend.install_gate.release();
        harness.wait_for(|snapshot| snapshot.installation() == InstallationStatus::Ready);
    }

    #[test]
    fn automatic_update_check_is_advisory_and_does_not_delay_play() {
        let backend = Arc::new(
            FakeBackend::new([audit(InstallationStatus::Ready)], UpdateStatus::Available)
                .with_check_blocked(),
        );
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| {
            snapshot.installation() == InstallationStatus::Ready
                && snapshot.update() == UpdateStatus::Checking
        });

        harness
            .intents
            .send(Intent::LaunchGame(
                GraphicsDriver::Standard,
                None,
                false,
                false,
            ))
            .expect("play intent");
        let running = harness.wait_for(|snapshot| snapshot.game() == GameView::Running);
        assert_eq!(running.installation(), InstallationStatus::Ready);
        assert_eq!(running.update(), UpdateStatus::Checking);

        backend.check_gate.release();
        let available = harness.wait_for(|snapshot| snapshot.update() == UpdateStatus::Available);
        assert_eq!(available.installation(), InstallationStatus::Ready);
        assert_eq!(available.game(), GameView::Running);
    }

    #[test]
    fn shutdown_abandons_only_the_bounded_read_only_update_check() {
        let backend = Arc::new(
            FakeBackend::new([audit(InstallationStatus::Ready)], UpdateStatus::Available)
                .with_check_blocked(),
        );
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| {
            snapshot.installation() == InstallationStatus::Ready
                && snapshot.update() == UpdateStatus::Checking
        });

        let runtime = harness.runtime;
        let (stopped_sender, stopped_receiver) = std::sync::mpsc::sync_channel(1);
        let shutdown = thread::spawn(move || {
            drop(runtime);
            let _ = stopped_sender.send(());
        });
        let did_not_wait_for_read_only_check = stopped_receiver
            .recv_timeout(Duration::from_millis(250))
            .is_ok();

        // Always release the fixture thread, including on a regression, so
        // the test process has no intentionally surviving helper.
        backend.check_gate.release();
        shutdown.join().expect("shutdown worker");
        assert!(did_not_wait_for_read_only_check);
    }

    #[test]
    fn failed_or_offline_automatic_update_check_keeps_ready_playable() {
        let backend = Arc::new(
            FakeBackend::new([audit(InstallationStatus::Ready)], UpdateStatus::Unknown)
                .with_check_failure(BackendFailure::new("offline")),
        );
        let mut harness = Harness::start(backend.clone());
        let snapshot = harness.wait_for(|snapshot| snapshot.update() == UpdateStatus::CheckFailed);
        assert_eq!(snapshot.installation(), InstallationStatus::Ready);
        assert!(snapshot.launch_preflight_passed());

        harness
            .intents
            .send(Intent::LaunchGame(
                GraphicsDriver::Compatibility,
                Some(GraphicsProfile::Performance),
                true,
                true,
            ))
            .expect("play intent");
        let running = harness.wait_for(|snapshot| snapshot.game() == GameView::Running);
        assert_eq!(running.installation(), InstallationStatus::Ready);
        assert_eq!(running.update(), UpdateStatus::CheckFailed);
        backend.wait_for(|state| {
            state.launch_calls == 1
                && state.launch_driver == GraphicsDriver::Compatibility
                && state.launch_profile == Some(GraphicsProfile::Performance)
                && state.launch_strict_memory_order
                && state.launch_detailed_logging
        });
    }

    #[test]
    fn with_update_checks_off_no_check_runs_and_play_starts() {
        let backend = Arc::new(FakeBackend::new(
            [audit(InstallationStatus::Ready)],
            UpdateStatus::Available,
        ));
        backend.state.lock().expect("state").settings =
            LaunchSettingsView::new(RendererPreferenceView::Glide, true, false)
                .with_update_checks(false);
        let mut harness = Harness::start(backend.clone());
        let ready = harness.wait_for(|snapshot| {
            snapshot.installation() == InstallationStatus::Ready
                && snapshot.launch_preflight_passed()
                && !snapshot.settings().update_checks()
        });
        assert_eq!(ready.update(), UpdateStatus::Unknown);

        // Check again runs the start's audit again, without the check.
        harness
            .intents
            .send(Intent::CheckAgain)
            .expect("check again");
        backend.wait_for(|state| state.calls.iter().filter(|call| **call == "audit").count() >= 2);
        harness
            .intents
            .send(Intent::LaunchGame(
                GraphicsDriver::Standard,
                None,
                false,
                false,
            ))
            .expect("play intent");
        let running = harness.wait_for(|snapshot| snapshot.game() == GameView::Running);
        assert_eq!(running.update(), UpdateStatus::Unknown);
        assert!(
            !backend.calls().contains(&"check-update"),
            "{:?}",
            backend.calls()
        );
    }

    #[test]
    fn turning_update_checks_on_audits_and_checks_and_off_forgets_the_update() {
        let backend = Arc::new(FakeBackend::new(
            [audit(InstallationStatus::Ready)],
            UpdateStatus::Available,
        ));
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.update() == UpdateStatus::Available);

        harness
            .intents
            .send(Intent::ChangeSettings(SettingsChange::UpdateChecks(false)))
            .expect("settings change");
        harness.wait_for(|snapshot| {
            !snapshot.settings().update_checks() && snapshot.update() == UpdateStatus::Unknown
        });
        // The client is held to the other rule at once.
        wait_for_calls(&backend, "audit", 2);
        wait_for_calls(&backend, "check-update", 1);

        harness
            .intents
            .send(Intent::ChangeSettings(SettingsChange::UpdateChecks(true)))
            .expect("settings change");
        harness.wait_for(|snapshot| {
            snapshot.settings().update_checks() && snapshot.update() == UpdateStatus::Available
        });
        wait_for_calls(&backend, "audit", 3);
        wait_for_calls(&backend, "check-update", 2);
    }

    #[test]
    fn turning_update_checks_off_while_the_game_runs_forgets_the_update_without_an_audit() {
        let backend = Arc::new(FakeBackend::new(
            [audit(InstallationStatus::Ready)],
            UpdateStatus::Available,
        ));
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.update() == UpdateStatus::Available);
        harness
            .intents
            .send(Intent::LaunchGame(
                GraphicsDriver::Standard,
                None,
                false,
                false,
            ))
            .expect("play intent");
        harness.wait_for(|snapshot| snapshot.game() == GameView::Running);
        let audits = |calls: &[&str]| calls.iter().filter(|call| **call == "audit").count();
        let before = audits(&backend.calls());

        harness
            .intents
            .send(Intent::ChangeSettings(SettingsChange::UpdateChecks(false)))
            .expect("settings change");
        let off = harness.wait_for(|snapshot| {
            !snapshot.settings().update_checks() && snapshot.update() == UpdateStatus::Unknown
        });
        assert_eq!(off.game(), GameView::Running);
        assert_eq!(audits(&backend.calls()), before, "no audit under the game");
    }

    /// An Android coordinator, with the channel its workers report on.
    fn android_coordinator() -> (
        Coordinator,
        Sender<Completion>,
        std::sync::mpsc::Receiver<Completion>,
    ) {
        let (events, _receiver) = std::sync::mpsc::sync_channel(1);
        let mut coordinator = Coordinator::new(events);
        coordinator.android_host = true;
        let (completions, finished) = channel();
        (coordinator, completions, finished)
    }

    /// On the Android host Project Diablo 2's update is checked only when
    /// the app asks (Check again, which it sends at Play), and only for an
    /// installed game: never at the engine's start.
    #[test]
    fn on_android_only_check_again_checks_for_an_update() {
        let backend = Arc::new(FakeBackend::new(
            [audit(InstallationStatus::Ready)],
            UpdateStatus::Available,
        ));
        let (mut coordinator, completions, finished) = android_coordinator();
        coordinator.startup(backend.clone(), completions.clone(), false);
        assert_eq!(
            coordinator.snapshot.installation(),
            InstallationStatus::Ready
        );
        assert_eq!(coordinator.snapshot.update(), UpdateStatus::Unknown);
        assert!(!coordinator.update_check_active);

        coordinator.handle_intent(Intent::CheckAgain, backend.clone(), completions);
        assert_eq!(coordinator.snapshot.update(), UpdateStatus::Checking);
        let checked = finished.recv_timeout(WAIT).expect("the check ends");
        assert!(!coordinator.handle_completion(checked));
        assert_eq!(coordinator.snapshot.update(), UpdateStatus::Available);
        assert_eq!(
            backend
                .calls()
                .iter()
                .filter(|call| **call == "check-update")
                .count(),
            1
        );

        // Check again before the game is installed checks nothing.
        let backend = Arc::new(FakeBackend::new(
            [audit(InstallationStatus::NeedsBaseGame)],
            UpdateStatus::Available,
        ));
        let (mut coordinator, completions, _finished) = android_coordinator();
        coordinator.handle_intent(Intent::CheckAgain, backend.clone(), completions);
        assert_eq!(coordinator.snapshot.update(), UpdateStatus::Unknown);
        assert!(!backend.calls().contains(&"check-update"));
    }

    /// On the Android host no check follows a job: the update is unknown
    /// after it until Play asks again. A failed job leaves the update as it
    /// was, so Update can be tried again.
    #[test]
    fn on_android_no_check_follows_a_job_and_the_update_is_unknown_after_it() {
        let backend = Arc::new(FakeBackend::new(
            [audit(InstallationStatus::Ready)],
            UpdateStatus::Available,
        ));
        let (mut coordinator, completions, finished) = android_coordinator();
        coordinator.startup(backend.clone(), completions.clone(), false);
        coordinator.handle_intent(Intent::CheckAgain, backend.clone(), completions.clone());
        let checked = finished.recv_timeout(WAIT).expect("the check ends");
        coordinator.handle_completion(checked);
        assert_eq!(coordinator.snapshot.update(), UpdateStatus::Available);

        coordinator.handle_intent(Intent::ApplyUpdate, backend.clone(), completions);
        let updated = finished.recv_timeout(WAIT).expect("the update ends");
        assert!(matches!(updated, Completion::MutationFinished { .. }));
        assert!(!coordinator.handle_completion(updated), "no check after it");
        assert_eq!(coordinator.snapshot.update(), UpdateStatus::Unknown);
        assert_eq!(
            backend
                .calls()
                .iter()
                .filter(|call| **call == "check-update")
                .count(),
            1
        );
    }

    /// On the Android host turning the checks on audits again but checks
    /// nothing: Play does.
    #[test]
    fn on_android_turning_update_checks_on_checks_nothing() {
        let backend = Arc::new(FakeBackend::new(
            [audit(InstallationStatus::Ready)],
            UpdateStatus::Available,
        ));
        backend.state.lock().expect("state").settings =
            LaunchSettingsView::new(RendererPreferenceView::Glide, true, false)
                .with_update_checks(false);
        let (mut coordinator, completions, _finished) = android_coordinator();
        coordinator.startup(backend.clone(), completions.clone(), false);
        assert!(!coordinator.snapshot.settings().update_checks());
        coordinator.handle_intent(
            Intent::ChangeSettings(SettingsChange::UpdateChecks(true)),
            backend.clone(),
            completions,
        );
        assert!(coordinator.snapshot.settings().update_checks());
        assert_eq!(coordinator.snapshot.update(), UpdateStatus::Unknown);
        let calls = backend.calls();
        assert_eq!(calls.iter().filter(|call| **call == "audit").count(), 2);
        assert!(!calls.contains(&"check-update"), "{calls:?}");
    }

    /// A check that was running when the checks were turned off says
    /// nothing, and runs no other check after it.
    #[test]
    fn a_check_that_ends_after_the_checks_were_turned_off_is_dropped() {
        let (events, _receiver) = std::sync::mpsc::sync_channel(64);
        let mut coordinator = Coordinator::new(events);
        coordinator.update_check_active = true;
        coordinator.update_check_pending = true;
        coordinator.change_settings(LaunchSettingsView::default().with_update_checks(false));
        let check_again =
            coordinator.handle_completion(Completion::UpdateChecked(Ok(UpdateStatus::Available)));
        assert!(!check_again);
        assert!(!coordinator.update_check_active && !coordinator.update_check_pending);
        assert_eq!(coordinator.snapshot.update(), UpdateStatus::Unknown);
    }

    #[test]
    fn first_d2gl_launch_notice_lasts_until_the_next_launch() {
        let backend = Arc::new(FakeBackend::new(
            [audit(InstallationStatus::Ready)],
            UpdateStatus::UpToDate,
        ));
        backend.state.lock().expect("state").launch_notice = Some(GameNoticeView::FirstD2glLaunch);
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.launch_preflight_passed());

        harness
            .intents
            .send(Intent::LaunchGame(
                GraphicsDriver::Standard,
                None,
                false,
                false,
            ))
            .expect("play intent");
        let running = harness.wait_for(|snapshot| snapshot.game() == GameView::Running);
        assert_eq!(running.game_notice(), Some(GameNoticeView::FirstD2glLaunch));

        harness
            .intents
            .send(Intent::RequestSafeStop)
            .expect("stop intent");
        // The tip stays once the game exits, where the player reads it.
        let exited = harness.wait_for(|snapshot| snapshot.game() == GameView::Idle);
        assert_eq!(exited.game_notice(), Some(GameNoticeView::FirstD2glLaunch));

        // D2GL has written its configuration by now; the next launch is quiet.
        backend.state.lock().expect("state").launch_notice = None;
        harness
            .intents
            .send(Intent::LaunchGame(
                GraphicsDriver::Standard,
                None,
                false,
                false,
            ))
            .expect("second play intent");
        let running = harness.wait_for(|snapshot| snapshot.game() == GameView::Running);
        assert_eq!(running.game_notice(), None);
    }

    #[test]
    fn restart_loses_the_in_memory_authorization_and_requests_a_safe_stop() {
        let backend = Arc::new(
            FakeBackend::new(
                [audit(InstallationStatus::NeedsBaseGame)],
                UpdateStatus::UpToDate,
            )
            .with_install_blocked(),
        );
        let mut first = Harness::start(backend.clone());
        first.wait_for(|snapshot| snapshot.host() == HostView::Supported);
        first.select_installers();
        first
            .intents
            .send(Intent::AuthorizeAndInstall(
                AuthorizationInput::new(
                    "AAAA-BBBB-CCCC-DDDD".to_owned(),
                    "EEEE-FFFF-GGGG-HHHH".to_owned(),
                    true,
                )
                .expect("authorization"),
            ))
            .expect("install intent");
        backend.wait_for(|state| state.install_calls == 1);
        first.runtime.shutdown();
        backend.wait_for(|state| state.cancellation_observed);

        let mut restarted = Harness::start(backend);
        let snapshot = restarted.wait_for(|snapshot| snapshot.host() == HostView::Supported);
        assert_eq!(snapshot.authorization(), AuthorizationView::Absent);
    }

    #[test]
    fn cancellation_is_published_only_with_the_successor_audit_state() {
        let backend = Arc::new(
            FakeBackend::new(
                [
                    audit(InstallationStatus::NeedsBaseGame),
                    audit(InstallationStatus::NeedsBaseGame),
                ],
                UpdateStatus::UpToDate,
            )
            .with_install_blocked(),
        );
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.installation() == InstallationStatus::NeedsBaseGame);
        harness.select_installers();
        harness
            .intents
            .send(Intent::AuthorizeAndInstall(
                AuthorizationInput::new(
                    "AAAA-BBBB-CCCC-DDDD".to_owned(),
                    "EEEE-FFFF-GGGG-HHHH".to_owned(),
                    true,
                )
                .unwrap(),
            ))
            .unwrap();
        backend.wait_for(|state| state.install_calls == 1);
        harness.intents.send(Intent::RequestSafeStop).unwrap();

        backend.wait_for(|state| state.calls.contains(&"safe-stop"));

        let cancelled = harness.wait_for(|snapshot| {
            snapshot
                .operation()
                .is_some_and(|operation| operation.status() == OperationStatusView::Cancelled)
        });
        assert_eq!(cancelled.installation(), InstallationStatus::NeedsBaseGame);
        assert_eq!(cancelled.evidence()[0].detail, "Fresh test evidence");
        backend.wait_for(|state| {
            state.cancellation_observed
                && state.calls.iter().filter(|call| **call == "audit").count() == 2
        });
    }

    #[test]
    fn unproven_wine_stop_remains_indeterminate_after_successor_audit() {
        let backend = Arc::new(
            FakeBackend::new(
                [
                    audit(InstallationStatus::NeedsBaseGame),
                    audit(InstallationStatus::Indeterminate),
                ],
                UpdateStatus::UpToDate,
            )
            .with_install_failure(BackendFailure::new("operation-indeterminate")),
        );
        let mut harness = Harness::start(backend);
        harness.wait_for(|snapshot| snapshot.installation() == InstallationStatus::NeedsBaseGame);
        harness.select_installers();
        harness
            .intents
            .send(Intent::AuthorizeAndInstall(
                AuthorizationInput::new(
                    "AAAA-BBBB-CCCC-DDDD".to_owned(),
                    "EEEE-FFFF-GGGG-HHHH".to_owned(),
                    true,
                )
                .unwrap(),
            ))
            .unwrap();

        let snapshot = harness.wait_for(|snapshot| {
            snapshot
                .operation()
                .is_some_and(|operation| operation.status() == OperationStatusView::Indeterminate)
        });
        assert_eq!(snapshot.installation(), InstallationStatus::Indeterminate);
    }

    #[test]
    fn losing_the_view_channel_does_not_cancel_a_durable_install_job() {
        let backend = Arc::new(
            FakeBackend::new(
                [
                    audit(InstallationStatus::NeedsBaseGame),
                    audit(InstallationStatus::Ready),
                ],
                UpdateStatus::UpToDate,
            )
            .with_install_blocked(),
        );
        let (intents, intent_receiver) = sync_channel(32);
        let (event_sender, _events) = sync_channel(256);
        let runtime = ServiceRuntime::start(
            ServiceEndpoint {
                intent_receiver,
                event_sender,
            },
            backend.clone(),
        )
        .expect("service runtime");
        backend.wait_for(|state| state.calls.contains(&"audit"));
        intents
            .send(Intent::SelectInstallerMedia(PathBuf::from(
                "/tmp/pd2-launcher-test-installers",
            )))
            .expect("installer media intent");
        backend.wait_for(|state| state.calls.contains(&"select-installer-media"));
        intents
            .send(Intent::AuthorizeAndInstall(
                AuthorizationInput::new(
                    "AAAA-BBBB-CCCC-DDDD".to_owned(),
                    "EEEE-FFFF-GGGG-HHHH".to_owned(),
                    true,
                )
                .expect("authorization"),
            ))
            .expect("install intent");
        backend.wait_for(|state| state.install_calls == 1);

        drop(intents);
        thread::sleep(Duration::from_millis(80));
        assert!(!backend.state.lock().expect("state").cancellation_observed);

        backend.install_gate.release();
        backend.wait_for(|state| state.calls.iter().filter(|call| **call == "audit").count() == 2);
        runtime.shutdown();
    }

    #[test]
    fn a_sequence_gap_is_recovered_only_by_a_full_snapshot() {
        let (events, receiver) = sync_channel(1);
        let mut coordinator = Coordinator::new(events);
        let mut reducer = UiReducer::new(UiSnapshot::initial());

        coordinator.change_host(HostView::Supported); // sequence 1 is queued
        coordinator.change_installation(InstallationStatus::Ready); // sequence 2 is dropped
        assert_eq!(
            reducer.reduce(receiver.recv().expect("sequence 1")),
            ReduceOutcome::Applied
        );
        coordinator.change_update(UpdateStatus::Available); // sequence 3 is queued
        assert_eq!(
            reducer.reduce(receiver.recv().expect("sequence 3")),
            ReduceOutcome::SnapshotRequired
        );

        coordinator.publish_full_snapshot();
        assert_eq!(
            reducer.reduce(receiver.recv().expect("full snapshot")),
            ReduceOutcome::Applied
        );
        assert_eq!(reducer.snapshot().installation(), InstallationStatus::Ready);
        assert_eq!(reducer.snapshot().update(), UpdateStatus::Available);
        assert_eq!(reducer.snapshot().last_event_sequence(), 4);
    }

    fn official_launcher_open(snapshot: &UiSnapshot) -> bool {
        snapshot.operation().is_some_and(|operation| {
            operation.phase() == OperationPhase::OfficialLauncher
                && operation.status() == OperationStatusView::Running
        })
    }

    #[test]
    fn the_official_launcher_holds_the_mutation_slot_and_re_audits_when_closed() {
        let backend = Arc::new(FakeBackend::new(
            [
                audit(InstallationStatus::Ready),
                // After it closes: the fresh audit decides the state.
                audit(InstallationStatus::Ready),
            ],
            UpdateStatus::UpToDate,
        ));
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.update() == UpdateStatus::UpToDate);

        harness
            .intents
            .send(Intent::OpenOfficialLauncher)
            .expect("official launcher intent");
        backend.wait_for(|state| state.official_launcher_calls == 1);
        let open = harness.wait_for(official_launcher_open);
        let deck = DeckPresentation::project(&open, false);
        assert_eq!(deck.primary.id, ActionId::Play);
        assert!(!deck.primary.enabled);
        assert_eq!(deck.primary.label, "Official launcher is open");
        assert_eq!(
            deck.secondary,
            [ActionView::enabled(
                ActionId::SafeStop,
                "Close official launcher"
            )]
        );
        assert!(!crate::presentation::official_launcher_available(&open));

        // Nothing else may run while it is open.
        for intent in [
            Intent::LaunchGame(GraphicsDriver::Standard, None, false, false),
            Intent::ApplyUpdate,
            Intent::ContinueInstallation,
            Intent::OpenOfficialLauncher,
        ] {
            harness.intents.send(intent).expect("blocked intent");
        }
        thread::sleep(Duration::from_millis(80));
        {
            let state = backend.state.lock().expect("state");
            assert_eq!(state.launch_calls, 0);
            assert_eq!(state.update_calls, 0);
            assert_eq!(state.install_calls, 0);
            assert_eq!(state.official_launcher_calls, 1);
        }
        let audits_before_close = backend
            .calls()
            .iter()
            .filter(|call| **call == "audit")
            .count();

        // Closing it re-audits and re-checks for an update.
        backend.official_launcher_gate.release();
        let closed = harness.wait_for(|snapshot| {
            snapshot.operation().is_some_and(|operation| {
                operation.phase() == OperationPhase::OfficialLauncher
                    && operation.status() == OperationStatusView::Succeeded
            })
        });
        assert_eq!(closed.installation(), InstallationStatus::Ready);
        assert!(crate::presentation::official_launcher_available(&closed));
        backend.wait_for(|state| {
            state.calls.iter().filter(|call| **call == "audit").count() > audits_before_close
                && state
                    .calls
                    .iter()
                    .filter(|call| **call == "check-update")
                    .count()
                    >= 2
        });
        harness
            .intents
            .send(Intent::LaunchGame(
                GraphicsDriver::Standard,
                None,
                false,
                false,
            ))
            .expect("play intent");
        backend.wait_for(|state| state.launch_calls == 1);
    }

    #[test]
    fn the_official_launcher_is_closed_by_a_safe_stop_and_never_opens_over_a_game() {
        let backend = Arc::new(FakeBackend::new(
            [audit(InstallationStatus::Ready)],
            UpdateStatus::UpToDate,
        ));
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.update() == UpdateStatus::UpToDate);

        harness
            .intents
            .send(Intent::LaunchGame(
                GraphicsDriver::Standard,
                None,
                false,
                false,
            ))
            .expect("play intent");
        harness.wait_for(|snapshot| snapshot.game() == GameView::Running);
        harness
            .intents
            .send(Intent::OpenOfficialLauncher)
            .expect("official launcher intent");
        thread::sleep(Duration::from_millis(80));
        assert_eq!(
            backend.state.lock().expect("state").official_launcher_calls,
            0
        );
        harness
            .intents
            .send(Intent::RequestSafeStop)
            .expect("stop game");
        harness.wait_for(|snapshot| snapshot.game() == GameView::Idle);

        harness
            .intents
            .send(Intent::OpenOfficialLauncher)
            .expect("official launcher intent");
        harness.wait_for(official_launcher_open);
        harness
            .intents
            .send(Intent::RequestSafeStop)
            .expect("close official launcher");
        let stopped = harness.wait_for(|snapshot| {
            snapshot.operation().is_some_and(|operation| {
                operation.phase() == OperationPhase::OfficialLauncher
                    && operation.status() == OperationStatusView::Cancelled
            })
        });
        assert!(backend.calls().contains(&"safe-stop"));
        assert_eq!(stopped.installation(), InstallationStatus::Ready);
    }

    #[test]
    fn a_stopped_official_launcher_checks_for_an_update_again() {
        let backend = Arc::new(FakeBackend::new(
            [audit(InstallationStatus::Ready)],
            UpdateStatus::Available,
        ));
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.update() == UpdateStatus::Available);
        harness
            .intents
            .send(Intent::OpenOfficialLauncher)
            .expect("official launcher intent");
        harness.wait_for(official_launcher_open);
        // It installed that update, and the stop accepted it.
        backend.state.lock().expect("state").update_status = UpdateStatus::UpToDate;
        harness
            .intents
            .send(Intent::RequestSafeStop)
            .expect("close official launcher");
        let checked = harness.wait_for(|snapshot| {
            snapshot.operation().is_some_and(|operation| {
                operation.phase() == OperationPhase::OfficialLauncher
                    && operation.status() == OperationStatusView::Cancelled
            }) && snapshot.update() == UpdateStatus::UpToDate
        });
        assert_eq!(checked.installation(), InstallationStatus::Ready);
        assert_eq!(
            backend
                .calls()
                .iter()
                .filter(|call| **call == "check-update")
                .count(),
            2
        );
    }

    #[test]
    fn game_and_update_are_mutually_exclusive_at_the_coordinator_guard() {
        let _terminal_game_states = [
            BackendGameState::Exited,
            BackendGameState::SupervisionFailed,
        ];
        let backend = Arc::new(
            FakeBackend::new([audit(InstallationStatus::Ready)], UpdateStatus::Available)
                .with_update_blocked(),
        );
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.update() == UpdateStatus::Available);

        harness
            .intents
            .send(Intent::LaunchGame(
                GraphicsDriver::Standard,
                None,
                false,
                false,
            ))
            .expect("play intent");
        harness.wait_for(|snapshot| snapshot.game() == GameView::Running);
        harness
            .intents
            .send(Intent::ApplyUpdate)
            .expect("update intent");
        thread::sleep(Duration::from_millis(80));
        assert_eq!(backend.state.lock().expect("state").update_calls, 0);

        harness
            .intents
            .send(Intent::RequestSafeStop)
            .expect("stop intent");
        harness.wait_for(|snapshot| snapshot.game() == GameView::Idle);
        harness
            .intents
            .send(Intent::ApplyUpdate)
            .expect("update intent");
        backend.wait_for(|state| state.update_calls == 1);
        harness
            .intents
            .send(Intent::LaunchGame(
                GraphicsDriver::Standard,
                None,
                false,
                false,
            ))
            .expect("play intent");
        thread::sleep(Duration::from_millis(80));
        assert_eq!(backend.state.lock().expect("state").launch_calls, 1);

        backend.update_gate.release();
        backend.wait_for(|state| {
            state
                .calls
                .iter()
                .filter(|call| **call == "check-update")
                .count()
                >= 2
        });
    }

    /// Prints the fake backend's calls, in order, when the test fails (for a failure that only
    /// CI shows, 2026-10-30).
    struct CallsOnPanic(Arc<FakeBackend>);

    impl Drop for CallsOnPanic {
        fn drop(&mut self) {
            if std::thread::panicking() {
                eprintln!("fake backend calls, in order: {:?}", self.0.calls());
            }
        }
    }

    fn authorize(harness: &Harness) {
        harness
            .intents
            .send(Intent::AuthorizeAndInstall(
                AuthorizationInput::new(
                    "AAAABBBBCCCCDDDD".to_owned(),
                    "EEEEFFFFGGGGHHHH".to_owned(),
                    true,
                )
                .unwrap(),
            ))
            .unwrap();
    }

    #[test]
    fn the_setup_record_is_written_as_the_setup_progresses() {
        // The install waits at its gate: an install that ends before the
        // service's next tick goes from no job to Ready, and the record from
        // none to installed with no started between.
        let backend = Arc::new(
            FakeBackend::new(
                [
                    audit(InstallationStatus::NeedsBaseGame),
                    audit(InstallationStatus::Ready),
                    audit(InstallationStatus::NeedsBaseGame),
                ],
                UpdateStatus::UpToDate,
            )
            .with_install_blocked(),
        );
        let _calls = CallsOnPanic(backend.clone());
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| {
            snapshot.installation() == InstallationStatus::NeedsBaseGame
                && snapshot.operation().is_none()
        });
        harness.select_installers();
        assert!(backend.state.lock().unwrap().saved_records.is_empty());

        // Started once an install step begins, installed once Play is
        // reachable, none again after Start over.
        authorize(&harness);
        harness.wait_for(|snapshot| snapshot.setup_record() == SetupRecord::Started);
        backend.install_gate.release();
        harness.wait_for(|snapshot| {
            snapshot.installation() == InstallationStatus::Ready
                && snapshot.setup_record() == SetupRecord::Installed
        });
        harness.intents.send(Intent::StartOver).unwrap();
        harness.wait_for(|snapshot| snapshot.setup_record() == SetupRecord::None);
        assert_eq!(
            backend.state.lock().unwrap().saved_records,
            [
                SetupRecord::Started,
                SetupRecord::Installed,
                SetupRecord::None
            ]
        );
    }

    #[test]
    fn a_broken_installed_game_keeps_its_record_and_continues_as_before() {
        let backend = Arc::new(FakeBackend::new(
            [audit(InstallationStatus::NeedsClient)],
            UpdateStatus::UpToDate,
        ));
        backend.state.lock().unwrap().setup_record = SetupRecord::Installed;
        let mut harness = Harness::start(backend.clone());
        let snapshot = harness.wait_for(|snapshot| {
            snapshot.installation() == InstallationStatus::NeedsClient
                && snapshot.operation().is_none()
        });
        // The start read the record, and the check sends the game to step 5.
        assert_eq!(snapshot.setup_record(), SetupRecord::Installed);
        assert_eq!(
            DeckPresentation::project(&snapshot, false).primary,
            ActionView::enabled(ActionId::InstallProjectDiablo, "Install Project Diablo 2")
        );
        harness.intents.send(Intent::InstallProjectDiablo).unwrap();
        finished(&mut harness, ActiveJobView::InstallProjectDiablo);
        assert!(backend.state.lock().unwrap().saved_records.is_empty());
    }

    #[test]
    fn open_game_folder_opens_the_installed_games_folder_and_says_how_it_went() {
        let backend = Arc::new(FakeBackend::new(
            [audit(InstallationStatus::NeedsBaseGame)],
            UpdateStatus::UpToDate,
        ));
        backend.state.lock().unwrap().location = Some(LocationView {
            path: PathBuf::from("/home/deck/.pd2launcher/game"),
            default: true,
            available: true,
            previous: None,
        });
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| {
            snapshot.installation() == InstallationStatus::NeedsBaseGame
                && snapshot.operation().is_none()
        });
        // Nothing installed: nothing to open.
        harness.intents.send(Intent::OpenGameFolder).unwrap();
        refused(&mut harness, "game-folder-not-installed");

        backend.state.lock().unwrap().latest_audit = audit(InstallationStatus::Ready);
        harness
            .intents
            .send(Intent::Navigate(Destination::Operations))
            .unwrap();
        harness.wait_for(|snapshot| snapshot.installation() == InstallationStatus::Ready);
        harness.intents.send(Intent::OpenGameFolder).unwrap();
        harness.wait_for(|snapshot| snapshot.game_files_open() == Some(FolderOpenView::Opened));
        assert_eq!(
            backend.state.lock().unwrap().opened_folders,
            [PathBuf::from(
                "/home/deck/.pd2launcher/game/prefix/drive_c/Program Files (x86)/Diablo II"
            )]
        );

        // A folder no file manager takes says so.
        backend.state.lock().unwrap().location = Some(LocationView {
            path: PathBuf::from("/unopenable/pd2-launcher"),
            default: false,
            available: true,
            previous: None,
        });
        harness.intents.send(Intent::CheckAgain).unwrap();
        harness.wait_for(|snapshot| {
            snapshot
                .location()
                .is_some_and(|location| location.path.starts_with("/unopenable"))
                && snapshot.operation().is_none()
        });
        harness.intents.send(Intent::OpenGameFolder).unwrap();
        harness.wait_for(|snapshot| snapshot.game_files_open() == Some(FolderOpenView::Failed));
    }

    #[test]
    fn characters_put_aside_are_told_on_play_until_dismissed_with_a_way_to_the_backup() {
        let interrupted = InstallationStatus::StageInterrupted(crate::InterruptedStage::BaseGame);
        let backend = Arc::new(FakeBackend::new(
            [audit(InstallationStatus::Ready)],
            UpdateStatus::UpToDate,
        ));
        let put_aside = |characters, files, others, folder: &str| SavesPutAsideView {
            characters,
            files,
            others,
            folder: PathBuf::from(folder),
            open: None,
        };
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.launch_preflight_passed());
        // The keep before Play put a character aside.
        backend.state.lock().unwrap().saves_put_aside = Some(put_aside(1, 0, 0, "/b/backup-1"));
        harness
            .intents
            .send(Intent::LaunchGame(
                GraphicsDriver::Standard,
                None,
                false,
                false,
            ))
            .unwrap();
        let running = harness.wait_for(|snapshot| snapshot.game() == GameView::Running);
        assert_eq!(
            running.saves_put_aside(),
            Some(&put_aside(1, 0, 0, "/b/backup-1"))
        );
        harness.intents.send(Intent::RequestSafeStop).unwrap();
        harness.wait_for(|snapshot| snapshot.game() == GameView::Idle);

        // A later keep (here Start over's) adds to what is told, in its
        // own backup.
        backend.state.lock().unwrap().latest_audit = audit(interrupted);
        harness.intents.send(Intent::CheckAgain).unwrap();
        harness.wait_for(|snapshot| snapshot.installation() == interrupted);
        backend.state.lock().unwrap().saves_put_aside = Some(put_aside(2, 2, 1, "/b/backup-2"));
        backend.state.lock().unwrap().latest_audit = audit(InstallationStatus::Ready);
        harness.intents.send(Intent::StartOver).unwrap();
        let told = harness.wait_for(|snapshot| {
            snapshot
                .saves_put_aside()
                .is_some_and(|notice| notice.characters == 3)
        });
        assert_eq!(
            told.saves_put_aside(),
            Some(&put_aside(3, 2, 1, "/b/backup-2"))
        );

        harness.intents.send(Intent::OpenSavesBackup).unwrap();
        harness.wait_for(|snapshot| {
            snapshot
                .saves_put_aside()
                .is_some_and(|notice| notice.open == Some(FolderOpenView::Opened))
        });
        assert_eq!(
            backend.state.lock().unwrap().opened_folders,
            [PathBuf::from("/b/backup-2")]
        );
        harness.intents.send(Intent::DismissSavesPutAside).unwrap();
        harness.wait_for(|snapshot| snapshot.saves_put_aside().is_none());
    }

    #[test]
    fn the_next_setup_record_follows_the_snapshot() {
        let supported = |installation| {
            UiSnapshot::initial()
                .with_host(HostView::Supported)
                .with_installation(installation)
                .with_operation(None)
        };
        let installing = supported(InstallationStatus::NeedsBaseGame)
            .with_active_job(Some(ActiveJobView::InstallBaseGame));
        let downloading = supported(InstallationStatus::NeedsBaseGame)
            .with_active_job(Some(ActiveJobView::Download));
        let reset = supported(InstallationStatus::NeedsBaseGame).with_operation(Some(
            OperationView::mutating(
                OperationPhase::Reset,
                OperationStatusView::Succeeded,
                "Operation complete",
            ),
        ));
        use SetupRecord::{Installed, Started};
        let none = SetupRecord::None;
        for (record, snapshot, next) in [
            (none, UiSnapshot::initial(), none),
            (Installed, UiSnapshot::initial(), Installed),
            (none, downloading, none),
            (none, installing.clone(), Started),
            (Installed, installing, Installed),
            (Started, supported(InstallationStatus::NeedsClient), Started),
            (Started, supported(InstallationStatus::Ready), Installed),
            (none, supported(InstallationStatus::Ready), Installed),
            (
                Installed,
                supported(InstallationStatus::RepairRequired),
                Installed,
            ),
            (Installed, reset, none),
        ] {
            assert_eq!(next_setup_record(record, &snapshot), next, "{snapshot:?}");
        }
    }

    fn refused(harness: &mut Harness, code: &'static str) {
        harness.wait_for(|snapshot| {
            snapshot
                .evidence()
                .iter()
                .any(|item| item.label == "Request not started" && item.detail == code)
        });
    }

    fn finished(harness: &mut Harness, job: ActiveJobView) -> UiSnapshot {
        harness.wait_for(|snapshot| {
            snapshot.active_job().is_none()
                && snapshot.operation().is_some_and(|operation| {
                    operation.job() == Some(job) && operation.status().is_terminal()
                })
        })
    }

    #[test]
    fn install_diablo_ii_waits_for_verified_downloads() {
        let backend = Arc::new(FakeBackend::new(
            [
                audit_without_downloads(InstallationStatus::NeedsBaseGame),
                audit(InstallationStatus::NeedsBaseGame),
                audit(InstallationStatus::NeedsClient),
            ],
            UpdateStatus::UpToDate,
        ));
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.installation() == InstallationStatus::NeedsBaseGame);
        harness.select_installers();
        // Keys can't start Install Diablo II before the downloads verified.
        authorize(&harness);
        refused(&mut harness, "downloads-required");
        assert_eq!(backend.state.lock().unwrap().install_calls, 0);

        // Step 3 runs on its own, without keys.
        harness.intents.send(Intent::DownloadAll).unwrap();
        let downloaded = finished(&mut harness, ActiveJobView::Download);
        assert!(downloaded.downloads().complete);
        assert_eq!(downloaded.authorization(), AuthorizationView::Absent);
        assert_eq!(backend.state.lock().unwrap().authorized_installs, 0);

        // Now step 4, with the keys.
        authorize(&harness);
        let installed = finished(&mut harness, ActiveJobView::InstallBaseGame);
        assert_eq!(installed.installation(), InstallationStatus::NeedsClient);
        assert_eq!(
            backend
                .calls()
                .into_iter()
                .filter(|call| call.starts_with("download") || call.starts_with("install"))
                .collect::<Vec<_>>(),
            ["download", "install-base-game"]
        );
        assert_eq!(backend.state.lock().unwrap().authorized_installs, 1);
    }

    fn setup_calls(backend: &FakeBackend) -> Vec<&'static str> {
        backend
            .calls()
            .into_iter()
            .filter(|call| {
                call.starts_with("download")
                    || call.starts_with("install")
                    || call.starts_with("discard")
            })
            .collect()
    }

    #[test]
    fn keys_given_during_the_download_start_install_diablo_ii_as_one_flow() {
        let backend = Arc::new(
            FakeBackend::new(
                [
                    audit_without_downloads(InstallationStatus::NeedsBaseGame),
                    audit(InstallationStatus::NeedsBaseGame),
                    audit(InstallationStatus::NeedsClient),
                ],
                UpdateStatus::UpToDate,
            )
            .with_install_blocked(),
        );
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.installation() == InstallationStatus::NeedsBaseGame);
        harness.select_installers();
        harness.intents.send(Intent::DownloadAll).unwrap();
        backend.wait_for(|state| state.calls.contains(&"download"));
        // Agree and install while the small downloads run: the keys wait.
        authorize(&harness);
        harness.wait_for(|snapshot| snapshot.authorization() == AuthorizationView::Authorized);
        assert_eq!(setup_calls(&backend), ["download"]);
        backend.install_gate.release();
        let installed = finished(&mut harness, ActiveJobView::InstallBaseGame);
        assert_eq!(installed.installation(), InstallationStatus::NeedsClient);
        assert_eq!(installed.authorization(), AuthorizationView::Absent);
        // One Download, then Install Diablo II with the keys: no second
        // download.
        assert_eq!(setup_calls(&backend), ["download", "install-base-game"]);
        assert_eq!(backend.state.lock().unwrap().authorized_installs, 1);
    }

    #[test]
    fn keys_refused_during_the_download_leave_it_running() {
        let backend = Arc::new(
            FakeBackend::new(
                [
                    audit_without_downloads(InstallationStatus::NeedsBaseGame),
                    audit(InstallationStatus::NeedsBaseGame),
                ],
                UpdateStatus::UpToDate,
            )
            .with_install_blocked(),
        );
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.installation() == InstallationStatus::NeedsBaseGame);
        // An existing installation chosen: keys have no install to wait
        // for. (A key the installers refuse never gets here: the Control
        // channel refuses it with its reason first.)
        harness
            .intents
            .send(Intent::SelectExistingInstall(PathBuf::from("/tmp/d2")))
            .unwrap();
        harness.intents.send(Intent::DownloadAll).unwrap();
        backend.wait_for(|state| state.calls.contains(&"download"));
        authorize(&harness);
        assert_eq!(harness.next_refusal(), "install-not-permitted");
        backend.install_gate.release();
        let downloaded = finished(&mut harness, ActiveJobView::Download);
        assert_eq!(
            downloaded.operation().map(OperationView::status),
            Some(OperationStatusView::Succeeded)
        );
        assert_eq!(downloaded.authorization(), AuthorizationView::Absent);
        assert_eq!(setup_calls(&backend), ["download"]);
    }

    #[test]
    fn waiting_keys_stay_for_a_try_again_of_a_failed_download() {
        let backend = Arc::new(
            FakeBackend::new(
                [
                    audit_without_downloads(InstallationStatus::NeedsBaseGame),
                    audit_without_downloads(InstallationStatus::NeedsBaseGame),
                    audit(InstallationStatus::NeedsBaseGame),
                    audit(InstallationStatus::NeedsClient),
                ],
                UpdateStatus::UpToDate,
            )
            .with_install_blocked()
            .with_install_failure(BackendFailure::new("download-failed")),
        );
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.installation() == InstallationStatus::NeedsBaseGame);
        harness.select_installers();
        harness.intents.send(Intent::DownloadAll).unwrap();
        backend.wait_for(|state| state.calls.contains(&"download"));
        authorize(&harness);
        harness.wait_for(|snapshot| snapshot.authorization() == AuthorizationView::Authorized);
        backend.install_gate.release();
        let failed = finished(&mut harness, ActiveJobView::Download);
        assert_eq!(
            failed.operation().and_then(OperationView::failure),
            Some("download-failed")
        );
        // The failure is reported; the keys wait for a Try again.
        assert_eq!(failed.authorization(), AuthorizationView::Authorized);
        backend.state.lock().unwrap().install_failure = None;
        harness.intents.send(Intent::DownloadAll).unwrap();
        let installed = finished(&mut harness, ActiveJobView::InstallBaseGame);
        assert_eq!(installed.installation(), InstallationStatus::NeedsClient);
        assert_eq!(
            setup_calls(&backend),
            ["download", "download", "install-base-game"]
        );
        assert_eq!(backend.state.lock().unwrap().authorized_installs, 1);
    }

    #[test]
    fn a_discard_stops_the_download_then_removes_the_downloads() {
        let backend = Arc::new(
            FakeBackend::new(
                [audit_without_downloads(InstallationStatus::NeedsBaseGame)],
                UpdateStatus::UpToDate,
            )
            .with_install_blocked(),
        );
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.installation() == InstallationStatus::NeedsBaseGame);
        harness.select_installers();
        harness.intents.send(Intent::DownloadAll).unwrap();
        backend.wait_for(|state| state.calls.contains(&"download"));
        authorize(&harness);
        harness.wait_for(|snapshot| snapshot.authorization() == AuthorizationView::Authorized);
        harness.intents.send(Intent::DiscardDownloads).unwrap();
        let removed = finished(&mut harness, ActiveJobView::DiscardDownloads);
        assert_eq!(
            removed.operation().map(OperationView::status),
            Some(OperationStatusView::Succeeded)
        );
        // Stopped first, then removed; the keys went with it.
        let calls = backend.calls();
        let stop = calls.iter().position(|call| *call == "safe-stop").unwrap();
        let discard = calls
            .iter()
            .position(|call| *call == "discard-downloads")
            .unwrap();
        assert!(stop < discard, "{calls:?}");
        assert_eq!(removed.authorization(), AuthorizationView::Absent);
        assert_eq!(setup_calls(&backend), ["download", "discard-downloads"]);

        // Idle, it removes at once; during an install it is refused.
        harness.intents.send(Intent::DiscardDownloads).unwrap();
        backend.wait_for(|state| {
            state
                .calls
                .iter()
                .filter(|call| **call == "discard-downloads")
                .count()
                == 2
        });
    }

    #[test]
    fn a_discard_is_refused_while_install_diablo_ii_runs() {
        let backend = Arc::new(
            FakeBackend::new(
                [audit(InstallationStatus::NeedsBaseGame)],
                UpdateStatus::UpToDate,
            )
            .with_install_blocked(),
        );
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.installation() == InstallationStatus::NeedsBaseGame);
        harness.select_installers();
        authorize(&harness);
        backend.wait_for(|state| state.calls.contains(&"install-base-game"));
        harness.intents.send(Intent::DiscardDownloads).unwrap();
        assert_eq!(harness.next_refusal(), "discard-downloads-not-permitted");
        backend.install_gate.release();
        finished(&mut harness, ActiveJobView::InstallBaseGame);
        assert!(!backend.calls().contains(&"discard-downloads"));
    }

    /// A Download that fails with keys waiting, as the Try again tests
    /// start from.
    fn failed_download_with_waiting_keys(
        audits: impl IntoIterator<Item = BackendAudit>,
    ) -> (Arc<FakeBackend>, Harness) {
        let backend = Arc::new(
            FakeBackend::new(audits, UpdateStatus::UpToDate)
                .with_install_blocked()
                .with_install_failure(BackendFailure::new("download-failed")),
        );
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.installation() == InstallationStatus::NeedsBaseGame);
        harness.select_installers();
        harness.intents.send(Intent::DownloadAll).unwrap();
        backend.wait_for(|state| state.calls.contains(&"download"));
        authorize(&harness);
        harness.wait_for(|snapshot| snapshot.authorization() == AuthorizationView::Authorized);
        backend.install_gate.release();
        finished(&mut harness, ActiveJobView::Download);
        backend.state.lock().unwrap().install_failure = None;
        (backend, harness)
    }

    #[test]
    fn keys_the_channel_refuses_during_the_download_drop_the_waiting_ones() {
        let backend = Arc::new(
            FakeBackend::new(
                [
                    audit_without_downloads(InstallationStatus::NeedsBaseGame),
                    audit(InstallationStatus::NeedsBaseGame),
                ],
                UpdateStatus::UpToDate,
            )
            .with_install_blocked(),
        );
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.installation() == InstallationStatus::NeedsBaseGame);
        harness.select_installers();
        harness.intents.send(Intent::DownloadAll).unwrap();
        backend.wait_for(|state| state.calls.contains(&"download"));
        authorize(&harness);
        harness.wait_for(|snapshot| snapshot.authorization() == AuthorizationView::Authorized);
        // A second authorize with a bad key: the Control channel refuses it
        // and tells the service to forget the waiting keys.
        harness.intents.send(Intent::ForgetWaitingKeys).unwrap();
        harness.wait_for(|snapshot| snapshot.authorization() == AuthorizationView::Absent);
        backend.install_gate.release();
        let downloaded = finished(&mut harness, ActiveJobView::Download);
        assert_eq!(downloaded.authorization(), AuthorizationView::Absent);
        assert_eq!(setup_calls(&backend), ["download"]);
    }

    #[test]
    fn new_keys_after_a_failed_download_wait_for_its_try_again() {
        let (backend, mut harness) = failed_download_with_waiting_keys([
            audit_without_downloads(InstallationStatus::NeedsBaseGame),
            audit_without_downloads(InstallationStatus::NeedsBaseGame),
            audit(InstallationStatus::NeedsBaseGame),
            audit(InstallationStatus::NeedsClient),
        ]);
        // Keys typed again: refused for now (no downloads), but they wait.
        authorize(&harness);
        assert_eq!(harness.next_refusal(), "downloads-required");
        assert_eq!(
            harness.reducer.snapshot().authorization(),
            AuthorizationView::Authorized
        );
        harness.intents.send(Intent::DownloadAll).unwrap();
        let installed = finished(&mut harness, ActiveJobView::InstallBaseGame);
        assert_eq!(installed.installation(), InstallationStatus::NeedsClient);
        assert_eq!(
            setup_calls(&backend),
            ["download", "download", "install-base-game"]
        );
        assert_eq!(backend.state.lock().unwrap().authorized_installs, 1);
    }

    #[test]
    fn a_refused_authorize_with_no_failed_download_leaves_no_keys() {
        let backend = Arc::new(FakeBackend::new(
            [audit_without_downloads(InstallationStatus::NeedsBaseGame)],
            UpdateStatus::UpToDate,
        ));
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.installation() == InstallationStatus::NeedsBaseGame);
        harness.select_installers();
        authorize(&harness);
        assert_eq!(harness.next_refusal(), "downloads-required");
        assert_eq!(
            harness.reducer.snapshot().authorization(),
            AuthorizationView::Absent
        );
    }

    #[test]
    fn a_new_installer_choice_or_another_job_drops_the_waiting_keys() {
        let (backend, mut harness) = failed_download_with_waiting_keys([
            audit_without_downloads(InstallationStatus::NeedsBaseGame),
            audit_without_downloads(InstallationStatus::NeedsBaseGame),
            audit(InstallationStatus::NeedsBaseGame),
        ]);
        assert_eq!(
            harness.reducer.snapshot().authorization(),
            AuthorizationView::Authorized
        );
        // Another choice of installers: the keys go.
        harness
            .intents
            .send(Intent::SelectInstallerMedia(PathBuf::from(
                "/tmp/other-installers",
            )))
            .unwrap();
        harness.wait_for(|snapshot| snapshot.authorization() == AuthorizationView::Absent);
        harness.intents.send(Intent::DownloadAll).unwrap();
        backend.wait_for(|state| {
            state
                .calls
                .iter()
                .filter(|call| **call == "download")
                .count()
                == 2
        });
        harness.wait_for(|snapshot| {
            snapshot.operation().is_some_and(|operation| {
                operation.job() == Some(ActiveJobView::Download)
                    && operation.status() == OperationStatusView::Succeeded
            })
        });
        assert_eq!(setup_calls(&backend), ["download", "download"]);

        // Another job (here the removal of the downloads) drops them too.
        let (backend, mut harness) = failed_download_with_waiting_keys([
            audit_without_downloads(InstallationStatus::NeedsBaseGame),
            audit_without_downloads(InstallationStatus::NeedsBaseGame),
        ]);
        harness.intents.send(Intent::DiscardDownloads).unwrap();
        let removed = finished(&mut harness, ActiveJobView::DiscardDownloads);
        assert_eq!(removed.authorization(), AuthorizationView::Absent);
        assert_eq!(setup_calls(&backend), ["download", "discard-downloads"]);
    }

    #[test]
    fn a_stop_at_the_very_end_of_the_download_starts_no_install() {
        let backend = Arc::new(
            FakeBackend::new(
                [
                    audit_without_downloads(InstallationStatus::NeedsBaseGame),
                    audit(InstallationStatus::NeedsBaseGame),
                    audit(InstallationStatus::NeedsBaseGame),
                    audit(InstallationStatus::NeedsClient),
                ],
                UpdateStatus::UpToDate,
            )
            .with_install_blocked(),
        );
        backend.state.lock().unwrap().stop_too_late = true;
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.installation() == InstallationStatus::NeedsBaseGame);
        harness.select_installers();
        harness.intents.send(Intent::DownloadAll).unwrap();
        backend.wait_for(|state| state.calls.contains(&"download"));
        authorize(&harness);
        harness.wait_for(|snapshot| snapshot.authorization() == AuthorizationView::Authorized);
        harness.intents.send(Intent::RequestSafeStop).unwrap();
        backend.wait_for(|state| state.calls.contains(&"safe-stop"));
        // The Download ends well all the same: no install, the keys wait.
        backend.install_gate.release();
        let downloaded = finished(&mut harness, ActiveJobView::Download);
        assert_eq!(
            downloaded.operation().map(OperationView::status),
            Some(OperationStatusView::Succeeded)
        );
        assert_eq!(downloaded.authorization(), AuthorizationView::Authorized);
        assert_eq!(setup_calls(&backend), ["download"]);
        // The snapshot shows the stop, so the app waits for no install.
        let stopped = harness.wait_for(|snapshot| snapshot.stop_result().is_some());
        assert_eq!(stopped.authorization(), AuthorizationView::Authorized);
        // A Download asked again installs with them.
        backend.state.lock().unwrap().stop_too_late = false;
        harness.intents.send(Intent::DownloadAll).unwrap();
        finished(&mut harness, ActiveJobView::InstallBaseGame);
        assert_eq!(
            setup_calls(&backend),
            ["download", "download", "install-base-game"]
        );
    }

    #[test]
    fn install_project_diablo_waits_for_diablo_ii() {
        let backend = Arc::new(FakeBackend::new(
            [
                audit(InstallationStatus::NeedsExpansion),
                audit(InstallationStatus::NeedsClient),
                audit(InstallationStatus::Ready),
            ],
            UpdateStatus::UpToDate,
        ));
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.installation() == InstallationStatus::NeedsExpansion);
        harness.intents.send(Intent::InstallProjectDiablo).unwrap();
        refused(&mut harness, "install-not-permitted");
        assert_eq!(backend.state.lock().unwrap().install_calls, 0);

        // Once Diablo II is installed (a fresh audit says so), it runs.
        harness
            .intents
            .send(Intent::Navigate(Destination::Operations))
            .unwrap();
        harness.wait_for(|snapshot| snapshot.installation() == InstallationStatus::NeedsClient);
        harness.intents.send(Intent::InstallProjectDiablo).unwrap();
        let ready = finished(&mut harness, ActiveJobView::InstallProjectDiablo);
        assert_eq!(ready.installation(), InstallationStatus::Ready);
        assert!(backend.calls().contains(&"install-project-diablo"));
        // A new installation warns about its first launch before Play.
        harness.wait_for(|snapshot| snapshot.game_notice().is_some());
    }

    #[test]
    fn continue_runs_the_step_the_installation_stands_at() {
        for (first, call) in [
            (
                audit_without_downloads(InstallationStatus::NeedsExpansion),
                "download",
            ),
            (
                audit(InstallationStatus::NeedsClient),
                "install-project-diablo",
            ),
            (
                audit(InstallationStatus::NeedsRuntime),
                "install-project-diablo",
            ),
        ] {
            let backend = Arc::new(FakeBackend::new([first], UpdateStatus::UpToDate));
            let mut harness = Harness::start(backend.clone());
            harness.wait_for(|snapshot| snapshot.host() == HostView::Supported);
            harness.select_installers();
            harness.intents.send(Intent::ContinueInstallation).unwrap();
            backend.wait_for(|state| state.install_calls == 1);
            assert!(backend.calls().contains(&call), "{call}");
            // Continue never takes keys.
            assert_eq!(backend.state.lock().unwrap().authorized_installs, 0);
        }
    }

    #[test]
    fn an_existing_installation_is_imported_without_keys() {
        let existing_downloads = audit_without_downloads(InstallationStatus::NeedsBaseGame)
            .with_downloads(DownloadsView {
                complete: true,
                installers: false,
                total_bytes: 1,
            });
        let backend = Arc::new(FakeBackend::new(
            [existing_downloads, audit(InstallationStatus::NeedsClient)],
            UpdateStatus::UpToDate,
        ));
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.installation() == InstallationStatus::NeedsBaseGame);
        // A folder without Lord of Destruction is shown with its reason and
        // blocks.
        harness
            .intents
            .send(Intent::SelectExistingInstall(PathBuf::from(
                "/games/classic",
            )))
            .unwrap();
        let classic = harness.wait_for(|snapshot| snapshot.installer_media().existing().is_some());
        assert!(!classic.installer_media().is_ready());
        assert!(!classic.downloads_ready());
        harness
            .intents
            .send(Intent::ImportExistingInstall(ImportModeView::Copy))
            .unwrap();
        refused(&mut harness, "import-not-permitted");

        harness
            .intents
            .send(Intent::SelectExistingInstall(PathBuf::from(
                "/games/Diablo II",
            )))
            .unwrap();
        let chosen = harness.wait_for(|snapshot| snapshot.installer_media().is_ready());
        // Downloads made for an existing installation serve it.
        assert!(chosen.downloads_ready());
        // Keys are not what imports it.
        authorize(&harness);
        refused(&mut harness, "install-not-permitted");
        harness
            .intents
            .send(Intent::ImportExistingInstall(ImportModeView::InPlace))
            .unwrap();
        let imported = finished(&mut harness, ActiveJobView::InstallBaseGame);
        assert_eq!(imported.installation(), InstallationStatus::NeedsClient);
        let state = backend.state.lock().unwrap();
        assert_eq!(state.imports, [ImportModeView::InPlace]);
        assert_eq!(state.authorized_installs, 0);
    }

    #[test]
    fn a_failed_install_stays_visible_as_a_result_card_until_dismissed() {
        let backend = Arc::new(
            FakeBackend::new(
                [
                    audit(InstallationStatus::NeedsBaseGame),
                    audit(InstallationStatus::NeedsBaseGame),
                ],
                UpdateStatus::UpToDate,
            )
            .with_install_failure(BackendFailure::new("base-game-key-rejected")),
        );
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.installation() == InstallationStatus::NeedsBaseGame);
        harness.select_installers();
        authorize(&harness);
        let failed = harness.wait_for(|snapshot| {
            snapshot
                .operation()
                .is_some_and(|operation| operation.status() == OperationStatusView::Failed)
        });
        assert_eq!(
            failed.operation().and_then(OperationView::failure),
            Some("base-game-key-rejected")
        );
        let card = crate::presentation::result_card(&failed).expect("result card");
        assert_eq!(card.title, "Diablo II's installer rejected the CD key");
        assert_eq!(
            card.message,
            "Diablo II's installer rejected the CD key. Check the Diablo II CD key and try again. Your downloads are kept."
        );
        // The keys are wanted again: the form is back, nothing needs Start over.
        assert_eq!(failed.installation(), InstallationStatus::NeedsBaseGame);
        assert_eq!(failed.authorization(), AuthorizationView::Absent);

        // It survives unrelated updates such as the Overview's fresh audit...
        harness
            .intents
            .send(Intent::Navigate(Destination::Operations))
            .unwrap();
        let before = harness.reducer.snapshot().last_event_sequence();
        let audited = harness.wait_for(|snapshot| snapshot.last_event_sequence() > before);
        assert!(crate::presentation::result_card(&audited).is_some());

        // ...until the user dismisses it.
        harness.intents.send(Intent::DismissResult).unwrap();
        let dismissed = harness.wait_for(|snapshot| snapshot.operation().is_none());
        assert_eq!(crate::presentation::result_card(&dismissed), None);
    }

    #[test]
    fn new_work_replaces_the_result_card() {
        let backend = Arc::new(
            FakeBackend::new(
                [
                    audit(InstallationStatus::NeedsBaseGame),
                    audit(InstallationStatus::NeedsBaseGame),
                    audit(InstallationStatus::NeedsBaseGame),
                ],
                UpdateStatus::UpToDate,
            )
            .with_install_failure(BackendFailure::new("expansion-install-failed")),
        );
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.installation() == InstallationStatus::NeedsBaseGame);
        harness.select_installers();
        authorize(&harness);
        let failed = harness.wait_for(|snapshot| {
            crate::presentation::result_card(snapshot).is_some_and(|card| {
                card.message
                    .starts_with("Installation stopped while installing Lord of Destruction.")
            })
        });
        assert!(failed.operation().is_some());
        backend.state.lock().unwrap().install_failure = None;
        authorize(&harness);
        harness.wait_for(|snapshot| {
            snapshot
                .operation()
                .is_some_and(|operation| !operation.status().is_terminal())
                || snapshot
                    .operation()
                    .is_some_and(|operation| operation.status() == OperationStatusView::Succeeded)
        });
        assert_eq!(
            crate::presentation::result_card(harness.reducer.snapshot()),
            None
        );
    }

    #[test]
    fn a_failed_play_says_what_to_try_next() {
        let backend = Arc::new(FakeBackend::new(
            [audit(InstallationStatus::Ready)],
            UpdateStatus::UpToDate,
        ));
        backend.state.lock().unwrap().launch_failure =
            Some(BackendFailure::new("game-launch-failed"));
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.launch_preflight_passed());
        harness
            .intents
            .send(Intent::LaunchGame(
                GraphicsDriver::Standard,
                None,
                false,
                false,
            ))
            .unwrap();
        let failed = harness.wait_for(|snapshot| {
            snapshot
                .operation()
                .is_some_and(|operation| operation.status() == OperationStatusView::Failed)
        });
        let card = crate::presentation::result_card(&failed).expect("result card");
        assert_eq!(card.title, "The game didn't start");
        assert_eq!(
            card.message,
            "The game didn't start. Try Play again; if it keeps failing, switch Graphics to DirectDraw in Settings or save a report from Diagnostics."
        );
        assert_eq!(failed.game(), GameView::Idle);
        // Play stays available for the retry.
        assert!(
            DeckPresentation::project(&failed, false)
                .available_actions()
                .contains(&ActionId::Play)
        );
    }

    #[test]
    fn a_rejected_play_preflight_is_reported_the_same_way() {
        let mut not_ready = audit(InstallationStatus::Ready);
        not_ready.launch_preflight = LaunchPreflight::Failed;
        let backend = Arc::new(FakeBackend::new(
            [audit(InstallationStatus::Ready), not_ready],
            UpdateStatus::UpToDate,
        ));
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.launch_preflight_passed());
        harness
            .intents
            .send(Intent::LaunchGame(
                GraphicsDriver::Standard,
                None,
                false,
                false,
            ))
            .unwrap();
        let failed = harness.wait_for(|snapshot| {
            snapshot
                .operation()
                .and_then(OperationView::failure)
                .is_some_and(|code| code == "play-preflight-rejected")
        });
        assert_eq!(
            crate::presentation::result_card(&failed).map(|card| card.title),
            Some("The game didn't start")
        );
        assert_eq!(backend.state.lock().unwrap().launch_calls, 0);
    }

    #[test]
    fn check_again_re_audits_a_computer_that_was_missing_prerequisites() {
        let mut unsupported = audit(InstallationStatus::NeedsBaseGame);
        unsupported.host = HostView::Unsupported;
        unsupported.system.missing = vec![crate::presentation::Prerequisite::VulkanDriver];
        let backend = Arc::new(FakeBackend::new(
            [unsupported, audit(InstallationStatus::NeedsBaseGame)],
            UpdateStatus::UpToDate,
        ));
        let mut harness = Harness::start(backend.clone());
        let missing = harness.wait_for(|snapshot| {
            snapshot.host() == HostView::Unsupported && !snapshot.system().missing.is_empty()
        });
        let deck = DeckPresentation::project(&missing, false);
        assert_eq!(
            deck.primary,
            ActionView::enabled(ActionId::CheckAgain, "Check again")
        );
        assert_eq!(
            deck.description,
            "Your computer is missing a few things the game needs:"
        );
        assert_eq!(
            missing.system().missing,
            [crate::presentation::Prerequisite::VulkanDriver]
        );

        harness.intents.send(Intent::CheckAgain).unwrap();
        harness.wait_for(|snapshot| snapshot.host() == HostView::Supported);
    }

    #[test]
    fn a_launcher_service_failure_is_not_a_missing_package() {
        let backend = Arc::new(FakeBackend::new(
            [audit(InstallationStatus::NeedsBaseGame)],
            UpdateStatus::UpToDate,
        ));
        backend.state.lock().unwrap().audit_failure =
            Some(BackendFailure::new("diagnostics-unavailable"));
        let mut harness = Harness::start(backend.clone());
        let failed =
            harness.wait_for(|snapshot| crate::presentation::service_failure(snapshot).is_some());
        assert_eq!(
            crate::presentation::service_failure(&failed),
            Some("diagnostics-unavailable")
        );
        let deck = DeckPresentation::project(&failed, false);
        assert_eq!(
            deck.primary,
            ActionView::enabled(ActionId::CheckAgain, "Try again")
        );
        assert_eq!(deck.title, "The launcher couldn't start");
        // No result card or prerequisite list competes with it.
        assert_eq!(crate::presentation::result_card(&failed), None);

        // Retry runs the startup checks again.
        harness.intents.send(Intent::CheckAgain).unwrap();
        let recovered = harness.wait_for(|snapshot| snapshot.host() == HostView::Supported);
        assert_eq!(crate::presentation::service_failure(&recovered), None);
    }

    #[test]
    fn a_successful_install_offers_the_first_launch_tip_before_play() {
        let backend = Arc::new(FakeBackend::new(
            [
                audit(InstallationStatus::NeedsBaseGame),
                audit(InstallationStatus::Ready),
            ],
            UpdateStatus::UpToDate,
        ));
        let mut harness = Harness::start(backend.clone());
        harness.wait_for(|snapshot| snapshot.installation() == InstallationStatus::NeedsBaseGame);
        harness.select_installers();
        authorize(&harness);
        let ready = harness.wait_for(|snapshot| {
            snapshot.installation() == InstallationStatus::Ready && snapshot.game_notice().is_some()
        });
        assert_eq!(ready.game(), GameView::Idle);
        assert!(
            DeckPresentation::project(&ready, false)
                .game_notice
                .is_some()
        );
    }

    #[test]
    fn a_ready_game_never_launched_warns_about_the_first_launch_at_startup() {
        let backend = Arc::new(FakeBackend::new(
            [audit(InstallationStatus::Ready).with_first_launch_pending(true)],
            UpdateStatus::UpToDate,
        ));
        let mut harness = Harness::start(backend);
        let ready = harness.wait_for(|snapshot| {
            snapshot.installation() == InstallationStatus::Ready && snapshot.game_notice().is_some()
        });
        assert_eq!(ready.game_notice(), Some(GameNoticeView::FirstD2glLaunch));

        // Once D2GL wrote its configuration, a fresh start is quiet.
        let backend = Arc::new(FakeBackend::new(
            [audit(InstallationStatus::Ready)],
            UpdateStatus::UpToDate,
        ));
        let mut harness = Harness::start(backend);
        let ready = harness.wait_for(|snapshot| {
            snapshot.installation() == InstallationStatus::Ready
                && snapshot.update() == UpdateStatus::UpToDate
        });
        assert_eq!(ready.game_notice(), None);
    }

    #[test]
    fn download_speed_is_smoothed_and_restarts_with_each_phase() {
        let mut rate = ByteRate::default();
        let start = Instant::now();
        let sample = |rate: &mut ByteRate, phase, completed, after| {
            rate.sample(phase, completed, None, start + after)
        };
        assert_eq!(
            sample(&mut rate, WorkPhase::BaseGameInstall, 0, Duration::ZERO),
            None
        );
        // Samples closer than a second apart keep the estimate.
        assert_eq!(
            sample(
                &mut rate,
                WorkPhase::BaseGameInstall,
                500_000,
                Duration::from_millis(500)
            ),
            None
        );
        assert_eq!(
            sample(
                &mut rate,
                WorkPhase::BaseGameInstall,
                2_000_000,
                Duration::from_secs(2)
            ),
            Some(1_000_000)
        );
        assert_eq!(
            sample(
                &mut rate,
                WorkPhase::BaseGameInstall,
                6_000_000,
                Duration::from_secs(4)
            ),
            Some(1_300_000)
        );
        // A new phase (or a restarted download) starts over.
        assert_eq!(
            sample(
                &mut rate,
                WorkPhase::ExpansionInstall,
                10,
                Duration::from_secs(5)
            ),
            None
        );
    }

    /// An update's small file, then, 16 s later (its check and the entries
    /// already in place), a big one that downloads at 23.4 MB/s.
    #[test]
    fn a_new_file_has_no_speed_until_its_own_bytes_are_measured() {
        let mut rate = ByteRate::default();
        let start = Instant::now();
        let mut sample = |completed, total, after| {
            rate.sample(
                WorkPhase::ClientDownload,
                completed,
                Some(total),
                start + after,
            )
        };
        assert_eq!(sample(64_000, 100_000, Duration::ZERO), None);
        assert_eq!(sample(100_000, 100_000, Duration::from_millis(200)), None);
        // The time since the small file is not the big one's: no speed of
        // 27 KB/s and no "about 88 minutes left".
        assert_eq!(sample(500_000, 143_900_000, Duration::from_secs(16)), None);
        assert_eq!(
            sample(18_400_000, 143_900_000, Duration::from_millis(16_800)),
            None
        );
        // A second of its own bytes gives its own speed.
        assert_eq!(
            sample(23_900_000, 143_900_000, Duration::from_secs(17)),
            Some(23_400_000)
        );
    }

    /// The running update's operation after the updater reports 2 of 10 MB
    /// of one file, file 35 of the manifest's 46, two seconds after it
    /// started that file.
    fn update_operation(android_host: bool) -> OperationView {
        let (events, _received) = sync_channel(64);
        let mut coordinator = Coordinator::new(events);
        coordinator.android_host = android_host;
        let (publisher, progress) = progress_channel();
        let cancellation = OperationControl::with_progress(publisher);
        coordinator.active_mutation = Some(ActiveMutation {
            kind: MutationKind::Update,
            cancellation: cancellation.clone(),
            progress,
        });
        coordinator.byte_rate = ByteRate {
            file: Some((WorkPhase::ClientDownload, Some(10_000_000))),
            last: Instant::now()
                .checked_sub(Duration::from_secs(2))
                .map(|at| (at, 0)),
            rate: None,
        };
        cancellation.report(
            crate::operation::WorkProgress::for_phase(WorkPhase::ClientDownload)
                .with_file_bytes(2_000_000, Some(10_000_000))
                .with_overall(crate::operation::ProgressUnit::Files, 35, Some(46))
                .with_verification(35, Some(46)),
        );
        coordinator.poll_progress();
        coordinator
            .snapshot
            .operation()
            .cloned()
            .expect("the update's operation")
    }

    #[test]
    fn on_android_an_update_shows_the_file_it_downloads_with_its_time_left() {
        let android = update_operation(true);
        assert_eq!(android.job(), Some(ActiveJobView::Update));
        assert_eq!(android.phase(), OperationPhase::ClientDownload);
        assert_eq!(android.progress_unit(), Some("bytes"));
        assert_eq!(android.progress(), Some((2_000_000, 10_000_000)));
        assert_eq!(android.progress_percent(), Some(20));
        let summary = android.progress_summary().expect("summary");
        assert!(summary.starts_with("2.0 MB of 10.0 MB · "), "{summary}");
        // 8 MB left at 1 MB/s: under a minute, never "about less than a minute".
        assert!(
            summary.ends_with("/s · less than a minute left for this file"),
            "{summary}"
        );
        assert!(
            !summary.contains("files") && !summary.contains("checked"),
            "{summary}"
        );

        // The desktop keeps the manifest's file counts.
        let desktop = update_operation(false);
        assert_eq!(desktop.progress_unit(), Some("files"));
        assert_eq!(desktop.progress(), Some((35, 46)));
        let summary = desktop.progress_summary().expect("summary");
        assert!(
            summary.ends_with(" · 35 of 46 files · 35 of 46 checked"),
            "{summary}"
        );
        assert!(!summary.contains("left"), "{summary}");
    }

    /// A big file's first bytes are not measured against the last sample of
    /// the small file before it, 16 s earlier: that gives a caption such as
    /// "18.4 of 143.9 MB · 26.8 KB/s · about 79 minutes left".
    #[test]
    fn on_android_a_new_file_shows_no_speed_from_the_file_before() {
        let (events, _received) = sync_channel(64);
        let mut coordinator = Coordinator::new(events);
        coordinator.android_host = true;
        let (publisher, progress) = progress_channel();
        let cancellation = OperationControl::with_progress(publisher);
        coordinator.active_mutation = Some(ActiveMutation {
            kind: MutationKind::Update,
            cancellation: cancellation.clone(),
            progress,
        });
        coordinator.byte_rate = ByteRate {
            file: Some((WorkPhase::ClientDownload, Some(100_000))),
            last: Instant::now()
                .checked_sub(Duration::from_secs(16))
                .map(|at| (at, 64_000)),
            rate: None,
        };
        cancellation.report(
            crate::operation::WorkProgress::for_phase(WorkPhase::ClientDownload)
                .with_file_bytes(500_000, Some(143_900_000))
                .with_overall(crate::operation::ProgressUnit::Files, 30, Some(46))
                .with_verification(30, Some(46)),
        );
        coordinator.poll_progress();
        let operation = coordinator.snapshot.operation().expect("operation");
        assert_eq!(operation.progress(), Some((500_000, 143_900_000)));
        assert_eq!(operation.progress_summary(), Some("500.0 KB of 143.9 MB"));
    }

    #[test]
    fn on_android_an_update_without_a_byte_total_shows_no_figure() {
        let (events, _received) = sync_channel(64);
        let mut coordinator = Coordinator::new(events);
        coordinator.android_host = true;
        let (publisher, progress) = progress_channel();
        let cancellation = OperationControl::with_progress(publisher);
        coordinator.active_mutation = Some(ActiveMutation {
            kind: MutationKind::Update,
            cancellation: cancellation.clone(),
            progress,
        });
        // The promotion counts files only.
        cancellation.report(crate::operation::WorkProgress::bounded(
            WorkPhase::Promotion,
            crate::operation::ProgressUnit::Files,
            0,
            1,
        ));
        coordinator.poll_progress();
        let operation = coordinator.snapshot.operation().expect("operation");
        assert_eq!(operation.phase(), OperationPhase::Promotion);
        assert_eq!(operation.status(), OperationStatusView::Committing);
        assert_eq!(operation.progress_percent(), None);
        assert_eq!(operation.progress_summary(), None);
    }

    #[test]
    fn the_time_left_says_less_than_a_minute_without_about() {
        assert_eq!(time_left(0, false), "less than a minute left");
        assert_eq!(time_left(59, false), "less than a minute left");
        assert_eq!(time_left(60, false), "about 1 minute left");
        assert_eq!(time_left(150, false), "about 3 minutes left");
        assert_eq!(time_left(30, true), "less than a minute left for this file");
        assert_eq!(time_left(150, true), "about 3 minutes left for this file");
    }

    /// An update of two changed files, entries 3 and 31 of the manifest's
    /// 46, as the updater reports it: each file's bytes, its check, then a
    /// count for every entry already in place.
    #[test]
    fn on_android_an_update_of_two_files_keeps_the_last_figure_between_them() {
        fn run(android_host: bool) -> Vec<OperationView> {
            use crate::operation::{ProgressUnit::Files, WorkProgress};
            let (events, _received) = sync_channel(64);
            let mut coordinator = Coordinator::new(events);
            coordinator.android_host = android_host;
            let (publisher, progress) = progress_channel();
            let cancellation = OperationControl::with_progress(publisher);
            coordinator.active_mutation = Some(ActiveMutation {
                kind: MutationKind::Update,
                cancellation: cancellation.clone(),
                progress,
            });
            coordinator.byte_rate = ByteRate {
                file: Some((WorkPhase::ClientDownload, Some(10_000_000))),
                last: Instant::now()
                    .checked_sub(Duration::from_secs(2))
                    .map(|at| (at, 0)),
                rate: None,
            };
            let downloading = |done, total, entry| {
                WorkProgress::for_phase(WorkPhase::ClientDownload)
                    .with_file_bytes(done, Some(total))
                    .with_overall(Files, entry, Some(46))
                    .with_verification(entry, Some(46))
            };
            let counted = |entry| {
                WorkProgress::for_phase(WorkPhase::ClientDownload)
                    .with_overall(Files, entry, Some(46))
                    .with_verification(entry, Some(46))
            };
            let reports = [
                WorkProgress::for_phase(WorkPhase::ClientManifest),
                downloading(4_000_000, 10_000_000, 2),
                downloading(10_000_000, 10_000_000, 2),
                WorkProgress::for_phase(WorkPhase::Hashing)
                    .with_file_bytes(10_000_000, None)
                    .with_overall(Files, 2, Some(46))
                    .with_verification(3, Some(46)),
                counted(3),
                counted(30),
                downloading(1_000_000, 50_000_000, 30),
                WorkProgress::bounded(WorkPhase::Promotion, Files, 0, 46),
            ];
            reports
                .into_iter()
                .map(|report| {
                    cancellation.report(report);
                    coordinator.poll_progress();
                    coordinator
                        .snapshot
                        .operation()
                        .cloned()
                        .expect("operation")
                })
                .collect()
        }

        let android = run(true);
        // The manifest: nothing to show yet.
        assert_eq!(android[0].progress_percent(), None);
        assert_eq!(android[0].progress_summary(), None);
        // The first file, by its bytes, and done.
        assert_eq!(android[1].progress_percent(), Some(40));
        assert_eq!(android[2].progress(), Some((10_000_000, 10_000_000)));
        let first = android[2].progress_summary().expect("summary").to_owned();
        assert_eq!(
            first,
            "10.0 MB of 10.0 MB · 2.0 MB/s · less than a minute left for this file"
        );
        // Its check and the entries in place keep its figure and summary.
        for kept in &android[3..6] {
            assert_eq!(kept.progress(), Some((10_000_000, 10_000_000)));
            assert_eq!(kept.progress_unit(), Some("bytes"));
            assert_eq!(kept.progress_summary(), Some(first.as_str()));
        }
        assert_eq!(android[3].phase(), OperationPhase::Verification);
        // The second file replaces them from its first bytes.
        assert_eq!(android[6].progress(), Some((1_000_000, 50_000_000)));
        assert_eq!(android[6].progress_percent(), Some(2));
        assert_eq!(android[6].progress_summary(), Some("1.0 MB of 50.0 MB"));
        // The swap has no figure of its own.
        assert_eq!(android[7].phase(), OperationPhase::Promotion);
        assert_eq!(android[7].progress_percent(), None);
        assert_eq!(android[7].progress_summary(), None);

        // The desktop counts the manifest's files between them.
        let desktop = run(false);
        assert_eq!(desktop[4].progress_unit(), Some("files"));
        assert_eq!(desktop[4].progress(), Some((3, 46)));
        assert_eq!(
            desktop[4].progress_summary(),
            Some("3 of 46 files · 3 of 46 checked")
        );
    }
}
