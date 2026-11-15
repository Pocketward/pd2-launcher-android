//! Concrete services owned by the process composition root.

use std::{
    collections::BTreeMap,
    env,
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use crate::{
    InstallationStatus, InterruptedStage, LaunchPreflight, UpdateStatus,
    audit::{
        AcceptedClientManifest, ActivityEvidence, AuditCheck, AuditLease, AuditLeaseGuard,
        AuditRequest, AuditSources, Auditor, CheckStatus, CustomComponent, FindingReason,
        HostPlatform, HostSupport, InstallationReport, LauncherArtifactEvidence,
        OfficialLauncherTrust, ProtectedInventoryStatus, WineLibrary, record_official_launcher,
    },
    coordinator::{
        BackendAudit, BackendFailure, BackendGameState, CancellationToken, CoordinatorBackend,
    },
    diagnostics::{DiagnosticCode, DiagnosticStore, JobId, JobKind, JobRecord, JobStatus},
    downloads::{DownloadNeed, DownloadPlan, DownloadReadiness},
    game_folder::{
        ExistingInstall, ExistingInstallProblem, GameFolder, GameFolderError, ImportMode,
    },
    host_execution::{HostExecution, SystemHostExecution, SystemHostProbe},
    install::{
        AcquiredArtifact, AdapterFailure, ArtifactCatalog, ArtifactSource, ArtifactSpec,
        ClientSynchronizer, InstallAudit, InstallError, InstallationAuditor,
        InstallationAuthorization, InstallerMedia, ManagedInstaller, ManagedPaths, PrefixNeeds,
        ProductionArtifactSource, ProductionInstallDriver, UpdaterClientSynchronizer,
    },
    location::{POINTER_FILE, SelectionLock},
    manifest::{ManifestResponse, PD2_MANIFEST_URL, Pd2ClientBase, Pd2Manifest},
    network::SystemPd2Source,
    operation::WorkPhase,
    persistence::{
        InstallSourceSetting, LauncherPaths, LayoutError, RendererSetting, SettingsStore,
        SetupRecord,
    },
    presentation::{
        DirectDrawRendererView, DistroFamily, DownloadSourceView, DownloadsView, EvidenceTone,
        EvidenceView, ExistingInstallView, ExistingProblemView, FexSettingView, FexSourceView,
        GameFolderView, GameNoticeView, GraphicsDriver, GraphicsProfile, HostView, ImportModeView,
        InstallSourceView, InstallerMediaView, LaunchSettingsView, LocationView,
        PREFIX_EVIDENCE_LABEL, Prerequisite, RendererPreferenceView, SavesPutAsideView,
        SettingsChange, SourceBadgeView, SystemContractView,
    },
    process::{
        D2glSetup, GameAudit, GameAuditFailure, GameAuditor, GameLaunchOptions, GameRenderer,
        GameState, GameSupervisor,
    },
    relocation::RelocationError,
    safe_fs::Directory,
    sources::{
        DownloadArtifact, DownloadSources, SourceOrigin, is_builtin_url, normalize_custom_url,
        redacted_host,
    },
    startup_credentials::{LaunchMode, download_source_environment},
    transaction::{
        CleanupLease, NoFaults, PruneAuditEvidence, TransactionManager, UpdateLayout,
        UpdateTransactionProbe,
    },
    update::{
        GameActivity, InstalledClientPd2Source, ManifestOnlyPd2Source, PrefetchedPd2Source,
        ProtectedInventoryEvidence, UpdateError, VerifiedUpdater,
    },
};

/// Why `fex` can't be saved as the device FEX: it must be the full path of
/// an executable file, whoever owns it.
fn fex_path_problem(fex: &Path) -> Option<&'static str> {
    use std::os::unix::fs::PermissionsExt;
    if !fex.is_absolute() {
        Some("Enter the full path of the FEX program.")
    } else if !std::fs::metadata(fex)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    {
        Some("There's no executable file there.")
    } else {
        None
    }
}

/// Whether this build can download and use its own FEX and RootFS
/// (Settings → FEX → Built-in FEX): ARM64 builds.
const BUILTIN_FEX_AVAILABLE: bool = cfg!(target_arch = "aarch64");

const ACCEPTED_MANIFEST_LIMIT: u64 = 1024 * 1024;
/// Saved reports kept in `logs/reports`.
const REPORTS_KEPT: usize = 10;
const PROJECT_D2_RELATIVE: &str = "prefix/drive_c/Program Files (x86)/Diablo II/ProjectD2";

/// [`PROJECT_D2_RELATIVE`] or, on the Android host, the ProjectD2 of the
/// game folder outside the prefix.
fn project_d2_relative() -> PathBuf {
    if crate::android_host() {
        Path::new(crate::game_folder::ANDROID_GAME_RELATIVE).join("ProjectD2")
    } else {
        PathBuf::from(PROJECT_D2_RELATIVE)
    }
}
pub(super) const AARCH64_BINARY_IDENTITY: &str = "native-aarch64-binary";
pub(super) const X86_64_BINARY_IDENTITY: &str = "native-x86_64-binary";
const COMPILED_ARTIFACT_IDENTITY: Option<&str> = option_env!("PD2_LAUNCHER_ARTIFACT_IDENTITY");

/// Fail-closed production backend. Construction never panics: if local paths
/// or approved network clients cannot initialize, the coordinator receives a
/// stable unavailable result and renders that immutable state.
pub(super) struct ProductionBackend {
    ready: Option<ReadyBackend>,
}

impl ProductionBackend {
    pub(super) fn new(
        host_probe: SystemHostProbe,
        host_execution: SystemHostExecution,
        mode: LaunchMode,
        paths: Option<LauncherPaths>,
    ) -> Self {
        Self {
            ready: ReadyBackend::new(host_probe, host_execution, mode, paths).ok(),
        }
    }

    fn ready(&self) -> Result<&ReadyBackend, BackendFailure> {
        self.ready
            .as_ref()
            .ok_or_else(|| BackendFailure::new("production-services-unavailable"))
    }
}

impl CoordinatorBackend for ProductionBackend {
    fn open_managed_layout(&self) -> Result<(), BackendFailure> {
        self.ready()?.open_managed_layout()
    }

    fn audit(&self, activity: ActivityEvidence) -> Result<BackendAudit, BackendFailure> {
        self.ready()?.audit(activity)
    }

    fn repair(&self, cancellation: &CancellationToken) -> Result<(), BackendFailure> {
        self.ready()?.repair(cancellation)
    }

    fn check_for_update(&self) -> Result<UpdateStatus, BackendFailure> {
        self.ready()?.check_for_update()
    }

    fn select_installer_media(&self, path: PathBuf) -> Result<InstallerMediaView, BackendFailure> {
        self.ready()?.select_installer_media(path)
    }

    fn select_installer_download(&self) -> Result<InstallerMediaView, BackendFailure> {
        self.ready()?.select_installer_download()
    }

    fn install_location(&self) -> Option<LocationView> {
        self.ready().ok().and_then(ReadyBackend::install_location)
    }

    fn download_sources(&self) -> Result<Vec<DownloadSourceView>, BackendFailure> {
        self.ready()?.download_sources()
    }

    fn update_download_source(
        &self,
        artifact: DownloadArtifact,
        url: Option<String>,
    ) -> Result<Vec<DownloadSourceView>, BackendFailure> {
        self.ready()?.update_download_source(artifact, url)
    }

    fn select_existing_install(&self, path: PathBuf) -> Result<InstallerMediaView, BackendFailure> {
        self.ready()?.select_existing_install(path)
    }

    fn fex_setting(&self) -> Option<FexSettingView> {
        self.ready().ok().and_then(ReadyBackend::fex_setting)
    }

    fn save_fex(&self, fex: Option<PathBuf>) -> Result<FexSettingView, BackendFailure> {
        self.ready()?.save_fex(fex)
    }

    fn choose_fex_source(&self, source: FexSourceView) -> Result<FexSettingView, BackendFailure> {
        self.ready()?.choose_fex_source(source)
    }

    fn restored_installer_choice(&self) -> Option<InstallerMediaView> {
        self.ready()
            .ok()
            .and_then(ReadyBackend::restored_installer_choice)
    }

    fn game_folder(&self) -> Option<GameFolderView> {
        self.ready().ok().and_then(ReadyBackend::game_folder)
    }

    fn download_all(&self, cancellation: &CancellationToken) -> Result<(), BackendFailure> {
        self.ready()?.download_all(cancellation)
    }

    fn discard_downloads(&self, cancellation: &CancellationToken) -> Result<(), BackendFailure> {
        self.ready()?.discard_downloads(cancellation)
    }

    fn install_base_game(
        &self,
        authorization: Option<InstallationAuthorization>,
        import: Option<ImportModeView>,
        cancellation: &CancellationToken,
    ) -> Result<(), BackendFailure> {
        self.ready()?
            .install_base_game(authorization, import, cancellation)
    }

    fn install_project_diablo(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<(), BackendFailure> {
        self.ready()?.install_project_diablo(cancellation)
    }

    fn apply_update(&self, cancellation: &CancellationToken) -> Result<(), BackendFailure> {
        self.ready()?.apply_update(cancellation)
    }

    fn open_official_launcher(
        &self,
        confirmed: bool,
        strict_memory_order: bool,
        detailed_logging: bool,
        cancellation: &CancellationToken,
    ) -> Result<(), BackendFailure> {
        self.ready()?.open_official_launcher(
            confirmed,
            strict_memory_order,
            detailed_logging,
            cancellation,
        )
    }

    fn request_mutation_safe_stop(&self) -> Result<(), BackendFailure> {
        self.ready()?.request_mutation_safe_stop()
    }

    fn reset_installation(&self, cancellation: &CancellationToken) -> Result<(), BackendFailure> {
        self.ready()?.reset_installation(cancellation)
    }

    fn relocate(
        &self,
        parent: PathBuf,
        fresh_only: bool,
        cancellation: &CancellationToken,
    ) -> Result<(), BackendFailure> {
        self.ready()?.relocate(parent, fresh_only, cancellation)
    }

    fn remove_previous_installation(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<(), BackendFailure> {
        self.ready()?.remove_previous_installation(cancellation)
    }

    fn launch_game(
        &self,
        driver: GraphicsDriver,
        profile: Option<GraphicsProfile>,
        strict_memory_order: bool,
        detailed_logging: bool,
    ) -> Result<Option<GameNoticeView>, BackendFailure> {
        self.ready()?
            .launch_game(driver, profile, strict_memory_order, detailed_logging)
    }

    fn poll_game(&self) -> BackendGameState {
        self.ready()
            .map_or(BackendGameState::SupervisionFailed, ReadyBackend::poll_game)
    }

    fn request_game_stop(&self) -> Result<(), BackendFailure> {
        self.ready()?.request_game_stop()
    }

    fn load_settings(&self) -> Result<LaunchSettingsView, BackendFailure> {
        self.ready()?.load_settings()
    }

    fn update_settings(
        &self,
        change: SettingsChange,
    ) -> Result<LaunchSettingsView, BackendFailure> {
        self.ready()?.update_settings(change)
    }

    fn diagnostics_evidence(&self) -> Result<Vec<EvidenceView>, BackendFailure> {
        self.ready()?.diagnostics_evidence()
    }

    fn save_report(&self) -> Result<PathBuf, BackendFailure> {
        self.ready()?.save_report()
    }

    fn reveal_report(&self, path: &Path) -> bool {
        crate::report::reveal(path)
    }

    fn open_game_folder(&self, folder: &Path) -> bool {
        // The saved report's way: its file manager shows Game.exe, so the
        // folder that opens is the game folder itself.
        crate::report::reveal(&folder.join("Game.exe"))
    }

    fn take_saves_put_aside(&self) -> Option<SavesPutAsideView> {
        self.ready()
            .ok()
            .and_then(ReadyBackend::take_saves_put_aside)
    }

    fn open_backups(&self, folder: &Path) -> bool {
        // The backups folder opens with this backup selected in it.
        crate::report::reveal(folder)
    }

    fn setup_record(&self) -> SetupRecord {
        self.ready().map_or(SetupRecord::None, |ready| {
            crate::persistence::load_setup_record(&ready.paths.setup_record_file())
        })
    }

    fn save_setup_record(&self, record: SetupRecord) -> Result<(), BackendFailure> {
        crate::persistence::save_setup_record(&self.ready()?.paths.setup_record_file(), record)
            .map_err(|_| backend_failure("launcher-state-unavailable"))
    }
}

/// Session-only installer media choice, or a Diablo II the user already
/// has, of any version.
#[derive(Clone)]
enum InstallerChoice {
    Folder(InstallerMedia),
    BlizzardDownload,
    Existing(ExistingInstall),
}

impl InstallerChoice {
    const fn plan(&self) -> DownloadPlan {
        match self {
            Self::Existing(_) => DownloadPlan::ExistingInstall,
            Self::Folder(_) | Self::BlizzardDownload => DownloadPlan::Installers,
        }
    }
}

/// The existing-installation folder the last Download was made for, so a
/// restart opens at Install Diablo II with it still chosen.
const EXISTING_CHOICE_FILE: &str = "existing-install.json";

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct ExistingChoiceRecord {
    schema: u32,
    path: String,
}

struct ReadyBackend {
    paths: LauncherPaths,
    /// The FEX guest programs run with since start-up (ARM64).
    fex_in_use: Option<PathBuf>,
    auditor: Arc<ProductionAuditor>,
    installer_media: Mutex<Option<InstallerChoice>>,
    install_driver: ProductionInstallDriver,
    /// `PD2_URL_*` overrides from the environment (and `.env.local` in
    /// unattended mode), read once.
    source_environment: BTreeMap<DownloadArtifact, String>,
    updater_source: Mutex<SystemPd2Source>,
    settings: SettingsStore,
    diagnostics: Arc<Mutex<Option<DiagnosticStore>>>,
    pending_job: Mutex<Option<(JobId, JobStatus)>>,
    game: Mutex<GameSupervisor>,
    transaction_sequence: AtomicU64,
    /// The root-selection lock, held shared while the root is open.
    selection: Mutex<Option<SelectionLock>>,
    /// The resolved location, or the unavailable one.
    location: Mutex<Option<LocationView>>,
    /// A game folder used in place, resolved with the root.
    game_folder: Mutex<Option<GameFolderView>>,
    /// What keeping the characters put in a backup since the coordinator
    /// last took it, for the player to be told.
    put_aside: Mutex<crate::save_keeper::PutAside>,
}

/// Add the Launcher to the desktop's app menu, or take it out (`app_menu`);
/// a failure is only reported.
#[cfg(feature = "gui")]
fn sync_app_menu(enabled: bool) {
    match crate::app_menu::sync(&crate::app_menu::Environment::current(), enabled) {
        Ok(crate::app_menu::Outcome::Written) => {
            eprintln!("pd2-launcher: added the launcher to the app menu");
        }
        Ok(crate::app_menu::Outcome::Skipped(skip)) if enabled => {
            eprintln!(
                "pd2-launcher: not added to the app menu: {}",
                skip.describe()
            );
        }
        Ok(_) => {}
        Err(error) => eprintln!("pd2-launcher: the app menu entry could not be updated: {error}"),
    }
}

/// On the Android host: Wine Gecko and Wine Mono that an older release put
/// in the prefix are removed (`install::remove_wine_gecko_and_mono`), only
/// while this Launcher holds the installation lease exclusively: no setup
/// step, official launcher session or game of any Launcher runs. (The app's
/// Wine keeps its wineserver socket in the prefix's `.wineserver` folder, so
/// the host's /tmp shows none; the lease is the guard.)
fn remove_android_wine_gecko_and_mono(managed_root: &Path) {
    let Ok(root) = Directory::open(managed_root) else {
        return;
    };
    let Ok(_lease) = InstallationMutationLease::acquire(&root) else {
        return;
    };
    let Ok(prefix) = root.open_directory(Path::new("prefix")) else {
        return;
    };
    match crate::install::remove_wine_gecko_and_mono(&prefix) {
        Ok(true) => eprintln!("pd2-launcher: removed Wine Gecko and Wine Mono from the prefix"),
        Ok(false) => {}
        Err(error) => {
            eprintln!(
                "pd2-launcher: Wine Gecko and Wine Mono not removed from the prefix: {error}"
            );
        }
    }
}

/// Proton-based Wine (Wine-GE among them) saves the registry through a
/// temporary file of a fixed name that it refuses to replace, so one left
/// by a Wine that was killed stops every later save ("Failed flushing
/// registry"). It is removed only while this Launcher holds the
/// installation exclusively (no setup step or game of any Launcher runs)
/// and no wineserver of the prefix answers: a game left running by a
/// Launcher that stopped keeps its server, and its save, untouched.
/// On Android the installation lease is the guard: the wineserver check
/// cannot see the server there.
fn remove_stale_registry_save(managed_root: &Path) {
    let Ok(root) = Directory::open(managed_root) else {
        return;
    };
    let Ok(_lease) = InstallationMutationLease::acquire(&root) else {
        return;
    };
    if wineserver_answers(&managed_root.join("prefix")) {
        return;
    }
    let Ok(prefix) = root.open_directory(Path::new("prefix")) else {
        return;
    };
    let name = std::ffi::OsStr::new("savereg.tmp");
    if let Ok(stale) = prefix.open_file(Path::new(name))
        && prefix.remove_open_file_bounded(name, &stale).is_ok()
    {
        eprintln!("pd2-launcher: removed a registry save left by a stopped Wine");
    }
}

/// Whether a wineserver serves `prefix`: Wine keeps its socket in
/// `/tmp/.wine-<uid>/server-<device>-<inode>/socket`.
fn wineserver_answers(prefix: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    let Ok(metadata) = std::fs::metadata(prefix) else {
        return false;
    };
    let socket = PathBuf::from(format!(
        "/tmp/.wine-{}/server-{:x}-{:x}/socket",
        rustix::process::getuid().as_raw(),
        metadata.dev(),
        metadata.ino()
    ));
    std::os::unix::net::UnixStream::connect(socket).is_ok()
}

/// Whether the player chose the built-in FEX and it is not yet
/// installed in `managed_root`: the Download step installs it.
fn builtin_fex_pending(settings: &SettingsStore, managed_root: &Path) -> bool {
    crate::builtin_fex::in_use(settings.load().ok().as_ref(), managed_root)
        && !crate::builtin_fex::installed(managed_root)
}

/// Save the step 1 default as the player's choice once downloads begin: the
/// managed root no longer looks fresh after them.
fn keep_default_fex_choice(settings: &SettingsStore, managed_root: &Path) {
    let Ok(saved) = settings.load() else {
        return;
    };
    if saved.fex_source_chosen() || !cfg!(target_arch = "aarch64") {
        return;
    }
    let source = if crate::builtin_fex::in_use(Some(&saved), managed_root) {
        crate::persistence::FexSource::Builtin
    } else {
        crate::persistence::FexSource::Device
    };
    let _ = settings.save(&saved.with_fex_source(source));
}

impl ReadyBackend {
    /// The saved official-launcher software rendering setting; an unreadable
    /// settings file keeps the default (on).
    fn official_launcher_software_rendering(&self) -> bool {
        self.settings.load().map_or(true, |settings| {
            settings.official_launcher_software_rendering()
        })
    }

    fn new(
        host_probe: SystemHostProbe,
        host_execution: SystemHostExecution,
        mode: LaunchMode,
        paths: Option<LauncherPaths>,
    ) -> Result<Self, BackendFailure> {
        let paths = match paths {
            Some(paths) => paths,
            None => LauncherPaths::discover().map_err(|_| backend_failure("paths-unavailable"))?,
        };
        let settings = SettingsStore::new(paths.settings_file());
        remove_stale_registry_save(&paths.managed_root());
        if crate::android_host() {
            remove_android_wine_gecko_and_mono(&paths.managed_root());
        }
        // The last Play's graphics driver, for every Wine process of this
        // engine from the first one on: the official launcher's game too,
        // before any Play.
        if host_probe.android_host().is_some() && compatibility_driver_saved(paths.config_root()) {
            crate::wine_environment::use_compatibility_driver(true);
        }
        #[cfg(feature = "gui")]
        if mode == LaunchMode::Interactive {
            sync_app_menu(
                settings
                    .load()
                    .map_or(true, |settings| settings.app_menu_entry()),
            );
        }
        let source_environment = download_source_environment(mode);
        let auditor = Arc::new(ProductionAuditor::new(
            paths.clone(),
            host_probe,
            settings.clone(),
            source_environment.clone(),
        ));
        let fex_in_use = host_execution.interpreter();
        let host: Arc<dyn HostExecution> = Arc::new(host_execution);
        let game = GameSupervisor::new(auditor.clone(), host.clone());
        Ok(Self {
            settings,
            paths,
            fex_in_use,
            auditor,
            installer_media: Mutex::new(None),
            install_driver: ProductionInstallDriver::new(host),
            source_environment,
            updater_source: Mutex::new(
                SystemPd2Source::new().map_err(|_| backend_failure("update-source-unavailable"))?,
            ),
            diagnostics: Arc::new(Mutex::new(None)),
            pending_job: Mutex::new(None),
            game: Mutex::new(game),
            transaction_sequence: AtomicU64::new(1),
            selection: Mutex::new(None),
            location: Mutex::new(None),
            game_folder: Mutex::new(None),
            put_aside: Mutex::new(crate::save_keeper::PutAside::default()),
        })
    }

    fn open_managed_layout(&self) -> Result<(), BackendFailure> {
        // A retry re-resolves from scratch; the previous hold is released
        // first so this Launcher's own shared lock never blocks itself.
        *self
            .selection
            .lock()
            .map_err(|_| backend_failure("managed-layout-unavailable"))? = None;
        let opened = self.paths.open_managed_layout();
        let (location, result) = match opened {
            Ok(selection) => {
                *self
                    .selection
                    .lock()
                    .map_err(|_| backend_failure("managed-layout-unavailable"))? = Some(selection);
                (
                    Some(LocationView {
                        path: self.paths.managed_root(),
                        default: self.paths.uses_default_root(),
                        available: true,
                        previous: crate::relocation::previous_root(&self.paths),
                    }),
                    Ok(()),
                )
            }
            Err(LayoutError::LocationUnavailable(path)) => (
                Some(LocationView {
                    path: path.unwrap_or_else(|| self.paths.config_root().join(POINTER_FILE)),
                    default: false,
                    available: false,
                    previous: None,
                }),
                Err(backend_failure("install-location-unavailable")),
            ),
            Err(LayoutError::SelectionBusy) => {
                (None, Err(backend_failure("install-location-busy")))
            }
            Err(LayoutError::Io) => (None, Err(backend_failure("managed-layout-unavailable"))),
        };
        *self
            .location
            .lock()
            .map_err(|_| backend_failure("managed-layout-unavailable"))? = location;
        // A game folder used in place must be the recorded one, or nothing
        // runs until it is back.
        let (game_folder, result) = match result {
            Ok(()) => match Directory::open(&self.paths.managed_root())
                .map_err(|_| GameFolderError::RecordInvalid)
                .and_then(|root| GameFolder::resolve(&root))
            {
                Ok(GameFolder::External(external)) => (
                    Some(GameFolderView {
                        path: external.path().to_owned(),
                        available: true,
                    }),
                    Ok(()),
                ),
                Ok(GameFolder::Managed) => (None, Ok(())),
                Err(GameFolderError::Unavailable(path)) => (
                    Some(GameFolderView {
                        path,
                        available: false,
                    }),
                    Err(backend_failure("game-folder-unavailable")),
                ),
                Err(GameFolderError::RecordInvalid) => (
                    Some(GameFolderView {
                        path: self
                            .paths
                            .managed_root()
                            .join("prefix")
                            .join(crate::game_folder::POINTER_NAME),
                        available: false,
                    }),
                    Err(backend_failure("game-folder-unavailable")),
                ),
            },
            Err(error) => (None, Err(error)),
        };
        *self
            .game_folder
            .lock()
            .map_err(|_| backend_failure("managed-layout-unavailable"))? = game_folder;
        // Diagnostics live in the state directory, not the managed root, so
        // they open even while the location is unavailable (a new location
        // is chosen as a recorded job). They open once per process: a
        // re-resolution after a location change keeps the store, and with
        // it the owner lease of the job that changed the location.
        if matches!(
            result.as_ref().err().map(|error| error.code()),
            None | Some("install-location-unavailable" | "game-folder-unavailable")
        ) {
            let mut diagnostics = self
                .diagnostics
                .lock()
                .map_err(|_| backend_failure("diagnostics-unavailable"))?;
            if diagnostics.is_none() {
                crate::activity::configure(self.paths.diagnostics_root());
                let mut store = DiagnosticStore::open(
                    self.paths.diagnostics_root().join("recent.json"),
                    self.paths.exports_root(),
                )
                .map_err(|_| backend_failure("diagnostics-unavailable"))?;
                store
                    .recover_interrupted()
                    .map_err(|_| backend_failure("diagnostics-unavailable"))?;
                *diagnostics = Some(store);
            }
            if result.is_ok() {
                diagnostics
                    .as_mut()
                    .expect("diagnostics were just opened")
                    .record_event(DiagnosticCode::LayoutOpened)
                    .map_err(|_| backend_failure("diagnostics-unavailable"))?;
            }
        }
        result
    }

    fn install_location(&self) -> Option<LocationView> {
        self.location
            .lock()
            .ok()
            .and_then(|location| location.clone())
    }

    fn game_folder(&self) -> Option<GameFolderView> {
        self.game_folder
            .lock()
            .ok()
            .and_then(|folder| folder.clone())
    }

    fn audit(&self, activity: ActivityEvidence) -> Result<BackendAudit, BackendFailure> {
        let root = Directory::open(&self.paths.managed_root())
            .map_err(|_| backend_failure("audit-lease-unavailable"))?;
        // Keep the same installation mutation lease from the evidence scan
        // through terminal pruning. A cooperating launcher therefore cannot
        // begin journal publication after audit reports Clear but before the
        // retained transaction evidence is normalized and pruned.
        let mutation = match InstallationMutationLease::acquire(&root) {
            Ok(mutation) => mutation,
            // Wine left from a dead session keeps the lease forever; stop it
            // and try once more. A live game or another Launcher stays busy.
            // The holders may also exit by themselves while it checks, so
            // the lease is tried again either way.
            Err(_) => {
                use crate::orphaned_wine::LeaseRelease;
                match crate::orphaned_wine::release_orphaned_wine(&self.paths.managed_root()) {
                    Ok(LeaseRelease::Stopped(_)) => {
                        let _ = self.record(DiagnosticCode::OrphanedWineStopped);
                    }
                    Ok(LeaseRelease::LiveSession) => {
                        return Err(backend_failure("game-still-running"));
                    }
                    Ok(LeaseRelease::OtherProgram) => {
                        return Err(backend_failure("installation-in-use"));
                    }
                    Ok(LeaseRelease::Free) | Err(_) => {}
                }
                InstallationMutationLease::acquire(&root)
                    .map_err(|_| backend_failure("audit-lease-unavailable"))?
            }
        };
        let lease = HeldAuditLease(&mutation);
        let report = self
            .auditor
            .audit_report_for_play(activity, &lease, &root)?;
        self.record(
            if report.installation_status() == InstallationStatus::Ready {
                DiagnosticCode::AuditReady
            } else {
                DiagnosticCode::AuditCompleted
            },
        )?;
        if report.check(AuditCheck::Transaction) == CheckStatus::Satisfied {
            let cleanup = CleanupLease::acquire(&self.paths.cleanup_lock())
                .map_err(|_| backend_failure("transaction-cleanup-failed"))?;
            TransactionManager::new(&self.update_layout(&root)?, &NoFaults)
                .prune_terminal(PruneAuditEvidence::FreshAndClear, &cleanup)
                .map_err(|_| backend_failure("transaction-cleanup-failed"))?;
        }
        self.finish_pending_job()?;
        let first_launch_pending = report.installation_status() == InstallationStatus::Ready
            && crate::process::first_d2gl_launch_pending(&root);
        let game_language = (report.installation_status() == InstallationStatus::Ready)
            .then(|| crate::game_language::GameLanguage::of_installation(&root))
            .flatten();
        let downloads = downloads_view(crate::downloads::readiness(&root).for_need(
            crate::downloads::DownloadNeed::for_status(
                report.installation_status(),
                report.check(AuditCheck::Prefix) != CheckStatus::Satisfied,
            ),
        ));
        // The renderer probe takes the game's own leases, so it runs only
        // after this audit's exclusive lease is released.
        drop(mutation);
        let game_software_rendering = if report.installation_status() == InstallationStatus::Ready
            && report.launch_preflight() == LaunchPreflight::Passed
        {
            self.game_renders_in_software(report.installed_runtime())
        } else {
            Some(false)
        };
        Ok(BackendAudit::new(
            host_view(report.host_support()),
            report.installation_status(),
            report.launch_preflight(),
            system_contract(&report),
            report_evidence(&report),
        )
        .with_official_launcher_unverified(
            matches!(
                report.official_launcher(),
                OfficialLauncherTrust::Unrecorded | OfficialLauncherTrust::Changed
            ) || self.settings.load().is_ok_and(|settings| {
                settings.launcher_program() != crate::persistence::DEFAULT_LAUNCHER_PROGRAM
            }),
        )
        .with_first_launch_pending(first_launch_pending)
        .with_downloads(downloads)
        .with_game_software_rendering(game_software_rendering)
        .with_game_language(game_language)
        .with_environment_ready(
            report.check(AuditCheck::Runtime) == CheckStatus::Satisfied
                && report.check(AuditCheck::Prefix) == CheckStatus::Satisfied,
        ))
    }

    /// Settings → FEX, on ARM64 builds only, and never on an Android host
    /// (its FEX is the app's).
    fn fex_setting(&self) -> Option<FexSettingView> {
        if self.auditor.android_host() {
            return None;
        }
        let settings = self.settings.load().ok();
        let root = self.paths.managed_root();
        let builtin_installed = crate::builtin_fex::installed(&root);
        cfg!(target_arch = "aarch64").then(|| FexSettingView {
            source: if crate::builtin_fex::in_use(settings.as_ref(), &root) {
                FexSourceView::Builtin
            } else {
                FexSourceView::Device
            },
            builtin_available: BUILTIN_FEX_AVAILABLE,
            builtin_installed,
            builtin_download_bytes: crate::builtin_fex::FexVariant::current().package_size()
                + crate::builtin_fex::rootfs().size(),
            // While the built-in FEX runs, it is the one in use.
            in_use: if builtin_installed && crate::builtin_fex::in_use(settings.as_ref(), &root) {
                Some(crate::builtin_fex::Layout::new(&root).interpreter())
            } else {
                self.fex_in_use.clone()
            },
            saved: settings
                .as_ref()
                .and_then(|settings| settings.fex().map(Path::to_owned)),
            problem: None,
        })
    }

    /// Use the built-in FEX or a device FEX. It counts from the next program
    /// the launcher starts; the built-in FEX downloads in step 3 first.
    fn choose_fex_source(&self, source: FexSourceView) -> Result<FexSettingView, BackendFailure> {
        self.fex_setting()
            .ok_or_else(|| backend_failure("fex-setting-unavailable"))?;
        let source = match source {
            FexSourceView::Builtin if !BUILTIN_FEX_AVAILABLE => {
                return Err(backend_failure("builtin-fex-unavailable"));
            }
            FexSourceView::Builtin => crate::persistence::FexSource::Builtin,
            FexSourceView::Device => crate::persistence::FexSource::Device,
        };
        let settings = self
            .settings
            .load()
            .map_err(|_| backend_failure("settings-load-failed"))?
            .with_fex_source(source);
        self.settings
            .save(&settings)
            .map_err(|_| backend_failure("settings-save-failed"))?;
        self.fex_setting()
            .ok_or_else(|| backend_failure("fex-setting-unavailable"))
    }

    /// Save where FEX is; it is used from the next start. Only an
    /// executable file is saved, whoever owns it.
    fn save_fex(&self, fex: Option<PathBuf>) -> Result<FexSettingView, BackendFailure> {
        let current = self
            .fex_setting()
            .ok_or_else(|| backend_failure("fex-setting-unavailable"))?;
        if let Some(problem) = fex.as_deref().and_then(fex_path_problem) {
            return Ok(FexSettingView {
                problem: Some(problem),
                ..current
            });
        }
        let settings = self
            .settings
            .load()
            .map_err(|_| backend_failure("settings-load-failed"))?
            .with_fex(fex);
        self.settings
            .save(&settings)
            .map_err(|_| backend_failure("settings-save-failed"))?;
        self.fex_setting()
            .ok_or_else(|| backend_failure("fex-setting-unavailable"))
    }

    /// Whether the installed game's OpenGL renderer draws in software, from
    /// the sealed probe run like the game. The answer is kept for the same
    /// runtime and host graphics (installed libraries, kernel GPU drivers,
    /// and session), so the probe runs again only when one changes. An
    /// unanswered probe, such as while the game runs, is `None` and is
    /// tried again by the next audit.
    fn game_renders_in_software(&self, runtime: crate::runtime::InstalledRuntime) -> Option<bool> {
        let key = game_graphics_key(runtime);
        let record = self.paths.config_root().join(GAME_GRAPHICS_RECORD);
        let renderer = match read_game_graphics(&record, &key) {
            Some(renderer) => renderer,
            None => {
                let probe = crate::install::gl_probe_helper()?;
                let probed = self
                    .game
                    .lock()
                    .ok()?
                    .probe_renderer(&self.paths.managed_root(), probe)
                    .ok()?;
                let _ = write_game_graphics(&record, &key, probed.as_deref());
                probed
            }
        };
        Some(renderer.is_some_and(|renderer| crate::process::software_renderer(&renderer)))
    }

    fn repair(&self, cancellation: &CancellationToken) -> Result<(), BackendFailure> {
        let activity = self.begin_job(JobKind::Repair, WorkPhase::Repair, cancellation)?;
        let result = (|| {
            reject_cancelled(cancellation)?;
            let root = Directory::open(&self.paths.managed_root())
                .map_err(|_| backend_failure("installation-busy"))?;
            let _lease = InstallationMutationLease::acquire(&root)?;
            let layout = self.update_layout(&root)?;
            TransactionManager::new(&layout, &NoFaults)
                .repair_controlled(cancellation)
                .map_err(|error| {
                    if matches!(error, crate::transaction::TransactionError::Cancelled) {
                        backend_failure("operation-cancelled")
                    } else {
                        backend_failure("repair-failed")
                    }
                })?;
            activity.progress(WorkPhase::Verification, None, None, None)
        })();
        activity.defer(&result)?;
        result
    }

    fn check_for_update(&self) -> Result<UpdateStatus, BackendFailure> {
        let root = Directory::open(&self.paths.managed_root())
            .map_err(|_| backend_failure("launcher-state-unavailable"))?;
        let layout = self.update_layout(&root)?;
        let mut source = self.updater_source()?;
        let manifest = match VerifiedUpdater::with_layout(layout.clone(), &mut *source).check() {
            Ok(manifest) => manifest,
            Err(_) => {
                self.record(DiagnosticCode::UpdateCheckFailed)?;
                return Err(backend_failure("update-check-failed"));
            }
        };
        let accepted = read_optional_bounded_at(&layout, ACCEPTED_MANIFEST_LIMIT)?;
        if accepted.as_deref() == Some(manifest.raw()) {
            return Ok(UpdateStatus::UpToDate);
        }
        // The first check after an import, whose record the copied files
        // made: the files themselves decide. A client that already is the
        // current release gets PD2's record, with no question and no
        // download; any other is offered the update.
        if accepted
            .as_deref()
            .is_some_and(|accepted| crate::install::is_copied_client_record(&root, accepted))
        {
            let base = crate::network::Pd2Source::client_base(&*source);
            drop(source);
            let mut fetched = crate::update::FetchedManifestSource::new(base, &manifest);
            let sequence = self.transaction_sequence.fetch_add(1, Ordering::Relaxed);
            // The record is written only while the installation's lease is
            // held, so that no game starts meanwhile.
            let hold = || {
                InstallationMutationLease::acquire(&root)
                    .ok()
                    .map(|lease| Box::new(lease) as Box<dyn std::any::Any>)
            };
            let current = VerifiedUpdater::with_layout(layout, &mut fetched).accept_if_current(
                &format!("check-{}-{sequence}", std::process::id()),
                &crate::operation::OperationControl::default(),
                &hold,
            );
            eprintln!(
                "pd2-launcher: the first update check after an import compared the copied files with Project Diablo 2's: {}",
                match current {
                    Ok(crate::update::CurrentClient::Recorded) => {
                        "they are the current release, so its record is written"
                    }
                    Ok(crate::update::CurrentClient::Busy) => {
                        "they are the current release; the installation was busy, so the next check writes its record"
                    }
                    Ok(crate::update::CurrentClient::Differs) => {
                        "a file differs or is missing, so the update is offered"
                    }
                    Err(_) => "they could not be compared, so the update is offered",
                }
            );
            if matches!(
                current,
                Ok(crate::update::CurrentClient::Recorded | crate::update::CurrentClient::Busy)
            ) {
                return Ok(UpdateStatus::UpToDate);
            }
        }
        Ok(UpdateStatus::Available)
    }

    fn select_installer_media(&self, path: PathBuf) -> Result<InstallerMediaView, BackendFailure> {
        let media = InstallerMedia::validate(path)
            .map_err(|_| backend_failure("installer-folder-must-contain-both-official-files"))?;
        let display_path = media.directory().to_path_buf();
        *self
            .installer_media
            .lock()
            .map_err(|_| backend_failure("installer-media-unavailable"))? =
            Some(InstallerChoice::Folder(media));
        Ok(InstallerMediaView::Validated(display_path))
    }

    /// The downloaded wrappers pass the same size, PE, filename, and
    /// Launcher-owned cache validation as wrappers from a selected folder.
    fn select_installer_download(&self) -> Result<InstallerMediaView, BackendFailure> {
        *self
            .installer_media
            .lock()
            .map_err(|_| backend_failure("installer-media-unavailable"))? =
            Some(InstallerChoice::BlizzardDownload);
        Ok(InstallerMediaView::BlizzardDownload)
    }

    /// Check a Diablo II folder the user already has. A usable folder, of
    /// any version, becomes the choice; one that isn't is returned with the
    /// reason and the earlier choice is kept.
    fn select_existing_install(&self, path: PathBuf) -> Result<InstallerMediaView, BackendFailure> {
        match crate::game_folder::validate_existing(&path, &self.paths.managed_root()) {
            Ok(existing) => {
                let view = existing_view(&existing, None);
                *self
                    .installer_media
                    .lock()
                    .map_err(|_| backend_failure("installer-media-unavailable"))? =
                    Some(InstallerChoice::Existing(existing));
                Ok(view)
            }
            Err(problem) => Ok(InstallerMediaView::Existing(ExistingInstallView {
                path,
                problem: Some(existing_problem_view(problem)),
                copy_bytes: 0,
                in_place_possible: false,
                project_diablo: false,
            })),
        }
    }

    /// The existing folder the last Download was made for, when it is
    /// still a usable folder and those downloads still hold.
    fn restored_installer_choice(&self) -> Option<InstallerMediaView> {
        let root = Directory::open(&self.paths.managed_root()).ok()?;
        if !crate::downloads::readiness(&root)
            .serves(DownloadPlan::ExistingInstall, DownloadNeed::NOTHING)
        {
            return None;
        }
        let config = Directory::open(self.paths.config_root()).ok()?;
        let file = config.open_file(Path::new(EXISTING_CHOICE_FILE)).ok()?;
        let mut bytes = Vec::new();
        file.take(8 * 1024).read_to_end(&mut bytes).ok()?;
        let record = serde_json::from_slice::<ExistingChoiceRecord>(&bytes)
            .ok()
            .filter(|record| record.schema == 1)?;
        let existing = crate::game_folder::validate_existing(
            Path::new(&record.path),
            &self.paths.managed_root(),
        )
        .ok()?;
        let view = existing_view(&existing, None);
        let mut choice = self.installer_media.lock().ok()?;
        if choice.is_none() {
            *choice = Some(InstallerChoice::Existing(existing));
            Some(view)
        } else {
            None
        }
    }

    /// Remember (or forget) the existing folder a Download was made for.
    fn record_existing_choice(&self, choice: &InstallerChoice) -> Result<(), BackendFailure> {
        let config = Directory::open(self.paths.config_root())
            .map_err(|_| backend_failure("launcher-state-unavailable"))?;
        let name = std::ffi::OsStr::new(EXISTING_CHOICE_FILE);
        match choice {
            InstallerChoice::Existing(existing) => {
                let bytes = serde_json::to_vec(&ExistingChoiceRecord {
                    schema: 1,
                    path: existing
                        .path()
                        .to_str()
                        .ok_or_else(|| backend_failure("launcher-state-unavailable"))?
                        .to_owned(),
                })
                .map_err(|_| backend_failure("launcher-state-unavailable"))?;
                config
                    .atomic_write(name, &bytes)
                    .map_err(|_| backend_failure("launcher-state-unavailable"))
            }
            InstallerChoice::Folder(_) | InstallerChoice::BlizzardDownload => {
                match config.open_file(Path::new(name)) {
                    Ok(file) => config
                        .remove_open_file_bounded(name, &file)
                        .map_err(|_| backend_failure("launcher-state-unavailable")),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                    Err(_) => Err(backend_failure("launcher-state-unavailable")),
                }
            }
        }
    }

    /// Effective sources: GUI setting > environment > built-in.
    fn effective_sources(&self) -> Result<DownloadSources, BackendFailure> {
        Ok(effective_sources(&self.settings, &self.source_environment))
    }

    fn download_sources(&self) -> Result<Vec<DownloadSourceView>, BackendFailure> {
        let sources = self.effective_sources()?;
        let installed = Directory::open(&self.paths.managed_root())
            .map(|root| installed_source_notes(&root, &sources))
            .unwrap_or_default();
        Ok(download_source_views(&sources, &installed))
    }

    fn update_download_source(
        &self,
        artifact: DownloadArtifact,
        url: Option<String>,
    ) -> Result<Vec<DownloadSourceView>, BackendFailure> {
        update_download_source_setting(&self.settings, artifact, url)?;
        self.download_sources()
    }

    /// The shared updater source, rebuilt when the effective PD2 client base
    /// changed since it was created.
    fn updater_source(&self) -> Result<std::sync::MutexGuard<'_, SystemPd2Source>, BackendFailure> {
        let base = client_base(&self.effective_sources()?);
        let mut source = self
            .updater_source
            .lock()
            .map_err(|_| backend_failure("update-source-unavailable"))?;
        if crate::network::Pd2Source::client_base(&*source) != base {
            *source = SystemPd2Source::with_client_base(base)
                .map_err(|_| backend_failure("update-source-unavailable"))?;
        }
        Ok(source)
    }

    fn record_custom_sources(
        &self,
        activity: &ActiveDiagnostic<'_>,
        sources: &DownloadSources,
        artifacts: &[DownloadArtifact],
    ) -> Result<(), BackendFailure> {
        let used = sources
            .custom_artifacts()
            .filter(|(artifact, _)| artifacts.contains(artifact))
            .map(|(artifact, url)| (artifact, redacted_host(url)))
            .collect::<Vec<_>>();
        if used.is_empty() {
            return Ok(());
        }
        self.diagnostics_mut()?
            .as_mut()
            .expect("diagnostics initialization was checked")
            .record_custom_sources(activity.id, used)
            .map_err(|_| backend_failure("diagnostics-unavailable"))
    }

    /// The current installer choice. Only a usable choice reaches a job.
    fn installer_choice(&self) -> Result<Option<InstallerChoice>, BackendFailure> {
        Ok(self
            .installer_media
            .lock()
            .map_err(|_| backend_failure("installer-media-unavailable"))?
            .clone())
    }

    /// Run one setup job's installer with the sources resolved once at its
    /// start, record the custom sources it used, and map its outcome to a
    /// stable failure code. `cache_only` jobs run only from the
    /// verified downloads.
    fn run_setup_job<T>(
        &self,
        activity: &ActiveDiagnostic<'_>,
        choice: Option<&InstallerChoice>,
        cache_only: bool,
        run: impl FnOnce(&mut ManagedInstaller<'_>) -> Result<T, InstallError>,
    ) -> Result<T, BackendFailure> {
        let mut sources = self.effective_sources()?;
        let artifacts = match choice {
            Some(InstallerChoice::Folder(media)) => {
                // A selected folder keeps its reviewed validation;
                // installer URL overrides apply only to downloads.
                sources = sources.without(&[
                    DownloadArtifact::D2Installer,
                    DownloadArtifact::LodInstaller,
                ]);
                ProductionArtifactSource::with_installer_media(media.clone())
            }
            _ => ProductionArtifactSource::new(),
        };
        let artifacts = if cache_only && self.auditor.android_host() {
            // On Android Blizzard's downloaders run in Install, from the
            // wrappers Download kept.
            artifacts.cache_only().with_wrapper_refetch()
        } else if cache_only {
            artifacts.cache_only()
        } else {
            artifacts
        };
        // Only custom sources this job actually used are recorded.
        let used = UsedSources::default();
        let artifacts = RecordingArtifactSource {
            inner: &artifacts,
            used: &used,
        };
        let client = RecordingClientSynchronizer {
            inner: UpdaterClientSynchronizer::with_client_base(client_base(&sources))
                .map_err(|_| backend_failure("updater-adapter-unavailable"))?,
            used: &used,
        };
        // The job's audits, runtime selection, and downloads all use the
        // sources resolved once at its start.
        let auditor = JobAuditor {
            auditor: self.auditor.as_ref(),
            sources: &sources,
        };
        let driver = self
            .install_driver
            .clone()
            .with_custom_wine(sources.custom_url(DownloadArtifact::Wine).is_some())
            .with_wpf_software_rendering(self.official_launcher_software_rendering());
        // Read at each job's start, like the sources.
        let direct_download = self.settings.load().is_ok_and(|settings| {
            settings.install_source() == InstallSourceSetting::DirectDownload
        });
        if direct_download {
            eprintln!("pd2-launcher: Blizzard's installers come from the direct download");
        }
        let mut installer = ManagedInstaller::new(
            &auditor,
            &artifacts,
            &driver,
            &client,
            ArtifactCatalog::with_sources(&sources),
        )
        .with_direct_download(direct_download)
        .with_client_as_installed(client_as_installed(&self.settings));
        let result = run(&mut installer);
        self.record_custom_sources(activity, &sources, &used.take())?;
        if let Err(error) = &result {
            // Also on standard error: some systems (DroidDeck) keep only that.
            eprintln!(
                "pd2-launcher: a setup step failed: {} ({})",
                install_failure_code(error),
                crate::activity::redacted(&format!("{error:?}"))
            );
            crate::activity::record(crate::activity::ActivityEvent::StageFailed {
                code: install_failure_code(error).to_owned(),
                detail: format!("{error:?}"),
            });
        }
        match result {
            Ok(value) => Ok(value),
            Err(InstallError::Cancelled) => Err(backend_failure("operation-cancelled")),
            Err(InstallError::Indeterminate) => Err(backend_failure("operation-indeterminate")),
            Err(InstallError::StageInterrupted) => Err(backend_failure("stage-interrupted")),
            // An install step found something it needs missing from the
            // verified downloads: Download runs again first.
            Err(InstallError::AcquisitionFailed { .. }) if cache_only => {
                Err(backend_failure("downloads-required"))
            }
            Err(error) => {
                let code = install_failure_code(&error);
                let event = match code {
                    "base-game-key-rejected" => Some(DiagnosticCode::BaseGameKeyRejected),
                    "expansion-key-rejected" => Some(DiagnosticCode::ExpansionKeyRejected),
                    "base-game-installer-error" | "expansion-installer-error" => {
                        Some(DiagnosticCode::VendorInstallerError)
                    }
                    "base-game-installer-menu" | "expansion-installer-menu" => {
                        Some(DiagnosticCode::VendorInstallerMenu)
                    }
                    _ => None,
                };
                if let Some(event) = event {
                    self.record(event)?;
                }
                Err(backend_failure(code))
            }
        }
    }

    /// Step 3: download and verify everything the remaining
    /// steps need, for the current installer choice. No CD key is needed.
    fn download_all(&self, cancellation: &CancellationToken) -> Result<(), BackendFailure> {
        let activity = self.begin_job(
            JobKind::Download,
            WorkPhase::RuntimeAcquisition,
            cancellation,
        )?;
        let result = (|| {
            reject_cancelled(cancellation)?;
            // The built-in FEX is the desktop's; an Android host
            // runs the app's ARM64 Wine.
            let builtin_fex_offered = !self.auditor.android_host();
            if builtin_fex_offered {
                keep_default_fex_choice(&self.settings, &self.paths.managed_root());
            }
            let choice = self.installer_choice()?;
            // An installed game needs no installer: only the built-in FEX
            // chosen since.
            if choice.is_none()
                && builtin_fex_offered
                && builtin_fex_pending(&self.settings, &self.paths.managed_root())
            {
                self.run_setup_job(&activity, None, false, |installer| {
                    installer.install_builtin_fex_only(&self.paths.managed_root(), cancellation)
                })?;
                reject_cancelled(cancellation)?;
                return activity.progress(WorkPhase::Verification, None, None, None);
            }
            let choice = choice.ok_or_else(|| backend_failure("installer-media-required"))?;
            self.run_setup_job(&activity, Some(&choice), false, |installer| {
                installer.download_all(&self.paths.managed_root(), choice.plan(), cancellation)
            })?;
            self.record_existing_choice(&choice)?;
            reject_cancelled(cancellation)?;
            activity.progress(WorkPhase::Verification, None, None, None)
        })();
        activity.defer(&result)?;
        result
    }

    /// Remove what this Launcher's own downloads made, for a player who
    /// imports the game instead; imported files stay.
    fn discard_downloads(&self, cancellation: &CancellationToken) -> Result<(), BackendFailure> {
        let activity = self.begin_job(
            JobKind::DiscardDownloads,
            WorkPhase::Verification,
            cancellation,
        )?;
        let result = (|| {
            reject_cancelled(cancellation)?;
            let removed = self.run_setup_job(&activity, None, true, |installer| match installer
                .discard_downloads(&self.paths.managed_root(), cancellation)
            {
                Ok(()) => Ok(true),
                Err(InstallError::StageFailed { .. }) => Ok(false),
                Err(error) => Err(error),
            })?;
            if !removed {
                return Err(backend_failure("discard-downloads-failed"));
            }
            activity.progress(WorkPhase::Verification, None, None, None)
        })();
        activity.defer(&result)?;
        result
    }

    /// Step 4: install Diablo II from the verified downloads, or
    /// import the existing installation. Refused before Download finished.
    fn install_base_game(
        &self,
        authorization: Option<InstallationAuthorization>,
        import: Option<ImportModeView>,
        cancellation: &CancellationToken,
    ) -> Result<(), BackendFailure> {
        let activity = self.begin_job(
            JobKind::InstallBaseGame,
            if import.is_some() {
                WorkPhase::ImportGame
            } else {
                WorkPhase::BaseGameInstall
            },
            cancellation,
        )?;
        let result = (|| {
            reject_cancelled(cancellation)?;
            let plan = if import.is_some() {
                DownloadPlan::ExistingInstall
            } else {
                DownloadPlan::Installers
            };
            let root = Directory::open(&self.paths.managed_root())
                .map_err(|_| backend_failure("installation-busy"))?;
            // The job's own cache-only source still refuses anything these
            // downloads don't hold for the status it audits.
            if !crate::downloads::readiness(&root).serves(plan, DownloadNeed::NOTHING) {
                return Err(backend_failure("downloads-required"));
            }
            drop(root);
            let managed_root = self.paths.managed_root();
            match import {
                Some(mode) => {
                    let Some(InstallerChoice::Existing(existing)) = self.installer_choice()? else {
                        return Err(backend_failure("existing-install-required"));
                    };
                    let mode = match mode {
                        ImportModeView::Copy => ImportMode::Copy,
                        ImportModeView::InPlace => ImportMode::InPlace,
                    };
                    self.run_setup_job(&activity, None, true, |installer| {
                        installer.import_existing(&managed_root, &existing, mode, cancellation)
                    })?;
                }
                None => {
                    let outcome = self.run_setup_job(&activity, None, true, |installer| {
                        installer.install_base_game(
                            &managed_root,
                            authorization.as_ref(),
                            cancellation,
                        )
                    })?;
                    if !matches!(
                        outcome.final_status(),
                        InstallationStatus::NeedsClient
                            | InstallationStatus::NeedsRuntime
                            | InstallationStatus::Ready
                    ) {
                        return Err(backend_failure("managed-install-failed"));
                    }
                }
            }
            reject_cancelled(cancellation)?;
            activity.progress(WorkPhase::Verification, None, None, None)
        })();
        activity.defer(&result)?;
        result
    }

    /// Step 5: install Project Diablo 2 from the verified
    /// downloads; only client files that changed since, or anything an
    /// older installation never downloaded, are fetched.
    fn install_project_diablo(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<(), BackendFailure> {
        let activity = self.begin_job(
            JobKind::InstallProjectDiablo,
            WorkPhase::ClientManifest,
            cancellation,
        )?;
        let result = (|| {
            reject_cancelled(cancellation)?;
            let managed_root = self.paths.managed_root();
            // Everything comes from the verified downloads when they hold
            // it; an installation from before the Download step (a Wine
            // component or the bootstrap never downloaded) fetches only
            // what is missing, with the same pins.
            let outcome = self.run_setup_job(&activity, None, false, |installer| {
                installer.install_project_diablo(&managed_root, cancellation)
            })?;
            if outcome.final_status() != InstallationStatus::Ready {
                return Err(backend_failure("managed-install-failed"));
            }
            reject_cancelled(cancellation)?;
            activity.progress(WorkPhase::Verification, None, None, None)
        })();
        activity.defer(&result)?;
        result
    }

    fn apply_update(&self, cancellation: &CancellationToken) -> Result<(), BackendFailure> {
        let activity = self.begin_job(JobKind::Update, WorkPhase::ClientManifest, cancellation)?;
        let result = (|| {
            reject_cancelled(cancellation)?;
            if matches!(
                self.game
                    .lock()
                    .map_err(|_| backend_failure("game-supervision-unavailable"))?
                    .state(),
                GameState::Running | GameState::SupervisionFailed
            ) {
                return Err(backend_failure("game-active"));
            }

            let root = Directory::open(&self.paths.managed_root())
                .map_err(|_| backend_failure("installation-busy"))?;
            let lease = InstallationMutationLease::acquire(&root)?;
            let audit_lease = HeldAuditLease(&lease);
            let audit = self.auditor.audit_report_with_lease(
                ActivityEvidence::Idle,
                &audit_lease,
                &root,
            )?;
            if audit.installation_status() != InstallationStatus::Ready
                || audit.protected_inventory_status() != ProtectedInventoryStatus::Complete
            {
                return Err(backend_failure("update-preflight-rejected"));
            }
            reject_cancelled(cancellation)?;
            let sequence = self.transaction_sequence.fetch_add(1, Ordering::Relaxed);
            let transaction_id = format!("update-{}-{sequence}", std::process::id());
            self.record_custom_sources(
                &activity,
                &self.effective_sources()?,
                &[DownloadArtifact::Pd2ClientBase],
            )?;
            let mut source = self.updater_source()?;
            let layout = self.update_layout(&root)?;
            let update = refresh_client_cache(&root, &layout, &mut *source, cancellation).and_then(
                |cache| {
                    let mut cached = PrefetchedPd2Source::new(&mut *source, Some(cache));
                    VerifiedUpdater::with_layout(layout, &mut cached).apply_controlled(
                        &transaction_id,
                        GameActivity::Idle,
                        ProtectedInventoryEvidence::Complete,
                        cancellation,
                    )
                },
            );
            if matches!(
                update,
                Err(UpdateError::Cancelled
                    | UpdateError::Transaction(crate::transaction::TransactionError::Cancelled))
            ) {
                return Err(backend_failure("operation-cancelled"));
            }
            if matches!(
                update,
                Err(UpdateError::Indeterminate
                    | UpdateError::Transaction(
                        crate::transaction::TransactionError::Indeterminate
                    ))
            ) {
                return Err(backend_failure("operation-indeterminate"));
            }
            if update.is_err() {
                return Err(backend_failure("verified-update-failed"));
            }
            reject_cancelled(cancellation)?;
            activity.progress(WorkPhase::Verification, None, None, None)
        })();
        activity.defer(&result)?;
        result
    }

    fn update_layout(&self, root: &Directory) -> Result<UpdateLayout, BackendFailure> {
        anchored_update_layout(&self.paths, root)
    }

    /// The fallback when this Launcher cannot update or start the game: run
    /// the official PD2 launcher in the managed prefix, holding the
    /// exclusive installation lease so no install, update, repair, or game
    /// launch overlaps it, until it and everything it started exit.
    /// `strict_memory_order`: a game that it starts gets Box64's strict x86
    /// memory order, as for Play; `detailed_logging`: its Wine and that game
    /// log in detail.
    fn open_official_launcher(
        &self,
        confirmed: bool,
        strict_memory_order: bool,
        detailed_logging: bool,
        cancellation: &CancellationToken,
    ) -> Result<(), BackendFailure> {
        let activity = self.begin_job(
            JobKind::OfficialLauncher,
            WorkPhase::OfficialLauncher,
            cancellation,
        )?;
        let result = (|| {
            reject_cancelled(cancellation)?;
            if matches!(
                self.game
                    .lock()
                    .map_err(|_| backend_failure("game-supervision-unavailable"))?
                    .state(),
                GameState::Running | GameState::SupervisionFailed
            ) {
                return Err(backend_failure("game-active"));
            }
            let root = Directory::open(&self.paths.managed_root())
                .map_err(|_| backend_failure("installation-busy"))?;
            let lease = InstallationMutationLease::acquire(&root)?;
            let audit = self.auditor.audit_report_with_lease(
                ActivityEvidence::Idle,
                &HeldAuditLease(&lease),
                &root,
            )?;
            if !official_launcher_permitted(&audit) {
                return Err(backend_failure("official-launcher-preflight-rejected"));
            }
            let settings = self
                .settings
                .load()
                .map_err(|_| backend_failure("settings-load-failed"))?;
            // Another program than PD2Launcher.exe is never verified: it
            // runs only once the player confirmed it.
            let other_program =
                settings.launcher_program() != crate::persistence::DEFAULT_LAUNCHER_PROGRAM;
            if other_program {
                require_program(&self.paths.managed_root(), settings.launcher_program())?;
                if !confirmed {
                    return Err(backend_failure("official-launcher-confirmation-required"));
                }
            } else if let Some(code) =
                official_launcher_refusal(audit.official_launcher(), confirmed)
            {
                return Err(backend_failure(code));
            }
            // Its Play starts the game, which saves characters too.
            self.keep_saves_under(&root)?;
            let driver = self
                .install_driver
                .clone()
                .with_custom_wine(audit.uses_custom_runtime())
                .with_wpf_software_rendering(self.official_launcher_software_rendering())
                .with_game_strict_memory_order(strict_memory_order)
                .with_detailed_logging(detailed_logging)
                .with_official_launcher(
                    settings.launcher_program(),
                    settings.launcher_custom_arguments(),
                );
            eprintln!(
                "pd2-launcher: the official launcher starts: {}{}",
                settings.launcher_program(),
                if settings.launcher_custom_arguments().is_empty() {
                    String::new()
                } else {
                    format!(" ({})", settings.launcher_custom_arguments())
                }
            );
            let session =
                driver.run_official_launcher(&self.paths.managed_root(), &lease.0, cancellation);
            // The official launcher updates itself; the binary it leaves
            // behind after a session under this lease is the copy trusted
            // next time (trust on first use), once its processes are gone.
            // Another program's session records nothing.
            if !other_program
                && cancellation.cancellation_safety()
                    != crate::operation::CancellationSafety::Unproven
            {
                let _ = record_official_launcher(&root);
            }
            // Another launcher's changes in the game folder are its own:
            // PD2's client is not reconciled after its session, as Play
            // does no PD2 check for another game program.
            if other_program {
                if !cancellation.is_requested() {
                    session.map_err(|_| backend_failure("official-launcher-failed"))?;
                }
            } else {
                after_official_launcher(session, cancellation, |files, control| {
                    self.reconcile_official_client(&activity, &root, &lease, files, control)
                })?;
            }
            activity.progress(WorkPhase::Verification, None, None, None)
        })();
        activity.defer(&result)?;
        result
    }

    /// The official launcher may have synchronized the client to a newer
    /// manifest. Under the same lease, the verified updater then accepts
    /// that manifest, so Ready is re-established from evidence instead of
    /// lost to a stale manifest. With [`OfficialClientFiles::Download`] it
    /// refreshes the client cache and downloads only objects that still
    /// differ. With [`OfficialClientFiles::InstalledOnly`] it fetches only
    /// the manifest, in one try, and leaves the cache as it is: a client the
    /// launcher updated only in part is not accepted, and it and its record
    /// stay as they are. With the update checks off, nothing is downloaded:
    /// what the official launcher left is recorded as it is installed
    /// ([`InstalledClientPd2Source`]). Only an updater result reported as
    /// indeterminate is `operation-indeterminate`, for Repair.
    fn reconcile_official_client(
        &self,
        activity: &ActiveDiagnostic<'_>,
        root: &Directory,
        lease: &InstallationMutationLease,
        files: OfficialClientFiles,
        cancellation: &CancellationToken,
    ) -> Result<(), BackendFailure> {
        let audit = self.auditor.audit_report_with_lease(
            ActivityEvidence::Idle,
            &HeldAuditLease(lease),
            root,
        )?;
        if audit.installation_status() != InstallationStatus::NeedsClient {
            // Nothing the official launcher changed needs accepting.
            return Ok(());
        }
        if audit.protected_inventory_status() != ProtectedInventoryStatus::Complete {
            // Reported as its own outcome, never as an accepted update.
            self.record(DiagnosticCode::OfficialClientNotReconciled)?;
            return Err(backend_failure("official-client-not-reconciled"));
        }
        self.record_custom_sources(
            activity,
            &self.effective_sources()?,
            &[DownloadArtifact::Pd2ClientBase],
        )?;
        let sequence = self.transaction_sequence.fetch_add(1, Ordering::Relaxed);
        let transaction_id = format!("official-{}-{sequence}", std::process::id());
        let mut source = self.updater_source()?;
        let layout = self.update_layout(root)?;
        let applied = if client_as_installed(&self.settings) {
            let project = layout
                .open_project()
                .map_err(|_| backend_failure("client-reconcile-failed"))?;
            let mut kept = InstalledClientPd2Source::new(&mut *source, &project);
            let applied = VerifiedUpdater::with_layout(layout, &mut kept)
                .with_retry_policy(crate::network::RetryPolicy::once())
                .apply_controlled(
                    &transaction_id,
                    GameActivity::Idle,
                    ProtectedInventoryEvidence::Complete,
                    cancellation,
                );
            eprintln!(
                "pd2-launcher: official launcher closed: the update checks are off, so its client is {}",
                if applied.is_ok() {
                    "recorded as it is installed"
                } else {
                    "not recorded"
                }
            );
            applied
        } else {
            match files {
                OfficialClientFiles::Download => {
                    refresh_client_cache(root, &layout, &mut *source, cancellation).and_then(
                        |cache| {
                            let mut cached = PrefetchedPd2Source::new(&mut *source, Some(cache));
                            VerifiedUpdater::with_layout(layout, &mut cached).apply_controlled(
                                &transaction_id,
                                GameActivity::Idle,
                                ProtectedInventoryEvidence::Complete,
                                cancellation,
                            )
                        },
                    )
                }
                OfficialClientFiles::InstalledOnly => {
                    // A failure only keeps what is there, so one try is enough.
                    let mut installed = ManifestOnlyPd2Source::new(&mut *source);
                    let applied = VerifiedUpdater::with_layout(layout, &mut installed)
                        .with_retry_policy(crate::network::RetryPolicy::once())
                        .apply_controlled(
                            &transaction_id,
                            GameActivity::Idle,
                            ProtectedInventoryEvidence::Complete,
                            cancellation,
                        );
                    eprintln!(
                        "pd2-launcher: official launcher stopped: {}",
                        stopped_client_outcome(&applied, installed.refused_a_file())
                    );
                    applied
                }
            }
        };
        applied.map(drop).map_err(|error| {
            backend_failure(if update_indeterminate(&error) {
                "operation-indeterminate"
            } else {
                "client-reconcile-failed"
            })
        })
    }

    /// Choose where the Managed installation lives. Holds the
    /// root-selection lock exclusively throughout, so no other Launcher can
    /// open or change a root meanwhile.
    fn relocate(
        &self,
        parent: PathBuf,
        fresh_only: bool,
        cancellation: &CancellationToken,
    ) -> Result<(), BackendFailure> {
        let activity = self.begin_job(JobKind::Relocation, WorkPhase::Relocation, cancellation)?;
        let result = (|| {
            reject_cancelled(cancellation)?;
            if matches!(
                self.game
                    .lock()
                    .map_err(|_| backend_failure("game-supervision-unavailable"))?
                    .state(),
                GameState::Running | GameState::SupervisionFailed
            ) {
                return Err(backend_failure("game-active"));
            }
            let mut selection = self
                .selection
                .lock()
                .map_err(|_| backend_failure(RelocationError::Busy.code()))?;
            if selection.is_none() {
                *selection = Some(
                    SelectionLock::acquire_shared(self.paths.state_root())
                        .map_err(|_| backend_failure(RelocationError::Busy.code()))?,
                );
            }
            let _exclusive = selection
                .as_ref()
                .expect("selection was just ensured")
                .upgrade()
                .map_err(|_| backend_failure(RelocationError::Busy.code()))?;
            let available = self
                .install_location()
                .is_some_and(|location| location.available);
            if available
                && parent.join(crate::location::INSTALL_DIRECTORY) == self.paths.managed_root()
            {
                return Err(backend_failure(RelocationError::SameLocation.code()));
            }
            if available && parent.starts_with(self.paths.managed_root()) {
                return Err(backend_failure(RelocationError::ParentInvalid.code()));
            }
            // An unavailable location or a root that holds nothing yet is
            // simply replaced by a fresh root. An installation on the same
            // filesystem is renamed there under its lease.
            if available {
                let root_path = self.paths.managed_root();
                let root = Directory::open(&root_path)
                    .map_err(|_| backend_failure("installation-busy"))?;
                let lease = InstallationMutationLease::acquire(&root)?;
                if crate::relocation::holds_installation(&root)
                    .map_err(|_| backend_failure(RelocationError::Io.code()))?
                {
                    if fresh_only {
                        return Err(backend_failure("location-not-fresh"));
                    }
                    let audit = self.auditor.audit_report_with_lease(
                        ActivityEvidence::Idle,
                        &HeldAuditLease(&lease),
                        &root,
                    )?;
                    if audit.check(AuditCheck::Transaction) != CheckStatus::Satisfied {
                        return Err(backend_failure("location-change-not-permitted"));
                    }
                    if crate::relocation::same_filesystem(&root_path, &parent)
                        .map_err(|error| backend_failure(error.code()))?
                    {
                        crate::relocation::move_same_filesystem(
                            &self.paths,
                            &root_path,
                            &parent,
                            crate::safe_fs::ROOT_MINIMUM_FREE_BYTES,
                            &crate::safe_fs::NoProbeFaults,
                            &crate::relocation::NoMoveFaults,
                        )
                        .map_err(|error| backend_failure(error.code()))?;
                        return activity.progress(WorkPhase::Verification, None, None, None);
                    }
                    // Another filesystem: a fresh installation there, seeded
                    // from this root's verified downloads; this root stays
                    // until the user removes it. Its characters move to
                    // their own folder first, so the new installation's
                    // game finds them; a keep that fails leaves
                    // them here, for the removal to move.
                    let _ = self.keep_saves_under(&root);
                    let catalog = ArtifactCatalog::pinned();
                    crate::relocation::move_to_new_filesystem(
                        &self.paths,
                        &root_path,
                        &parent,
                        crate::safe_fs::ROOT_MINIMUM_FREE_BYTES,
                        &crate::safe_fs::NoProbeFaults,
                        |old, new| {
                            crate::install::seed_verified_cache(&catalog, old, new, cancellation)
                                .map(drop)
                                .map_err(|_| std::io::Error::other("cache seeding failed"))
                        },
                    )
                    .map_err(|error| {
                        if cancellation.is_requested() {
                            backend_failure("operation-cancelled")
                        } else {
                            backend_failure(error.code())
                        }
                    })?;
                    return activity.progress(WorkPhase::Verification, None, None, None);
                }
            }
            let new_root = crate::relocation::create_root(
                &parent,
                crate::safe_fs::ROOT_MINIMUM_FREE_BYTES,
                &crate::safe_fs::NoProbeFaults,
            )
            .map_err(|error| backend_failure(error.code()))?;
            if crate::relocation::adopt(&self.paths, &new_root).is_err() {
                new_root.discard();
                return Err(backend_failure(RelocationError::Io.code()));
            }
            activity.progress(WorkPhase::Verification, None, None, None)
        })();
        activity.defer(&result)?;
        result
    }

    /// Remove the installation a move to another filesystem left behind,
    /// which the user confirmed once the new one was Ready.
    fn remove_previous_installation(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<(), BackendFailure> {
        let activity = self.begin_job(JobKind::Relocation, WorkPhase::Relocation, cancellation)?;
        let result = (|| {
            reject_cancelled(cancellation)?;
            let selection = self
                .selection
                .lock()
                .map_err(|_| backend_failure(RelocationError::Busy.code()))?;
            let _exclusive = selection
                .as_ref()
                .ok_or_else(|| backend_failure(RelocationError::Busy.code()))?
                .upgrade()
                .map_err(|_| backend_failure(RelocationError::Busy.code()))?;
            let kept = self.paths.kept_saves().filter(|_| !crate::android_host());
            crate::relocation::remove_previous(&self.paths, |old, old_path| match &kept {
                // Its characters come out before it goes, those
                // of a Save Path its registry names inside it too.
                Some(kept) => {
                    let now = std::time::SystemTime::now();
                    let (_, mut put_aside) = crate::save_keeper::rescue_managed(old, kept, now)?;
                    let (_, elsewhere) =
                        crate::save_keeper::keep_save_path(old, old_path, kept, now)?;
                    put_aside.add(elsewhere);
                    self.note_put_aside(put_aside);
                    Ok(())
                }
                None => Ok(()),
            })
            .map_err(|error| backend_failure(error.code()))?;
            activity.progress(WorkPhase::Verification, None, None, None)
        })();
        activity.defer(&result)?;
        result
    }

    /// Start over. The audit and the removal share one exclusive
    /// installation lease, so no install, update, repair, launch, or official
    /// launcher session can start between the guard and the removal.
    fn reset_installation(&self, cancellation: &CancellationToken) -> Result<(), BackendFailure> {
        let activity = self.begin_job(JobKind::Reset, WorkPhase::Reset, cancellation)?;
        let result = (|| {
            reject_cancelled(cancellation)?;
            if matches!(
                self.game
                    .lock()
                    .map_err(|_| backend_failure("game-supervision-unavailable"))?
                    .state(),
                GameState::Running | GameState::SupervisionFailed
            ) {
                return Err(backend_failure("game-active"));
            }
            let root = Directory::open(&self.paths.managed_root())
                .map_err(|_| backend_failure("installation-busy"))?;
            let lease = InstallationMutationLease::acquire(&root)?;
            let kept = self.paths.kept_saves().filter(|_| !crate::android_host());
            let put_aside = start_over(
                &root,
                &self.paths.managed_root(),
                &lease,
                kept.as_ref(),
                || {
                    self.auditor
                        .audit_report_with_lease(
                            ActivityEvidence::Idle,
                            &HeldAuditLease(&lease),
                            &root,
                        )
                        .map(|report| report.check(AuditCheck::Transaction))
                },
            )?;
            self.note_put_aside(put_aside);
            activity.progress(WorkPhase::Verification, None, None, None)
        })();
        activity.defer(&result)?;
        result
    }

    /// Before the game (or the official launcher, whose Play starts it)
    /// runs on the desktop, the Managed game folder's characters are kept in
    /// their own folder, under the installation lease. A keep that
    /// fails, or a lease another program holds, never stops Play: the game
    /// saves where it always did, and the next keep moves those characters.
    /// Only a game folder left without its `Save` stops it
    /// ([`keep_before_play`]).
    fn keep_saves(&self) -> Result<(), BackendFailure> {
        if crate::android_host() {
            return Ok(());
        }
        let Ok(root) = Directory::open(&self.paths.managed_root()) else {
            return Ok(());
        };
        let Ok(_lease) = InstallationMutationLease::acquire(&root) else {
            return Ok(());
        };
        self.keep_saves_under(&root)
    }

    /// [`Self::keep_saves`] for a caller that holds the lease.
    fn keep_saves_under(&self, root: &Directory) -> Result<(), BackendFailure> {
        let Some(kept) = self.paths.kept_saves().filter(|_| !crate::android_host()) else {
            return Ok(());
        };
        if GameFolder::resolve(root) != Ok(GameFolder::Managed) {
            return Ok(());
        }
        self.note_put_aside(keep_before_play(root, &kept)?);
        Ok(())
    }

    /// Add what a keep put in a backup to what the player is told next.
    fn note_put_aside(&self, put_aside: crate::save_keeper::PutAside) {
        if put_aside.is_empty() {
            return;
        }
        if let Ok(mut noted) = self.put_aside.lock() {
            noted.add(put_aside);
        }
    }

    /// What keeps put in a backup since this was last asked.
    fn take_saves_put_aside(&self) -> Option<SavesPutAsideView> {
        let put_aside = std::mem::take(&mut *self.put_aside.lock().ok()?);
        if put_aside.is_empty() {
            return None;
        }
        Some(SavesPutAsideView {
            characters: put_aside.characters,
            files: put_aside.files,
            others: put_aside.others,
            folder: put_aside.folder?,
            open: None,
        })
    }

    fn request_mutation_safe_stop(&self) -> Result<(), BackendFailure> {
        let mut diagnostics = self.diagnostics_mut()?;
        let store = diagnostics
            .as_mut()
            .expect("diagnostics initialization was checked");
        let id = store
            .active_job()
            .map(JobRecord::id)
            .ok_or_else(|| backend_failure("diagnostics-unavailable"))?;
        store
            .request_safe_stop(id)
            .map_err(|_| backend_failure("diagnostics-unavailable"))
    }

    fn launch_game(
        &self,
        driver: GraphicsDriver,
        profile: Option<GraphicsProfile>,
        strict_memory_order: bool,
        detailed_logging: bool,
    ) -> Result<Option<GameNoticeView>, BackendFailure> {
        let settings = self
            .settings
            .load()
            .map_err(|_| backend_failure("settings-load-failed"))?;
        // The Android app's graphics driver: its Compatibility values reach
        // every Wine process started from now on, and from the next engine's
        // start (`GRAPHICS_DRIVER_RECORD`), until a Play asks for the other
        // driver.
        if self.auditor.android_host() {
            let compatibility = driver == GraphicsDriver::Compatibility;
            if let Err(error) = keep_graphics_driver(self.paths.config_root(), compatibility) {
                eprintln!(
                    "pd2-launcher: the graphics driver is not kept for the next start: {error}"
                );
            }
            crate::wine_environment::use_compatibility_driver(compatibility);
        }
        let mut options = game_launch_options(&settings);
        if let Some(profile) = profile {
            options = options.with_graphics_profile(match profile {
                GraphicsProfile::Default => crate::process::GraphicsProfile::Default,
                GraphicsProfile::Performance => crate::process::GraphicsProfile::Performance,
            });
        }
        options = options
            .with_strict_memory_order(strict_memory_order)
            .with_detailed_logging(detailed_logging);
        if let Err(failure) = self.keep_saves() {
            self.record(DiagnosticCode::GameLaunchFailed)?;
            return Err(failure);
        }
        // The phone's fonts in Wine, as before the official launcher: Wine
        // runs for them only when the prefix lacks the current list. Best
        // effort; the game starts either way.
        if self.auditor.android_host()
            && let Ok(root) = Directory::open(&self.paths.managed_root())
            && let Ok(lease) = InstallationMutationLease::acquire(&root)
        {
            let _ = self.install_driver.sync_phone_fonts_at(
                &self.paths.managed_root(),
                &lease.0,
                &crate::operation::OperationControl::default(),
            );
        }
        let mut game = self
            .game
            .lock()
            .map_err(|_| backend_failure("game-supervision-unavailable"))?;
        let started = game.start(&self.paths.managed_root(), options);
        drop(game);
        let Ok(d2gl_setup) = started else {
            self.record(DiagnosticCode::GameLaunchFailed)?;
            return Err(backend_failure("game-launch-failed"));
        };
        // The game is already running; a diagnostics failure must not report
        // the launch as failed.
        let _ = match d2gl_setup {
            D2glSetup::Written => self.record(DiagnosticCode::DisplayConfigSeeded),
            D2glSetup::Skipped => self.record(DiagnosticCode::DisplayConfigSkipped),
            D2glSetup::NotRequested | D2glSetup::Unchanged | D2glSetup::ConfigurationAbsent => {
                Ok(())
            }
        };
        Ok(game_launch_notice(d2gl_setup))
    }

    fn poll_game(&self) -> BackendGameState {
        let Ok(mut game) = self.game.lock() else {
            return BackendGameState::SupervisionFailed;
        };
        match game.poll() {
            GameState::Idle => BackendGameState::Idle,
            GameState::Running => BackendGameState::Running,
            GameState::Exited(_) => BackendGameState::Exited,
            GameState::SupervisionFailed => BackendGameState::SupervisionFailed,
        }
    }

    fn request_game_stop(&self) -> Result<(), BackendFailure> {
        self.game
            .lock()
            .map_err(|_| backend_failure("game-supervision-unavailable"))?
            .request_stop()
            .map_err(|_| backend_failure("game-stop-failed"))
    }

    fn load_settings(&self) -> Result<LaunchSettingsView, BackendFailure> {
        let settings = self
            .settings
            .load()
            .map_err(|_| backend_failure("settings-load-failed"))?;
        Ok(launch_settings_view(&settings))
    }

    fn update_settings(
        &self,
        change: SettingsChange,
    ) -> Result<LaunchSettingsView, BackendFailure> {
        update_launch_settings(&self.settings, &self.paths.managed_root(), change)
    }

    fn diagnostics_evidence(&self) -> Result<Vec<EvidenceView>, BackendFailure> {
        let diagnostics = self
            .diagnostics
            .lock()
            .map_err(|_| backend_failure("diagnostics-unavailable"))?;
        let store = diagnostics
            .as_ref()
            .ok_or_else(|| backend_failure("diagnostics-unavailable"))?;
        let mut evidence = store
            .active_job()
            .map(|job| job_evidence(job, EvidenceTone::Working))
            .into_iter()
            .chain(
                store
                    .recent_jobs()
                    .rev()
                    .take(10)
                    .map(|job| job_evidence(job, job_tone(job.status()))),
            )
            .chain(store.recent_events().rev().take(10).map(|code| {
                EvidenceView::new("Recent event", code.message(), EvidenceTone::Verified)
            }))
            .chain(store.recent_custom_sources().rev().take(10).map(|record| {
                EvidenceView::new(
                    format!("Custom source · job #{}", record.job().value()),
                    format!(
                        "{} from {} (unverified)",
                        record.artifact().label(),
                        record.host()
                    ),
                    EvidenceTone::Advisory,
                )
            }))
            .collect::<Vec<_>>();
        if let Some(advisory) = ignored_sources_advisory(&self.settings) {
            evidence.insert(0, advisory);
        }
        if evidence.is_empty() {
            evidence.push(verified_evidence(
                "Recent events",
                "No completed work recorded",
            ));
        }
        Ok(evidence)
    }

    /// Write the report archive into `logs/reports` (the newest
    /// [`REPORTS_KEPT`] are kept) and return its path.
    fn save_report(&self) -> Result<PathBuf, BackendFailure> {
        let inputs = self.report_inputs()?;
        let archive = crate::report::build(&inputs);
        let history = {
            let _cleanup = CleanupLease::acquire(&self.paths.cleanup_lock())
                .map_err(|_| backend_failure("report-failed"))?;
            let mut diagnostics = self.diagnostics_mut()?;
            diagnostics
                .as_mut()
                .expect("diagnostics initialization was checked")
                .export()
                .ok()
                .and_then(|path| std::fs::read_to_string(path).ok())
        };
        let mut archive = archive;
        if let Some(history) = history.and_then(|text| serde_json::from_str(&text).ok()) {
            archive.add_json("logs/launcher-history.json", &history);
        }
        let reports = self.paths.reports_root();
        Directory::ensure_absolute(&reports)
            .map_err(|_| backend_failure("report-folder-unavailable"))?;
        let path = archive
            .write_into(&reports, inputs.now)
            .map_err(|_| backend_failure("report-write-failed"))?;
        crate::report::prune(&reports, REPORTS_KEPT);
        Ok(path)
    }

    fn report_inputs(&self) -> Result<crate::report::ReportInputs, BackendFailure> {
        let managed_root = self.paths.managed_root();
        let audit = Directory::open(&managed_root)
            .map_err(|_| "install location unavailable".to_owned())
            .and_then(|root| {
                let mutation = InstallationMutationLease::acquire(&root)
                    .map_err(|_| "installation busy: a task was running".to_owned())?;
                let report = self
                    .auditor
                    .audit_report_with_lease(
                        ActivityEvidence::Idle,
                        &HeldAuditLease(&mutation),
                        &root,
                    )
                    .map_err(|error| error.code().to_owned())?;
                let downloads = format!("{:?}", crate::downloads::readiness(&root));
                Ok((report, downloads))
            });
        let (audit, downloads) = match audit {
            Ok((report, downloads)) => (Ok(report), Some(downloads)),
            Err(reason) => (Err(reason), None),
        };
        let renderer_probe = audit.as_ref().ok().and_then(|report| {
            read_game_graphics(
                &self.paths.config_root().join(GAME_GRAPHICS_RECORD),
                &game_graphics_key(report.installed_runtime()),
            )
        });
        let (active_job, jobs, events, custom_source_records) = {
            let diagnostics = self.diagnostics_mut()?;
            let store = diagnostics
                .as_ref()
                .expect("diagnostics initialization was checked");
            (
                store.active_job().cloned(),
                store.recent_jobs().cloned().collect(),
                store.recent_events().collect(),
                store.recent_custom_sources().cloned().collect(),
            )
        };
        let sources = self
            .effective_sources()
            .map(|sources| {
                DownloadArtifact::ALL
                    .into_iter()
                    .filter(|artifact| artifact.applies())
                    .map(|artifact| {
                        let custom = sources.custom_url(artifact);
                        crate::report::SourceRow {
                            artifact: artifact.label(),
                            custom: custom.is_some(),
                            host: redacted_host(
                                &custom.map_or_else(|| artifact.builtin_url(), str::to_owned),
                            ),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let settings = self.settings.load().ok().map(|settings| {
            serde_json::json!({
                "renderer": format!("{:?}", settings.renderer()),
                "skip_to_bnet": settings.skip_to_bnet(),
                "sound_in_background": settings.sound_in_background(),
                "official_launcher_software_rendering":
                    settings.official_launcher_software_rendering(),
                "update_checks": settings.update_checks(),
                "custom_download_sources": settings
                    .download_sources()
                    .iter()
                    .map(|(artifact, url)| serde_json::json!({
                        "artifact": artifact.label(),
                        "host": redacted_host(url),
                    }))
                    .collect::<Vec<_>>(),
                "ignored_download_sources": settings.ignored_download_sources(),
            })
        });
        let installer_choice = self
            .installer_choice()
            .ok()
            .flatten()
            .map(|choice| match choice {
                InstallerChoice::Folder(_) => "installer folder",
                InstallerChoice::BlizzardDownload => "download from Blizzard",
                InstallerChoice::Existing(_) => "existing Diablo II",
            });
        Ok(crate::report::ReportInputs {
            now: crate::activity::now(),
            redactor: crate::redact::Redactor::from_environment(),
            build: crate::report::BuildInfo {
                version: env!("CARGO_PKG_VERSION"),
                commit: option_env!("PD2_LAUNCHER_COMMIT"),
                artifact_identity: portable_identity(launcher_artifact()),
                debug: cfg!(debug_assertions),
                x86_64_host_row: cfg!(target_arch = "x86_64"),
            },
            paths: crate::report::ReportPaths {
                managed_root,
                diagnostics_root: self.paths.diagnostics_root(),
                default_location: self.paths.uses_default_root(),
                game_folder: self
                    .game_folder()
                    .filter(|folder| folder.available)
                    .map(|folder| folder.path),
            },
            host: self
                .auditor
                .host
                .latest
                .lock()
                .ok()
                .and_then(|latest| latest.clone()),
            android: self.auditor.host.inner.android_host().cloned(),
            audit: Some(audit),
            downloads,
            renderer_probe,
            active_job,
            jobs,
            events,
            custom_source_records,
            sources,
            settings,
            installer_choice,
        })
    }

    fn record(&self, code: DiagnosticCode) -> Result<(), BackendFailure> {
        self.diagnostics_mut()?
            .as_mut()
            .expect("diagnostics initialization was checked")
            .record_event(code)
            .map_err(|_| backend_failure("diagnostics-unavailable"))
    }

    fn begin_job(
        &self,
        kind: JobKind,
        phase: WorkPhase,
        control: &CancellationToken,
    ) -> Result<ActiveDiagnostic<'_>, BackendFailure> {
        let id = self
            .diagnostics_mut()?
            .as_mut()
            .expect("diagnostics initialization was checked")
            .start_job(kind, phase)
            .map_err(|_| backend_failure("diagnostics-unavailable"))?;
        register_durable_progress(Arc::clone(&self.diagnostics), id, phase, control);
        Ok(ActiveDiagnostic {
            backend: self,
            id,
            deferred: false,
        })
    }

    fn finish_pending_job(&self) -> Result<(), BackendFailure> {
        let pending = self
            .pending_job
            .lock()
            .map_err(|_| backend_failure("diagnostics-unavailable"))?
            .take();
        if let Some((id, status)) = pending {
            self.diagnostics_mut()?
                .as_mut()
                .expect("diagnostics initialization was checked")
                .finish_job(id, status)
                .map_err(|_| backend_failure("diagnostics-unavailable"))?;
        }
        Ok(())
    }

    fn diagnostics_mut(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, Option<DiagnosticStore>>, BackendFailure> {
        let guard = self
            .diagnostics
            .lock()
            .map_err(|_| backend_failure("diagnostics-unavailable"))?;
        if guard.is_none() {
            return Err(backend_failure("diagnostics-unavailable"));
        }
        Ok(guard)
    }
}

struct ActiveDiagnostic<'a> {
    backend: &'a ReadyBackend,
    id: JobId,
    deferred: bool,
}

impl ActiveDiagnostic<'_> {
    fn progress(
        &self,
        phase: WorkPhase,
        unit: Option<crate::operation::ProgressUnit>,
        completed: Option<u64>,
        total: Option<u64>,
    ) -> Result<(), BackendFailure> {
        self.backend
            .diagnostics_mut()?
            .as_mut()
            .expect("diagnostics initialization was checked")
            .update_job(self.id, phase, unit, completed, total)
            .map_err(|_| backend_failure("diagnostics-unavailable"))
    }

    /// Leave the job for the next audit to finish with `result`'s status,
    /// recording a failure's code for the saved report.
    fn defer(mut self, result: &Result<(), BackendFailure>) -> Result<(), BackendFailure> {
        let status = job_status(result);
        if let Err(error) = result {
            crate::activity::record_for(
                self.id.value(),
                crate::activity::ActivityEvent::JobFailed {
                    code: error.code().to_owned(),
                },
            );
        }
        if status == JobStatus::Cancelled {
            self.backend
                .diagnostics_mut()?
                .as_mut()
                .expect("diagnostics initialization was checked")
                .request_safe_stop(self.id)
                .map_err(|_| backend_failure("diagnostics-unavailable"))?;
        }
        let mut pending = self
            .backend
            .pending_job
            .lock()
            .map_err(|_| backend_failure("diagnostics-unavailable"))?;
        if pending.is_some() {
            return Err(backend_failure("diagnostics-unavailable"));
        }
        *pending = Some((self.id, status));
        self.deferred = true;
        Ok(())
    }
}

impl Drop for ActiveDiagnostic<'_> {
    fn drop(&mut self) {
        if !self.deferred {
            let _ = self.backend.diagnostics_mut().and_then(|mut diagnostics| {
                diagnostics
                    .as_mut()
                    .expect("diagnostics initialization was checked")
                    .finish_job(self.id, JobStatus::Failed)
                    .map_err(|_| backend_failure("diagnostics-unavailable"))
            });
        }
    }
}

fn register_durable_progress(
    diagnostics: Arc<Mutex<Option<DiagnosticStore>>>,
    id: JobId,
    phase: WorkPhase,
    control: &CancellationToken,
) {
    let coalescing = Arc::new(Mutex::new((phase, Instant::now())));
    control.observe(Arc::new(move |progress| {
        let now = Instant::now();
        let should_persist = {
            let Ok(mut latest) = coalescing.lock() else {
                return;
            };
            if progress.phase() == latest.0 && now.duration_since(latest.1) < Duration::from_secs(1)
            {
                false
            } else {
                *latest = (progress.phase(), now);
                true
            }
        };
        if should_persist
            && let Ok(mut diagnostics) = diagnostics.lock()
            && let Some(store) = diagnostics.as_mut()
        {
            let _ = store.update_job_progress(id, progress);
        }
    }));
}

fn job_status(result: &Result<(), BackendFailure>) -> JobStatus {
    match result {
        Ok(()) => JobStatus::Succeeded,
        Err(error) if error.code() == "operation-cancelled" => JobStatus::Cancelled,
        Err(error) if error.code() == "operation-indeterminate" => JobStatus::Indeterminate,
        Err(_) => JobStatus::Failed,
    }
}

fn job_tone(status: JobStatus) -> EvidenceTone {
    match status {
        JobStatus::Running | JobStatus::SafeStopping => EvidenceTone::Working,
        JobStatus::Succeeded | JobStatus::Cancelled => EvidenceTone::Verified,
        JobStatus::Failed | JobStatus::Indeterminate => EvidenceTone::Failure,
    }
}

fn job_evidence(job: &JobRecord, tone: EvidenceTone) -> EvidenceView {
    let progress = job.progress().map_or_else(String::new, |progress| {
        let dimensions = [
            progress.file_bytes().map(|value| ("current", value)),
            progress.overall().map(|value| ("overall", value)),
            progress.verification().map(|value| ("verified", value)),
        ]
        .into_iter()
        .flatten()
        .map(|(label, value)| {
            let unit = match value.unit() {
                crate::operation::ProgressUnit::Bytes => "bytes",
                crate::operation::ProgressUnit::Files => "files",
                crate::operation::ProgressUnit::Stages => "stages",
            };
            value.total().map_or_else(
                || format!("{label} {} {unit} total unknown", value.completed()),
                |total| format!("{label} {}/{} {unit}", value.completed(), total),
            )
        })
        .collect::<Vec<_>>();
        if !dimensions.is_empty() {
            return format!(" · {}", dimensions.join(" · "));
        }
        let unit = match progress.unit() {
            crate::operation::ProgressUnit::Bytes => "bytes",
            crate::operation::ProgressUnit::Files => "files",
            crate::operation::ProgressUnit::Stages => "stages",
        };
        progress.total().map_or_else(
            || format!(" · {} {unit} · total unknown", progress.completed()),
            |total| format!(" · {}/{} {unit}", progress.completed(), total),
        )
    });
    EvidenceView::new(
        format!("{} job #{}", job.kind().message(), job.id().value()),
        format!(
            "{} · {}{progress}",
            job.status().message(),
            job.phase().message()
        ),
        tone,
    )
}

/// The host probe, keeping its latest evidence for the saved report.
struct RecordingHostProbe {
    inner: SystemHostProbe,
    latest: Mutex<Option<crate::audit::HostEvidence>>,
}

impl crate::audit::HostProbe for RecordingHostProbe {
    fn probe(
        &self,
        runtime: &crate::audit::RuntimePreflightRequest,
    ) -> io::Result<crate::audit::HostEvidence> {
        let evidence = self.inner.probe(runtime)?;
        if let Ok(mut latest) = self.latest.lock() {
            *latest = Some(evidence.clone());
        }
        Ok(evidence)
    }
}

struct ProductionAuditor {
    paths: LauncherPaths,
    auditor: Auditor,
    host: RecordingHostProbe,
    settings: SettingsStore,
    source_environment: BTreeMap<DownloadArtifact, String>,
}

impl ProductionAuditor {
    fn new(
        paths: LauncherPaths,
        host: SystemHostProbe,
        settings: SettingsStore,
        source_environment: BTreeMap<DownloadArtifact, String>,
    ) -> Self {
        Self {
            paths,
            auditor: Auditor::production(),
            host: RecordingHostProbe {
                inner: host,
                latest: Mutex::new(None),
            },
            settings,
            source_environment,
        }
    }

    /// Audit against the currently effective download sources, with the
    /// client held to the accepted manifest: the audit of every job (an
    /// update, the official launcher and its reconciliation, a move, Start
    /// over) and of the report.
    fn audit_report_with_lease(
        &self,
        activity: ActivityEvidence,
        lease: &dyn AuditLease,
        root: &Directory,
    ) -> Result<InstallationReport, BackendFailure> {
        let sources = effective_sources(&self.settings, &self.source_environment);
        self.audit_report_with_sources(activity, lease, root, &sources, false)
    }

    /// The audit that decides Play: the Launcher's own (its state) and the
    /// launch's. With the update checks off it takes the client as installed
    /// ([`client_as_installed`]); jobs keep the accepted manifest's rule, so
    /// the official launcher's update is still accepted into it.
    fn audit_report_for_play(
        &self,
        activity: ActivityEvidence,
        lease: &dyn AuditLease,
        root: &Directory,
    ) -> Result<InstallationReport, BackendFailure> {
        let sources = effective_sources(&self.settings, &self.source_environment);
        let as_installed = client_as_installed(&self.settings);
        self.audit_report_with_sources(activity, lease, root, &sources, as_installed)
    }

    /// A custom-unverified receipt satisfies the audit only for components
    /// whose source in `sources` is custom. `as_installed`: the client is
    /// taken as installed ([`AuditRequest::with_client_as_installed`]).
    fn audit_report_with_sources(
        &self,
        activity: ActivityEvidence,
        lease: &dyn AuditLease,
        root: &Directory,
        sources: &DownloadSources,
        as_installed: bool,
    ) -> Result<InstallationReport, BackendFailure> {
        let layout = anchored_update_layout(&self.paths, root)?;
        let transaction = UpdateTransactionProbe::new(layout.clone());
        let mut request = AuditRequest::new(self.paths.managed_root(), activity)
            .with_launcher_artifact(launcher_artifact());
        if self.android_host() {
            request = request.with_xinput_bridge();
        }
        if as_installed {
            request = request.with_client_as_installed();
        }
        for (artifact, component) in [
            (DownloadArtifact::Wine, CustomComponent::WineRuntime),
            (DownloadArtifact::Gecko, CustomComponent::Gecko),
            (DownloadArtifact::WineMono, CustomComponent::WineMono),
            (
                DownloadArtifact::DotnetDesktop,
                CustomComponent::DotnetDesktop,
            ),
        ] {
            if sources.custom_url(artifact).is_some() {
                request = request.with_custom_source(component);
            }
        }
        if let Some(manifest) = load_accepted_manifest(&layout)? {
            request = request.with_client_manifest(manifest);
        }
        Ok(self.auditor.audit_with_root(
            &request,
            AuditSources::new(&self.host, &transaction, lease),
            root,
        ))
    }
}

impl ProductionAuditor {
    fn install_audit(
        &self,
        paths: &crate::install::ManagedPaths,
        sources: &DownloadSources,
    ) -> Result<InstallAudit, AdapterFailure> {
        if paths.root() != self.paths.managed_root() {
            return Err(AdapterFailure::here());
        }
        let lock = paths
            .directory()
            .open_file(Path::new(".pd2launcher.lock"))
            .map_err(|_| AdapterFailure::here())?;
        let lease = RetainedFileAuditLease(lock);
        let report = self
            .audit_report_with_sources(
                ActivityEvidence::Idle,
                &lease,
                paths.directory(),
                sources,
                false,
            )
            .map_err(|_| AdapterFailure::here())?;
        let components = report.prefix_components();
        // A prefix whose own Wine files are not established is recreated
        // and needs every component; otherwise only rejected ones.
        let fresh = components.base != CheckStatus::Satisfied;
        let android = self.android_host();
        let audit = InstallAudit::new(
            report.installation_status(),
            report.host_support(),
            report.check(AuditCheck::Runtime),
            report.check(AuditCheck::Prefix),
        )
        .with_prefix_needs(if android {
            // The app provides the prefix (its template) and its .NET; the
            // Launcher installs neither Gecko nor Mono there.
            PrefixNeeds::default()
        } else {
            PrefixNeeds {
                gecko: fresh || components.gecko != CheckStatus::Satisfied,
                mono: fresh || components.mono != CheckStatus::Satisfied,
                dotnet: fresh || components.dotnet != CheckStatus::Satisfied,
            }
        })
        .with_builtin_fex_needed(!android && builtin_fex_pending(&self.settings, paths.root()));
        Ok(if android {
            audit.with_android_host()
        } else {
            audit
        })
    }

    /// Whether this backend's host is the Android app's: it runs
    /// ARM64 Wine, never FEX.
    fn android_host(&self) -> bool {
        self.host.inner.android_host().is_some()
    }
}

impl InstallationAuditor for ProductionAuditor {
    fn audit(&self, paths: &crate::install::ManagedPaths) -> Result<InstallAudit, AdapterFailure> {
        self.install_audit(
            paths,
            &effective_sources(&self.settings, &self.source_environment),
        )
    }
}

/// Download artifacts one job actually acquired or synchronized from.
#[derive(Default)]
struct UsedSources(Mutex<Vec<DownloadArtifact>>);

impl UsedSources {
    fn record(&self, artifact: DownloadArtifact) {
        if let Ok(mut used) = self.0.lock()
            && !used.contains(&artifact)
        {
            used.push(artifact);
        }
    }

    fn take(&self) -> Vec<DownloadArtifact> {
        self.0
            .lock()
            .map(|mut used| std::mem::take(&mut *used))
            .unwrap_or_default()
    }
}

struct RecordingArtifactSource<'a> {
    inner: &'a dyn ArtifactSource,
    used: &'a UsedSources,
}

impl ArtifactSource for RecordingArtifactSource<'_> {
    fn acquire_at(
        &self,
        spec: &ArtifactSpec,
        cache: &Path,
        cache_directory: &Directory,
        control: &crate::operation::OperationControl,
    ) -> Result<AcquiredArtifact, AdapterFailure> {
        let acquired = self
            .inner
            .acquire_at(spec, cache, cache_directory, control)?;
        if spec.is_custom() {
            self.used.record(spec.kind().download_artifact());
        }
        Ok(acquired)
    }
}

struct RecordingClientSynchronizer<'a> {
    inner: UpdaterClientSynchronizer,
    used: &'a UsedSources,
}

impl ClientSynchronizer for RecordingClientSynchronizer<'_> {
    fn prefetch(
        &self,
        paths: &ManagedPaths,
        control: &crate::operation::OperationControl,
    ) -> Result<(), AdapterFailure> {
        self.used.record(DownloadArtifact::Pd2ClientBase);
        self.inner.prefetch(paths, control)
    }

    fn synchronize(
        &self,
        paths: &ManagedPaths,
        control: &crate::operation::OperationControl,
    ) -> Result<(), AdapterFailure> {
        self.used.record(DownloadArtifact::Pd2ClientBase);
        self.inner.synchronize(paths, control)
    }

    fn accept_installed(
        &self,
        paths: &ManagedPaths,
        control: &crate::operation::OperationControl,
    ) -> Result<(), AdapterFailure> {
        self.used.record(DownloadArtifact::Pd2ClientBase);
        self.inner.accept_installed(paths, control)
    }

    /// Offline: no download source is used.
    fn record_copied(
        &self,
        paths: &ManagedPaths,
        control: &crate::operation::OperationControl,
    ) -> Result<(), AdapterFailure> {
        self.inner.record_copied(paths, control)
    }
}

/// The Auditor for one install job, bound to the sources resolved at the
/// job's start so every audit agrees with the job's downloads and runtime.
struct JobAuditor<'a> {
    auditor: &'a ProductionAuditor,
    sources: &'a DownloadSources,
}

impl InstallationAuditor for JobAuditor<'_> {
    fn audit(&self, paths: &crate::install::ManagedPaths) -> Result<InstallAudit, AdapterFailure> {
        self.auditor.install_audit(paths, self.sources)
    }
}

impl GameAuditor for ProductionAuditor {
    fn audit_for_launch(
        &self,
        managed_root: &Path,
        retained_root: &Directory,
    ) -> Result<GameAudit, GameAuditFailure> {
        if managed_root != self.paths.managed_root() {
            return Err(GameAuditFailure);
        }
        let lock = retained_root
            .open_file(Path::new(".pd2launcher.lock"))
            .map_err(|_| GameAuditFailure)?;
        let lease = RetainedFileAuditLease(lock);
        let report = self
            .audit_report_for_play(ActivityEvidence::Idle, &lease, retained_root)
            .map_err(|_| GameAuditFailure)?;
        let layout =
            anchored_update_layout(&self.paths, retained_root).map_err(|_| GameAuditFailure)?;
        let manifest = load_accepted_manifest(&layout)
            .map_err(|_| GameAuditFailure)?
            .ok_or(GameAuditFailure)?;
        let game_path = crate::RelativeGamePath::parse("Game.exe").map_err(|_| GameAuditFailure)?;
        let game_digest = manifest
            .digest_for(&game_path)
            .ok_or(GameAuditFailure)?
            .bytes();
        let runtime = report.installed_runtime();
        Ok(
            GameAudit::new(report.installation_status(), report.launch_preflight())
                .with_game_executable_md5(game_digest)
                .with_runtime(runtime),
        )
    }
}

/// Start over under the held lease: remove the Managed prefix once no
/// update is in flight. A recorded game folder that is unavailable is not
/// audited (it cannot be, and its update state lives in that folder, which
/// Start over never touches), so Start over stays the way to stop using it.
/// On the desktop the characters are kept first (`kept`): those of the
/// Managed game folder unless the record proves a
/// folder used in place (an unreadable record proves nothing), and those of
/// a `Save Path` the prefix's registry names inside the root at
/// `root_path`. When they can't be, nothing is removed. Returns what the
/// keep put in a backup.
fn start_over(
    root: &Directory,
    root_path: &Path,
    lease: &InstallationMutationLease,
    kept: Option<&crate::save_keeper::KeptSaves>,
    transaction: impl FnOnce() -> Result<CheckStatus, BackendFailure>,
) -> Result<crate::save_keeper::PutAside, BackendFailure> {
    start_over_on(
        root,
        root_path,
        lease,
        kept,
        transaction,
        crate::android_host(),
    )
}

/// [`start_over`] on the Android host (`android`) or the desktop. On the
/// Android host, while a started base-game marker stands (a Start over that
/// a stop left part way writes it first, `install::start_over_android`; a
/// base-game step that a stop left part way, which only Start over
/// resolves, leaves one too), Start over needs no transaction preflight:
/// a Start over that began has removed part of the game folder, so what is
/// left of the update records in it is no update in flight (the lease is
/// exclusive), and only Start over can go on. A stop inside the removal of
/// `ProjectD2/.pd2launcher` can leave a journal beside an emptied staging
/// folder, which the probe reads as Unsafe. The desktop does not change.
fn start_over_on(
    root: &Directory,
    root_path: &Path,
    lease: &InstallationMutationLease,
    kept: Option<&crate::save_keeper::KeptSaves>,
    transaction: impl FnOnce() -> Result<CheckStatus, BackendFailure>,
    android: bool,
) -> Result<crate::save_keeper::PutAside, BackendFailure> {
    let game = GameFolder::resolve(root);
    let begun = android
        && crate::stage_marker::read(root)
            == crate::stage_marker::MarkerState::Present(crate::InterruptedStage::BaseGame);
    if game.is_ok() && !begun && transaction()? != CheckStatus::Satisfied {
        return Err(backend_failure("start-over-preflight-rejected"));
    }
    let mut put_aside = crate::save_keeper::PutAside::default();
    if let Some(kept) = kept {
        let not_kept = |error: std::io::Error| {
            eprintln!(
                "pd2-launcher: the characters could not be kept, so Start over did not run: {error}"
            );
            backend_failure(crate::save_keeper::SAVES_NOT_KEPT)
        };
        let now = std::time::SystemTime::now();
        let in_place = matches!(
            game,
            Ok(GameFolder::External(_)) | Err(GameFolderError::Unavailable(_))
        );
        if !in_place {
            put_aside.add(
                crate::save_keeper::keep_managed(root, kept, now)
                    .map_err(not_kept)?
                    .put_aside(),
            );
        }
        let (_, elsewhere) =
            crate::save_keeper::keep_save_path(root, root_path, kept, now).map_err(not_kept)?;
        put_aside.add(elsewhere);
    }
    crate::install::reset_managed_prefix(root, &lease.0)
        .map_err(|_| backend_failure("start-over-failed"))?;
    Ok(put_aside)
}

/// Keep the Managed game folder's characters below `root` before the game or
/// the official launcher runs. A keep that fails never stops
/// them while the game folder still has the `Save` the game finds the
/// characters in: a `Save` a stopped keep stepped aside is put back, or the
/// link made again. When neither can be, the game does not start
/// (`saves-unavailable`). Returns what the keep put in a backup.
fn keep_before_play(
    root: &Directory,
    kept: &crate::save_keeper::KeptSaves,
) -> Result<crate::save_keeper::PutAside, BackendFailure> {
    match crate::save_keeper::keep_managed(root, kept, std::time::SystemTime::now()) {
        Ok(result) => {
            if let crate::save_keeper::Kept::Moved { saves, .. } = &result
                && !saves.is_empty()
            {
                eprintln!(
                    "pd2-launcher: moved {} characters and {} other saved files into {}",
                    saves.characters,
                    saves.files,
                    kept.path().display()
                );
            }
            Ok(result.put_aside())
        }
        Err(error) => {
            eprintln!("pd2-launcher: the characters stay in the game folder for now: {error}");
            crate::save_keeper::restore_managed(root, kept).map_err(|error| {
                eprintln!(
                    "pd2-launcher: the game folder has no Save the game would find the characters in, so it was not started: {error}"
                );
                backend_failure(crate::save_keeper::SAVES_UNAVAILABLE)
            })?;
            Ok(crate::save_keeper::PutAside::default())
        }
    }
}

struct InstallationMutationLease(File);

/// Audit capability bound to a live exclusive installation lease. This lets
/// update preflight evidence remain valid until verified promotion completes,
/// instead of reopening a path after the evidence scan.
struct HeldAuditLease<'a>(&'a InstallationMutationLease);

struct HeldAuditLeaseGuard<'a> {
    _lease: &'a InstallationMutationLease,
}

impl AuditLease for HeldAuditLease<'_> {
    fn try_acquire(&self) -> io::Result<Box<dyn AuditLeaseGuard + '_>> {
        Ok(Box::new(HeldAuditLeaseGuard { _lease: self.0 }))
    }
}

impl AuditLeaseGuard for HeldAuditLeaseGuard<'_> {}

struct RetainedFileAuditLease(File);

struct RetainedFileAuditLeaseGuard(File);

impl AuditLease for RetainedFileAuditLease {
    fn try_acquire(&self) -> io::Result<Box<dyn AuditLeaseGuard + '_>> {
        let file = self.0.try_clone()?;
        File::try_lock_shared(&file)?;
        Ok(Box::new(RetainedFileAuditLeaseGuard(file)))
    }
}

impl AuditLeaseGuard for RetainedFileAuditLeaseGuard {}

impl Drop for RetainedFileAuditLeaseGuard {
    fn drop(&mut self) {
        let _ = File::unlock(&self.0);
    }
}

impl InstallationMutationLease {
    fn acquire(root: &Directory) -> Result<Self, BackendFailure> {
        let file = root
            .open_file(Path::new(".pd2launcher.lock"))
            .map_err(|_| backend_failure("installation-busy"))?;
        if !file.metadata().is_ok_and(|metadata| metadata.is_file()) {
            return Err(backend_failure("installation-busy"));
        }
        File::try_lock(&file).map_err(|_| backend_failure("installation-busy"))?;
        Ok(Self(file))
    }
}

impl Drop for InstallationMutationLease {
    fn drop(&mut self) {
        let _ = File::unlock(&self.0);
    }
}

fn load_accepted_manifest(
    layout: &UpdateLayout,
) -> Result<Option<AcceptedClientManifest>, BackendFailure> {
    let Some(raw) = read_optional_bounded_at(layout, ACCEPTED_MANIFEST_LIMIT)? else {
        return Ok(None);
    };
    let manifest = Pd2Manifest::parse(ManifestResponse {
        requested_url: PD2_MANIFEST_URL.to_owned(),
        final_url: PD2_MANIFEST_URL.to_owned(),
        declared_length: Some(raw.len() as u64),
        body: raw,
    })
    .map_err(|_| backend_failure("accepted-manifest-invalid"))?;
    manifest
        .accepted()
        .map(Some)
        .map_err(|_| backend_failure("accepted-manifest-invalid"))
}

/// Bring the client cache to the manifest an update applies, so
/// it stays one whole client for Start over: objects the installed client
/// already holds are copied, the rest downloaded, and objects the manifest
/// no longer names removed. The updater then copies from the cache.
fn refresh_client_cache(
    root: &Directory,
    layout: &UpdateLayout,
    source: &mut dyn crate::network::Pd2Source,
    cancellation: &CancellationToken,
) -> Result<Directory, UpdateError> {
    let cache = root.ensure_directory(Path::new(crate::install::CLIENT_PREFETCH))?;
    let installed = layout.open_project().ok();
    crate::update::prefetch_client(source, &cache, installed.as_ref(), cancellation)?;
    Ok(cache)
}

fn anchored_update_layout(
    paths: &LauncherPaths,
    root: &Directory,
) -> Result<UpdateLayout, BackendFailure> {
    match GameFolder::read(root).map_err(|_| backend_failure("game-folder-unavailable"))? {
        GameFolder::Managed => Ok(UpdateLayout::anchored(
            paths.project_d2_root(),
            root.try_clone()
                .map_err(|_| backend_failure("launcher-state-unavailable"))?,
            project_d2_relative(),
        )),
        // A folder used in place: its ProjectD2, below the proven folder.
        GameFolder::External(external) => Ok(UpdateLayout::anchored(
            external.path().join("ProjectD2"),
            external
                .open()
                .map_err(|_| backend_failure("game-folder-unavailable"))?,
            PathBuf::from("ProjectD2"),
        )),
    }
}

fn downloads_view(readiness: DownloadReadiness) -> DownloadsView {
    match readiness {
        DownloadReadiness::Incomplete => DownloadsView::default(),
        DownloadReadiness::Complete {
            plan, total_bytes, ..
        } => DownloadsView {
            complete: true,
            installers: plan == DownloadPlan::Installers,
            total_bytes,
        },
    }
}

fn existing_view(
    existing: &ExistingInstall,
    problem: Option<ExistingProblemView>,
) -> InstallerMediaView {
    InstallerMediaView::Existing(ExistingInstallView {
        path: existing.path().to_owned(),
        problem,
        copy_bytes: existing.copy_bytes(),
        in_place_possible: crate::game_folder::usable_in_place(existing),
        project_diablo: existing.project_diablo(),
    })
}

fn existing_problem_view(problem: ExistingInstallProblem) -> ExistingProblemView {
    match problem {
        ExistingInstallProblem::NotFound => ExistingProblemView::NotFound,
        ExistingInstallProblem::Unreadable => ExistingProblemView::Unreadable,
        ExistingInstallProblem::NotDiablo => ExistingProblemView::NotDiablo,
        ExistingInstallProblem::NoExpansion => ExistingProblemView::NoExpansion,
        ExistingInstallProblem::MissingFile(name) => ExistingProblemView::MissingFile(name),
        ExistingInstallProblem::Links => ExistingProblemView::Links,
        ExistingInstallProblem::InsideLauncher => ExistingProblemView::InsideLauncher,
        ExistingInstallProblem::UnsupportedPath => ExistingProblemView::UnsupportedPath,
    }
}

fn read_optional_bounded_at(
    layout: &UpdateLayout,
    limit: u64,
) -> Result<Option<Vec<u8>>, BackendFailure> {
    let Some(file) = layout
        .open_accepted_manifest()
        .map_err(|_| backend_failure("launcher-state-unavailable"))?
    else {
        return Ok(None);
    };
    let metadata = file
        .metadata()
        .map_err(|_| backend_failure("launcher-state-unavailable"))?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(backend_failure("launcher-state-unavailable"));
    }
    let mut raw = Vec::with_capacity(metadata.len() as usize);
    file.take(limit + 1)
        .read_to_end(&mut raw)
        .map_err(|_| backend_failure("launcher-state-unavailable"))?;
    if raw.len() as u64 > limit {
        return Err(backend_failure("launcher-state-unavailable"));
    }
    Ok(Some(raw))
}

pub(super) fn launcher_artifact() -> LauncherArtifactEvidence {
    launcher_artifact_from_identity(env::consts::ARCH, COMPILED_ARTIFACT_IDENTITY)
}

/// The standalone executable's build embeds its identity, which attests the
/// binary by itself. Development builds have no compiled identity and
/// therefore cannot attest it. Each compile-target architecture accepts only
/// its own row's identity.
fn launcher_artifact_from_identity(
    architecture: &str,
    compiled_identity: Option<&str>,
) -> LauncherArtifactEvidence {
    let (identity, artifact) = match HostPlatform::from_architecture(architecture) {
        HostPlatform::Aarch64 => (
            AARCH64_BINARY_IDENTITY,
            LauncherArtifactEvidence::NativeAarch64StandaloneBinary,
        ),
        HostPlatform::X86_64 => (
            X86_64_BINARY_IDENTITY,
            LauncherArtifactEvidence::NativeX86_64StandaloneBinary,
        ),
        HostPlatform::Other(_) => return LauncherArtifactEvidence::Unverified,
    };
    if compiled_identity == Some(identity) {
        artifact
    } else {
        LauncherArtifactEvidence::Unverified
    }
}

/// The identity string of an attested artifact.
pub(super) fn portable_identity(artifact: LauncherArtifactEvidence) -> Option<&'static str> {
    match artifact {
        LauncherArtifactEvidence::NativeAarch64StandaloneBinary => Some(AARCH64_BINARY_IDENTITY),
        LauncherArtifactEvidence::NativeX86_64StandaloneBinary => Some(X86_64_BINARY_IDENTITY),
        LauncherArtifactEvidence::Unverified => None,
    }
}

/// Which client files accepting the official launcher's update may use.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OfficialClientFiles {
    /// After its session ended by itself: the files that still differ are
    /// downloaded.
    Download,
    /// After a stop: only the files already installed, with nothing
    /// downloaded.
    InstalledOnly,
}

/// What follows the official launcher's session, which ran under the job's
/// control `cancellation`. After a session that ended by itself, `reconcile`
/// accepts the client it updated with [`OfficialClientFiles::Download`],
/// under that control. Only a stop proven safe (the launcher and everything
/// it started have ended) lets `reconcile` accept the installed files
/// ([`OfficialClientFiles::InstalledOnly`]), under a control of its own, as
/// the job's is cancelled. The job then still ends as stopped, except when
/// the protected-file inventory kept the client from being accepted or the
/// outcome cannot be proven.
fn after_official_launcher(
    session: Result<(), AdapterFailure>,
    cancellation: &CancellationToken,
    reconcile: impl FnOnce(OfficialClientFiles, &CancellationToken) -> Result<(), BackendFailure>,
) -> Result<(), BackendFailure> {
    if !cancellation.is_requested() {
        session.map_err(|_| backend_failure("official-launcher-failed"))?;
        return reconcile(OfficialClientFiles::Download, cancellation);
    }
    if cancellation.cancellation_safety() != crate::operation::CancellationSafety::ProvenSafe {
        return Err(backend_failure("operation-indeterminate"));
    }
    // The job's control still shows progress: the check of the client.
    cancellation.report(crate::operation::WorkProgress::for_phase(
        WorkPhase::Verification,
    ));
    Err(
        match reconcile(
            OfficialClientFiles::InstalledOnly,
            &CancellationToken::default(),
        ) {
            Err(error)
                if matches!(
                    error.code(),
                    "official-client-not-reconciled" | "operation-indeterminate"
                ) =>
            {
                error
            }
            _ => backend_failure("operation-cancelled"),
        },
    )
}

/// An update that stopped where its outcome cannot be proven: Repair
/// resolves it.
fn update_indeterminate(error: &UpdateError) -> bool {
    matches!(
        error,
        UpdateError::Indeterminate
            | UpdateError::Transaction(crate::transaction::TransactionError::Indeterminate)
    )
}

/// The engine log's words on the client after the official launcher was
/// stopped. `refused`: an installed file differed from the manifest.
fn stopped_client_outcome(
    applied: &Result<crate::update::ApplyOutcome, UpdateError>,
    refused: bool,
) -> String {
    match applied {
        Ok(_) => "client accepted, nothing downloaded".to_owned(),
        Err(error) if update_indeterminate(error) => {
            format!("client not accepted: indeterminate ({error})")
        }
        Err(_) if refused => {
            "client not accepted: partial (a file differs from the current release)".to_owned()
        }
        Err(UpdateError::Network(error)) => format!("client not accepted: offline ({error})"),
        Err(error) => format!("client not accepted: {error}"),
    }
}

/// The official launcher runs the audited runtime in the audited prefix: the
/// game must be installed (Ready, or only the client out of date) with the
/// runtime and prefix accepted and no launch or transaction in the way.
fn official_launcher_permitted(report: &InstallationReport) -> bool {
    matches!(
        report.installation_status(),
        InstallationStatus::Ready | InstallationStatus::NeedsClient
    ) && report.host_support() == HostSupport::Supported
        && [
            AuditCheck::Runtime,
            AuditCheck::Prefix,
            AuditCheck::Transaction,
        ]
        .into_iter()
        .all(|check| report.check(check) == CheckStatus::Satisfied)
}

/// Why the official launcher binary as found may not run, or None when it
/// may. A missing binary has its own code: the game folder lacks
/// ProjectD2/PD2Launcher.exe (an incomplete imported copy, for example), and
/// only the player can fix the folder.
fn official_launcher_refusal(
    trust: OfficialLauncherTrust,
    confirmed: bool,
) -> Option<&'static str> {
    match trust {
        OfficialLauncherTrust::Verified => None,
        OfficialLauncherTrust::Unrecorded | OfficialLauncherTrust::Changed if confirmed => None,
        OfficialLauncherTrust::Unrecorded | OfficialLauncherTrust::Changed => {
            Some("official-launcher-confirmation-required")
        }
        OfficialLauncherTrust::Absent => Some("official-launcher-missing"),
        OfficialLauncherTrust::Indeterminate => Some("official-launcher-preflight-rejected"),
    }
}

fn host_view(host: HostSupport) -> HostView {
    match host {
        HostSupport::Supported => HostView::Supported,
        HostSupport::Unsupported | HostSupport::Indeterminate => HostView::Unsupported,
    }
}

fn report_evidence(report: &InstallationReport) -> Vec<EvidenceView> {
    [
        (AuditCheck::Host, "Host"),
        (AuditCheck::Runtime, "Managed runtime"),
        (AuditCheck::Prefix, PREFIX_EVIDENCE_LABEL),
        (AuditCheck::BaseGame, "Diablo II"),
        (AuditCheck::Expansion, "Lord of Destruction"),
        (AuditCheck::Client, "PD2 Game client"),
        (AuditCheck::Transaction, "Launcher transaction"),
    ]
    .into_iter()
    .map(|(check, label)| {
        let status = report.check(check);
        let detail = if check == AuditCheck::Host {
            host_prerequisite_detail(report)
        } else if custom_source_not_configured(report, check) {
            CUSTOM_NOT_CONFIGURED_DETAIL.to_owned()
        } else {
            check_status_label(status).to_owned()
        };
        EvidenceView::new(label, detail, check_status_tone(status))
    })
    .chain(match report.official_launcher() {
        OfficialLauncherTrust::Changed => Some(EvidenceView::new(
            "Official PD2 launcher",
            "PD2Launcher.exe differs from the accepted file list and from the copy this launcher recorded after its last session; you'll be asked before it runs",
            EvidenceTone::Advisory,
        )),
        OfficialLauncherTrust::Unrecorded => Some(EvidenceView::new(
            "Official PD2 launcher",
            "PD2Launcher.exe is not in the accepted file list and has not been run by this launcher yet; you'll be asked before it runs",
            EvidenceTone::Advisory,
        )),
        _ => None,
    })
    .chain(report.custom_sources().iter().map(|source| {
        EvidenceView::new(
            custom_component_label(source.component()),
            format!("Custom source (unverified) · {}", source.host()),
            EvidenceTone::Advisory,
        )
    }))
    .collect()
}

/// A Wine runtime or prefix component installed from a custom source while the
/// built-in source is selected: reported, never silently switched.
const CUSTOM_NOT_CONFIGURED_DETAIL: &str = "Installed from a custom source, but the built-in source is selected. The Launcher replaces it with the verified built-in during installation; save the custom source again to keep it.";

fn custom_source_not_configured(report: &InstallationReport, check: AuditCheck) -> bool {
    report.findings().iter().any(|finding| {
        finding.check() == check && finding.reason() == FindingReason::CustomSourceNotConfigured
    })
}

fn custom_component_label(component: CustomComponent) -> &'static str {
    match component {
        CustomComponent::WineRuntime => "Wine runtime",
        CustomComponent::Gecko => "Wine Gecko",
        CustomComponent::WineMono => "Wine Mono",
        CustomComponent::DotnetDesktop => ".NET Desktop Runtime 6",
    }
}

fn system_contract(report: &InstallationReport) -> SystemContractView {
    SystemContractView {
        host: match report.host_support() {
            HostSupport::Supported => "Everything the game needs is installed",
            HostSupport::Unsupported => "A few things are missing (listed above)",
            HostSupport::Indeterminate => "This computer couldn't be checked",
        }
        .to_owned(),
        runtime: report
            .custom_sources()
            .iter()
            .find(|source| source.component() == CustomComponent::WineRuntime)
            .filter(|_| report.check(AuditCheck::Runtime) == CheckStatus::Satisfied)
            .map_or_else(
                || {
                    if custom_source_not_configured(report, AuditCheck::Runtime) {
                        CUSTOM_NOT_CONFIGURED_DETAIL.to_owned()
                    } else {
                        managed_runtime_detail(
                            report.check(AuditCheck::Runtime),
                            report.installed_runtime(),
                        )
                    }
                },
                |source| {
                    format!(
                        "Custom Wine runtime from {} · custom source (unverified)",
                        source.host()
                    )
                },
            ),
        prefix: if custom_source_not_configured(report, AuditCheck::Prefix) {
            CUSTOM_NOT_CONFIGURED_DETAIL
        } else {
            managed_prefix_detail(report.check(AuditCheck::Prefix))
        }
        .to_owned(),
        artifact_health: installation_status_label(report.installation_status()).to_owned(),
        missing: if report.host_support() == HostSupport::Supported {
            Vec::new()
        } else {
            missing_prerequisites(
                report
                    .findings()
                    .iter()
                    .filter(|finding| finding.check() == AuditCheck::Host)
                    .map(|finding| finding.reason()),
            )
        },
        distro: distro_family(),
    }
}

/// The host findings as the plain list the main screen shows.
fn missing_prerequisites(reasons: impl IntoIterator<Item = FindingReason>) -> Vec<Prerequisite> {
    let mut missing = Vec::new();
    for reason in reasons {
        let prerequisite = match reason {
            FindingReason::ArchitectureUnsupported => Prerequisite::Architecture,
            FindingReason::DisplaySessionUnavailable => Prerequisite::DisplaySession,
            FindingReason::FexUnavailable => Prerequisite::Fex,
            FindingReason::PageSizeUnsupported => Prerequisite::PageSize,
            FindingReason::FexRootFsUnavailable => Prerequisite::FexRootFs,
            FindingReason::GuestExecutionUnavailable => Prerequisite::WineCannotRun,
            FindingReason::Ia32ExecutionUnavailable => Prerequisite::Ia32Execution,
            FindingReason::GuestExecutionDenied => Prerequisite::NoExec,
            FindingReason::I386LoaderUnavailable => Prerequisite::I386Loader,
            FindingReason::MultilibUnavailable => Prerequisite::Libraries,
            FindingReason::WineLibraryUnavailable(library) => Prerequisite::WineLibrary(library),
            FindingReason::LibunwindUnavailable => Prerequisite::Libunwind,
            FindingReason::X11DisplayUnavailable => Prerequisite::XWayland,
            FindingReason::VulkanLoaderUnavailable => Prerequisite::VulkanLoader,
            FindingReason::VulkanDriverUnavailable => Prerequisite::VulkanDriver,
            FindingReason::HardwareGraphicsUnavailable => Prerequisite::HardwareGraphics,
            FindingReason::LauncherArtifactUnverified => Prerequisite::LauncherBuild,
            FindingReason::HostUnavailable => Prerequisite::HostInformation,
            FindingReason::HostRowUncertified => Prerequisite::UncertifiedBuild,
            _ => continue,
        };
        if !missing.contains(&prerequisite) {
            missing.push(prerequisite);
        }
    }
    missing
}

/// The distribution family named by os-release(5), for install commands.
fn distro_family() -> DistroFamily {
    let read = |path: &str| {
        let mut text = String::new();
        std::fs::File::open(path)
            .and_then(|file| file.take(64 * 1024).read_to_string(&mut text))
            .map(|_| text)
    };
    read("/etc/os-release")
        .or_else(|_| read("/usr/lib/os-release"))
        .map_or(DistroFamily::Unknown, |text| {
            DistroFamily::from_os_release(&text)
        })
}

/// The last renderer probe's answer, in the Launcher's config directory.
const GAME_GRAPHICS_RECORD: &str = "game-graphics.json";

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct GameGraphicsRecord {
    key: String,
    renderer: Option<String>,
}

/// What the game's OpenGL renderer depends on: the runtime (pinned or
/// custom), the installed libraries (installing or removing a GPU driver rewrites the
/// loader cache), each GPU's kernel driver, and which display the session
/// offers.
fn game_graphics_key(runtime: crate::runtime::InstalledRuntime) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(match runtime {
        crate::runtime::InstalledRuntime::Custom => "custom",
        crate::runtime::InstalledRuntime::App => crate::runtime::APP_RUNTIME_DIRECTORY,
        crate::runtime::InstalledRuntime::BuiltIn(runtime) => runtime.identity,
    });
    let mut cache = Vec::new();
    if File::open("/etc/ld.so.cache")
        .and_then(|file| file.take(64 * 1024 * 1024).read_to_end(&mut cache))
        .is_ok()
    {
        hasher.update(&cache);
    }
    let mut drivers = std::fs::read_dir("/sys/class/drm")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|card| {
            let driver = std::fs::read_link(card.path().join("device/driver")).ok()?;
            Some(format!(
                "{}={}",
                card.file_name().to_string_lossy(),
                driver.file_name()?.to_string_lossy()
            ))
        })
        .collect::<Vec<_>>();
    drivers.sort();
    hasher.update(drivers.join(",").as_bytes());
    for name in ["DISPLAY", "WAYLAND_DISPLAY"] {
        hasher.update([u8::from(std::env::var_os(name).is_some())]);
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The recorded renderer for `key`: `Some(None)` when the probe got no
/// OpenGL context, `None` when nothing was recorded for this key.
fn read_game_graphics(path: &Path, key: &str) -> Option<Option<String>> {
    let mut text = String::new();
    File::open(path)
        .and_then(|file| file.take(4096).read_to_string(&mut text))
        .ok()?;
    let record = serde_json::from_str::<GameGraphicsRecord>(&text).ok()?;
    (record.key == key).then_some(record.renderer)
}

fn write_game_graphics(path: &Path, key: &str, renderer: Option<&str>) -> io::Result<()> {
    let record = GameGraphicsRecord {
        key: key.to_owned(),
        renderer: renderer.map(str::to_owned),
    };
    crate::persistence::atomic_write(
        path,
        &serde_json::to_vec(&record).map_err(io::Error::other)?,
    )
}

/// The Android app's graphics driver of the last Play, in the Launcher's
/// config directory: `compatibility` or `standard`. The engine starts with
/// it, so every Wine process gets that driver's values, the official
/// launcher's game too before any Play of the engine.
const GRAPHICS_DRIVER_RECORD: &str = "graphics-driver";

/// Whether the last Play asked for the Compatibility driver, by the record
/// in the config directory `config` (none: the standard driver).
fn compatibility_driver_saved(config: &Path) -> bool {
    crate::persistence::read_bounded_regular(&config.join(GRAPHICS_DRIVER_RECORD), 64)
        .is_ok_and(|text| text.trim_ascii() == b"compatibility")
}

/// Keeps this Play's graphics driver in the record in `config` for the
/// next start; written only when it changes.
fn keep_graphics_driver(config: &Path, compatibility: bool) -> io::Result<()> {
    if compatibility_driver_saved(config) == compatibility {
        return Ok(());
    }
    let driver = if compatibility {
        "compatibility\n"
    } else {
        "standard\n"
    };
    Directory::ensure_absolute(config)?.atomic_write(
        std::ffi::OsStr::new(GRAPHICS_DRIVER_RECORD),
        driver.as_bytes(),
    )
}

fn host_prerequisite_detail(report: &InstallationReport) -> String {
    host_prerequisite_detail_for(
        report.host_support(),
        report
            .findings()
            .iter()
            .filter(|finding| finding.check() == AuditCheck::Host)
            .map(|finding| finding.reason()),
        &HostPlatform::current(),
    )
}

/// Host remedies are generated for the running binary's row: an x86-64 user
/// is never told to install FEX or an ARM64 build, and an ARM64 user is never
/// told to install 32-bit x86 libraries.
fn host_prerequisite_detail_for(
    support: HostSupport,
    reasons: impl IntoIterator<Item = FindingReason>,
    platform: &HostPlatform,
) -> String {
    if support == HostSupport::Supported {
        return match platform {
            HostPlatform::Aarch64 => {
                "FEX, display, Vulkan, hardware graphics, and Launcher bundle verified"
            }
            // The x86-64 row checks no Vulkan or hardware graphics: the open
            // window already proved the Launcher's own graphics.
            HostPlatform::X86_64 => {
                "X11 display, Wine's 64-bit libraries, and Launcher bundle verified"
            }
            HostPlatform::Other(_) => "Host prerequisites verified",
        }
        .to_owned();
    }
    let mut uncertified = false;
    let mut missing = Vec::new();
    for reason in reasons {
        let remedy = match reason {
            FindingReason::ArchitectureUnsupported => {
                "a Launcher build for a supported CPU (ARM64 or x86-64)"
            }
            FindingReason::DisplaySessionUnavailable => "a Wayland or X11 session",
            FindingReason::FexUnavailable => {
                "FEX-Emu (FEX or FEXInterpreter in /usr/bin or /usr/local/bin)"
            }
            FindingReason::PageSizeUnsupported => "a 4K-page kernel for FEX-Emu",
            FindingReason::FexRootFsUnavailable => {
                "an x86-64 RootFS for FEX-Emu (FEXRootFSFetcher)"
            }
            FindingReason::GuestExecutionUnavailable => match platform {
                HostPlatform::X86_64 => "direct execution of the pinned WoW64 Wine",
                _ => "a supported way to run the pinned x86 Wine",
            },
            FindingReason::Ia32ExecutionUnavailable => {
                "kernel 32-bit x86 (IA-32) execution; remove ia32_emulation=0"
            }
            FindingReason::GuestExecutionDenied => {
                "exec permission for the Launcher data directory (no noexec mount)"
            }
            FindingReason::I386LoaderUnavailable => {
                "the 32-bit glibc loader /lib/ld-linux.so.2 (lib32-glibc / libc6:i386 / glibc.i686)"
            }
            FindingReason::MultilibUnavailable => match platform {
                // The WoW64 runtime links only 64-bit host libraries: the
                // loader named one that is missing.
                HostPlatform::X86_64 => "a 64-bit system library Wine could not load",
                _ => {
                    "32-bit (multilib) libraries: lib32-glibc / libc6:i386 / glibc.i686 plus 32-bit X11, OpenGL, FreeType, Fontconfig, GnuTLS, and ALSA or PulseAudio"
                }
            },
            FindingReason::LibunwindUnavailable => "64-bit libunwind (libunwind / libunwind8)",
            FindingReason::WineLibraryUnavailable(library) => match library {
                WineLibrary::X11 => "the 64-bit X11 library (libX11.so.6)",
                WineLibrary::Xext => "the 64-bit X11 extension library (libXext.so.6)",
                WineLibrary::FreeType => "the 64-bit FreeType library (libfreetype.so.6)",
                WineLibrary::Fontconfig => "the 64-bit Fontconfig library (libfontconfig.so.1)",
                WineLibrary::OpenGl => "64-bit OpenGL (libEGL.so.1 or libGL.so.1)",
            },
            FindingReason::X11DisplayUnavailable => "an X11 or XWayland display (DISPLAY)",
            FindingReason::VulkanLoaderUnavailable => "the Vulkan loader",
            FindingReason::VulkanDriverUnavailable => "a Vulkan driver",
            FindingReason::HardwareGraphicsUnavailable => "hardware Vulkan rendering",
            FindingReason::LauncherArtifactUnverified => match platform {
                HostPlatform::Aarch64 => "the verified ARM64 launcher download",
                HostPlatform::X86_64 => "the verified x86-64 launcher download",
                HostPlatform::Other(_) => "a verified launcher download",
            },
            FindingReason::HostUnavailable => "readable Linux host information",
            FindingReason::HostRowUncertified => {
                uncertified = true;
                continue;
            }
            _ => continue,
        };
        if !missing.contains(&remedy) {
            missing.push(remedy);
        }
    }
    let uncertified_note = "this x86-64 host row is not certified yet";
    match (missing.is_empty(), uncertified) {
        (true, false) => "Host prerequisites could not be established".to_owned(),
        (true, true) => format!("Host prerequisites present, but {uncertified_note}"),
        (false, false) => format!("Install or enable {}", missing.join(", ")),
        (false, true) => format!(
            "Install or enable {}; {uncertified_note}",
            missing.join(", ")
        ),
    }
}

fn managed_runtime_detail(
    status: CheckStatus,
    installed: crate::runtime::InstalledRuntime,
) -> String {
    let runtime = installed.display_name();
    if installed == crate::runtime::InstalledRuntime::App {
        // The app provides it; the Launcher never downloads it.
        return match status {
            CheckStatus::Satisfied => format!("{runtime} from the app verified"),
            _ => format!("{runtime} from the app is not ready"),
        };
    }
    match status {
        CheckStatus::Satisfied => format!("Launcher-managed {runtime} verified"),
        CheckStatus::Missing => format!("Not installed yet — Launcher will download {runtime}"),
        CheckStatus::Invalid => format!("Managed {runtime} files need attention"),
        CheckStatus::Indeterminate => format!("Managed {runtime} could not be inspected safely"),
    }
}

fn managed_prefix_detail(status: CheckStatus) -> &'static str {
    match status {
        CheckStatus::Satisfied => "Launcher-owned Wine prefix verified",
        CheckStatus::Missing => "Not created yet — Launcher will create a private Wine prefix",
        CheckStatus::Invalid => "Launcher-owned Wine prefix needs attention",
        CheckStatus::Indeterminate => "Wine prefix could not be inspected safely",
    }
}

fn installation_status_label(status: InstallationStatus) -> &'static str {
    match status {
        InstallationStatus::NeedsBaseGame => "Diablo II required",
        InstallationStatus::NeedsExpansion => "Lord of Destruction required",
        InstallationStatus::NeedsClient => "PD2 Game client required",
        InstallationStatus::NeedsRuntime => "Runtime components required",
        InstallationStatus::Ready => "All required evidence verified",
        InstallationStatus::RepairRequired => "Transaction repair required",
        InstallationStatus::Indeterminate => "Installation evidence indeterminate",
        InstallationStatus::StageInterrupted(InterruptedStage::Prefix) => {
            "Wine prefix creation was interrupted; Start over required"
        }
        InstallationStatus::StageInterrupted(InterruptedStage::BaseGame) => {
            "Diablo II installer was interrupted; Start over required"
        }
        InstallationStatus::StageInterrupted(InterruptedStage::Expansion) => {
            "Lord of Destruction installer was interrupted; Start over required"
        }
    }
}

fn check_status_label(status: CheckStatus) -> &'static str {
    match status {
        CheckStatus::Satisfied => "Verified",
        CheckStatus::Missing => "Required evidence is missing",
        CheckStatus::Invalid => "Evidence did not match the accepted contract",
        CheckStatus::Indeterminate => "Evidence could not be established safely",
    }
}

fn check_status_tone(status: CheckStatus) -> EvidenceTone {
    match status {
        CheckStatus::Satisfied => EvidenceTone::Verified,
        CheckStatus::Missing => EvidenceTone::Advisory,
        CheckStatus::Invalid | CheckStatus::Indeterminate => EvidenceTone::Failure,
    }
}

fn verified_evidence(label: impl Into<String>, detail: impl Into<String>) -> EvidenceView {
    EvidenceView::new(label, detail, EvidenceTone::Verified)
}

fn launch_settings_view(settings: &crate::persistence::LauncherSettings) -> LaunchSettingsView {
    LaunchSettingsView::new(
        match settings.renderer() {
            RendererSetting::Glide => RendererPreferenceView::Glide,
            RendererSetting::DirectDraw => RendererPreferenceView::DirectDraw,
        },
        settings.skip_to_bnet(),
        settings.sound_in_background(),
    )
    .with_official_launcher_software_rendering(settings.official_launcher_software_rendering())
    .with_app_menu_entry(settings.app_menu_entry())
    .with_install_source(match settings.install_source() {
        InstallSourceSetting::Official => InstallSourceView::Official,
        InstallSourceSetting::DirectDownload => InstallSourceView::DirectDownload,
    })
    .with_update_checks(settings.update_checks())
    .with_custom_arguments(settings.custom_arguments().to_owned())
    .with_game_program(settings.game_program().to_owned())
    .with_launcher_program(settings.launcher_program().to_owned())
    .with_launcher_custom_arguments(settings.launcher_custom_arguments().to_owned())
    .with_direct_draw_renderer(match settings.direct_draw_renderer() {
        crate::persistence::DirectDrawRendererSetting::Untouched
        | crate::persistence::DirectDrawRendererSetting::Pd2 => DirectDrawRendererView::Pd2,
        crate::persistence::DirectDrawRendererSetting::OpenGl => DirectDrawRendererView::OpenGl,
        crate::persistence::DirectDrawRendererSetting::Direct3d9 => {
            DirectDrawRendererView::Direct3d9
        }
        crate::persistence::DirectDrawRendererSetting::Software => DirectDrawRendererView::Software,
    })
}

fn direct_draw_setting(
    view: DirectDrawRendererView,
) -> crate::persistence::DirectDrawRendererSetting {
    use crate::persistence::DirectDrawRendererSetting as Setting;
    match view {
        DirectDrawRendererView::Pd2 => Setting::Pd2,
        DirectDrawRendererView::OpenGl => Setting::OpenGl,
        DirectDrawRendererView::Direct3d9 => Setting::Direct3d9,
        DirectDrawRendererView::Software => Setting::Software,
    }
}

fn game_launch_options(settings: &crate::persistence::LauncherSettings) -> GameLaunchOptions {
    let renderer = match settings.renderer() {
        RendererSetting::Glide => GameRenderer::Glide,
        RendererSetting::DirectDraw => GameRenderer::DirectDraw,
    };
    GameLaunchOptions::new(renderer, settings.skip_to_bnet())
        .with_sound_in_background(settings.sound_in_background())
        .with_custom_arguments(settings.custom_arguments())
        .with_program(settings.game_program())
        .with_direct_draw_renderer(settings.direct_draw_renderer().cnc_ddraw_value())
}

/// Only D2GL's first launch, before it has written its configuration, needs
/// the player to resize it once; every other outcome is silent.
fn game_launch_notice(d2gl_setup: D2glSetup) -> Option<GameNoticeView> {
    (d2gl_setup == D2glSetup::ConfigurationAbsent).then_some(GameNoticeView::FirstD2glLaunch)
}

/// The player turned Project Diablo 2's update checks off: the audits that
/// decide Play take the client as installed. Settings that cannot be read
/// keep the checks on, and with them the accepted manifest's rule.
fn client_as_installed(settings: &SettingsStore) -> bool {
    settings
        .load()
        .is_ok_and(|settings| !settings.update_checks())
}

/// `program-path-missing` when the game folder of the Managed root at
/// `managed_root` is there and holds no file at the program `path`; a game
/// folder that cannot be opened is no refusal (Play checks again).
fn require_program(managed_root: &Path, path: &str) -> Result<(), BackendFailure> {
    // PD2's own programs are never refused here: Default must always save,
    // and a missing PD2 program has its own refusals at Play and at the
    // official launcher.
    if path == crate::persistence::DEFAULT_GAME_PROGRAM
        || path == crate::persistence::DEFAULT_LAUNCHER_PROGRAM
    {
        return Ok(());
    }
    let Some(folder) = Directory::open(managed_root).ok().and_then(|root| {
        crate::game_folder::GameFolder::resolve(&root)
            .ok()
            .and_then(|folder| folder.open(&root).ok())
    }) else {
        return Ok(());
    };
    if folder
        .open_file(Path::new(path))
        .is_ok_and(|file| file.metadata().is_ok_and(|metadata| metadata.is_file()))
    {
        Ok(())
    } else {
        Err(backend_failure("program-path-missing"))
    }
}

fn update_launch_settings(
    store: &SettingsStore,
    managed_root: &Path,
    change: SettingsChange,
) -> Result<LaunchSettingsView, BackendFailure> {
    let settings = store
        .load()
        .map_err(|_| backend_failure("settings-load-failed"))?;
    let settings = match change {
        SettingsChange::Renderer(RendererPreferenceView::Glide) => {
            settings.with_renderer(RendererSetting::Glide)
        }
        SettingsChange::Renderer(RendererPreferenceView::DirectDraw) => {
            settings.with_renderer(RendererSetting::DirectDraw)
        }
        SettingsChange::SkipToBnet(enabled) => settings.with_skip_to_bnet(enabled),
        SettingsChange::SoundInBackground(enabled) => settings.with_sound_in_background(enabled),
        SettingsChange::OfficialLauncherSoftwareRendering(enabled) => {
            settings.with_official_launcher_software_rendering(enabled)
        }
        SettingsChange::DirectDrawRenderer(renderer) => {
            settings.with_direct_draw_renderer(direct_draw_setting(renderer))
        }
        SettingsChange::AppMenuEntry(enabled) => settings.with_app_menu_entry(enabled),
        SettingsChange::UpdateChecks(enabled) => settings.with_update_checks(enabled),
        SettingsChange::InstallSource(source) => settings.with_install_source(match source {
            InstallSourceView::Official => InstallSourceSetting::Official,
            InstallSourceView::DirectDownload => InstallSourceSetting::DirectDownload,
        }),
        SettingsChange::CustomArguments(ref arguments) => {
            settings.with_custom_arguments(arguments.clone())
        }
        SettingsChange::GameProgram(ref path) => {
            require_program(managed_root, path)?;
            settings.with_game_program(path.clone())
        }
        SettingsChange::LauncherProgram(ref path) => {
            require_program(managed_root, path)?;
            settings.with_launcher_program(path.clone())
        }
        SettingsChange::LauncherCustomArguments(ref arguments) => {
            settings.with_launcher_custom_arguments(arguments.clone())
        }
    };
    store
        .save(&settings)
        .map_err(|_| backend_failure("settings-save-failed"))?;
    #[cfg(feature = "gui")]
    if let SettingsChange::AppMenuEntry(enabled) = change {
        sync_app_menu(enabled);
    }
    Ok(launch_settings_view(&settings))
}

/// Effective sources: GUI setting > environment > built-in. An
/// unreadable settings file never blocks installs, updates, or audits; the
/// environment and built-in sources still apply.
fn effective_sources(
    settings: &SettingsStore,
    environment: &BTreeMap<DownloadArtifact, String>,
) -> DownloadSources {
    let saved = settings
        .load()
        .map(|settings| settings.download_sources().clone())
        .unwrap_or_default();
    DownloadSources::resolve(&saved, environment)
}

/// Saved download sources that were dropped on load (unknown artifact or
/// invalid URL) are reported instead of failing installs or settings.
fn ignored_sources_advisory(settings: &SettingsStore) -> Option<EvidenceView> {
    let ignored = settings.load().ok()?.ignored_download_sources();
    (ignored > 0).then(|| {
        EvidenceView::new(
            "Download sources",
            format!(
                "{ignored} invalid download source{} ignored; built-in or environment sources apply. Save or reset a source to rewrite the settings.",
                if ignored == 1 { "" } else { "s" }
            ),
            EvidenceTone::Advisory,
        )
    })
}

fn client_base(sources: &DownloadSources) -> Pd2ClientBase {
    sources
        .custom_url(DownloadArtifact::Pd2ClientBase)
        .map_or(Pd2ClientBase::BuiltIn, |url| {
            Pd2ClientBase::Custom(url.to_owned())
        })
}

/// For the Wine runtime, Gecko, Wine Mono, and .NET Desktop Runtime rows:
/// what is actually installed, when that differs from the configured source.
fn installed_source_notes(
    root: &Directory,
    sources: &DownloadSources,
) -> BTreeMap<DownloadArtifact, String> {
    [
        (DownloadArtifact::Wine, CustomComponent::WineRuntime),
        (DownloadArtifact::Gecko, CustomComponent::Gecko),
        (DownloadArtifact::WineMono, CustomComponent::WineMono),
        (DownloadArtifact::DotnetDesktop, CustomComponent::DotnetDesktop),
    ]
    .into_iter()
    .filter_map(|(artifact, component)| {
        let installed = crate::audit::installed_provenance(root, component);
        let configured_custom = sources.custom_url(artifact).is_some();
        let note = match (configured_custom, installed) {
            // A custom runtime is used while the Wine source is custom; a
            // pinned Gecko, Mono, or .NET receipt always outranks a custom
            // one.
            (true, provenance)
                if provenance.pinned
                    && (component != CustomComponent::WineRuntime
                        || provenance.custom_host.is_none()) =>
            {
                "Installed: built-in (verified). The custom source applies to the next download."
                    .to_owned()
            }
            (false, provenance) if !provenance.pinned => {
                let host = provenance.custom_host?;
                format!(
                    "Installed: custom (unverified) from {host}. It is not used with the built-in source; the Launcher replaces it with the verified built-in when it installs, or save the custom URL again to keep it."
                )
            }
            _ => return None,
        };
        Some((artifact, note))
    })
    .collect()
}

fn download_source_views(
    sources: &DownloadSources,
    installed: &BTreeMap<DownloadArtifact, String>,
) -> Vec<DownloadSourceView> {
    DownloadArtifact::ALL
        .into_iter()
        .filter(|artifact| artifact.applies())
        .map(|artifact| DownloadSourceView {
            artifact,
            effective_url: sources.effective_url(artifact),
            badge: match sources.origin(artifact) {
                SourceOrigin::BuiltIn => SourceBadgeView::BuiltInVerified,
                SourceOrigin::Environment => SourceBadgeView::CustomFromEnvironment,
                SourceOrigin::Settings => SourceBadgeView::Custom,
            },
            error: None,
            installed: installed.get(&artifact).cloned(),
        })
        .collect()
}

/// Save (`Some`) or remove (`None`) a GUI override. An empty value or the
/// built-in URL itself removes the override instead of pinning a "custom"
/// copy of the built-in source, which would skip its verification.
fn update_download_source_setting(
    store: &SettingsStore,
    artifact: DownloadArtifact,
    url: Option<String>,
) -> Result<(), BackendFailure> {
    let url = match url {
        Some(url) if url.trim().is_empty() => None,
        Some(url) => {
            let normalized = normalize_custom_url(artifact, &url)
                .map_err(|error| backend_failure(error.code()))?;
            (!is_builtin_url(artifact, &normalized)).then_some(normalized)
        }
        None => None,
    };
    let settings = store
        .load()
        .map_err(|_| backend_failure("settings-load-failed"))?
        .with_download_source(artifact, url);
    store
        .save(&settings)
        .map_err(|_| backend_failure("settings-save-failed"))
}

/// The stable code of an install that stopped at a known stage, so the view
/// can say which step failed and what to do next.
fn install_failure_code(error: &InstallError) -> &'static str {
    use crate::install::{ArtifactKind, DirectFailure, DownloaderFailure, InstallStage};
    match error {
        InstallError::KeyRejected {
            stage: InstallStage::BaseGame,
        } => "base-game-key-rejected",
        InstallError::KeyRejected {
            stage: InstallStage::Expansion,
        } => "expansion-key-rejected",
        InstallError::InstallerError {
            stage: InstallStage::BaseGame,
        } => "base-game-installer-error",
        InstallError::InstallerError {
            stage: InstallStage::Expansion,
        } => "expansion-installer-error",
        InstallError::InstallerMenu {
            stage: InstallStage::BaseGame,
        } => "base-game-installer-menu",
        InstallError::InstallerMenu {
            stage: InstallStage::Expansion,
        } => "expansion-installer-menu",
        InstallError::StageFailed { stage }
        | InstallError::EvidenceDidNotAdvance { stage, .. }
        | InstallError::KeyRejected { stage }
        | InstallError::InstallerError { stage }
        | InstallError::InstallerMenu { stage } => match stage {
            InstallStage::Download => "download-failed",
            InstallStage::ManagedRuntime => "runtime-setup-failed",
            // Not the downloader (DownloaderFailed) nor fetching it: what
            // follows a verified download, such as storing the files.
            InstallStage::BaseGameDownload | InstallStage::ExpansionDownload => {
                "installer-payload-store-failed"
            }
            InstallStage::ClientPrefetch => "client-download-failed",
            InstallStage::BaseGame => "base-game-install-failed",
            InstallStage::Expansion => "expansion-install-failed",
            InstallStage::Import => "import-failed",
            InstallStage::ClientBootstrap | InstallStage::ClientSynchronization => {
                "client-install-failed"
            }
            InstallStage::BuiltinFex => "builtin-fex-setup-failed",
        },
        // How Blizzard's downloader failed, each its own code, as only the
        // first is a connection problem for certain.
        InstallError::DownloaderFailed { failure, .. } => match failure {
            DownloaderFailure::Stalled => "installer-download-stalled",
            DownloaderFailure::ErrorDialog => "installer-downloader-error",
            DownloaderFailure::Incomplete => "installer-download-incomplete",
            DownloaderFailure::Rejected => "installer-download-rejected",
            DownloaderFailure::Ended => "installer-downloader-failed",
        },
        InstallError::KeyInvalid { stage, problem } => key_failure_code(*stage, *problem),
        // How the direct download failed, each its own code.
        InstallError::DirectDownloadFailed { failure, .. } => match failure {
            DirectFailure::Refused => "direct-download-refused",
            DirectFailure::Incomplete => "direct-download-incomplete",
            DirectFailure::Mismatch => "direct-download-mismatch",
        },
        InstallError::BaseGameRequired => "base-game-required",
        InstallError::BaseGameInstalled => "base-game-installed",
        InstallError::ExistingInstallInvalid(_) => "existing-install-invalid",
        InstallError::GameFolderUnavailable => "game-folder-unavailable",
        InstallError::ClientNotKept => "client-not-kept",
        InstallError::AuthorizationRequired => "authorization-required",
        InstallError::DownloadCacheUnusable { .. } => "download-cache-unusable",
        InstallError::AcquisitionFailed { artifact }
        | InstallError::ArtifactRejected { artifact, .. } => match artifact {
            ArtifactKind::BaseGameWrapper | ArtifactKind::ExpansionWrapper => {
                "installer-download-failed"
            }
            _ => "download-failed",
        },
        _ => "managed-install-failed",
    }
}

/// Why a CD key failed the installers' own check, for its field.
fn key_failure_code(
    stage: crate::install::InstallStage,
    problem: crate::blizzard::KeyProblem,
) -> &'static str {
    use crate::blizzard::KeyProblem;
    let expansion = stage == crate::install::InstallStage::Expansion;
    match (expansion, problem) {
        (false, KeyProblem::WrongLength) => "base-game-key-wrong-length",
        (false, KeyProblem::OldKeyLength) => "base-game-key-16-characters",
        (false, KeyProblem::InvalidCharacter) => "base-game-key-invalid-character",
        (false, KeyProblem::ExpansionKeyInBaseField) => "base-game-key-is-expansion-key",
        (false, KeyProblem::BaseKeyInExpansionField | KeyProblem::WrongProduct) => {
            "base-game-key-not-valid"
        }
        (true, KeyProblem::WrongLength) => "expansion-key-wrong-length",
        (true, KeyProblem::OldKeyLength) => "expansion-key-16-characters",
        (true, KeyProblem::InvalidCharacter) => "expansion-key-invalid-character",
        (true, KeyProblem::BaseKeyInExpansionField) => "expansion-key-is-base-game-key",
        (true, KeyProblem::ExpansionKeyInBaseField | KeyProblem::WrongProduct) => {
            "expansion-key-not-valid"
        }
    }
}

fn reject_cancelled(cancellation: &CancellationToken) -> Result<(), BackendFailure> {
    if cancellation.is_requested() {
        Err(backend_failure("operation-cancelled"))
    } else {
        Ok(())
    }
}

const fn backend_failure(code: &'static str) -> BackendFailure {
    BackendFailure::new(code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation::WorkProgress;
    use std::thread;
    use tempfile::TempDir;

    #[test]
    fn a_missing_official_launcher_binary_has_its_own_failure() {
        use OfficialLauncherTrust::{Absent, Changed, Indeterminate, Unrecorded, Verified};
        for confirmed in [false, true] {
            assert_eq!(official_launcher_refusal(Verified, confirmed), None);
            assert_eq!(
                official_launcher_refusal(Absent, confirmed),
                Some("official-launcher-missing")
            );
            assert_eq!(
                official_launcher_refusal(Indeterminate, confirmed),
                Some("official-launcher-preflight-rejected")
            );
        }
        for trust in [Unrecorded, Changed] {
            assert_eq!(
                official_launcher_refusal(trust, false),
                Some("official-launcher-confirmation-required")
            );
            assert_eq!(official_launcher_refusal(trust, true), None);
        }
    }

    #[test]
    fn a_device_fex_must_be_the_full_path_of_an_executable_file() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = TempDir::new().unwrap();
        let fex = fixture.path().join("FEX");
        assert_eq!(
            fex_path_problem(Path::new("FEX")),
            Some("Enter the full path of the FEX program.")
        );
        assert_eq!(
            fex_path_problem(&fex),
            Some("There's no executable file there.")
        );
        std::fs::write(&fex, b"#!/bin/sh\n").unwrap();
        assert_eq!(
            fex_path_problem(&fex),
            Some("There's no executable file there.")
        );
        std::fs::set_permissions(&fex, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(fex_path_problem(&fex), None);
        assert_eq!(
            fex_path_problem(fixture.path()),
            Some("There's no executable file there.")
        );
        const { assert!(!BUILTIN_FEX_AVAILABLE, "built-in FEX is not offered yet") };
    }

    #[test]
    fn a_stale_registry_save_is_removed_only_while_nothing_holds_the_installation() {
        let fixture = TempDir::new().unwrap();
        let root = fixture.path();
        std::fs::create_dir(root.join("prefix")).unwrap();
        std::fs::write(root.join(".pd2launcher.lock"), b"").unwrap();
        let stale = root.join("prefix/savereg.tmp");
        std::fs::write(&stale, b"half a registry").unwrap();
        // A setup step or game holds the installation: kept.
        let held = File::open(root.join(".pd2launcher.lock")).unwrap();
        File::lock_shared(&held).unwrap();
        remove_stale_registry_save(root);
        assert!(stale.exists());
        // Unlock before closing: a child process another test forks at this
        // moment holds a copy of the descriptor until it execs, which would
        // keep a merely closed lock held.
        File::unlock(&held).unwrap();
        drop(held);
        remove_stale_registry_save(root);
        assert!(!stale.exists());
    }

    #[test]
    fn an_android_host_refuses_every_fex_setting() {
        let fixture = TempDir::new().unwrap();
        let paths = LauncherPaths::from_home(fixture.path().join("launcher"));
        let facts = crate::control_channel::HostFacts {
            android_release: "13".to_owned(),
            sdk: 33,
            device_model: "Retroid Pocket Nova".to_owned(),
            gpu_name: "Adreno (TM) 740".to_owned(),
            chip_id: 0x4305_0a01,
            kernel_release: "5.15.153-android13-8".to_owned(),
            page_size: 4096,
            data_dir: "/data/data/com.pd2launcher.app".to_owned(),
        };
        let guest = crate::host_execution::GuestExecutor::Direct;
        let backend = ReadyBackend::new(
            SystemHostProbe::new(None, guest.clone()).with_android_host(facts),
            SystemHostExecution::new(guest),
            LaunchMode::Engine,
            Some(paths.clone()),
        )
        .unwrap_or_else(|_| panic!("backend"));
        assert!(backend.auditor.android_host());
        assert!(backend.fex_setting().is_none());
        for source in [FexSourceView::Builtin, FexSourceView::Device] {
            assert_eq!(
                backend
                    .choose_fex_source(source)
                    .err()
                    .map(|failure| failure.code().to_owned()),
                Some("fex-setting-unavailable".to_owned())
            );
        }
        assert_eq!(
            backend
                .save_fex(Some(PathBuf::from("/usr/bin/FEX")))
                .err()
                .map(|failure| failure.code().to_owned()),
            Some("fex-setting-unavailable".to_owned())
        );
        assert!(!paths.settings_file().exists(), "nothing was saved");
        // On Android the app provides the runtime, its prefix template and
        // its .NET: nothing of the prefix is downloaded, and never Wine Gecko
        // or Wine Mono.
        use crate::install::{InstallationAuditor as _, ManagedPaths};
        let root = paths.managed_root();
        let managed = ManagedPaths::prepare(&root).unwrap();
        if !root.join(".pd2launcher.lock").exists() {
            std::fs::write(root.join(".pd2launcher.lock"), b"").unwrap();
        }
        let audit = |backend: &ReadyBackend| {
            backend
                .auditor
                .audit(&managed)
                .unwrap_or_else(|_| panic!("audit"))
        };
        let android = audit(&backend);
        assert_eq!(
            android.prefix_needs(),
            crate::install::PrefixNeeds::default()
        );
        assert!(android.android_host());
        // The desktop offers it (on its ARM64 builds).
        let desktop = ReadyBackend::new(
            SystemHostProbe::new(None, crate::host_execution::GuestExecutor::Direct),
            SystemHostExecution::new(crate::host_execution::GuestExecutor::Direct),
            LaunchMode::Engine,
            Some(paths),
        )
        .unwrap_or_else(|_| panic!("backend"));
        assert!(!desktop.auditor.android_host());
        // Its plan is the desktop's.
        let desktop = audit(&desktop);
        assert!(desktop.prefix_needs().dotnet && !desktop.android_host());
        assert!(desktop.prefix_needs().gecko && desktop.prefix_needs().mono);
    }

    #[test]
    fn the_last_plays_graphics_driver_is_in_use_from_the_next_engines_start() {
        /// The standard driver again when the test ends, also after a failed
        /// assertion, so the next test starts from it.
        struct Standard;
        impl Drop for Standard {
            fn drop(&mut self) {
                crate::wine_environment::use_compatibility_driver(false);
            }
        }
        let _driver = crate::wine_environment::DRIVER_IN_USE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _standard = Standard;
        let fixture = TempDir::new().unwrap();
        let paths = LauncherPaths::from_home(fixture.path().join("launcher"));
        // A new engine, on the Android host or the desktop.
        let engine = |android: bool| {
            let guest = crate::host_execution::GuestExecutor::Direct;
            let mut probe = SystemHostProbe::new(None, guest.clone());
            if android {
                probe = probe.with_android_host(crate::control_channel::HostFacts {
                    android_release: "13".to_owned(),
                    sdk: 33,
                    device_model: "Retroid Pocket Nova".to_owned(),
                    gpu_name: "Adreno (TM) 740".to_owned(),
                    chip_id: 0x4305_0a01,
                    kernel_release: "5.15.153-android13-8".to_owned(),
                    page_size: 4096,
                    data_dir: "/data/data/com.pd2launcher.app".to_owned(),
                });
            }
            ReadyBackend::new(
                probe,
                SystemHostExecution::new(guest),
                LaunchMode::Engine,
                Some(paths.clone()),
            )
            .unwrap_or_else(|_| panic!("backend"))
        };
        // Whether a Wine process started now gets the Compatibility values.
        let in_use = || {
            let app = vec![(
                std::ffi::OsString::from("PD2_APP_COMPATIBILITY_TU_DEBUG"),
                std::ffi::OsString::from("sysmem,noubwc"),
            )];
            crate::wine_environment::compose_on(app, &[], true)
                .iter()
                .any(|(name, value)| name == "TU_DEBUG" && value == "sysmem,noubwc")
        };
        // A new process starts with the standard driver in use.
        let restart = || crate::wine_environment::use_compatibility_driver(false);
        // A Play with the Compatibility driver (no game starts here).
        let _ = engine(true).launch_game(GraphicsDriver::Compatibility, None, false, false);
        assert!(in_use());
        // The next engine uses it from its start: the official launcher's
        // game gets it before any Play.
        restart();
        let _next = engine(true);
        assert!(in_use());
        // A Play with the standard driver, and the engine after it.
        let _ = engine(true).launch_game(GraphicsDriver::Standard, None, false, false);
        assert!(!in_use());
        restart();
        let _next = engine(true);
        assert!(!in_use());
        // The desktop keeps no driver and uses none.
        let _ = engine(true).launch_game(GraphicsDriver::Compatibility, None, false, false);
        restart();
        let desktop = engine(false);
        assert!(!in_use());
        let _ = desktop.launch_game(GraphicsDriver::Standard, None, false, false);
        assert!(compatibility_driver_saved(paths.config_root()));
    }

    #[test]
    fn the_installer_is_told_when_the_built_in_fex_is_still_to_come() {
        use crate::persistence::{FexSource, LauncherSettings};
        let fixture = TempDir::new().unwrap();
        let settings = SettingsStore::new(fixture.path().join("config/settings.json"));
        let root = fixture.path().join("game");
        std::fs::create_dir_all(root.join("prefix")).unwrap();
        // An installation set up with the device's FEX keeps it.
        assert!(!builtin_fex_pending(&settings, &root));
        settings
            .save(&LauncherSettings::default().with_fex_source(FexSource::Builtin))
            .unwrap();
        // Only ARM64 builds use the built-in FEX.
        assert_eq!(
            builtin_fex_pending(&settings, &root),
            cfg!(target_arch = "aarch64")
        );
        // The step 1 default is kept once downloads begin.
        let fresh = TempDir::new().unwrap();
        let fresh_settings = SettingsStore::new(fresh.path().join("config/settings.json"));
        keep_default_fex_choice(&fresh_settings, &fresh.path().join("game"));
        let saved = fresh_settings.load().unwrap();
        assert_eq!(saved.fex_source_chosen(), cfg!(target_arch = "aarch64"));
        if cfg!(target_arch = "aarch64") {
            assert_eq!(saved.fex_source(), FexSource::Builtin);
        }
    }

    #[test]
    fn install_failures_name_the_stage_and_a_rejected_key() {
        use crate::install::{ArtifactKind, DirectFailure, DownloaderFailure, InstallStage};
        for (error, code) in [
            (
                InstallError::KeyRejected {
                    stage: InstallStage::BaseGame,
                },
                "base-game-key-rejected",
            ),
            (
                InstallError::KeyRejected {
                    stage: InstallStage::Expansion,
                },
                "expansion-key-rejected",
            ),
            (
                InstallError::InstallerError {
                    stage: InstallStage::BaseGame,
                },
                "base-game-installer-error",
            ),
            (
                InstallError::InstallerMenu {
                    stage: InstallStage::BaseGame,
                },
                "base-game-installer-menu",
            ),
            (
                InstallError::InstallerMenu {
                    stage: InstallStage::Expansion,
                },
                "expansion-installer-menu",
            ),
            (
                InstallError::StageFailed {
                    stage: InstallStage::Expansion,
                },
                "expansion-install-failed",
            ),
            (
                InstallError::StageFailed {
                    stage: InstallStage::ManagedRuntime,
                },
                "runtime-setup-failed",
            ),
            (
                InstallError::AcquisitionFailed {
                    artifact: ArtifactKind::BaseGameWrapper,
                },
                "installer-download-failed",
            ),
            (
                InstallError::AcquisitionFailed {
                    artifact: ArtifactKind::ManagedRuntime,
                },
                "download-failed",
            ),
            // The built-in FEX's package after every source of it failed.
            (
                InstallError::AcquisitionFailed {
                    artifact: ArtifactKind::FexPackage,
                },
                "download-failed",
            ),
            (
                InstallError::StageFailed {
                    stage: InstallStage::BaseGameDownload,
                },
                "installer-payload-store-failed",
            ),
            (
                InstallError::DownloaderFailed {
                    stage: InstallStage::BaseGameDownload,
                    failure: DownloaderFailure::Stalled,
                },
                "installer-download-stalled",
            ),
            (
                InstallError::DownloaderFailed {
                    stage: InstallStage::ExpansionDownload,
                    failure: DownloaderFailure::ErrorDialog,
                },
                "installer-downloader-error",
            ),
            (
                InstallError::DownloaderFailed {
                    stage: InstallStage::BaseGameDownload,
                    failure: DownloaderFailure::Incomplete,
                },
                "installer-download-incomplete",
            ),
            (
                InstallError::DownloaderFailed {
                    stage: InstallStage::BaseGameDownload,
                    failure: DownloaderFailure::Rejected,
                },
                "installer-download-rejected",
            ),
            (
                InstallError::DownloaderFailed {
                    stage: InstallStage::BaseGameDownload,
                    failure: DownloaderFailure::Ended,
                },
                "installer-downloader-failed",
            ),
            (
                InstallError::DirectDownloadFailed {
                    stage: InstallStage::BaseGameDownload,
                    failure: DirectFailure::Refused,
                },
                "direct-download-refused",
            ),
            (
                InstallError::KeyInvalid {
                    stage: InstallStage::BaseGame,
                    problem: crate::blizzard::KeyProblem::ExpansionKeyInBaseField,
                },
                "base-game-key-is-expansion-key",
            ),
            (
                InstallError::KeyInvalid {
                    stage: InstallStage::Expansion,
                    problem: crate::blizzard::KeyProblem::OldKeyLength,
                },
                "expansion-key-16-characters",
            ),
            (
                InstallError::KeyInvalid {
                    stage: InstallStage::Expansion,
                    problem: crate::blizzard::KeyProblem::WrongProduct,
                },
                "expansion-key-not-valid",
            ),
            (
                InstallError::DirectDownloadFailed {
                    stage: InstallStage::ExpansionDownload,
                    failure: DirectFailure::Incomplete,
                },
                "direct-download-incomplete",
            ),
            (
                InstallError::DirectDownloadFailed {
                    stage: InstallStage::BaseGameDownload,
                    failure: DirectFailure::Mismatch,
                },
                "direct-download-mismatch",
            ),
            (InstallError::AuditUnavailable, "managed-install-failed"),
        ] {
            assert_eq!(install_failure_code(&error), code, "{error:?}");
        }
    }

    #[test]
    fn cooperating_audit_cannot_enter_while_an_installation_mutation_is_live() {
        let fixture = TempDir::new().unwrap();
        std::fs::write(fixture.path().join(".pd2launcher.lock"), []).unwrap();
        let root = Directory::open(fixture.path()).unwrap();
        let first = InstallationMutationLease::acquire(&root).unwrap();
        let root_path = fixture.path().to_owned();
        let blocked = thread::spawn(move || {
            let second_root = Directory::open(&root_path).unwrap();
            InstallationMutationLease::acquire(&second_root).is_err()
        })
        .join()
        .unwrap();
        assert!(blocked, "a second audit entered during journal mutation");

        drop(first);
        assert!(InstallationMutationLease::acquire(&root).is_ok());
    }

    #[test]
    fn the_official_launchers_session_end_picks_the_client_files_and_the_control() {
        use crate::operation::{WorkProgress, progress_channel};
        // It closed by itself: downloads, under the job's control.
        let job = CancellationToken::default();
        let mut used = None;
        after_official_launcher(Ok(()), &job, |files, control| {
            used = Some((files, std::ptr::eq(control, &job)));
            Ok(())
        })
        .unwrap();
        assert_eq!(used, Some((OfficialClientFiles::Download, true)));
        let failed = after_official_launcher(Err(AdapterFailure), &job, |_, _| {
            unreachable!("a failed session takes no client")
        });
        assert_eq!(failed.unwrap_err().code(), "official-launcher-failed");

        // A stop not proven safe: the launcher may still run.
        for unproven in [false, true] {
            let job = CancellationToken::default();
            job.request_cancel();
            if unproven {
                job.mark_cancel_unproven();
            }
            let stopped = after_official_launcher(Ok(()), &job, |_, _| {
                unreachable!("a stop not proven safe takes no client")
            });
            assert_eq!(stopped.unwrap_err().code(), "operation-indeterminate");
        }

        // A safe stop: the installed files only, under a control of its own
        // that is not cancelled, while the job's control shows the check.
        // The job ends as stopped, also when the client is not accepted.
        for (reconciled, ends) in [
            (Ok::<(), &'static str>(()), "operation-cancelled"),
            (Err("client-reconcile-failed"), "operation-cancelled"),
            (
                Err("official-client-not-reconciled"),
                "official-client-not-reconciled",
            ),
            (Err("operation-indeterminate"), "operation-indeterminate"),
        ] {
            let (publisher, progress) = progress_channel();
            let job = CancellationToken::with_progress(publisher);
            job.request_cancel();
            job.mark_cancelled_safe();
            let mut used = None;
            let stopped = after_official_launcher(Err(AdapterFailure), &job, |files, control| {
                used = Some((files, std::ptr::eq(control, &job), control.is_requested()));
                reconciled.map_err(backend_failure)
            });
            assert_eq!(stopped.unwrap_err().code(), ends);
            assert_eq!(
                used,
                Some((OfficialClientFiles::InstalledOnly, false, false))
            );
            assert_eq!(
                progress.take_latest(),
                Some(WorkProgress::for_phase(WorkPhase::Verification))
            );
        }
    }

    #[test]
    fn the_engine_log_says_why_a_stopped_launchers_client_was_or_was_not_accepted() {
        use crate::update::ApplyOutcome;
        let accepted = Ok(ApplyOutcome::Applied {
            downloaded: 0,
            promoted: 0,
            preserved: 1,
        });
        let offline = || {
            Err(UpdateError::Network(
                crate::network::NetworkError::transient("offline"),
            ))
        };
        let indeterminate = Err(UpdateError::Transaction(
            crate::transaction::TransactionError::Indeterminate,
        ));
        assert!(stopped_client_outcome(&accepted, false).starts_with("client accepted"));
        assert!(stopped_client_outcome(&offline(), false).contains("offline"));
        // A refused file is partial, though it fails as a network error.
        assert!(stopped_client_outcome(&offline(), true).contains("partial"));
        assert!(stopped_client_outcome(&indeterminate, true).contains("indeterminate"));
    }

    #[test]
    fn only_an_absent_d2gl_configuration_produces_the_first_launch_notice() {
        assert_eq!(
            game_launch_notice(D2glSetup::ConfigurationAbsent),
            Some(GameNoticeView::FirstD2glLaunch)
        );
        for silent in [
            D2glSetup::NotRequested,
            D2glSetup::Unchanged,
            D2glSetup::Written,
            D2glSetup::Skipped,
        ] {
            assert_eq!(game_launch_notice(silent), None, "{silent:?}");
        }
    }

    #[test]
    fn launch_preferences_persist_across_store_reconstruction() {
        let fixture = TempDir::new().expect("fixture");
        let path = fixture.path().join("config/settings.json");
        let store = SettingsStore::new(&path);

        update_launch_settings(
            &store,
            fixture.path(),
            SettingsChange::Renderer(RendererPreferenceView::DirectDraw),
        )
        .expect("renderer");
        update_launch_settings(&store, fixture.path(), SettingsChange::SkipToBnet(false))
            .expect("online shortcut");
        let saved = update_launch_settings(
            &store,
            fixture.path(),
            SettingsChange::SoundInBackground(true),
        )
        .expect("background sound");
        assert_eq!(
            saved,
            LaunchSettingsView::new(RendererPreferenceView::DirectDraw, false, true)
        );
        let saved = update_launch_settings(
            &store,
            fixture.path(),
            SettingsChange::OfficialLauncherSoftwareRendering(false),
        )
        .expect("official launcher software rendering");
        let expected = LaunchSettingsView::new(RendererPreferenceView::DirectDraw, false, true)
            .with_official_launcher_software_rendering(false);
        assert_eq!(saved, expected);

        let restarted = SettingsStore::new(path);
        let loaded = restarted.load().expect("restart load");
        assert_eq!(launch_settings_view(&loaded), expected);
        assert_eq!(
            game_launch_options(&loaded),
            GameLaunchOptions::new(GameRenderer::DirectDraw, false).with_sound_in_background(true)
        );
    }

    #[test]
    fn update_checks_turned_off_persist_and_reach_the_settings_view() {
        let fixture = TempDir::new().expect("fixture");
        let path = fixture.path().join("config/settings.json");
        let store = SettingsStore::new(&path);
        assert!(launch_settings_view(&store.load().expect("load")).update_checks());

        assert!(!client_as_installed(&store));
        let saved =
            update_launch_settings(&store, fixture.path(), SettingsChange::UpdateChecks(false))
                .expect("update checks");
        assert!(!saved.update_checks());
        assert!(client_as_installed(&store));
        let loaded = SettingsStore::new(&path).load().expect("restart load");
        assert!(!launch_settings_view(&loaded).update_checks());
        // The other settings stay as they were.
        assert_eq!(
            launch_settings_view(&loaded).with_update_checks(true),
            launch_settings_view(&crate::persistence::LauncherSettings::default())
        );
    }

    #[test]
    fn only_custom_sources_a_job_acquired_are_recorded() {
        struct FakeSource;
        impl ArtifactSource for FakeSource {
            fn acquire_at(
                &self,
                spec: &ArtifactSpec,
                cache: &Path,
                _cache_directory: &Directory,
                _control: &crate::operation::OperationControl,
            ) -> Result<AcquiredArtifact, AdapterFailure> {
                if spec.kind() == crate::install::ArtifactKind::ClientBootstrap {
                    return Err(AdapterFailure::here());
                }
                Ok(AcquiredArtifact::new(
                    cache.join("artifact"),
                    spec.request_url(),
                    spec.request_url(),
                    None,
                ))
            }
        }
        let fixture = TempDir::new().expect("fixture");
        let directory = Directory::open(fixture.path()).unwrap();
        let sources = DownloadSources::resolve(
            &[
                (
                    DownloadArtifact::Wine,
                    "https://m.example/w.tar.xz".to_owned(),
                ),
                (
                    DownloadArtifact::Pd2Installer,
                    "https://m.example/i.exe".to_owned(),
                ),
                (
                    DownloadArtifact::D2Installer,
                    "https://m.example/d2.exe".to_owned(),
                ),
            ]
            .into(),
            &BTreeMap::new(),
        );
        let catalog = ArtifactCatalog::with_sources(&sources);
        let used = UsedSources::default();
        let recording = RecordingArtifactSource {
            inner: &FakeSource,
            used: &used,
        };
        let control = crate::operation::OperationControl::default();
        for kind in [
            crate::install::ArtifactKind::ManagedRuntime,
            crate::install::ArtifactKind::Gecko,
            crate::install::ArtifactKind::ClientBootstrap,
        ] {
            let _ = recording.acquire_at(catalog.spec(kind), fixture.path(), &directory, &control);
        }
        // The built-in Gecko, the failed PD2 installer, and the
        // never-acquired Diablo II installer are not recorded.
        assert_eq!(used.take(), [DownloadArtifact::Wine]);
    }

    #[test]
    fn invalid_saved_sources_never_block_sources_saves_or_resets() {
        let fixture = TempDir::new().expect("fixture");
        let path = fixture.path().join("config/settings.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            br#"{"schema":1,"renderer":"glide","skip_to_bnet":true,"sound_in_background":false,"network_concurrency":2,"download_sources":{"wine":"ftp://bad","nonsense":"https://x.example/","gecko":"https://mirror.example/g.msi"}}"#,
        )
        .unwrap();
        let store = SettingsStore::new(&path);
        let environment: BTreeMap<_, _> = [(
            DownloadArtifact::Wine,
            "https://env.example/wine.tar.xz".to_owned(),
        )]
        .into();

        let sources = effective_sources(&store, &environment);
        assert_eq!(
            sources.origin(DownloadArtifact::Wine),
            SourceOrigin::Environment
        );
        assert_eq!(
            sources.origin(DownloadArtifact::Gecko),
            SourceOrigin::Settings
        );
        assert!(
            ignored_sources_advisory(&store)
                .expect("advisory")
                .detail
                .starts_with("2 invalid download sources ignored")
        );

        update_download_source_setting(&store, DownloadArtifact::Gecko, None).expect("reset");
        update_download_source_setting(
            &store,
            DownloadArtifact::Pd2Installer,
            Some("https://mirror.example/pd2.exe".to_owned()),
        )
        .expect("save");
        assert!(ignored_sources_advisory(&store).is_none());

        // An unreadable settings file still leaves environment and built-in
        // sources in effect instead of failing installs and updates.
        std::fs::write(&path, b"{not json").unwrap();
        let sources = effective_sources(&store, &environment);
        assert_eq!(
            sources.origin(DownloadArtifact::Wine),
            SourceOrigin::Environment
        );
        assert_eq!(
            sources.origin(DownloadArtifact::Pd2Installer),
            SourceOrigin::BuiltIn
        );
    }

    #[test]
    fn source_rows_name_an_installed_source_that_differs_from_the_configured_one() {
        let fixture = TempDir::new().expect("fixture");
        let prefix = fixture.path().join("prefix");
        std::fs::create_dir_all(&prefix).unwrap();
        std::fs::write(
            prefix.join(".pd2launcher-gecko-custom.json"),
            crate::sources::CustomSourceReceipt::new(
                DownloadArtifact::Gecko,
                "https://mirror.example/gecko.msi?token=secret",
                &"a".repeat(64),
            )
            .encode(),
        )
        .unwrap();
        std::fs::write(prefix.join(".pd2launcher-mono.sha256"), b"pinned").unwrap();
        // Receipts of the retired 1.14d patch stage are ignored.
        std::fs::write(prefix.join(".pd2launcher-patch.sha256"), b"pinned").unwrap();
        std::fs::write(prefix.join(".pd2launcher-patch-custom.json"), b"{}").unwrap();
        let root = Directory::open(fixture.path()).unwrap();

        let notes = installed_source_notes(&root, &DownloadSources::builtin());
        let gecko = notes.get(&DownloadArtifact::Gecko).expect("gecko differs");
        assert!(gecko.starts_with("Installed: custom (unverified) from mirror.example"));
        assert!(!gecko.contains("secret"));
        assert_eq!(notes.len(), 1, "{notes:?}");

        let custom = DownloadSources::resolve(
            &[
                (
                    DownloadArtifact::Gecko,
                    "https://mirror.example/gecko.msi".to_owned(),
                ),
                (
                    DownloadArtifact::WineMono,
                    "https://mirror.example/mono.msi".to_owned(),
                ),
            ]
            .into(),
            &BTreeMap::new(),
        );
        let notes = installed_source_notes(&root, &custom);
        assert!(!notes.contains_key(&DownloadArtifact::Gecko));
        assert!(
            notes[&DownloadArtifact::WineMono].starts_with("Installed: built-in (verified)"),
            "{notes:?}"
        );
        let views = download_source_views(&custom, &notes);
        assert!(
            views
                .iter()
                .any(|view| view.artifact == DownloadArtifact::WineMono && view.installed.is_some())
        );
    }

    #[test]
    fn gui_download_sources_persist_and_win_over_the_environment() {
        let fixture = TempDir::new().expect("fixture");
        let store = SettingsStore::new(fixture.path().join("config/settings.json"));
        let environment: BTreeMap<_, _> = [
            (
                DownloadArtifact::Wine,
                "https://env.example/wine.tar.xz".to_owned(),
            ),
            (
                DownloadArtifact::Gecko,
                "https://env.example/gecko.msi".to_owned(),
            ),
        ]
        .into();
        let resolve = |store: &SettingsStore| {
            DownloadSources::resolve(store.load().unwrap().download_sources(), &environment)
        };

        update_download_source_setting(
            &store,
            DownloadArtifact::Wine,
            Some(" https://gui.example/wine.tar.xz ".to_owned()),
        )
        .expect("save");
        let views = download_source_views(
            &resolve(&SettingsStore::new(
                fixture.path().join("config/settings.json"),
            )),
            &BTreeMap::new(),
        );
        let view = |artifact| {
            views
                .iter()
                .find(|view| view.artifact == artifact)
                .cloned()
                .unwrap()
        };
        assert_eq!(view(DownloadArtifact::Wine).badge, SourceBadgeView::Custom);
        assert_eq!(
            view(DownloadArtifact::Wine).effective_url,
            "https://gui.example/wine.tar.xz"
        );
        assert_eq!(
            view(DownloadArtifact::Gecko).badge,
            SourceBadgeView::CustomFromEnvironment
        );
        assert_eq!(
            view(DownloadArtifact::D2Installer).badge,
            SourceBadgeView::BuiltInVerified
        );
        assert_eq!(
            view(DownloadArtifact::D2Installer).effective_url,
            DownloadArtifact::D2Installer.builtin_url()
        );

        // Reset falls back to the environment, then the built-in source.
        update_download_source_setting(&store, DownloadArtifact::Wine, None).expect("reset");
        assert_eq!(
            resolve(&store).origin(DownloadArtifact::Wine),
            SourceOrigin::Environment
        );
        // Saving the built-in URL (or nothing) never creates a "custom" copy
        // that would skip the built-in verification.
        update_download_source_setting(
            &store,
            DownloadArtifact::D2Installer,
            Some(DownloadArtifact::D2Installer.builtin_url()),
        )
        .expect("builtin");
        update_download_source_setting(&store, DownloadArtifact::Pd2Installer, Some(String::new()))
            .expect("empty");
        assert!(store.load().unwrap().download_sources().is_empty());

        assert_eq!(
            update_download_source_setting(
                &store,
                DownloadArtifact::Wine,
                Some("ftp://nope.example/wine".to_owned()),
            ),
            Err(backend_failure("source-url-must-be-http-or-https"))
        );
        update_download_source_setting(
            &store,
            DownloadArtifact::Pd2ClientBase,
            Some("https://mirror.example/pd2/".to_owned()),
        )
        .expect("base");
        assert_eq!(
            client_base(&resolve(&store)),
            Pd2ClientBase::Custom("https://mirror.example/pd2".to_owned())
        );
        assert_eq!(
            client_base(&DownloadSources::builtin()),
            Pd2ClientBase::BuiltIn
        );
    }

    #[test]
    fn production_progress_bridge_persists_typed_phase_and_counts() {
        let fixture = TempDir::new().unwrap();
        let history = fixture.path().join("diagnostics/recent.json");
        let exports = fixture.path().join("exports");
        let mut store = DiagnosticStore::open(&history, &exports).unwrap();
        let id = store
            .start_job(JobKind::Update, WorkPhase::ClientManifest)
            .unwrap();
        let diagnostics = Arc::new(Mutex::new(Some(store)));
        let control = CancellationToken::default();
        register_durable_progress(
            Arc::clone(&diagnostics),
            id,
            WorkPhase::ClientManifest,
            &control,
        );

        control.report(WorkProgress::bounded(
            WorkPhase::ClientDownload,
            crate::operation::ProgressUnit::Files,
            7,
            7,
        ));
        let diagnostics = diagnostics.lock().unwrap();
        let job = diagnostics.as_ref().unwrap().active_job().unwrap();
        assert_eq!(job.phase(), WorkPhase::ClientDownload);
        assert_eq!(job.progress().unwrap().completed(), 7);
        drop(diagnostics);

        let reopened = DiagnosticStore::open(history, exports).unwrap();
        assert_eq!(
            reopened.active_job().unwrap().progress().unwrap().total(),
            Some(7)
        );
    }

    #[test]
    fn a_program_path_needs_its_file_in_the_game_folder_when_the_folder_is_there() {
        let fixture = TempDir::new().expect("fixture");
        let store = SettingsStore::new(fixture.path().join("config/settings.json"));
        // No game folder yet: the path is saved, and Play checks it again.
        let saved = update_launch_settings(
            &store,
            fixture.path(),
            SettingsChange::GameProgram("ZYLPD2/Game.exe".to_owned()),
        )
        .expect("game program");
        assert_eq!(saved.game_program(), "ZYLPD2/Game.exe");

        let game = fixture
            .path()
            .join("prefix/drive_c/Program Files (x86)/Diablo II");
        std::fs::create_dir_all(game.join("ProjectD2")).unwrap();
        std::fs::write(game.join("ProjectD2/PD2Launcher.exe"), b"MZ").unwrap();
        assert_eq!(
            update_launch_settings(
                &store,
                fixture.path(),
                SettingsChange::LauncherProgram("ZYLPD2/Launcher.exe".to_owned()),
            ),
            Err(backend_failure("program-path-missing"))
        );
        // A folder is not a program.
        assert_eq!(
            update_launch_settings(
                &store,
                fixture.path(),
                SettingsChange::LauncherProgram("ProjectD2".to_owned()),
            ),
            Err(backend_failure("program-path-missing"))
        );
        let saved = update_launch_settings(
            &store,
            fixture.path(),
            SettingsChange::LauncherProgram("ProjectD2/PD2Launcher.exe".to_owned()),
        )
        .expect("launcher program");
        assert_eq!(saved.launcher_program(), "ProjectD2/PD2Launcher.exe");
        let saved = update_launch_settings(
            &store,
            fixture.path(),
            SettingsChange::LauncherCustomArguments("-lang zh".to_owned()),
        )
        .expect("launcher start options");
        assert_eq!(saved.launcher_custom_arguments(), "-lang zh");
        assert_eq!(
            game_launch_options(&store.load().expect("load")),
            GameLaunchOptions::default().with_program("ZYLPD2/Game.exe")
        );
    }

    #[test]
    fn invalid_settings_return_only_the_stable_load_failure() {
        let fixture = TempDir::new().expect("fixture");
        let path = fixture.path().join("settings.json");
        std::fs::write(&path, b"not-json").expect("invalid settings");
        let result = update_launch_settings(
            &SettingsStore::new(path),
            fixture.path(),
            SettingsChange::SkipToBnet(false),
        );
        assert_eq!(result, Err(backend_failure("settings-load-failed")));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_settings_return_only_the_stable_load_failure() {
        use std::os::unix::fs::symlink;

        let fixture = TempDir::new().expect("fixture");
        let target = fixture.path().join("target.json");
        std::fs::write(&target, b"{}").expect("target");
        let path = fixture.path().join("settings.json");
        symlink(target, &path).expect("symlink");
        let result = update_launch_settings(
            &SettingsStore::new(path),
            fixture.path(),
            SettingsChange::SoundInBackground(true),
        );
        assert_eq!(result, Err(backend_failure("settings-load-failed")));
    }

    #[test]
    fn standalone_binary_identity_is_attested_by_its_build_alone() {
        for (architecture, identity, artifact) in [
            (
                "aarch64",
                AARCH64_BINARY_IDENTITY,
                LauncherArtifactEvidence::NativeAarch64StandaloneBinary,
            ),
            (
                "x86_64",
                X86_64_BINARY_IDENTITY,
                LauncherArtifactEvidence::NativeX86_64StandaloneBinary,
            ),
        ] {
            assert_eq!(
                launcher_artifact_from_identity(architecture, Some(identity)),
                artifact
            );
            assert_eq!(portable_identity(artifact), Some(identity));
            // A development build has no compiled identity.
            assert_eq!(
                launcher_artifact_from_identity(architecture, None),
                LauncherArtifactEvidence::Unverified
            );
        }
        // Nor does a binary built for one architecture pass on the other.
        assert_eq!(
            launcher_artifact_from_identity("x86_64", Some(AARCH64_BINARY_IDENTITY)),
            LauncherArtifactEvidence::Unverified
        );
        assert_eq!(
            portable_identity(LauncherArtifactEvidence::Unverified),
            None
        );
    }

    #[test]
    fn package_identity_never_certifies_an_unaccepted_architecture_or_value() {
        assert_eq!(
            launcher_artifact_from_identity("aarch64", Some(X86_64_BINARY_IDENTITY)),
            LauncherArtifactEvidence::Unverified
        );
        // The identities of the package formats that were removed attest
        // nothing.
        for identity in ["native-aarch64-appimage", "native-aarch64-deb", "other"] {
            assert_eq!(
                launcher_artifact_from_identity("aarch64", Some(identity)),
                LauncherArtifactEvidence::Unverified
            );
        }
        assert_eq!(
            launcher_artifact_from_identity("riscv64", Some(AARCH64_BINARY_IDENTITY)),
            LauncherArtifactEvidence::Unverified
        );
    }

    /// The renderer answer is reused only for the same key, and an answer
    /// without OpenGL is kept as such.
    #[test]
    fn the_game_graphics_record_answers_only_for_its_key() {
        let directory = tempfile::tempdir().unwrap();
        let record = directory.path().join(GAME_GRAPHICS_RECORD);
        assert_eq!(read_game_graphics(&record, "key-a"), None);
        write_game_graphics(&record, "key-a", Some("llvmpipe (LLVM 20.1.2, 256 bits)")).unwrap();
        assert_eq!(
            read_game_graphics(&record, "key-a"),
            Some(Some("llvmpipe (LLVM 20.1.2, 256 bits)".to_owned()))
        );
        assert_eq!(read_game_graphics(&record, "key-b"), None);
        write_game_graphics(&record, "key-b", None).unwrap();
        assert_eq!(read_game_graphics(&record, "key-b"), Some(None));
        // The key is stable for the same host and separates custom runtimes.
        let pinned = crate::runtime::InstalledRuntime::BuiltIn(crate::runtime::current());
        let custom = crate::runtime::InstalledRuntime::Custom;
        assert_eq!(game_graphics_key(pinned), game_graphics_key(pinned));
        assert_ne!(game_graphics_key(pinned), game_graphics_key(custom));
    }

    #[test]
    fn host_remedies_are_generated_for_the_running_row() {
        let x86_64 = host_prerequisite_detail_for(
            HostSupport::Unsupported,
            [
                FindingReason::MultilibUnavailable,
                FindingReason::WineLibraryUnavailable(WineLibrary::Xext),
                FindingReason::WineLibraryUnavailable(WineLibrary::OpenGl),
                FindingReason::X11DisplayUnavailable,
                FindingReason::LauncherArtifactUnverified,
                FindingReason::HostRowUncertified,
            ],
            &HostPlatform::X86_64,
        );
        // Only the libraries actually missing are named.
        assert!(
            x86_64.contains(
                "a 64-bit system library Wine could not load, the 64-bit X11 extension library (libXext.so.6), 64-bit OpenGL (libEGL.so.1 or libGL.so.1), "
            ),
            "{x86_64}"
        );
        for absent in ["libX11.so.6", "FreeType", "Fontconfig"] {
            assert!(!x86_64.contains(absent), "{x86_64}");
        }
        for multilib in ["lib32-glibc", "libc6:i386", "32-bit"] {
            assert!(!x86_64.contains(multilib), "{x86_64}");
        }
        assert!(x86_64.contains("XWayland"), "{x86_64}");
        assert!(x86_64.contains("x86-64 launcher download"), "{x86_64}");
        assert!(x86_64.ends_with("this x86-64 host row is not certified yet"));
        for forbidden in ["FEX", "ARM64"] {
            assert!(!x86_64.contains(forbidden), "{x86_64}");
        }
        assert_eq!(
            host_prerequisite_detail_for(
                HostSupport::Unsupported,
                [FindingReason::HostRowUncertified],
                &HostPlatform::X86_64,
            ),
            "Host prerequisites present, but this x86-64 host row is not certified yet"
        );
        let x86_64_supported =
            host_prerequisite_detail_for(HostSupport::Supported, [], &HostPlatform::X86_64);
        assert!(!x86_64_supported.contains("FEX"));
        // The x86-64 row never checks Vulkan or hardware graphics.
        assert_eq!(
            x86_64_supported,
            "X11 display, Wine's 64-bit libraries, and Launcher bundle verified"
        );

        let aarch64 = host_prerequisite_detail_for(
            HostSupport::Unsupported,
            [
                FindingReason::FexUnavailable,
                FindingReason::LauncherArtifactUnverified,
            ],
            &HostPlatform::Aarch64,
        );
        assert_eq!(
            aarch64,
            "Install or enable FEX-Emu (FEX or FEXInterpreter in /usr/bin or /usr/local/bin), the verified ARM64 launcher download"
        );
        assert_eq!(
            host_prerequisite_detail_for(HostSupport::Supported, [], &HostPlatform::Aarch64),
            "FEX, display, Vulkan, hardware graphics, and Launcher bundle verified"
        );
    }

    #[test]
    fn updates_and_the_accepted_manifest_follow_a_game_folder_used_in_place() {
        use std::os::unix::fs::MetadataExt;
        let home = tempfile::tempdir().unwrap();
        let paths = LauncherPaths::from_home(home.path());
        let root_path = paths.managed_root();
        let managed_project = root_path.join(PROJECT_D2_RELATIVE);
        std::fs::create_dir_all(managed_project.join(".pd2launcher")).unwrap();
        std::fs::write(
            managed_project.join(".pd2launcher/accepted-manifest.json"),
            b"managed",
        )
        .unwrap();
        let root = Directory::open(&root_path).unwrap();
        let read = |root: &Directory| {
            read_optional_bounded_at(&anchored_update_layout(&paths, root).unwrap(), 64).unwrap()
        };
        assert_eq!(read(&root).as_deref(), Some(&b"managed"[..]));

        // Used in place: its own ProjectD2, where the updater promotes.
        let external = home.path().join("Games/Diablo II");
        std::fs::create_dir_all(external.join("ProjectD2/.pd2launcher")).unwrap();
        std::fs::write(
            external.join("ProjectD2/.pd2launcher/accepted-manifest.json"),
            b"external",
        )
        .unwrap();
        let metadata = std::fs::metadata(&external).unwrap();
        std::fs::write(
            root_path
                .join("prefix")
                .join(crate::game_folder::POINTER_NAME),
            format!(
                r#"{{"schema":1,"path":"{}","filesystem":{},"inode":{}}}"#,
                external.display(),
                rustix::fs::statvfs(&external).unwrap().f_fsid,
                metadata.ino()
            ),
        )
        .unwrap();
        assert_eq!(read(&root).as_deref(), Some(&b"external"[..]));
        // Gone: fail closed, never back to the managed copy.
        std::fs::rename(&external, home.path().join("moved")).unwrap();
        assert!(anchored_update_layout(&paths, &root).is_err());
    }

    #[test]
    fn start_over_removes_the_prefix_while_the_game_folder_used_in_place_is_unavailable() {
        use std::os::unix::fs::MetadataExt;
        let home = tempfile::tempdir().unwrap();
        let root_path = home.path().join("managed");
        std::fs::create_dir_all(root_path.join("prefix/drive_c")).unwrap();
        std::fs::write(root_path.join(".pd2launcher.lock"), b"").unwrap();
        let external = home.path().join("Games/Diablo II");
        std::fs::create_dir_all(external.join("ProjectD2")).unwrap();
        std::fs::write(
            root_path
                .join("prefix")
                .join(crate::game_folder::POINTER_NAME),
            format!(
                r#"{{"schema":1,"path":"{}","filesystem":{},"inode":{}}}"#,
                external.display(),
                rustix::fs::statvfs(&external).unwrap().f_fsid,
                std::fs::metadata(&external).unwrap().ino()
            ),
        )
        .unwrap();
        let moved = home.path().join("moved");
        std::fs::rename(&external, &moved).unwrap();
        let root = Directory::open(&root_path).unwrap();
        let lease = InstallationMutationLease::acquire(&root).unwrap();

        // The folder cannot be audited; Start over does not need it to be.
        // A folder used where it is keeps its own characters: nothing is
        // kept or linked for it.
        let kept = crate::save_keeper::KeptSaves::below(home.path());
        start_over(&root, &root_path, &lease, Some(&kept), || {
            unreachable!("an unavailable game folder is not audited")
        })
        .expect("start over");
        assert!(!root_path.join("prefix").exists());
        assert!(moved.join("ProjectD2").is_dir());
        assert!(!kept.path().exists());
        assert_eq!(GameFolder::resolve(&root), Ok(GameFolder::Managed));
    }

    /// A second Start over on the Android host after a stop inside the
    /// first one's removal of `ProjectD2/.pd2launcher`, where the update
    /// records read as Unsafe (`transaction::tests::
    /// update_records_that_a_removal_cut_part_way_read_as_unsafe`): the
    /// audit's transaction check is Indeterminate. With the started
    /// base-game marker that the first one wrote, it goes on; without it,
    /// with a finished marker, and on the desktop it is refused, as before.
    #[test]
    fn a_start_over_that_its_marker_shows_as_begun_needs_no_transaction_preflight() {
        use crate::stage_marker::{MarkerState, finish, read, write};
        let home = tempfile::tempdir().unwrap();
        let root_path = home.path().join("managed");
        std::fs::create_dir_all(root_path.join("prefix/drive_c")).unwrap();
        std::fs::write(root_path.join(".pd2launcher.lock"), b"").unwrap();
        let root = Directory::open(&root_path).unwrap();
        let lease = InstallationMutationLease::acquire(&root).unwrap();
        let unsafe_records = || Ok(CheckStatus::Indeterminate);
        let refused = Some(backend_failure("start-over-preflight-rejected"));
        // No marker: refused.
        assert_eq!(
            start_over_on(&root, &root_path, &lease, None, unsafe_records, true).err(),
            refused
        );
        // A finished marker is no Start over under way: refused.
        finish(&root, crate::InterruptedStage::BaseGame).unwrap();
        assert_eq!(
            start_over_on(&root, &root_path, &lease, None, unsafe_records, true).err(),
            refused
        );
        // The desktop: no change.
        write(&root, crate::InterruptedStage::BaseGame).unwrap();
        assert_eq!(
            start_over_on(&root, &root_path, &lease, None, unsafe_records, false).err(),
            refused
        );
        assert!(root_path.join("prefix").is_dir());
        // The Android host with the started marker: it goes on, and its end
        // clears the marker.
        start_over_on(&root, &root_path, &lease, None, unsafe_records, true)
            .expect("start over goes on");
        assert!(!root_path.join("prefix").exists());
        assert_eq!(read(&root), MarkerState::Absent);
    }

    #[test]
    fn start_over_keeps_the_characters_first_and_removes_nothing_when_it_cannot() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let root_path = home.path().join("managed");
        let save = root_path
            .join(crate::game_folder::GAME_RELATIVE)
            .join("Save");
        std::fs::create_dir_all(&save).unwrap();
        std::fs::write(save.join("Hero.d2s"), b"hero").unwrap();
        std::fs::write(root_path.join(".pd2launcher.lock"), b"").unwrap();
        let root = Directory::open(&root_path).unwrap();
        let lease = InstallationMutationLease::acquire(&root).unwrap();
        let kept = crate::save_keeper::KeptSaves::below(home.path());
        let audit = || Ok(CheckStatus::Satisfied);

        // The kept folder can't be written: Start over does not run.
        std::fs::create_dir_all(kept.path()).unwrap();
        std::fs::set_permissions(kept.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        assert_eq!(
            start_over(&root, &root_path, &lease, Some(&kept), audit).err(),
            Some(backend_failure(crate::save_keeper::SAVES_NOT_KEPT))
        );
        assert_eq!(std::fs::read(save.join("Hero.d2s")).unwrap(), b"hero");

        // Now it can: the characters are kept, then the prefix goes.
        std::fs::set_permissions(kept.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        start_over(&root, &root_path, &lease, Some(&kept), audit).expect("start over");
        assert!(!root_path.join("prefix").exists());
        assert_eq!(
            std::fs::read(kept.path().join("Hero.d2s")).unwrap(),
            b"hero"
        );
    }

    /// A managed root below a temporary home whose Managed game folder's
    /// `name` folder holds `Hero.d2s`, with its lease held.
    fn root_with_characters(
        name: &str,
    ) -> (
        tempfile::TempDir,
        PathBuf,
        Directory,
        InstallationMutationLease,
        crate::save_keeper::KeptSaves,
    ) {
        let home = tempfile::tempdir().unwrap();
        let root_path = home.path().join("managed");
        let save = root_path.join(crate::game_folder::GAME_RELATIVE).join(name);
        std::fs::create_dir_all(&save).unwrap();
        std::fs::write(save.join("Hero.d2s"), b"hero").unwrap();
        std::fs::write(root_path.join(".pd2launcher.lock"), b"").unwrap();
        let root = Directory::open(&root_path).unwrap();
        let lease = InstallationMutationLease::acquire(&root).unwrap();
        let kept = crate::save_keeper::KeptSaves::below(home.path());
        (home, root_path, root, lease, kept)
    }

    #[test]
    fn start_over_keeps_the_characters_when_the_game_folder_record_is_unreadable() {
        use std::os::unix::fs::PermissionsExt;
        let (_home, root_path, root, lease, kept) = root_with_characters("Save");
        std::fs::write(
            root_path
                .join("prefix")
                .join(crate::game_folder::POINTER_NAME),
            b"not a record",
        )
        .unwrap();
        assert_eq!(
            GameFolder::resolve(&root),
            Err(GameFolderError::RecordInvalid)
        );
        let unaudited = || unreachable!("an unreadable record is not audited");
        // It proves no folder used in place, so the Managed one's characters
        // are kept, and when they can't be nothing is removed.
        std::fs::create_dir_all(kept.path()).unwrap();
        std::fs::set_permissions(kept.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        assert_eq!(
            start_over(&root, &root_path, &lease, Some(&kept), unaudited).err(),
            Some(backend_failure(crate::save_keeper::SAVES_NOT_KEPT))
        );
        assert!(root_path.join("prefix").is_dir());
        std::fs::set_permissions(kept.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        start_over(&root, &root_path, &lease, Some(&kept), unaudited).expect("start over");
        assert!(!root_path.join("prefix").exists());
        assert_eq!(
            std::fs::read(kept.path().join("Hero.d2s")).unwrap(),
            b"hero"
        );
    }

    #[test]
    fn start_over_does_not_run_while_a_save_folder_in_another_case_cannot_be_kept() {
        let (_home, root_path, root, lease, kept) = root_with_characters("save");
        let save = root_path
            .join(crate::game_folder::GAME_RELATIVE)
            .join("save");
        // A folder in it too deep to back up: the keep fails.
        let mut deep = save.join("old");
        for level in 0..20 {
            deep.push(format!("d{level}"));
        }
        std::fs::create_dir_all(&deep).unwrap();
        let audit = || Ok(CheckStatus::Satisfied);
        assert_eq!(
            start_over(&root, &root_path, &lease, Some(&kept), audit).err(),
            Some(backend_failure(crate::save_keeper::SAVES_NOT_KEPT))
        );
        assert_eq!(std::fs::read(save.join("Hero.d2s")).unwrap(), b"hero");
        std::fs::remove_dir_all(save.join("old")).unwrap();
        start_over(&root, &root_path, &lease, Some(&kept), audit).expect("start over");
        assert!(!root_path.join("prefix").exists());
        assert_eq!(
            std::fs::read(kept.path().join("Hero.d2s")).unwrap(),
            b"hero"
        );
    }

    #[test]
    fn start_over_keeps_the_characters_of_a_save_path_inside_the_prefix() {
        let (_home, root_path, root, lease, kept) = root_with_characters("Save");
        let elsewhere = root_path.join("prefix/drive_c/users/me/Saved Games/Diablo II");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("Other.d2s"), b"other").unwrap();
        std::fs::write(
            root_path.join("prefix/user.reg"),
            "WINE REGISTRY Version 2\n\n[Software\\\\Blizzard Entertainment\\\\Diablo II] 1800000000\n\"Save Path\"=\"C:\\\\users\\\\me\\\\Saved Games\\\\Diablo II\\\\\"\n",
        )
        .unwrap();
        let put_aside = start_over(&root, &root_path, &lease, Some(&kept), || {
            Ok(CheckStatus::Satisfied)
        })
        .expect("start over");
        assert!(put_aside.is_empty());
        assert!(!root_path.join("prefix").exists());
        for (name, content) in [("Hero.d2s", &b"hero"[..]), ("Other.d2s", b"other")] {
            assert_eq!(std::fs::read(kept.path().join(name)).unwrap(), content);
        }
    }

    #[test]
    fn a_keep_before_play_that_fails_leaves_the_game_its_save_or_stops_it() {
        use std::os::unix::fs::PermissionsExt;
        let (_home, root_path, root, _lease, kept) = root_with_characters("Save");
        let game = root_path.join(crate::game_folder::GAME_RELATIVE);
        // A keep stopped after stepping Save aside; this one fails too (the
        // kept folder can't be written), and Save is put back for the game.
        std::fs::rename(game.join("Save"), game.join("Save.pd2launcher-moved")).unwrap();
        std::fs::create_dir_all(kept.path()).unwrap();
        std::fs::set_permissions(kept.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        assert_eq!(
            keep_before_play(&root, &kept),
            Ok(crate::save_keeper::PutAside::default())
        );
        assert_eq!(std::fs::read(game.join("Save/Hero.d2s")).unwrap(), b"hero");

        // A stepped-aside Save that can't be put back: the game would not
        // see those characters, so it does not start.
        std::fs::set_permissions(kept.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::rename(game.join("Save"), game.join("Save.pd2launcher-moved")).unwrap();
        std::fs::set_permissions(&game, std::fs::Permissions::from_mode(0o500)).unwrap();
        assert_eq!(
            keep_before_play(&root, &kept),
            Err(backend_failure(crate::save_keeper::SAVES_UNAVAILABLE))
        );
        // No Save at all and no link can be made: the game makes its own.
        std::fs::set_permissions(&game, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::remove_dir_all(game.join("Save.pd2launcher-moved")).unwrap();
        std::fs::set_permissions(&game, std::fs::Permissions::from_mode(0o500)).unwrap();
        assert_eq!(
            keep_before_play(&root, &kept),
            Ok(crate::save_keeper::PutAside::default())
        );
        std::fs::set_permissions(&game, std::fs::Permissions::from_mode(0o700)).unwrap();

        // A keep that works reports what it put aside, for Play's notice.
        std::fs::write(kept.path().join("Hero.d2s"), b"kept").unwrap();
        std::fs::create_dir(game.join("Save")).unwrap();
        std::fs::write(game.join("Save/Hero.d2s"), b"other").unwrap();
        let put_aside = keep_before_play(&root, &kept).unwrap();
        assert_eq!(put_aside.characters, 1);
        assert!(put_aside.folder.is_some());
    }
}
