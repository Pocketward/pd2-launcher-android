//! GPUI presentation adapter.
//!
//! Rendering reads display state and emits intent only. I/O and workflows are
//! owned by the composition root and non-presentation modules.

use std::{
    fmt,
    path::PathBuf,
    sync::mpsc::{Receiver, SyncSender, sync_channel},
    time::Duration,
};
#[cfg(feature = "gui")]
use std::{
    ops::Range,
    sync::mpsc::{TryRecvError, TrySendError},
};

#[cfg(feature = "gui")]
use gpui_kit::component::{
    Sizable,
    button::{Button, ButtonVariants},
    input::InputState,
};
#[cfg(feature = "gui")]
use gpui_kit::{
    App, Bounds, Context, CursorStyle, Element, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, FocusHandle, Focusable, GlobalElementId, GpuSpecs, KeyBinding, LayoutId,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, PathPromptOptions,
    Pixels, Point, ShapedLine, SharedString, Style, Subscription, Task, TextRun, UTF16Selection,
    Window, WindowBounds, WindowDecorations, WindowOptions, actions, div, fill, point, prelude::*,
    px, relative, size,
};
use zeroize::Zeroize;

#[cfg(feature = "gui")]
mod page;
#[cfg(all(feature = "gui", debug_assertions))]
pub(crate) mod preview;
#[cfg(feature = "gui")]
mod theme;

use crate::{
    InstallationStatus, UpdateStatus,
    audit::WineLibrary,
    automation::AfterReady,
    operation::{EnvironmentPart, EnvironmentProgress},
    persistence::SetupRecord,
    sources::DownloadArtifact,
};
#[cfg(feature = "gui")]
use crate::{
    InterruptedStage,
    automation::AutomationRequest,
    startup_credentials::{
        InvalidInstallerSource, LaunchMode, StartupInputs, StartupInstallerSource,
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HostView {
    Checking,
    Supported,
    Unsupported,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AuthorizationView {
    Absent,
    Authorized,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum InstallerMediaView {
    NotSelected,
    Validated(PathBuf),
    /// Acquire the official downloader wrappers from Blizzard during install.
    BlizzardDownload,
    Invalid(&'static str),
    /// A Diablo II the user already has, with the result
    /// of checking it.
    Existing(ExistingInstallView),
}

impl InstallerMediaView {
    pub(crate) fn is_ready(&self) -> bool {
        match self {
            Self::Validated(_) | Self::BlizzardDownload => true,
            Self::Existing(existing) => existing.problem.is_none(),
            Self::NotSelected | Self::Invalid(_) => false,
        }
    }

    /// The existing installation chosen, if that is the choice.
    pub(crate) fn existing(&self) -> Option<&ExistingInstallView> {
        match self {
            Self::Existing(existing) => Some(existing),
            _ => None,
        }
    }
}

/// A Diablo II folder the user chose instead of Blizzard's installers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ExistingInstallView {
    pub(crate) path: PathBuf,
    /// Why it can't be used; `None` for a usable folder, of any version.
    pub(crate) problem: Option<ExistingProblemView>,
    /// What a copy writes (with its ProjectD2, on the desktop).
    pub(crate) copy_bytes: u64,
    /// Whether Windows programs can use it where it is.
    pub(crate) in_place_possible: bool,
    /// It holds a Project Diablo 2 too, which the install adopts instead of
    /// running the PD2 installer.
    pub(crate) project_diablo: bool,
}

/// Why a chosen Diablo II folder can't be used, in player terms.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExistingProblemView {
    NotFound,
    Unreadable,
    NotDiablo,
    NoExpansion,
    MissingFile(&'static str),
    Links,
    InsideLauncher,
    UnsupportedPath,
}

impl ExistingProblemView {
    pub(crate) fn message(self) -> String {
        match self {
            Self::NotFound => "That folder doesn't exist or can't be opened.".to_owned(),
            Self::Unreadable => {
                "The launcher can't read everything in this folder. Check its permissions.".to_owned()
            }
            Self::NotDiablo => {
                "Diablo II isn't in this folder. Choose the folder that contains Game.exe.".to_owned()
            }
            Self::NoExpansion => {
                "Lord of Destruction isn't installed here. Install it with Blizzard's installer first."
                    .to_owned()
            }
            Self::MissingFile(name) => format!(
                "A game file is missing ({name}). Repair or reinstall Diablo II there first."
            ),
            Self::Links => {
                "This folder contains links to other places. Choose a folder with the real game files."
                    .to_owned()
            }
            Self::InsideLauncher => {
                "That's the launcher's own folder. Choose your own Diablo II folder.".to_owned()
            }
            Self::UnsupportedPath => {
                "Windows programs can't use this folder's name. Move it to a folder with a plain name."
                    .to_owned()
            }
        }
    }
}

/// What was found in a usable Diablo II folder, in one line: both games when
/// it holds a Project Diablo 2 too, which Install brings up to date instead
/// of running the PD2 installer.
pub(crate) fn existing_found_line(existing: &ExistingInstallView) -> String {
    if existing.project_diablo {
        format!(
            "Diablo II and Project Diablo 2 found in {}. Install brings Project Diablo 2 up to date.",
            existing.path.display()
        )
    } else {
        format!("Diablo II found in {}", existing.path.display())
    }
}

/// How an existing installation becomes the game.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ImportModeView {
    /// Copy it into the launcher's game folder (recommended).
    Copy,
    /// Use it where it is.
    InPlace,
}

/// The Download step's result, from the service's evidence.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct DownloadsView {
    /// Everything the remaining install needs is downloaded and verified.
    pub(crate) complete: bool,
    /// The downloads include Blizzard's installers (not only what an
    /// existing installation needs).
    pub(crate) installers: bool,
    pub(crate) total_bytes: u64,
}

/// A game folder used in place, and whether it is reachable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GameFolderView {
    pub(crate) path: PathBuf,
    pub(crate) available: bool,
}

/// Where the effective URL of one download source comes from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SourceBadgeView {
    BuiltInVerified,
    CustomFromEnvironment,
    Custom,
}

impl SourceBadgeView {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::BuiltInVerified => "Built-in verified",
            Self::CustomFromEnvironment => "Custom from environment",
            Self::Custom => "Custom",
        }
    }
}

/// One row of the Diagnostics "Download sources" section.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DownloadSourceView {
    pub(crate) artifact: DownloadArtifact,
    pub(crate) effective_url: String,
    pub(crate) badge: SourceBadgeView,
    /// Stable code of the last rejected Save for this row.
    pub(crate) error: Option<&'static str>,
    /// The source the installed component actually came from, shown only
    /// when it differs from the configured source.
    pub(crate) installed: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GameView {
    Idle,
    Running,
}

/// A notice about the game's first launch. It is shown before that launch,
/// while that game runs, and after it exits, and never gates Play.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GameNoticeView {
    /// D2GL has not written its configuration yet: the game's first launch
    /// with it.
    FirstD2glLaunch,
}

impl GameNoticeView {
    pub(crate) const fn title(self) -> &'static str {
        match self {
            Self::FirstD2glLaunch => "First launch: the game's resolution will probably be wrong",
        }
    }

    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::FirstD2glLaunch => {
                "The picture may be too small with black borders, stretched, or cut off at the edges. The main menu has no setting for this — start or join a game first, then press Esc and go to Video → D2GL → Advanced. Turn Fullscreen off and back on, and the game keeps the right size from then on."
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RendererPreferenceView {
    Glide,
    DirectDraw,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SettingsPersistenceView {
    Loading,
    Saved,
    Failed(&'static str),
}

/// How DirectDraw mode draws (PD2's DirectDraw wrapper, cnc-ddraw).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum DirectDrawRendererView {
    #[default]
    Pd2,
    OpenGl,
    Direct3d9,
    Software,
}

/// Where Blizzard's installers are downloaded from: Blizzard's own
/// downloader, or the direct download (`crate::blizzard`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum InstallSourceView {
    #[default]
    Official,
    DirectDownload,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LaunchSettingsView {
    renderer: RendererPreferenceView,
    direct_draw_renderer: DirectDrawRendererView,
    skip_to_bnet: bool,
    sound_in_background: bool,
    official_launcher_software_rendering: bool,
    app_menu_entry: bool,
    /// Project Diablo 2's update checks: off, none runs and the client is
    /// accepted as installed.
    update_checks: bool,
    install_source: InstallSourceView,
    /// The player's own start options for Play, after the Launcher's own
    /// (the Android app's Custom start options); empty for none.
    custom_arguments: String,
    /// The program Play starts, from the game folder (the Android app's
    /// Game program).
    game_program: String,
    /// The program the official PD2 launcher button opens, from the game
    /// folder (the Android app's Launcher program).
    launcher_program: String,
    /// The player's own start options for the official PD2 launcher; empty
    /// for none.
    launcher_custom_arguments: String,
    persistence: SettingsPersistenceView,
}

impl Default for LaunchSettingsView {
    fn default() -> Self {
        Self {
            renderer: RendererPreferenceView::Glide,
            direct_draw_renderer: DirectDrawRendererView::Pd2,
            skip_to_bnet: true,
            sound_in_background: false,
            official_launcher_software_rendering: true,
            app_menu_entry: true,
            update_checks: true,
            install_source: InstallSourceView::Official,
            custom_arguments: String::new(),
            game_program: crate::persistence::DEFAULT_GAME_PROGRAM.to_owned(),
            launcher_program: crate::persistence::DEFAULT_LAUNCHER_PROGRAM.to_owned(),
            launcher_custom_arguments: String::new(),
            persistence: SettingsPersistenceView::Loading,
        }
    }
}

impl LaunchSettingsView {
    pub(crate) fn new(
        renderer: RendererPreferenceView,
        skip_to_bnet: bool,
        sound_in_background: bool,
    ) -> Self {
        Self {
            renderer,
            direct_draw_renderer: DirectDrawRendererView::Pd2,
            skip_to_bnet,
            sound_in_background,
            official_launcher_software_rendering: true,
            app_menu_entry: true,
            update_checks: true,
            install_source: InstallSourceView::Official,
            custom_arguments: String::new(),
            game_program: crate::persistence::DEFAULT_GAME_PROGRAM.to_owned(),
            launcher_program: crate::persistence::DEFAULT_LAUNCHER_PROGRAM.to_owned(),
            launcher_custom_arguments: String::new(),
            persistence: SettingsPersistenceView::Saved,
        }
    }

    pub(crate) const fn with_direct_draw_renderer(
        mut self,
        renderer: DirectDrawRendererView,
    ) -> Self {
        self.direct_draw_renderer = renderer;
        self
    }

    pub(crate) const fn direct_draw_renderer(&self) -> DirectDrawRendererView {
        self.direct_draw_renderer
    }

    pub(crate) const fn with_official_launcher_software_rendering(mut self, enabled: bool) -> Self {
        self.official_launcher_software_rendering = enabled;
        self
    }

    pub(crate) const fn with_app_menu_entry(mut self, enabled: bool) -> Self {
        self.app_menu_entry = enabled;
        self
    }

    pub(crate) const fn app_menu_entry(&self) -> bool {
        self.app_menu_entry
    }

    pub(crate) const fn with_update_checks(mut self, enabled: bool) -> Self {
        self.update_checks = enabled;
        self
    }

    pub(crate) const fn update_checks(&self) -> bool {
        self.update_checks
    }

    pub(crate) const fn with_install_source(mut self, source: InstallSourceView) -> Self {
        self.install_source = source;
        self
    }

    pub(crate) const fn install_source(&self) -> InstallSourceView {
        self.install_source
    }

    pub(crate) fn with_custom_arguments(mut self, arguments: String) -> Self {
        self.custom_arguments = arguments;
        self
    }

    pub(crate) fn custom_arguments(&self) -> &str {
        &self.custom_arguments
    }

    pub(crate) fn with_game_program(mut self, path: String) -> Self {
        self.game_program = path;
        self
    }

    pub(crate) fn game_program(&self) -> &str {
        &self.game_program
    }

    pub(crate) fn with_launcher_program(mut self, path: String) -> Self {
        self.launcher_program = path;
        self
    }

    pub(crate) fn launcher_program(&self) -> &str {
        &self.launcher_program
    }

    pub(crate) fn with_launcher_custom_arguments(mut self, arguments: String) -> Self {
        self.launcher_custom_arguments = arguments;
        self
    }

    pub(crate) fn launcher_custom_arguments(&self) -> &str {
        &self.launcher_custom_arguments
    }

    pub(crate) const fn failed(mut self, code: &'static str) -> Self {
        self.persistence = SettingsPersistenceView::Failed(code);
        self
    }

    pub(crate) const fn renderer(&self) -> RendererPreferenceView {
        self.renderer
    }

    pub(crate) const fn skip_to_bnet(&self) -> bool {
        self.skip_to_bnet
    }

    pub(crate) const fn sound_in_background(&self) -> bool {
        self.sound_in_background
    }

    pub(crate) const fn official_launcher_software_rendering(&self) -> bool {
        self.official_launcher_software_rendering
    }

    pub(crate) const fn persistence(&self) -> SettingsPersistenceView {
        self.persistence
    }
}

/// The Android app's graphics driver for one Play (its Settings, Graphics
/// driver): the App userland's Turnip as it is, or the same Turnip with the
/// app's compatibility values. The desktop always
/// plays with `Standard`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum GraphicsDriver {
    #[default]
    Standard,
    Compatibility,
}

/// The Android app's D2GL graphics profile for one Play (its Settings, Game,
/// Graphics profile, with Automatic resolved by the app): the engine writes
/// the profile's values into `d2gl.json` when it is not the one last
/// applied ([`crate::process::GraphicsProfile`]). The desktop sends none.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GraphicsProfile {
    /// Project Diablo 2's own D2GL settings.
    Default,
    /// D2GL's advanced settings tuned down for older devices.
    Performance,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SettingsChange {
    Renderer(RendererPreferenceView),
    SkipToBnet(bool),
    SoundInBackground(bool),
    OfficialLauncherSoftwareRendering(bool),
    DirectDrawRenderer(DirectDrawRendererView),
    AppMenuEntry(bool),
    UpdateChecks(bool),
    /// Where Blizzard's installers are downloaded from (the Android app's
    /// Control channel only, for now).
    InstallSource(InstallSourceView),
    /// The player's own start options for Play, already accepted by
    /// [`crate::persistence::custom_arguments`] (the Android app's Control
    /// channel only).
    CustomArguments(String),
    /// The program Play starts, already accepted by
    /// [`crate::persistence::program_path`] (the Android app's Control
    /// channel only).
    GameProgram(String),
    /// The program the official PD2 launcher button opens, already accepted
    /// by [`crate::persistence::program_path`] (the Android app's Control
    /// channel only).
    LauncherProgram(String),
    /// The official PD2 launcher's own start options, already accepted by
    /// [`crate::persistence::custom_arguments`] (the Android app's Control
    /// channel only).
    LauncherCustomArguments(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    dead_code,
    reason = "the normative state vocabulary includes finer-grained adapter phases"
)]
pub(crate) enum OperationPhase {
    HostAudit,
    RuntimeAcquisition,
    PrefixInitialization,
    BaseGameInstall,
    ExpansionInstall,
    BaseGameDownload,
    ExpansionDownload,
    ImportGame,
    ClientDownload,
    Verification,
    Promotion,
    Repair,
    GameExecution,
    OfficialLauncher,
    Reset,
    Relocation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    dead_code,
    reason = "the normative operation protocol retains safe transition states"
)]
pub(crate) enum OperationStatusView {
    Queued,
    Planning,
    Running,
    SafeStopping,
    Verifying,
    Committing,
    Succeeded,
    Failed,
    Indeterminate,
    Cancelled,
}

impl OperationStatusView {
    pub(crate) fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Indeterminate | Self::Cancelled
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OperationView {
    phase: OperationPhase,
    status: OperationStatusView,
    title: String,
    detail: String,
    evidence: Vec<String>,
    progress_percent: Option<u8>,
    progress_completed: Option<u64>,
    progress_total: Option<u64>,
    progress_unit: Option<String>,
    progress_summary: Option<String>,
    mutating: bool,
    /// The stable code of a Failed, Cancelled, or Indeterminate outcome.
    failure: Option<&'static str>,
    /// The job this outcome belongs to.
    job: Option<ActiveJobView>,
    /// The game environment's part being set up (the Android app shows it).
    environment: Option<EnvironmentProgress>,
}

impl OperationView {
    pub(crate) fn mutating(
        phase: OperationPhase,
        status: OperationStatusView,
        title: impl Into<String>,
    ) -> Self {
        Self {
            phase,
            status,
            title: title.into(),
            detail: "Launcher-owned work is active. You may request a safe stop when the current phase permits it.".to_owned(),
            evidence: Vec::new(),
            progress_percent: None,
            progress_completed: None,
            progress_total: None,
            progress_unit: None,
            progress_summary: None,
            mutating: true,
            failure: None,
            job: None,
            environment: None,
        }
    }

    pub(crate) fn read_only(
        phase: OperationPhase,
        status: OperationStatusView,
        title: impl Into<String>,
    ) -> Self {
        Self {
            phase,
            status,
            title: title.into(),
            detail: "The Launcher is reading evidence without changing the Managed installation."
                .to_owned(),
            evidence: Vec::new(),
            progress_percent: None,
            progress_completed: None,
            progress_total: None,
            progress_unit: None,
            progress_summary: None,
            mutating: false,
            failure: None,
            job: None,
            environment: None,
        }
    }

    pub(crate) fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }

    pub(crate) fn with_evidence(mut self, evidence: Vec<String>) -> Self {
        self.evidence = evidence;
        self
    }

    pub(crate) fn with_typed_progress(
        mut self,
        completed: u64,
        total: Option<u64>,
        unit: impl Into<String>,
    ) -> Self {
        self.progress_unit = Some(unit.into());
        self.progress_completed = Some(completed);
        if let Some(total) = total {
            let total = total.max(1);
            let completed = completed.min(total);
            self.progress_percent = Some(((completed.saturating_mul(100)) / total) as u8);
            self.progress_completed = Some(completed);
            self.progress_total = Some(total);
        }
        self
    }

    pub(crate) fn with_progress_summary(mut self, summary: impl Into<String>) -> Self {
        self.progress_summary = Some(summary.into());
        self
    }

    pub(crate) fn with_failure(mut self, code: &'static str) -> Self {
        self.failure = Some(code);
        self
    }

    pub(crate) fn with_job(mut self, job: ActiveJobView) -> Self {
        self.job = Some(job);
        self
    }

    pub(crate) fn job(&self) -> Option<ActiveJobView> {
        self.job
    }

    pub(crate) fn with_environment(mut self, environment: EnvironmentProgress) -> Self {
        self.environment = Some(environment);
        self
    }

    pub(crate) const fn environment(&self) -> Option<EnvironmentProgress> {
        self.environment
    }

    /// The game environment's running part, if this operation sets it up.
    pub(crate) fn environment_line(&self) -> Option<String> {
        self.environment.map(environment_part_line)
    }

    pub(crate) fn failure(&self) -> Option<&'static str> {
        self.failure
    }

    pub(crate) fn owns_mutation(&self) -> bool {
        self.mutating && !self.status.is_terminal()
    }

    pub(crate) const fn phase(&self) -> OperationPhase {
        self.phase
    }

    pub(crate) fn title(&self) -> &str {
        &self.title
    }

    pub(crate) const fn progress_percent(&self) -> Option<u8> {
        self.progress_percent
    }

    pub(crate) const fn progress_completed(&self) -> Option<u64> {
        self.progress_completed
    }

    pub(crate) const fn progress_total(&self) -> Option<u64> {
        self.progress_total
    }

    pub(crate) fn progress_unit(&self) -> Option<&str> {
        self.progress_unit.as_deref()
    }

    pub(crate) const fn status(&self) -> OperationStatusView {
        self.status
    }

    #[cfg(test)]
    pub(crate) const fn progress(&self) -> Option<(u64, u64)> {
        match (self.progress_completed, self.progress_total) {
            (Some(completed), Some(total)) => Some((completed, total)),
            _ => None,
        }
    }

    pub(crate) fn detail(&self) -> &str {
        &self.detail
    }

    pub(crate) fn progress_summary(&self) -> Option<&str> {
        self.progress_summary.as_deref()
    }
}

/// The game environment's running part, its place and how many parts this
/// setup runs: "Setting up Windows (1 of 4)". An environment that lacks
/// only some parts runs just those ("Adding .NET (1 of 2)").
pub(crate) fn environment_part_line(progress: EnvironmentProgress) -> String {
    let planned = progress.planned().collect::<Vec<_>>();
    let place = planned
        .iter()
        .position(|part| *part == progress.running())
        .map_or(1, |index| index + 1);
    let name = match progress.running() {
        EnvironmentPart::Windows => "Setting up Windows",
        EnvironmentPart::WebComponents => "Adding web components",
        EnvironmentPart::Dotnet => "Adding .NET",
        EnvironmentPart::LauncherSupport => "Adding official launcher support",
    };
    format!("{name} ({place} of {})", planned.len().max(place))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum EvidenceTone {
    Verified,
    Working,
    Advisory,
    Failure,
}

/// The evidence label of an intent the coordinator refused; its detail is
/// the reason code (see [`SequencedEvent::refusal`]).
pub(crate) const REQUEST_NOT_STARTED: &str = "Request not started";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EvidenceView {
    pub(crate) label: String,
    pub(crate) detail: String,
    pub(crate) tone: EvidenceTone,
}

impl EvidenceView {
    pub(crate) fn new(
        label: impl Into<String>,
        detail: impl Into<String>,
        tone: EvidenceTone,
    ) -> Self {
        Self {
            label: label.into(),
            detail: detail.into(),
            tone,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SystemContractView {
    pub(crate) host: String,
    pub(crate) runtime: String,
    pub(crate) prefix: String,
    pub(crate) artifact_health: String,
    /// What this computer is missing, in the order the audit found it.
    pub(crate) missing: Vec<Prerequisite>,
    /// Which package manager the missing items' commands use.
    pub(crate) distro: DistroFamily,
}

/// One thing a computer needs before the Launcher can install the game.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Prerequisite {
    DisplaySession,
    Fex,
    FexRootFs,
    PageSize,
    WineCannotRun,
    Ia32Execution,
    NoExec,
    I386Loader,
    /// Wine could not start: the dynamic loader named a missing library.
    Libraries,
    /// One 64-bit library the WoW64 runtime loads for the game.
    WineLibrary(WineLibrary),
    Libunwind,
    XWayland,
    VulkanLoader,
    VulkanDriver,
    HardwareGraphics,
    LauncherBuild,
    Architecture,
    HostInformation,
    UncertifiedBuild,
}

/// A distribution family whose package manager the Launcher can name.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum DistroFamily {
    /// Debian, Ubuntu, and derivatives: apt.
    Debian,
    /// Fedora and RHEL-like: dnf.
    Fedora,
    /// Arch and derivatives: pacman.
    Arch,
    /// openSUSE and SUSE: zypper.
    OpenSuse,
    #[default]
    Unknown,
}

impl DistroFamily {
    /// Classify os-release(5) text by `ID`, then `ID_LIKE`.
    pub(crate) fn from_os_release(text: &str) -> Self {
        let value = |name: &str| {
            text.lines()
                .filter_map(|line| line.trim().split_once('='))
                .find(|(key, _)| key.trim() == name)
                .map(|(_, value)| {
                    value
                        .trim()
                        .trim_matches(|c| c == '"' || c == '\'')
                        .to_ascii_lowercase()
                })
                .unwrap_or_default()
        };
        let classify = |id: &str| match id {
            "debian" | "ubuntu" | "linuxmint" | "pop" | "elementary" | "zorin" | "raspbian" => {
                Some(Self::Debian)
            }
            "fedora" | "rhel" | "centos" | "rocky" | "almalinux" | "nobara" | "bazzite" => {
                Some(Self::Fedora)
            }
            "arch" | "manjaro" | "endeavouros" | "cachyos" | "garuda" | "steamos" => {
                Some(Self::Arch)
            }
            id if id == "suse" || id.starts_with("opensuse") || id == "sles" => {
                Some(Self::OpenSuse)
            }
            _ => None,
        };
        std::iter::once(value("ID"))
            .chain(value("ID_LIKE").split_whitespace().map(str::to_owned))
            .find_map(|id| classify(&id))
            .unwrap_or_default()
    }

    const fn install_prefix(self) -> Option<&'static str> {
        match self {
            Self::Debian => Some("sudo apt install"),
            Self::Fedora => Some("sudo dnf install"),
            Self::Arch => Some("sudo pacman -S --needed"),
            Self::OpenSuse => Some("sudo zypper install"),
            Self::Unknown => None,
        }
    }
}

impl Prerequisite {
    /// What is missing, in plain words.
    pub(crate) const fn plain(self) -> &'static str {
        match self {
            Self::DisplaySession => "A desktop session (Wayland or X11)",
            Self::Fex => {
                "FEX-Emu, which runs Windows programs on ARM computers (on Ubuntu, from the FEX-Emu PPA: ppa:fex-emu/fex)"
            }
            Self::FexRootFs => {
                "FEX-Emu's x86 system files (its RootFS): run FEXRootFSFetcher in a terminal"
            }
            Self::PageSize => "A kernel with 4K memory pages, which FEX-Emu needs",
            Self::WineCannotRun => "Permission to run the launcher's Windows compatibility layer",
            Self::Ia32Execution => "32-bit program support in the kernel",
            Self::NoExec => "Permission to run programs from the launcher's folder",
            Self::I386Loader => "32-bit system libraries",
            Self::Libraries => "A system library Windows programs need to start",
            Self::WineLibrary(WineLibrary::X11) => {
                "The X11 library (libX11), which the game's windows need"
            }
            Self::WineLibrary(WineLibrary::Xext) => {
                "The X11 extension library (libXext), which the game's windows need"
            }
            Self::WineLibrary(WineLibrary::FreeType) => {
                "The FreeType font library, which the Diablo II installer needs"
            }
            Self::WineLibrary(WineLibrary::Fontconfig) => {
                "The Fontconfig library, without which Windows support can't finish setting up"
            }
            Self::WineLibrary(WineLibrary::OpenGl) => {
                "OpenGL (libEGL or libGL), which the game draws with"
            }
            Self::Libunwind => "The libunwind library",
            Self::XWayland => "XWayland, so Windows programs can open windows",
            Self::VulkanLoader => "Vulkan support (the Vulkan loader)",
            Self::VulkanDriver => "A Vulkan graphics driver",
            Self::HardwareGraphics => {
                "Graphics acceleration (the game can't use software rendering)"
            }
            Self::LauncherBuild => "The official PD2 Launcher download for this computer",
            Self::Architecture => "A computer with an x86-64 or ARM64 processor",
            Self::HostInformation => "Readable system information",
            Self::UncertifiedBuild => {
                "This build of the launcher hasn't been approved for this kind of computer yet"
            }
        }
    }

    /// Packages that provide it, for `family`'s package manager.
    pub(crate) const fn packages(self, family: DistroFamily) -> &'static [&'static str] {
        use DistroFamily::{Arch, Debian, Fedora, OpenSuse};
        match (self, family) {
            (Self::Libunwind, Debian) => &["libunwind8"],
            (Self::Libunwind, Fedora | Arch | OpenSuse) => &["libunwind"],
            (Self::XWayland, Debian) => &["xwayland"],
            (Self::XWayland, Fedora) => &["xorg-x11-server-Xwayland"],
            (Self::XWayland, Arch) => &["xorg-xwayland"],
            (Self::XWayland, OpenSuse) => &["xwayland"],
            (Self::VulkanLoader, Debian | OpenSuse) => &["libvulkan1"],
            (Self::VulkanLoader, Fedora) => &["vulkan-loader"],
            (Self::VulkanLoader, Arch) => &["vulkan-icd-loader"],
            (Self::VulkanDriver | Self::HardwareGraphics, Debian | Fedora) => {
                &["mesa-vulkan-drivers"]
            }
            (Self::VulkanDriver | Self::HardwareGraphics, Arch) => {
                &["vulkan-radeon", "vulkan-intel"]
            }
            (Self::VulkanDriver | Self::HardwareGraphics, OpenSuse) => {
                &["libvulkan_radeon", "libvulkan_intel"]
            }
            (Self::WineLibrary(WineLibrary::X11), Debian) => &["libx11-6"],
            (Self::WineLibrary(WineLibrary::X11), Arch) => &["libx11"],
            (Self::WineLibrary(WineLibrary::Xext), Debian) => &["libxext6"],
            (Self::WineLibrary(WineLibrary::Xext), Arch) => &["libxext"],
            (Self::WineLibrary(WineLibrary::FreeType), Debian) => &["libfreetype6"],
            (Self::WineLibrary(WineLibrary::FreeType), Arch) => &["freetype2"],
            (Self::WineLibrary(WineLibrary::Fontconfig), Debian) => &["libfontconfig1"],
            (Self::WineLibrary(WineLibrary::Fontconfig), Arch) => &["fontconfig"],
            // GLVND's libEGL plus Mesa, which renders in software when no
            // GPU driver is present (Debian's libegl1 pulls libegl-mesa0).
            (Self::WineLibrary(WineLibrary::OpenGl), Debian) => &["libegl1"],
            (Self::WineLibrary(WineLibrary::OpenGl), Arch) => &["libglvnd", "mesa"],
            _ => &[],
        }
    }
}

/// One copyable command that installs every missing package the Launcher
/// can name for `family`, or `None` when it names none.
pub(crate) fn install_command(family: DistroFamily, missing: &[Prerequisite]) -> Option<String> {
    let prefix = family.install_prefix()?;
    let mut packages: Vec<&str> = Vec::new();
    for package in missing
        .iter()
        .flat_map(|prerequisite| prerequisite.packages(family))
    {
        if !packages.contains(package) {
            packages.push(package);
        }
    }
    (!packages.is_empty()).then(|| format!("{prefix} {}", packages.join(" ")))
}

impl SystemContractView {
    fn pinned() -> Self {
        Self {
            host: "Checking this computer".to_owned(),
            runtime: format!(
                "The launcher downloads {} · no system Wine needed",
                crate::runtime::current().display_name
            ),
            prefix: "The launcher sets up its own game environment".to_owned(),
            artifact_health: "Not checked yet".to_owned(),
            missing: Vec::new(),
            distro: DistroFamily::Unknown,
        }
    }
}

/// Where the Managed installation lives.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LocationView {
    pub(crate) path: PathBuf,
    /// The default XDG data location rather than a chosen one.
    pub(crate) default: bool,
    /// The root was resolved and proven by its marker. An unavailable
    /// location is never installed into.
    pub(crate) available: bool,
    /// The installation a move to another filesystem left behind.
    pub(crate) previous: Option<PathBuf>,
}

/// The Launcher job that owns the single mutation slot, as the coordinator
/// started it. Phases alone cannot tell an install from an update.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ActiveJobView {
    /// Step 3: download and verify everything.
    Download,
    /// Step 4: install (or import) Diablo II.
    InstallBaseGame,
    /// Step 5: install Project Diablo 2.
    InstallProjectDiablo,
    Update,
    Repair,
    OfficialLauncher,
    Reset,
    Relocation,
    /// Removing what this Launcher's own downloads made, for an import.
    DiscardDownloads,
}

impl ActiveJobView {
    /// One of the three setup jobs.
    pub(crate) const fn is_setup(self) -> bool {
        matches!(
            self,
            Self::Download | Self::InstallBaseGame | Self::InstallProjectDiablo
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct UiSnapshot {
    host: HostView,
    authorization: AuthorizationView,
    installer_media: InstallerMediaView,
    /// Why the last installer choice was refused while a prior valid one
    /// was kept.
    installer_media_rejection: Option<&'static str>,
    installation: InstallationStatus,
    update: UpdateStatus,
    operation: Option<OperationView>,
    game: GameView,
    game_notice: Option<GameNoticeView>,
    unresolved_journal: bool,
    launch_preflight_passed: bool,
    system: SystemContractView,
    settings: LaunchSettingsView,
    evidence: Vec<EvidenceView>,
    download_sources: Vec<DownloadSourceView>,
    active_job: Option<ActiveJobView>,
    /// The setup step whose editor is open, if any.
    editing: Option<SetupStep>,
    /// What a user-requested install stop kept, until dismissed.
    stop_result: Option<StopResultView>,
    location: Option<LocationView>,
    /// Why the last location choice or move was refused.
    location_error: Option<&'static str>,
    /// Opening the official launcher asks first (its binary is unverified).
    official_launcher_unverified: bool,
    /// The installed game's OpenGL renderer draws in software; never a gate.
    game_software_rendering: bool,
    /// The installed copy's language, when an audit read it; PD2 needs
    /// English, so another one is a warning, never a gate.
    game_language: Option<crate::game_language::GameLanguage>,
    /// The last audit accepted the Managed runtime and prefix (the game
    /// environment is set up).
    environment_ready: bool,
    downloads: DownloadsView,
    /// Set only for a game folder used in place.
    game_folder: Option<GameFolderView>,
    /// The report being saved, or the last one saved, until dismissed.
    report: Option<ReportView>,
    /// The FEX setting, on ARM64 builds only.
    fex_setting: Option<FexSettingView>,
    /// How far the setup got, by the Launcher's record: the page a start
    /// opens on while its first check runs ([`opening_step`]).
    setup_record: SetupRecord,
    /// Open game folder was pressed: opening it, or how that went.
    game_files_open: Option<FolderOpenView>,
    /// What keeping the characters put in a backup, until dismissed.
    saves_put_aside: Option<SavesPutAsideView>,
    last_event_seq: u64,
}

/// Shown when the system has no file picker for Settings → FEX.
pub(crate) const FEX_PICKER_UNAVAILABLE: &str =
    "The system file picker is unavailable. Type the full path of the FEX program instead.";

/// Which FEX runs the launcher's x86 Wine on ARM64.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum FexSourceView {
    /// The FEX and RootFS the launcher keeps in its own folder.
    Builtin,
    /// A FEX on this device.
    #[default]
    Device,
}

/// Settings → FEX on ARM64 builds: built-in or device FEX, the device FEX
/// the player set, and the one in use since the launcher started (a change
/// takes effect on restart).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct FexSettingView {
    /// The FEX in use: the player's choice, or the step 1 default (the
    /// built-in FEX on a fresh installation).
    pub(crate) source: FexSourceView,
    /// Whether this build can install its own FEX.
    pub(crate) builtin_available: bool,
    /// The built-in FEX is installed and current.
    pub(crate) builtin_installed: bool,
    /// What the built-in FEX downloads: its package and RootFS, in bytes.
    pub(crate) builtin_download_bytes: u64,
    pub(crate) saved: Option<PathBuf>,
    pub(crate) in_use: Option<PathBuf>,
    /// Why the last path entered was not saved.
    pub(crate) problem: Option<&'static str>,
}

impl FexSettingView {
    /// The built-in FEX is in use but still to be downloaded (step 3).
    pub(crate) fn builtin_pending(&self) -> bool {
        self.source == FexSourceView::Builtin && !self.builtin_installed
    }
}

/// Settings → Game files → Open game folder: opening the game folder in
/// the file manager.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FolderOpenView {
    Opening,
    Opened,
    /// No file manager took it: the path stays on screen to copy.
    Failed,
}

/// Characters, or other things of a `Save` folder, that keeping the
/// characters in their own folder put in a backup instead: one
/// line on Play, with a way to the backup, until dismissed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SavesPutAsideView {
    /// Characters that had another copy that differed: the one saved last
    /// is kept, the other is in the backup.
    pub(crate) characters: usize,
    /// Other saved files (the shared stash, PD2's files in `Save/ProjectD2`)
    /// that had another copy that differed: the one saved last is kept, the
    /// other is in the backup.
    pub(crate) files: usize,
    /// Other things the game's `Save` folder held (hidden files, links).
    pub(crate) others: usize,
    /// The backup folder written last, which Open backups shows.
    pub(crate) folder: PathBuf,
    /// Open backups was pressed: opening it, or how that went.
    pub(crate) open: Option<FolderOpenView>,
}

impl SavesPutAsideView {
    pub(crate) const TITLE: &'static str = "Some saved files were put aside";

    /// What was put aside, in plain words: characters apart from the other
    /// saved files.
    pub(crate) fn message(&self) -> String {
        let character_count = match self.characters {
            0 => None,
            1 => Some("1 character".to_owned()),
            n => Some(format!("{n} characters")),
        };
        let file_count = match (self.files, self.characters) {
            (0, _) => None,
            (1, 0) => Some("1 saved file".to_owned()),
            (1, _) => Some("1 other saved file".to_owned()),
            (n, 0) => Some(format!("{n} saved files")),
            (n, _) => Some(format!("{n} other saved files")),
        };
        let copies = character_count
            .into_iter()
            .chain(file_count)
            .collect::<Vec<_>>();
        let characters = (!copies.is_empty()).then(|| {
            let kept = if self.characters + self.files == 1 {
                "the one saved last is kept and the other is in backups"
            } else {
                "the ones saved last are kept and the others are in backups"
            };
            format!(
                "{} had another copy that differed: {kept}.",
                copies.join(" and ")
            )
        });
        let others = match self.others {
            0 => None,
            1 => Some("1 other item from the game's Save folder is in backups.".to_owned()),
            n => Some(format!(
                "{n} other items from the game's Save folder are in backups."
            )),
        };
        characters
            .into_iter()
            .chain(others)
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Both together, in the later one's folder.
    fn and(self, later: Self) -> Self {
        Self {
            characters: self.characters + later.characters,
            files: self.files + later.files,
            others: self.others + later.others,
            folder: later.folder,
            open: None,
        }
    }
}

/// Diagnostics → Save report, as the player sees it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ReportView {
    Creating,
    /// Saved at `path`; `opened` says whether its folder opened in the file
    /// manager (`None` while that is still being tried).
    Saved {
        path: PathBuf,
        opened: Option<bool>,
    },
    Failed(&'static str),
}

impl UiSnapshot {
    pub(crate) fn initial() -> Self {
        Self {
            host: HostView::Checking,
            authorization: AuthorizationView::Absent,
            installer_media: InstallerMediaView::NotSelected,
            installer_media_rejection: None,
            installation: InstallationStatus::NeedsBaseGame,
            update: UpdateStatus::Unknown,
            operation: Some(OperationView::read_only(
                OperationPhase::HostAudit,
                OperationStatusView::Running,
                "Checking this device",
            )),
            game: GameView::Idle,
            game_notice: None,
            unresolved_journal: false,
            launch_preflight_passed: false,
            system: SystemContractView::pinned(),
            settings: LaunchSettingsView::default(),
            evidence: Vec::new(),
            download_sources: Vec::new(),
            active_job: None,
            editing: None,
            stop_result: None,
            location: None,
            location_error: None,
            official_launcher_unverified: false,
            game_software_rendering: false,
            game_language: None,
            environment_ready: false,
            downloads: DownloadsView::default(),
            game_folder: None,
            report: None,
            fex_setting: None,
            setup_record: SetupRecord::None,
            game_files_open: None,
            saves_put_aside: None,
            last_event_seq: 0,
        }
    }

    pub(crate) fn with_saves_put_aside(mut self, put_aside: Option<SavesPutAsideView>) -> Self {
        self.saves_put_aside = put_aside;
        self
    }

    pub(crate) fn saves_put_aside(&self) -> Option<&SavesPutAsideView> {
        self.saves_put_aside.as_ref()
    }

    /// The notice with `later` added to what it already says.
    pub(crate) fn saves_put_aside_with(&self, later: SavesPutAsideView) -> SavesPutAsideView {
        match self.saves_put_aside.clone() {
            Some(earlier) => earlier.and(later),
            None => later,
        }
    }

    pub(crate) fn with_game_files_open(mut self, open: Option<FolderOpenView>) -> Self {
        self.game_files_open = open;
        self
    }

    pub(crate) fn game_files_open(&self) -> Option<FolderOpenView> {
        self.game_files_open
    }

    pub(crate) fn with_setup_record(mut self, record: SetupRecord) -> Self {
        self.setup_record = record;
        self
    }

    pub(crate) fn setup_record(&self) -> SetupRecord {
        self.setup_record
    }

    pub(crate) fn with_fex_setting(mut self, fex_setting: Option<FexSettingView>) -> Self {
        self.fex_setting = fex_setting;
        self
    }

    pub(crate) fn fex_setting(&self) -> Option<&FexSettingView> {
        self.fex_setting.as_ref()
    }

    pub(crate) fn with_report(mut self, report: Option<ReportView>) -> Self {
        self.report = report;
        self
    }

    pub(crate) fn report(&self) -> Option<&ReportView> {
        self.report.as_ref()
    }

    pub(crate) fn with_downloads(mut self, downloads: DownloadsView) -> Self {
        self.downloads = downloads;
        self
    }

    pub(crate) fn downloads(&self) -> DownloadsView {
        self.downloads
    }

    pub(crate) fn with_game_folder(mut self, game_folder: Option<GameFolderView>) -> Self {
        self.game_folder = game_folder;
        self
    }

    pub(crate) fn game_folder(&self) -> Option<&GameFolderView> {
        self.game_folder.as_ref()
    }

    /// A game folder used in place can't be reached: nothing runs until it
    /// is back.
    pub(crate) fn game_folder_unavailable(&self) -> bool {
        self.game_folder
            .as_ref()
            .is_some_and(|folder| !folder.available)
    }

    /// The downloads serve the current installer choice: an existing
    /// installation needs no installers, anything else does.
    pub(crate) fn downloads_ready(&self) -> bool {
        self.downloads.complete
            && (self.downloads.installers
                || self
                    .installer_media
                    .existing()
                    .is_some_and(|existing| existing.problem.is_none()))
    }

    pub(crate) fn with_host(mut self, host: HostView) -> Self {
        self.host = host;
        self
    }

    pub(crate) fn with_authorization(mut self, authorization: AuthorizationView) -> Self {
        self.authorization = authorization;
        self
    }

    pub(crate) fn with_installer_media(mut self, installer_media: InstallerMediaView) -> Self {
        self.installer_media = installer_media;
        self
    }

    pub(crate) fn with_installer_media_rejection(mut self, code: Option<&'static str>) -> Self {
        self.installer_media_rejection = code;
        self
    }

    pub(crate) fn installer_media_rejection(&self) -> Option<&'static str> {
        self.installer_media_rejection
    }

    pub(crate) fn with_installation(mut self, installation: InstallationStatus) -> Self {
        self.installation = installation;
        self
    }

    pub(crate) fn with_update(mut self, update: UpdateStatus) -> Self {
        self.update = update;
        self
    }

    pub(crate) fn with_operation(mut self, operation: Option<OperationView>) -> Self {
        self.operation = operation;
        self
    }

    pub(crate) fn with_game(mut self, game: GameView) -> Self {
        self.game = game;
        self
    }

    pub(crate) fn with_game_notice(mut self, notice: Option<GameNoticeView>) -> Self {
        self.game_notice = notice;
        self
    }

    pub(crate) fn with_journal(mut self, present: bool) -> Self {
        self.unresolved_journal = present;
        self
    }

    pub(crate) fn with_launch_preflight(mut self, passed: bool) -> Self {
        self.launch_preflight_passed = passed;
        self
    }

    pub(crate) fn with_evidence(mut self, evidence: Vec<EvidenceView>) -> Self {
        self.evidence = evidence;
        self
    }

    pub(crate) fn with_system(mut self, system: SystemContractView) -> Self {
        self.system = system;
        self
    }

    pub(crate) fn with_settings(mut self, settings: LaunchSettingsView) -> Self {
        self.settings = settings;
        self
    }

    pub(crate) fn with_download_sources(mut self, sources: Vec<DownloadSourceView>) -> Self {
        self.download_sources = sources;
        self
    }

    pub(crate) fn download_sources(&self) -> &[DownloadSourceView] {
        &self.download_sources
    }

    pub(crate) fn with_active_job(mut self, job: Option<ActiveJobView>) -> Self {
        self.active_job = job;
        self
    }

    pub(crate) fn active_job(&self) -> Option<ActiveJobView> {
        self.active_job
    }

    pub(crate) fn with_editing(mut self, step: Option<SetupStep>) -> Self {
        self.editing = step;
        self
    }

    pub(crate) fn editing(&self) -> Option<SetupStep> {
        self.editing
    }

    pub(crate) fn with_stop_result(mut self, result: Option<StopResultView>) -> Self {
        self.stop_result = result;
        self
    }

    pub(crate) fn stop_result(&self) -> Option<StopResultView> {
        self.stop_result
    }

    pub(crate) fn with_location(mut self, location: Option<LocationView>) -> Self {
        self.location = location;
        self
    }

    pub(crate) fn location(&self) -> Option<&LocationView> {
        self.location.as_ref()
    }

    pub(crate) fn with_location_error(mut self, code: Option<&'static str>) -> Self {
        self.location_error = code;
        self
    }

    pub(crate) fn location_error(&self) -> Option<&'static str> {
        self.location_error
    }

    pub(crate) fn with_official_launcher_unverified(mut self, unverified: bool) -> Self {
        self.official_launcher_unverified = unverified;
        self
    }

    pub(crate) fn official_launcher_unverified(&self) -> bool {
        self.official_launcher_unverified
    }

    pub(crate) fn with_game_software_rendering(mut self, software: bool) -> Self {
        self.game_software_rendering = software;
        self
    }

    pub(crate) fn game_software_rendering(&self) -> bool {
        self.game_software_rendering
    }

    pub(crate) fn with_game_language(
        mut self,
        language: Option<crate::game_language::GameLanguage>,
    ) -> Self {
        self.game_language = language;
        self
    }

    pub(crate) fn game_language(&self) -> Option<crate::game_language::GameLanguage> {
        self.game_language
    }

    /// The installed copy is not in English, which PD2 needs.
    pub(crate) fn game_not_english(&self) -> Option<crate::game_language::GameLanguage> {
        self.game_language.filter(|language| !language.is_english())
    }

    pub(crate) fn with_environment_ready(mut self, ready: bool) -> Self {
        self.environment_ready = ready;
        self
    }

    pub(crate) fn environment_ready(&self) -> bool {
        self.environment_ready
    }

    /// The resolved location is known to be unavailable.
    pub(crate) fn location_unavailable(&self) -> bool {
        self.location
            .as_ref()
            .is_some_and(|location| !location.available)
    }

    /// Any download source is custom (unverified), from the settings or
    /// the environment.
    pub(crate) fn custom_sources_active(&self) -> bool {
        self.download_sources
            .iter()
            .any(|source| source.badge != SourceBadgeView::BuiltInVerified)
    }

    /// Hosts of the custom D2/LoD installer sources that "Download" would
    /// use; empty when both installers come from Blizzard's built-in source.
    pub(crate) fn custom_installer_hosts(&self) -> Vec<String> {
        let mut hosts = Vec::new();
        for source in &self.download_sources {
            if matches!(
                source.artifact,
                DownloadArtifact::D2Installer | DownloadArtifact::LodInstaller
            ) && source.badge != SourceBadgeView::BuiltInVerified
            {
                let host = crate::sources::redacted_host(&source.effective_url);
                if !hosts.contains(&host) {
                    hosts.push(host);
                }
            }
        }
        hosts
    }

    pub(crate) fn with_sequence(mut self, sequence: u64) -> Self {
        self.last_event_seq = sequence;
        self
    }

    pub(crate) fn host(&self) -> HostView {
        self.host
    }

    pub(crate) fn authorization(&self) -> AuthorizationView {
        self.authorization
    }

    pub(crate) fn installer_media(&self) -> &InstallerMediaView {
        &self.installer_media
    }

    pub(crate) fn installation(&self) -> InstallationStatus {
        self.installation
    }

    pub(crate) fn update(&self) -> UpdateStatus {
        self.update
    }

    pub(crate) fn game(&self) -> GameView {
        self.game
    }

    pub(crate) fn game_notice(&self) -> Option<GameNoticeView> {
        self.game_notice
    }

    pub(crate) fn unresolved_journal(&self) -> bool {
        self.unresolved_journal
    }

    pub(crate) fn launch_preflight_passed(&self) -> bool {
        self.launch_preflight_passed
    }

    pub(crate) fn settings(&self) -> LaunchSettingsView {
        self.settings.clone()
    }

    #[cfg(test)]
    pub(crate) fn last_event_sequence(&self) -> u64 {
        self.last_event_seq
    }

    pub(crate) fn system(&self) -> &SystemContractView {
        &self.system
    }

    #[cfg(test)]
    pub(crate) fn evidence(&self) -> &[EvidenceView] {
        &self.evidence
    }

    pub(crate) fn operation(&self) -> Option<&OperationView> {
        self.operation.as_ref()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ActionId {
    None,
    AuthorizeAndInstall,
    ContinueInstallation,
    Play,
    Update,
    SafeStop,
    Repairing,
    OpenDiagnostics,
    ExportDiagnostics,
    OpenOfficialLauncher,
    /// Asks for confirmation; only the confirmation emits `Intent::StartOver`.
    StartOver,
    RetryLocation,
    /// Check this computer again (missing prerequisites, or a launcher
    /// service that could not start).
    CheckAgain,
    /// The guided first run's next step. Disabled, its label says why.
    GuidedContinue,
    /// The guided first run's previous step.
    GuidedBack,
    /// Step 3: download and verify everything.
    Download,
    /// Step 4 for an existing installation (copy or in place).
    ImportExisting,
    /// Step 5.
    InstallProjectDiablo,
    /// Back from Settings, Diagnostics or About to the Overview.
    ReturnToOverview,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ActionView {
    pub(crate) id: ActionId,
    pub(crate) label: &'static str,
    pub(crate) enabled: bool,
}

impl ActionView {
    pub(crate) fn enabled(id: ActionId, label: &'static str) -> Self {
        Self {
            id,
            label,
            enabled: true,
        }
    }

    fn disabled(id: ActionId, label: &'static str) -> Self {
        Self {
            id,
            label,
            enabled: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FocusTarget {
    Diagnostics,
    Settings,
    BaseGameKey,
    ExpansionKey,
    Consent,
    Primary,
    Update,
    SafeStop,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum JourneyState {
    Complete,
    Active,
    Pending,
    Blocked,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct JourneyItem {
    pub(crate) label: &'static str,
    pub(crate) detail: &'static str,
    pub(crate) state: JourneyState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DeckMode {
    Setup,
    Ready,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DeckPresentation {
    pub(crate) mode: DeckMode,
    pub(crate) title: &'static str,
    pub(crate) description: &'static str,
    pub(crate) status: &'static str,
    pub(crate) primary: ActionView,
    pub(crate) secondary: Vec<ActionView>,
    pub(crate) advisory: Option<String>,
    pub(crate) game_notice: Option<GameNoticeView>,
    pub(crate) focus_order: Vec<FocusTarget>,
    pub(crate) journey: Vec<JourneyItem>,
}

impl DeckPresentation {
    pub(crate) fn project(snapshot: &UiSnapshot, authorization_complete: bool) -> Self {
        // An installed game, by the setup record, waits on Play while the
        // first check runs.
        let mode = if (snapshot.host == HostView::Supported
            && snapshot.installation == InstallationStatus::Ready)
            || opening_step(snapshot) == Some(SetupStep::Play)
        {
            DeckMode::Ready
        } else {
            DeckMode::Setup
        };
        let mut focus_order = vec![FocusTarget::Diagnostics, FocusTarget::Settings];
        let mut secondary = Vec::new();
        let mutation_active = snapshot
            .operation
            .as_ref()
            .is_some_and(OperationView::owns_mutation);
        let official_launcher_open = snapshot.operation.as_ref().is_some_and(|operation| {
            operation.owns_mutation() && operation.phase == OperationPhase::OfficialLauncher
        });
        let can_play = snapshot.host == HostView::Supported
            && snapshot.installation == InstallationStatus::Ready
            && snapshot.game == GameView::Idle
            && !mutation_active
            && !snapshot.unresolved_journal
            && snapshot.launch_preflight_passed;

        let (title, description, status, primary) = match snapshot.host {
            _ if snapshot.game_folder_unavailable() => (
                "Diablo II folder unavailable",
                "The Diablo II folder the launcher uses where it is can't be reached, or isn't the same folder anymore. Nothing runs until it is back: reconnect or put it back and check again, or start over.",
                "Folder unavailable",
                ActionView::enabled(ActionId::CheckAgain, "Check again"),
            ),
            _ if snapshot.location_unavailable() => (
                "Install location unavailable",
                "The folder that holds this installation can't be reached or isn't the Launcher's. Nothing is installed there until it is back: reconnect the drive and check again, or choose a new location for a fresh installation.",
                "Location unavailable",
                ActionView::enabled(ActionId::RetryLocation, "Check again"),
            ),
            _ if let Some((headline, detail)) =
                service_failure(snapshot).and_then(installation_busy_message) =>
            {
                (
                    headline,
                    detail,
                    "Installation in use",
                    ActionView::enabled(ActionId::CheckAgain, "Check again"),
                )
            }
            _ if service_failure(snapshot).is_some() => (
                "The launcher couldn't start",
                "The launcher couldn't start its background service. Open Diagnostics and choose Save report to ask for help.",
                "Launcher service unavailable",
                if service_failure(snapshot).is_some_and(|code| !service_failure_retryable(code)) {
                    ActionView::enabled(ActionId::OpenDiagnostics, "Open Diagnostics")
                } else {
                    ActionView::enabled(ActionId::CheckAgain, "Try again")
                },
            ),
            HostView::Checking => match opening_step(snapshot) {
                Some(SetupStep::Play) => (
                    "Getting ready…",
                    "The launcher checks the game before it starts. This takes a few seconds.",
                    "Getting ready",
                    ActionView::disabled(ActionId::Play, "Getting ready"),
                ),
                Some(_) => (
                    "Getting ready…",
                    "The launcher checks where the installation stands before it goes on. This takes a few seconds.",
                    "Getting ready",
                    ActionView::disabled(ActionId::None, "Checking this device"),
                ),
                None => (
                    "Checking this device",
                    "The launcher is checking this computer before it downloads or changes anything.",
                    "Checking",
                    ActionView::disabled(ActionId::None, "Checking this device"),
                ),
            },
            HostView::Unsupported => (
                "Your computer needs a few things first",
                "Your computer is missing a few things the game needs:",
                "Missing requirements",
                ActionView::enabled(ActionId::CheckAgain, "Check again"),
            ),
            HostView::Supported => match snapshot.installation {
                InstallationStatus::RepairRequired => (
                    "Repairing the last update",
                    "The launcher is finishing or undoing unfinished work. You can play once it's done.",
                    "Repairing",
                    ActionView::disabled(ActionId::Repairing, "Repair in progress"),
                ),
                InstallationStatus::StageInterrupted(_) => (
                    "The installation was interrupted",
                    "A step stopped part way. Start over installs Diablo II again; your downloads are kept.",
                    "Start over needed",
                    if mutation_active {
                        ActionView::disabled(ActionId::StartOver, "Starting over")
                    } else {
                        ActionView::enabled(ActionId::StartOver, "Start over")
                    },
                ),
                InstallationStatus::Indeterminate => (
                    "The launcher can't tell what's installed",
                    "Nothing will be guessed or deleted. Open Diagnostics to see what was found.",
                    "Needs a look",
                    ActionView::enabled(ActionId::OpenDiagnostics, "Open Diagnostics"),
                ),
                InstallationStatus::Ready => {
                    let action = if can_play {
                        ActionView::enabled(ActionId::Play, "Play")
                    } else if snapshot.game == GameView::Running {
                        ActionView::disabled(ActionId::Play, "Game is running")
                    } else if official_launcher_open {
                        ActionView::disabled(ActionId::Play, "Official launcher is open")
                    } else if mutation_active {
                        ActionView::disabled(ActionId::Play, "Play unavailable during update")
                    } else {
                        ActionView::disabled(ActionId::Play, "Play preflight required")
                    };
                    (
                        "Ready to play",
                        "Project Diablo 2 is installed and checked.",
                        "Ready",
                        action,
                    )
                }
                InstallationStatus::NeedsBaseGame
                | InstallationStatus::NeedsExpansion
                | InstallationStatus::NeedsClient
                | InstallationStatus::NeedsRuntime => {
                    if mutation_active {
                        (
                            "Installing",
                            "The launcher is working. Keep this window open.",
                            "Working",
                            ActionView::disabled(
                                ActionId::ContinueInstallation,
                                "Installation in progress",
                            ),
                        )
                    } else {
                        match standing_step(snapshot) {
                            SetupStep::Download => (
                                "Download everything first",
                                if snapshot.installer_media.existing().is_some() {
                                    "The launcher downloads and checks everything it needs before it installs anything. Your own Diablo II needs no download."
                                } else if snapshot.custom_installer_hosts().is_empty() {
                                    "The launcher downloads and checks everything before it installs anything, with Diablo II from Blizzard. No CD keys are needed yet."
                                } else {
                                    "The launcher downloads and checks everything before it installs anything, with Diablo II from your custom sources (unverified). No CD keys are needed yet."
                                },
                                "Download needed",
                                if snapshot.installer_media.is_ready() {
                                    ActionView::enabled(ActionId::Download, "Download")
                                } else {
                                    ActionView::disabled(ActionId::Download, "Download")
                                },
                            ),
                            SetupStep::InstallPd2
                                if snapshot.installation == InstallationStatus::NeedsRuntime =>
                            {
                                (
                                    "Finish setting up",
                                    "The game is installed. The launcher still needs to add a few Windows components; no CD keys are needed and the game is kept.",
                                    "Ready to continue",
                                    ActionView::enabled(
                                        ActionId::InstallProjectDiablo,
                                        "Continue installation",
                                    ),
                                )
                            }
                            SetupStep::InstallPd2 => (
                                "Install Project Diablo 2",
                                "Diablo II and Lord of Destruction are installed. The remaining steps need no CD keys.",
                                "Ready to continue",
                                ActionView::enabled(
                                    ActionId::InstallProjectDiablo,
                                    "Install Project Diablo 2",
                                ),
                            ),
                            _ if snapshot.installer_media.existing().is_some() => (
                                "Use your Diablo II",
                                "The launcher copies your Diablo II, or uses it where it is. No CD keys are needed.",
                                "Ready to install",
                                if snapshot.installer_media.is_ready() {
                                    ActionView::enabled(
                                        ActionId::ImportExisting,
                                        "Install Diablo II",
                                    )
                                } else {
                                    ActionView::disabled(
                                        ActionId::ImportExisting,
                                        "Install Diablo II",
                                    )
                                },
                            ),
                            _ => {
                                focus_order.extend([
                                    FocusTarget::BaseGameKey,
                                    FocusTarget::ExpansionKey,
                                    FocusTarget::Consent,
                                ]);
                                (
                                    "Install Diablo II",
                                    "Enter the CD keys for Diablo II and Lord of Destruction. Everything was downloaded and checked already.",
                                    "CD keys needed",
                                    if authorization_complete && snapshot.installer_media.is_ready()
                                    {
                                        ActionView::enabled(
                                            ActionId::AuthorizeAndInstall,
                                            "Install Diablo II",
                                        )
                                    } else {
                                        ActionView::disabled(
                                            ActionId::AuthorizeAndInstall,
                                            "Install Diablo II",
                                        )
                                    },
                                )
                            }
                        }
                    }
                }
            },
        };

        if snapshot.update == UpdateStatus::Available
            && snapshot.installation == InstallationStatus::Ready
        {
            secondary.push(if snapshot.game == GameView::Idle && !mutation_active {
                ActionView::enabled(ActionId::Update, "Update")
            } else {
                ActionView::disabled(ActionId::Update, "Update unavailable while busy")
            });
        }
        if official_launcher_open {
            secondary.push(ActionView::enabled(
                ActionId::SafeStop,
                "Close official launcher",
            ));
        } else if mutation_active {
            secondary.push(ActionView::enabled(ActionId::SafeStop, "Request safe stop"));
        }

        focus_order.push(FocusTarget::Primary);
        for action in &secondary {
            focus_order.push(match action.id {
                ActionId::Update => FocusTarget::Update,
                ActionId::SafeStop => FocusTarget::SafeStop,
                _ => continue,
            });
        }

        let advisory = match snapshot.update {
            UpdateStatus::Available if snapshot.installation == InstallationStatus::Ready => Some(
                "An update is available. Online play may require it; single-player remains available."
                    .to_owned(),
            ),
            UpdateStatus::CheckFailed if snapshot.installation == InstallationStatus::Ready => {
                Some(
                    "Update status is unavailable. Your verified local installation can still be played."
                        .to_owned(),
                )
            }
            _ => None,
        };
        // D2GL's first-launch tip: before that launch, while it runs, and
        // after it exits, as long as the game uses D2GL (Glide).
        let game_notice = snapshot.game_notice.filter(|_| {
            snapshot.installation == InstallationStatus::Ready
                && snapshot.settings.renderer == RendererPreferenceView::Glide
        });

        Self {
            mode,
            title,
            description,
            status,
            primary,
            secondary,
            advisory,
            game_notice,
            focus_order,
            journey: project_journey(snapshot),
        }
    }

    #[cfg(test)]
    pub(crate) fn available_actions(&self) -> Vec<ActionId> {
        std::iter::once(&self.primary)
            .chain(&self.secondary)
            .filter(|action| action.enabled)
            .map(|action| action.id)
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn visible_text(&self) -> Vec<&str> {
        let mut text = vec![
            "PD2 Launcher",
            "Operations Deck",
            "Diagnostics",
            "Settings",
            self.status,
            self.title,
            self.description,
            self.primary.label,
        ];
        text.extend(self.secondary.iter().map(|action| action.label));
        text.extend(self.advisory.as_deref());
        text.extend(
            self.game_notice
                .into_iter()
                .flat_map(|notice| [notice.title(), notice.message()]),
        );
        if self.mode == DeckMode::Setup {
            text.extend(
                self.journey
                    .iter()
                    .flat_map(|item| [item.label, item.detail]),
            );
        }
        text
    }
}

/// Where Project Diablo 2's update stands on Play: offered (Update takes the
/// main button, with Play without updating), running or stopping (its figure
/// where "Ready to play" goes, Stop), stopped (Resume), failed (Try again), or
/// the check after it running.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PlayUpdate {
    None,
    Offered,
    Running,
    Stopping,
    Stopped,
    Failed,
    /// The update finished and the check after it has not said yet.
    Rechecking,
}

/// Where the check after a finished update stands.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Recheck {
    /// The update finished; the check after it has not shown yet.
    Waiting,
    Checking,
}

fn update_operation(snapshot: &UiSnapshot) -> Option<&OperationView> {
    snapshot
        .operation
        .as_ref()
        .filter(|operation| operation.job == Some(ActiveJobView::Update))
}

/// The check after a finished update, from `current` (where it stood after
/// the operation `previous`) and the next snapshot: it begins when an
/// update's success lands, runs while the check says Checking, and ends
/// once the check says anything else (Up to date, or Available again after
/// it ran: a newer update), or another operation replaces the update's.
/// Until then the snapshot's Available is the one from before the update,
/// which Play must not offer again.
pub(crate) fn recheck(
    current: Option<Recheck>,
    previous: Option<&OperationView>,
    next: &UiSnapshot,
) -> Option<Recheck> {
    let succeeded = update_operation(next)
        .is_some_and(|operation| operation.status == OperationStatusView::Succeeded);
    if !succeeded {
        return None;
    }
    let stands = if previous != next.operation.as_ref() {
        Some(Recheck::Waiting)
    } else {
        current
    };
    match (stands, next.update) {
        (None, _) => None,
        (Some(_), UpdateStatus::Checking) => Some(Recheck::Checking),
        (Some(Recheck::Waiting), UpdateStatus::Available) => Some(Recheck::Waiting),
        _ => None,
    }
}

/// The update on Play, from the snapshot and the check after a finished one.
/// Offered only for an installed game while neither the game nor the
/// official launcher runs, and only once the check said Available: a failed
/// check never stands in Play's way.
pub(crate) fn play_update(snapshot: &UiSnapshot, recheck: Option<Recheck>) -> PlayUpdate {
    if snapshot.host != HostView::Supported
        || snapshot.installation != InstallationStatus::Ready
        || snapshot.game == GameView::Running
        || snapshot.active_job == Some(ActiveJobView::OfficialLauncher)
    {
        return PlayUpdate::None;
    }
    let operation = update_operation(snapshot);
    if snapshot.active_job == Some(ActiveJobView::Update)
        || operation.is_some_and(|operation| !operation.status.is_terminal())
    {
        return if operation
            .is_some_and(|operation| operation.status == OperationStatusView::SafeStopping)
        {
            PlayUpdate::Stopping
        } else {
            PlayUpdate::Running
        };
    }
    if recheck.is_some() {
        return PlayUpdate::Rechecking;
    }
    if snapshot.update != UpdateStatus::Available {
        return PlayUpdate::None;
    }
    match operation.map(|operation| operation.status) {
        Some(OperationStatusView::Failed | OperationStatusView::Indeterminate) => {
            PlayUpdate::Failed
        }
        Some(OperationStatusView::Cancelled) => PlayUpdate::Stopped,
        _ => PlayUpdate::Offered,
    }
}

/// Play's main button: Play, or the update's next step (Update, Stop,
/// Resume, Try again). The update starts only while nothing else runs.
pub(crate) fn play_main(
    snapshot: &UiSnapshot,
    deck: &DeckPresentation,
    update: PlayUpdate,
) -> ActionView {
    let can_start = snapshot.active_job.is_none()
        && snapshot.game == GameView::Idle
        && !snapshot.unresolved_journal
        && !snapshot
            .operation
            .as_ref()
            .is_some_and(OperationView::owns_mutation);
    let start = |label| {
        if can_start {
            ActionView::enabled(ActionId::Update, label)
        } else {
            ActionView::disabled(ActionId::Update, label)
        }
    };
    match update {
        PlayUpdate::None | PlayUpdate::Rechecking => deck.primary.clone(),
        PlayUpdate::Offered => start("Update"),
        PlayUpdate::Stopped => start("Resume"),
        PlayUpdate::Failed => start("Try again"),
        PlayUpdate::Running => ActionView::enabled(ActionId::SafeStop, "Stop"),
        PlayUpdate::Stopping => ActionView::disabled(ActionId::SafeStop, "Stopping…"),
    }
}

/// The one line of facts under Play: the renderer the next launch uses, the
/// graphics card the launcher's window runs on, and Project Diablo 2's
/// update state once checked. Only what is known.
pub(crate) fn play_facts(snapshot: &UiSnapshot, gpu: Option<&str>) -> Vec<String> {
    let mut facts = vec![
        match snapshot.settings.renderer() {
            RendererPreferenceView::Glide => "Glide",
            RendererPreferenceView::DirectDraw => "DirectDraw",
        }
        .to_owned(),
    ];
    if let Some(gpu) = gpu.map(str::trim).filter(|gpu| !gpu.is_empty()) {
        facts.push(gpu.to_owned());
    }
    match snapshot.update {
        UpdateStatus::UpToDate => facts.push("Up to date".to_owned()),
        UpdateStatus::Available => facts.push("Update available".to_owned()),
        UpdateStatus::Checking => facts.push("Checking for updates".to_owned()),
        UpdateStatus::CheckFailed => facts.push("Update check failed".to_owned()),
        UpdateStatus::Unknown => {}
    }
    facts
}

/// Play stays possible beside an update that does not run.
pub(crate) const fn plays_without_updating(update: PlayUpdate) -> bool {
    matches!(
        update,
        PlayUpdate::Offered | PlayUpdate::Stopped | PlayUpdate::Failed
    )
}

/// Whether the official PD2 launcher fallback can open now, mirroring the
/// coordinator's guard: an installed game (Ready, or only the client out of
/// date) with nothing else running.
pub(crate) fn official_launcher_available(snapshot: &UiSnapshot) -> bool {
    snapshot.host == HostView::Supported
        && matches!(
            snapshot.installation,
            InstallationStatus::Ready | InstallationStatus::NeedsClient
        )
        && snapshot.game == GameView::Idle
        && !snapshot
            .operation
            .as_ref()
            .is_some_and(OperationView::owns_mutation)
        && !snapshot.unresolved_journal
}

/// What unattended mode does now about its after-Ready request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AfterReadyStep {
    Wait,
    LaunchGame,
    /// The same intent the Open official PD2 launcher button emits.
    OpenOfficialLauncher,
    /// An unverified official launcher needs the user's confirmation,
    /// which has no unattended form: the run ends without opening it.
    RefuseUnverifiedOfficialLauncher,
}

fn after_ready_step(after_ready: AfterReady, snapshot: &UiSnapshot) -> AfterReadyStep {
    let ready = snapshot.installation() == InstallationStatus::Ready
        && snapshot.launch_preflight_passed()
        && snapshot.game() == GameView::Idle
        && !snapshot.unresolved_journal()
        && !snapshot
            .operation
            .as_ref()
            .is_some_and(OperationView::owns_mutation);
    match after_ready {
        AfterReady::LaunchGame if ready => AfterReadyStep::LaunchGame,
        AfterReady::OpenOfficialLauncher if ready && official_launcher_available(snapshot) => {
            if snapshot.official_launcher_unverified() {
                AfterReadyStep::RefuseUnverifiedOfficialLauncher
            } else {
                AfterReadyStep::OpenOfficialLauncher
            }
        }
        _ => AfterReadyStep::Wait,
    }
}

/// Whether a confirmed Start over may run now, mirroring the coordinator's
/// guard.
pub(crate) fn start_over_available(snapshot: &UiSnapshot) -> bool {
    snapshot.host == HostView::Supported
        && snapshot.game == GameView::Idle
        && !snapshot
            .operation
            .as_ref()
            .is_some_and(OperationView::owns_mutation)
        && !snapshot.unresolved_journal
        && snapshot.installation != InstallationStatus::RepairRequired
}

/// The code of the service failure a Launcher-internal problem left: the
/// service could not start, or could not establish evidence (fail-closed).
/// It is never a missing system package.
pub(crate) fn service_failure(snapshot: &UiSnapshot) -> Option<&'static str> {
    if snapshot.location_unavailable() || snapshot.game_folder_unavailable() {
        // An unreachable install location or game folder has its own view
        // and Check again.
        return None;
    }
    snapshot
        .operation
        .as_ref()
        .filter(|operation| {
            operation.phase == OperationPhase::HostAudit
                && operation.status == OperationStatusView::Failed
        })
        .and_then(OperationView::failure)
}

/// The installation lease is held by something still running, in words the
/// player can act on (headline, detail). `None` for any other failure.
pub(crate) fn installation_busy_message(code: &str) -> Option<(&'static str, &'static str)> {
    match code {
        "game-still-running" => Some((
            "Project Diablo 2 is still running",
            "The game or the official PD2 launcher from an earlier session is still open. Close it, then choose Check again.",
        )),
        "installation-in-use" => Some((
            "The installation is in use",
            "Another PD2 Launcher, or a program the game opened, is using the installation. Close it, then choose Check again.",
        )),
        _ => None,
    }
}

/// The service thread itself could not start: nothing can retry it.
pub(crate) const SERVICE_RUNTIME_UNAVAILABLE: &str = "service-runtime-unavailable";

/// Whether checking again can help: not when the service or its production
/// services never started in this process.
fn service_failure_retryable(code: &str) -> bool {
    !matches!(
        code,
        SERVICE_RUNTIME_UNAVAILABLE | "production-services-unavailable"
    )
}

/// What a result card offers besides dismissing it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResultNextStep {
    /// Run the same work again (the action bar's own button).
    TryAgain,
    OpenDiagnostics,
    StartOver,
}

/// The last operation that did not succeed, in plain words, until it is
/// dismissed or new work starts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ResultCardView {
    pub(crate) title: &'static str,
    pub(crate) message: &'static str,
    pub(crate) next: Option<ResultNextStep>,
}

/// The card for a CD key that failed the installers' own check before any
/// installer ran.
fn key_card(code: &str) -> Option<ResultCardView> {
    let (title, message) = match code {
        "base-game-key-wrong-length" => (
            "The Diablo II CD key is not right",
            "Diablo II CD keys have 26 letters and numbers. Check the Diablo II CD key and try again.",
        ),
        "expansion-key-wrong-length" => (
            "The Lord of Destruction CD key is not right",
            "Lord of Destruction CD keys have 26 letters and numbers. Check the Lord of Destruction CD key and try again.",
        ),
        "base-game-key-16-characters" => (
            "The Diablo II CD key is not right",
            "This is a 16-character Diablo II CD key. Blizzard's installers take only 26-character keys: your Blizzard account shows the 26-character key for it.",
        ),
        "expansion-key-16-characters" => (
            "The Lord of Destruction CD key is not right",
            "This is a 16-character Lord of Destruction CD key. Blizzard's installers take only 26-character keys: your Blizzard account shows the 26-character key for it.",
        ),
        "base-game-key-invalid-character" => (
            "The Diablo II CD key is not right",
            "The Diablo II CD key has a letter or number that CD keys do not use. Keys use 2 4 6 7 8 9 and B C D E F G H J K M N P R T V W X Y Z. Check it and try again.",
        ),
        "expansion-key-invalid-character" => (
            "The Lord of Destruction CD key is not right",
            "The Lord of Destruction CD key has a letter or number that CD keys do not use. Keys use 2 4 6 7 8 9 and B C D E F G H J K M N P R T V W X Y Z. Check it and try again.",
        ),
        "base-game-key-is-expansion-key" => (
            "The Diablo II CD key is not right",
            "This is the Lord of Destruction CD key. Type the Diablo II CD key in the Diablo II field.",
        ),
        "expansion-key-is-base-game-key" => (
            "The Lord of Destruction CD key is not right",
            "This is the Diablo II CD key. Type the Lord of Destruction CD key in the Lord of Destruction field.",
        ),
        "base-game-key-not-valid" => (
            "The Diablo II CD key is not right",
            "The Diablo II CD key is not valid: Blizzard's installer would refuse it. Check the Diablo II CD key and try again.",
        ),
        "expansion-key-not-valid" => (
            "The Lord of Destruction CD key is not right",
            "The Lord of Destruction CD key is not valid: Blizzard's installer would refuse it. Check the Lord of Destruction CD key and try again.",
        ),
        _ => return None,
    };
    Some(ResultCardView {
        title,
        message,
        next: Some(ResultNextStep::TryAgain),
    })
}

pub(crate) fn result_card(snapshot: &UiSnapshot) -> Option<ResultCardView> {
    let operation = snapshot.operation.as_ref()?;
    if !matches!(
        operation.status,
        OperationStatusView::Failed
            | OperationStatusView::Cancelled
            | OperationStatusView::Indeterminate
    ) || service_failure(snapshot).is_some()
        || operation.phase == OperationPhase::HostAudit
    {
        return None;
    }
    // A user-requested install stop has its own card of what was kept.
    if operation.status == OperationStatusView::Cancelled && snapshot.stop_result.is_some() {
        return None;
    }
    let card = |title, message, next| {
        Some(ResultCardView {
            title,
            message,
            next,
        })
    };
    use ResultNextStep::{OpenDiagnostics, StartOver, TryAgain};
    if operation.status == OperationStatusView::Indeterminate {
        return card(
            "The launcher couldn't confirm a safe stop",
            "Nothing will be guessed or deleted. Open Diagnostics and choose Save report to ask for help.",
            Some(OpenDiagnostics),
        );
    }
    if operation.phase == OperationPhase::GameExecution {
        if operation.failure == Some(crate::save_keeper::SAVES_UNAVAILABLE) {
            return card("The game didn't start", SAVES_UNAVAILABLE, None);
        }
        return card("The game didn't start", PLAY_FAILED, None);
    }
    if operation.status == OperationStatusView::Cancelled {
        return match operation.job {
            Some(ActiveJobView::Download) => card(
                "Download stopped",
                "Everything already downloaded is kept. Download again to continue where it stopped.",
                None,
            ),
            Some(ActiveJobView::InstallBaseGame | ActiveJobView::InstallProjectDiablo) => card(
                "Installation stopped",
                "Your downloads are kept. Continue whenever you're ready.",
                None,
            ),
            Some(ActiveJobView::Update) => card(
                "Update stopped",
                "The update stopped at a safe point. You can update again later.",
                None,
            ),
            _ => card("Stopped", "The task stopped at a safe point.", None),
        };
    }
    let stopped = |message| card("Installation stopped", message, Some(TryAgain));
    if let Some(key_card) = key_card(operation.failure.unwrap_or_default()) {
        return Some(key_card);
    }
    match operation.failure.unwrap_or_default() {
        "base-game-key-rejected" => card(
            "Diablo II's installer rejected the CD key",
            "Diablo II's installer rejected the CD key. Check the Diablo II CD key and try again. Your downloads are kept.",
            Some(TryAgain),
        ),
        "expansion-key-rejected" => card(
            "Lord of Destruction's installer rejected the CD key",
            "Lord of Destruction's installer rejected the CD key. Check the Lord of Destruction CD key and try again. Diablo II and your downloads are kept.",
            Some(TryAgain),
        ),
        "base-game-installer-error" => card(
            "Diablo II's installer stopped with an error",
            "Diablo II's installer showed an error and stopped. Check the Diablo II CD key and try again. Your downloads are kept. If it happens again, open Diagnostics and choose Save report to ask for help.",
            Some(TryAgain),
        ),
        "expansion-installer-error" => card(
            "Lord of Destruction's installer stopped with an error",
            "Lord of Destruction's installer showed an error and stopped. Check the Lord of Destruction CD key and try again. Diablo II and your downloads are kept. If it happens again, open Diagnostics and choose Save report to ask for help.",
            Some(TryAgain),
        ),
        "base-game-installer-menu" | "expansion-installer-menu" => card(
            "Blizzard's installer opened its menu",
            "Blizzard's installer opened its menu instead of installing. Try again, or choose Download from Blizzard.",
            Some(TryAgain),
        ),
        "base-game-install-failed" => stopped(
            "Installation stopped while installing Diablo II. Your downloads are kept. Try again, or open Diagnostics and choose Save report to ask for help.",
        ),
        "expansion-install-failed" => stopped(
            "Installation stopped while installing Lord of Destruction. Diablo II and your downloads are kept. Try again, or open Diagnostics and choose Save report to ask for help.",
        ),
        "runtime-setup-failed" => stopped(
            "Installation stopped while setting up the game environment. Your downloads are kept. Try again, or open Diagnostics and choose Save report to ask for help.",
        ),
        "builtin-fex-setup-failed" => stopped(
            "The built-in FEX couldn't be set up. It needs about 2 GB of free space. Your downloads are kept. Try again, or choose Device FEX in Settings.",
        ),
        "client-install-failed" => stopped(
            "Installation stopped while installing Project Diablo 2. Your downloads are kept. Try again, or open Diagnostics and choose Save report to ask for help.",
        ),
        "installer-download-failed" => card(
            "Download didn't finish",
            "Blizzard's downloader couldn't be downloaded. Check your internet connection and try again. Anything already downloaded is kept.",
            Some(TryAgain),
        ),
        "installer-download-stalled" => card(
            "Download didn't finish",
            "Blizzard's download got no new data for a minute. Check your internet connection and try again. Anything already downloaded is kept.",
            Some(TryAgain),
        ),
        "installer-downloader-error" => card(
            "Blizzard's downloader showed an error",
            "Try again. If it happens again, open Diagnostics and choose Save report to ask for help; the report has the downloader's message.",
            Some(TryAgain),
        ),
        "installer-download-incomplete" => card(
            "Download didn't finish",
            "Blizzard's downloader stopped before the download was complete. Try again, or open Diagnostics and choose Save report to ask for help.",
            Some(TryAgain),
        ),
        "installer-download-rejected" => card(
            "The downloaded files didn't pass the check",
            "The files from Blizzard's downloader aren't the expected Diablo II files. Try again, or open Diagnostics and choose Save report to ask for help.",
            Some(TryAgain),
        ),
        "installer-downloader-failed" => card(
            "Download didn't finish",
            "Blizzard's downloader stopped unexpectedly. Try again, or open Diagnostics and choose Save report to ask for help.",
            Some(TryAgain),
        ),
        "installer-payload-store-failed" => card(
            "Download didn't finish",
            "Blizzard's download finished, but its files couldn't be stored. Check that the install location has free space and try again.",
            Some(TryAgain),
        ),
        "direct-download-refused" => card(
            "Download didn't finish",
            "Blizzard's server refused the download. Try again later. Anything already downloaded is kept.",
            Some(TryAgain),
        ),
        "direct-download-incomplete" => card(
            "Download didn't finish",
            "Some parts of the download didn't arrive. Check your internet connection and try again. Anything already downloaded is kept.",
            Some(TryAgain),
        ),
        "direct-download-mismatch" => card(
            "The downloaded files didn't pass the check",
            "Blizzard's download isn't the expected Diablo II files. Try again, or open Diagnostics and choose Save report to ask for help.",
            Some(TryAgain),
        ),
        "download-failed" => card(
            "Download didn't finish",
            "A download didn't finish. Check your internet connection and try again. Anything already downloaded is kept.",
            Some(TryAgain),
        ),
        "download-cache-unusable" => card(
            "The download cache can't be used",
            "Nothing was downloaded: the launcher's download cache has files or state it can't account for, for example a launcher folder copied from another computer while it was busy. Delete the cache folder inside the install location (by default ~/.pd2launcher/game/cache) and try again; everything is downloaded again.",
            Some(TryAgain),
        ),
        "client-download-failed" => card(
            "Download didn't finish",
            "Project Diablo 2's files couldn't all be downloaded. Check your internet connection and try again. Anything already downloaded is kept.",
            Some(TryAgain),
        ),
        "downloads-required" => card(
            "Something needs downloading first",
            "A file the install needs isn't in the verified downloads anymore. Go back to Download and download again; anything still there is kept.",
            None,
        ),
        "import-failed" => card(
            "Your Diablo II couldn't be set up",
            "Copying or linking your Diablo II stopped part way. Your own folder was not changed. Start over and try again, or open Diagnostics and choose Save report.",
            Some(StartOver),
        ),
        "existing-install-invalid" => card(
            "Your Diablo II can't be used anymore",
            "The folder changed since you chose it and is no longer a complete Diablo II and Lord of Destruction. Choose it again in Location & installers to see why.",
            None,
        ),
        "base-game-required" => card(
            "Diablo II isn't installed yet",
            "Install Diablo II first; Project Diablo 2 is installed on top of it.",
            None,
        ),
        "game-folder-unavailable" => card(
            "Your Diablo II folder can't be reached",
            "The folder the launcher uses where it is was moved, removed, or can't be read. Reconnect or put it back, then check again.",
            None,
        ),
        "saves-unavailable" => card("The official launcher didn't open", SAVES_UNAVAILABLE, None),
        "saves-not-kept" => card(
            "Start over didn't run",
            "Your characters couldn't be moved to their own folder first, so nothing was removed. Check that your home folder has free space, then try again.",
            None,
        ),
        "stage-interrupted" => card(
            "The installation was interrupted",
            "A step stopped part way, so it has to start over. Your downloads are kept.",
            Some(StartOver),
        ),
        "official-launcher-missing" => card(
            "The official launcher isn't in your game folder",
            "ProjectD2\\PD2Launcher.exe is missing. If you chose an existing Diablo II folder, check that it is complete, with the ProjectD2 folder, then choose it again.",
            None,
        ),
        "official-launcher-confirmation-required" => card(
            "The official launcher wasn't opened",
            "PD2Launcher.exe isn't verified, so running it needs your confirmation. Nothing was changed.",
            None,
        ),
        "official-client-not-reconciled" => card(
            "The official launcher's update wasn't accepted",
            "The official launcher closed, but the game update it made couldn't be checked. Open Diagnostics to see what was found.",
            Some(OpenDiagnostics),
        ),
        "safe-stop-unrecorded" => card(
            "The stop request didn't go through",
            "The task is still running. Try stopping it again in a moment.",
            None,
        ),
        _ => match operation.job {
            Some(ActiveJobView::Download) => card(
                "Download didn't finish",
                "The download stopped. Anything already downloaded is kept. Try again, or open Diagnostics and choose Save report to ask for help.",
                Some(TryAgain),
            ),
            Some(ActiveJobView::InstallBaseGame | ActiveJobView::InstallProjectDiablo) => stopped(
                "Installation stopped. Your downloads are kept. Try again, or open Diagnostics and choose Save report to ask for help.",
            ),
            Some(ActiveJobView::Update) => card(
                "The update didn't finish",
                "Try again, or open Diagnostics and choose Save report to ask for help.",
                Some(TryAgain),
            ),
            Some(ActiveJobView::Relocation) => card(
                "The install location wasn't changed",
                "Nothing was moved. Open the Setup step to see why.",
                None,
            ),
            Some(ActiveJobView::Reset) => card(
                "Start over didn't finish",
                "Open Diagnostics and choose Save report to ask for help.",
                Some(OpenDiagnostics),
            ),
            _ => card(
                "Something went wrong",
                "The task stopped at a safe point. Try again, or open Diagnostics and choose Save report to ask for help.",
                Some(OpenDiagnostics),
            ),
        },
    }
}

/// A byte count in decimal units, as download sizes are advertised.
pub(crate) fn human_bytes(bytes: u64) -> String {
    const UNITS: [(&str, f64); 3] = [("GB", 1e9), ("MB", 1e6), ("KB", 1e3)];
    UNITS
        .iter()
        .find(|(_, size)| bytes as f64 >= *size)
        .map_or_else(
            || format!("{bytes} bytes"),
            |(unit, size)| format!("{:.1} {unit}", bytes as f64 / size),
        )
}

/// A remaining time, rounded up to whole minutes.
pub(crate) fn human_duration(seconds: u64) -> String {
    let minutes = seconds.div_ceil(60);
    match minutes {
        0 | 1 if seconds < 60 => "less than a minute".to_owned(),
        1 => "1 minute".to_owned(),
        2..=89 => format!("{minutes} minutes"),
        _ => {
            let (hours, rest) = (minutes / 60, minutes % 60);
            let hours = if hours == 1 {
                "1 hour".to_owned()
            } else {
                format!("{hours} hours")
            };
            match rest {
                0 => hours,
                1 => format!("{hours} 1 minute"),
                rest => format!("{hours} {rest} minutes"),
            }
        }
    }
}

/// Why Play did not start, and what to try next.
pub(crate) const PLAY_FAILED: &str = "The game didn't start. Try Play again; if it keeps failing, switch Graphics to DirectDraw in Settings or save a report from Diagnostics.";

/// Why the game did not start after a keep left the game folder without a
/// way to the characters (`saves-unavailable`).
const SAVES_UNAVAILABLE: &str = "The game wouldn't have found your characters: their folder couldn't be connected to the game folder. Check that your home folder has free space, then try again.";

/// What closing the window does now.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CloseDecision {
    /// Nothing is running that closing would interrupt.
    Close,
    /// Ask first: work is running.
    Confirm,
    /// "Stop and close" was chosen: close once the work has stopped.
    WaitForStop,
}

pub(crate) fn close_decision(snapshot: &UiSnapshot, stopping: bool) -> CloseDecision {
    let working = snapshot
        .operation
        .as_ref()
        .is_some_and(OperationView::owns_mutation);
    match (working, stopping) {
        (false, _) => CloseDecision::Close,
        (true, true) => CloseDecision::WaitForStop,
        (true, false) => CloseDecision::Confirm,
    }
}

/// The close confirmation's explanation for the running job.
pub(crate) fn close_warning(snapshot: &UiSnapshot) -> &'static str {
    match snapshot.active_job {
        Some(ActiveJobView::Download) => {
            "Downloads are still running. Closing stops them at a safe point; everything already downloaded is kept."
        }
        Some(ActiveJobView::InstallBaseGame) => {
            "Installation is still running. Closing stops it at a safe point; your downloads are kept, but you'll need to enter your CD keys again."
        }
        Some(ActiveJobView::InstallProjectDiablo) => {
            "Installation is still running. Closing stops it at a safe point; your downloads are kept."
        }
        Some(ActiveJobView::Update) => {
            "An update is still running. Closing stops it at a safe point; you can update again later."
        }
        _ => "The launcher is still working. Closing stops it at a safe point first.",
    }
}

/// The official launcher fallback is shown once the game is installed.
/// The official launcher only exists once Project Diablo 2 is installed, so
/// it is offered for a Ready installation only; during setup (including the
/// Install Project Diablo 2 step) there is nothing to open.
pub(crate) fn official_launcher_offered(snapshot: &UiSnapshot) -> bool {
    snapshot.host == HostView::Supported && snapshot.installation == InstallationStatus::Ready
}

/// One user-facing setup step. The header's four step marks
/// stand for them ([`FlowStep`]); each step ends with a visible result
/// before the next is offered.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum SetupStep {
    Device,
    /// Install location and how to get the game.
    Setup,
    /// Download and verify everything.
    Download,
    /// Install (or import) Diablo II, with the CD keys when needed.
    InstallD2,
    InstallPd2,
    Play,
}

impl SetupStep {
    pub(crate) const ALL: [Self; 6] = [
        Self::Device,
        Self::Setup,
        Self::Download,
        Self::InstallD2,
        Self::InstallPd2,
        Self::Play,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Device => "Check your computer",
            Self::Setup => "Location & installers",
            Self::Download => "Download",
            Self::InstallD2 => "Install Diablo II",
            Self::InstallPd2 => "Install Project Diablo 2",
            Self::Play => "Play",
        }
    }

    pub(crate) const fn hint(self) -> &'static str {
        match self {
            Self::Device => "Makes sure this computer can run the game",
            Self::Setup => "Where to install, and how to get Diablo II",
            Self::Download => "Downloads and checks everything first",
            Self::InstallD2 => "Diablo II and Lord of Destruction",
            Self::InstallPd2 => "Installs Project Diablo 2 from the downloads",
            Self::Play => "Everything is ready",
        }
    }

    const fn next(self) -> Option<Self> {
        match self {
            Self::Device => Some(Self::Setup),
            Self::Setup => Some(Self::Download),
            Self::Download => Some(Self::InstallD2),
            Self::InstallD2 => Some(Self::InstallPd2),
            Self::InstallPd2 => Some(Self::Play),
            Self::Play => None,
        }
    }

    const fn previous(self) -> Option<Self> {
        match self {
            Self::Device => None,
            Self::Setup => Some(Self::Device),
            Self::Download => Some(Self::Setup),
            Self::InstallD2 => Some(Self::Download),
            Self::InstallPd2 => Some(Self::InstallD2),
            Self::Play => Some(Self::InstallPd2),
        }
    }
}

/// The four step marks along the header: Check · Download · Install · Play. The six setup steps map onto them;
/// choosing where to install and how to get Diablo II begins Download.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum FlowStep {
    Check,
    Download,
    Install,
    Play,
}

impl FlowStep {
    pub(crate) const ALL: [Self; 4] = [Self::Check, Self::Download, Self::Install, Self::Play];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Check => "Check",
            Self::Download => "Download",
            Self::Install => "Install",
            Self::Play => "Play",
        }
    }

    pub(crate) const fn of(step: SetupStep) -> Self {
        match step {
            SetupStep::Device => Self::Check,
            SetupStep::Setup | SetupStep::Download => Self::Download,
            SetupStep::InstallD2 | SetupStep::InstallPd2 => Self::Install,
            SetupStep::Play => Self::Play,
        }
    }

    /// The setup steps the mark stands for, in order.
    pub(crate) fn steps(self) -> impl Iterator<Item = SetupStep> {
        SetupStep::ALL
            .into_iter()
            .filter(move |step| Self::of(*step) == self)
    }
}

/// How one step mark shows: ✓ done, a filled number for the current one, !
/// when the current one stopped on a problem, a ring for those ahead.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StepMark {
    Done,
    Current,
    Failed,
    Ahead,
}

/// The mark of `step` while the flow is at `current`; `failed`: the current
/// one stopped on a problem.
pub(crate) fn step_mark(step: FlowStep, current: FlowStep, failed: bool) -> StepMark {
    match step.cmp(&current) {
        std::cmp::Ordering::Less => StepMark::Done,
        std::cmp::Ordering::Greater => StepMark::Ahead,
        std::cmp::Ordering::Equal if failed => StepMark::Failed,
        std::cmp::Ordering::Equal => StepMark::Current,
    }
}

/// Whether the flow's current mark stopped on a problem: this computer is
/// missing something, the step's last job failed or was interrupted, or the
/// installation needs a look.
pub(crate) fn flow_failed(
    snapshot: &UiSnapshot,
    current: FlowStep,
    deck: &DeckPresentation,
) -> bool {
    if service_failure(snapshot).is_some() {
        return true;
    }
    let blocked =
        deck.journey.iter().zip(SetupStep::ALL).any(|(item, step)| {
            item.state == JourneyState::Blocked && FlowStep::of(step) == current
        });
    let job_failed = snapshot.operation.as_ref().is_some_and(|operation| {
        matches!(
            operation.status,
            OperationStatusView::Failed | OperationStatusView::Indeterminate
        ) && operation.job.and_then(|job| match job {
            ActiveJobView::Download => Some(FlowStep::Download),
            ActiveJobView::InstallBaseGame | ActiveJobView::InstallProjectDiablo => {
                Some(FlowStep::Install)
            }
            _ => None,
        }) == Some(current)
    });
    let interrupted = current == FlowStep::Install
        && matches!(
            snapshot.installation,
            InstallationStatus::StageInterrupted(_)
        );
    blocked || job_failed || interrupted
}

/// The running download's or install's percent, for the current mark and
/// the line along the header; `None` while nothing runs or its amount is
/// unknown.
pub(crate) fn flow_progress(snapshot: &UiSnapshot) -> Option<u8> {
    running_step(snapshot)?;
    snapshot
        .operation
        .as_ref()
        .and_then(OperationView::progress_percent)
        .map(|percent| percent.min(100))
}

/// The step the installation stands at, from the service's evidence: the
/// first step whose result is not there yet.
pub(crate) fn standing_step(snapshot: &UiSnapshot) -> SetupStep {
    if snapshot.host != HostView::Supported {
        return SetupStep::Device;
    }
    match snapshot.installation {
        // Start over is offered at Install Diablo II, even when the
        // interruption came before the downloads were complete (the
        // Download step's prefix setup).
        InstallationStatus::StageInterrupted(_) => SetupStep::InstallD2,
        InstallationStatus::NeedsBaseGame
        | InstallationStatus::NeedsExpansion
        | InstallationStatus::RepairRequired
        | InstallationStatus::Indeterminate => {
            if snapshot.downloads_ready() {
                SetupStep::InstallD2
            } else {
                SetupStep::Download
            }
        }
        InstallationStatus::NeedsClient | InstallationStatus::NeedsRuntime => SetupStep::InstallPd2,
        InstallationStatus::Ready => SetupStep::Play,
    }
}

/// The page the setup record opens on while the first check runs, before
/// any evidence: Play for an installed game (waiting, "Getting ready…"),
/// Install Diablo II once an install began (the first install step, so the
/// check can only move it on). `None` keeps Check your computer. Once the
/// check ends the evidence decides, as for anyone else.
pub(crate) fn opening_step(snapshot: &UiSnapshot) -> Option<SetupStep> {
    if snapshot.host != HostView::Checking {
        return None;
    }
    match snapshot.setup_record {
        SetupRecord::Installed => Some(SetupStep::Play),
        SetupRecord::Started => Some(SetupStep::InstallD2),
        SetupRecord::None => None,
    }
}

/// The status of the operation of `step`'s setup job, if the snapshot's
/// operation is that job's (or a job's whose name it does not carry yet).
fn job_outcome(snapshot: &UiSnapshot, step: SetupStep) -> Option<OperationStatusView> {
    let operation = snapshot.operation.as_ref()?;
    let job = match step {
        SetupStep::Download => ActiveJobView::Download,
        SetupStep::InstallD2 => ActiveJobView::InstallBaseGame,
        SetupStep::InstallPd2 => ActiveJobView::InstallProjectDiablo,
        _ => return None,
    };
    match operation.job {
        Some(found) if found == job => Some(operation.status),
        None if operation.mutating && !operation.status.is_terminal() => Some(operation.status),
        _ => None,
    }
}

/// Install Diablo II has just put Diablo II and Lord of Destruction in:
/// Install Project Diablo 2 follows without a press, as soon as
/// nothing runs.
pub(crate) fn hands_on_to_project_diablo(snapshot: &UiSnapshot) -> bool {
    snapshot.host == HostView::Supported
        && snapshot.game == GameView::Idle
        && snapshot.active_job.is_none()
        && !snapshot.unresolved_journal
        && matches!(
            snapshot.installation,
            InstallationStatus::NeedsClient | InstallationStatus::NeedsRuntime
        )
        && snapshot.operation.as_ref().is_some_and(|operation| {
            operation.job == Some(ActiveJobView::InstallBaseGame)
                && operation.status == OperationStatusView::Succeeded
        })
}

/// The step a running setup job belongs to.
pub(crate) fn running_step(snapshot: &UiSnapshot) -> Option<SetupStep> {
    let running = snapshot
        .operation
        .as_ref()
        .is_some_and(OperationView::owns_mutation);
    match snapshot.active_job.filter(|_| running)? {
        ActiveJobView::Download => Some(SetupStep::Download),
        ActiveJobView::InstallBaseGame => Some(SetupStep::InstallD2),
        ActiveJobView::InstallProjectDiablo => Some(SetupStep::InstallPd2),
        _ => None,
    }
}

/// One thing step 3 downloads (or sets up), in the order the Download job
/// gets to it.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum DownloadItem {
    /// Wine and the Windows components it installs.
    Windows,
    /// The game environment, set up for Blizzard's downloader, which runs
    /// in it.
    Environment,
    /// Diablo II and Lord of Destruction: Blizzard's installers.
    Diablo,
    /// Project Diablo 2's installer and game files.
    ProjectDiablo,
}

impl DownloadItem {
    /// The item a Download job's phase works on. Checking what was
    /// downloaded comes last, with Project Diablo 2's files.
    const fn for_phase(phase: OperationPhase) -> Option<Self> {
        match phase {
            OperationPhase::RuntimeAcquisition => Some(Self::Windows),
            OperationPhase::PrefixInitialization => Some(Self::Environment),
            OperationPhase::BaseGameDownload | OperationPhase::ExpansionDownload => {
                Some(Self::Diablo)
            }
            OperationPhase::ClientDownload | OperationPhase::Verification => {
                Some(Self::ProjectDiablo)
            }
            _ => None,
        }
    }
}

/// How one Download item stands.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DownloadItemState {
    Waiting,
    Running,
    Done,
    /// Nothing to download: the player's own Diablo II.
    NotNeeded,
}

/// One row of step 3's list: the item, how it stands, and one line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DownloadRowView {
    pub(crate) item: DownloadItem,
    pub(crate) label: &'static str,
    pub(crate) detail: String,
    pub(crate) state: DownloadItemState,
}

/// What step 3 downloads, each with its own state: waiting before the
/// Download starts, done up to the item the running job works on, and all
/// done once the downloads are verified. The game environment is listed
/// only when Blizzard's downloader runs, and is done once set up.
pub(crate) fn download_rows(snapshot: &UiSnapshot) -> Vec<DownloadRowView> {
    let media = &snapshot.installer_media;
    let done = standing_step(snapshot) > SetupStep::Download;
    let running = (running_step(snapshot) == Some(SetupStep::Download))
        .then_some(snapshot.operation.as_ref())
        .flatten();
    let at = running.and_then(|operation| DownloadItem::for_phase(operation.phase));
    let blizzard_downloader = *media == InstallerMediaView::BlizzardDownload
        && snapshot.custom_installer_hosts().is_empty()
        && matches!(
            snapshot.installation,
            InstallationStatus::NeedsBaseGame | InstallationStatus::NeedsExpansion
        );
    let (diablo, diablo_detail) = match media {
        InstallerMediaView::Existing(existing) => (
            "Diablo II",
            format!(
                "Not needed: you're using your own at {}",
                existing.path.display()
            ),
        ),
        InstallerMediaView::Validated(path) => (
            "Diablo II and Lord of Destruction",
            format!("From your installer files in {}", path.display()),
        ),
        _ if !snapshot.custom_installer_hosts().is_empty() => (
            "Diablo II and Lord of Destruction",
            "From your custom sources (unverified)".to_owned(),
        ),
        _ => (
            "Diablo II and Lord of Destruction",
            "From Blizzard, about 2.1 GB · the longest part".to_owned(),
        ),
    };
    let mut items = vec![(
        DownloadItem::Windows,
        "Windows support",
        "What Windows programs need, with web components and .NET, about 0.4 GB".to_owned(),
    )];
    if blizzard_downloader {
        items.push((
            DownloadItem::Environment,
            "The game environment",
            "Set up first, since Blizzard's downloader runs in it".to_owned(),
        ));
    }
    items.push((DownloadItem::Diablo, diablo, diablo_detail));
    items.push((
        DownloadItem::ProjectDiablo,
        "Project Diablo 2",
        "Its installer and game files, about 0.3 GB".to_owned(),
    ));
    items
        .into_iter()
        .map(|(item, label, waiting)| {
            let state = if item == DownloadItem::Diablo && media.existing().is_some() {
                DownloadItemState::NotNeeded
            } else if done
                || at.is_some_and(|at| item < at)
                || (item == DownloadItem::Environment && snapshot.environment_ready)
            {
                DownloadItemState::Done
            } else if at == Some(item) {
                DownloadItemState::Running
            } else {
                DownloadItemState::Waiting
            };
            let detail = match state {
                DownloadItemState::Waiting | DownloadItemState::NotNeeded => waiting,
                DownloadItemState::Done if item == DownloadItem::Environment => "Set up".to_owned(),
                DownloadItemState::Done => "Downloaded and checked".to_owned(),
                DownloadItemState::Running => match item {
                    DownloadItem::Environment => running
                        .and_then(OperationView::environment_line)
                        .unwrap_or_else(|| "Setting up…".to_owned()),
                    DownloadItem::Diablo if matches!(media, InstallerMediaView::Validated(_)) => {
                        "Copying from your installer files…".to_owned()
                    }
                    DownloadItem::ProjectDiablo
                        if running.is_some_and(|operation| {
                            operation.phase == OperationPhase::Verification
                        }) =>
                    {
                        "Checking the downloads…".to_owned()
                    }
                    _ => "Downloading…".to_owned(),
                },
            };
            DownloadRowView {
                item,
                label,
                detail,
                state,
            }
        })
        .collect()
}

/// One task of Install, in order, mapped onto what the desktop's two install
/// jobs do (Install Diablo II, then Install Project Diablo 2).
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum InstallTask {
    /// Wine's first start in a new game environment and the Windows
    /// components (done in Download when Blizzard's downloader ran there).
    Environment,
    /// Blizzard's Diablo II installer, or the player's Diablo II copied or
    /// used where it is.
    Game,
    /// Blizzard's Lord of Destruction installer (not for the player's own).
    Expansion,
    /// The PD2 installer and Project Diablo 2's files, from the downloads.
    ProjectDiablo,
    /// The new files put in place and the last check.
    Verify,
}

/// How one Install task stands.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TaskState {
    Waiting,
    Running,
    Done,
    /// The install was stopped here; it goes on from here.
    Paused,
    Failed,
}

/// One row of Install's task list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InstallTaskRow {
    pub(crate) task: InstallTask,
    pub(crate) label: &'static str,
    pub(crate) detail: Option<String>,
    pub(crate) state: TaskState,
}

/// What an install consists of, for the task list and the one weighted
/// figure: whether the game environment is still to be set up, and whether
/// the player's own Diablo II is copied or used where it is instead of
/// Blizzard's installers running. The window fixes it when an install
/// starts, so the figure keeps one scale through both jobs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct InstallPlan {
    environment: bool,
    import: bool,
    in_place: bool,
    /// The player's Diablo II brings a Project Diablo 2, which is updated.
    project_diablo: bool,
}

/// How long each part of a desktop install typically takes, in seconds,
/// weighting the one figure for the whole install, on an x86-64 PC; under
/// FEX on ARM64 every part takes about twice as long, so the shape holds.
/// A part the engine counts bytes or files for fills by those; one it
/// counts nothing for fills by its typical time, up to [`TIMED_LIMIT`], so
/// the figure never stands still.
const ENVIRONMENT_SECONDS: u32 = 40;
const BASE_INSTALLER_SECONDS: u32 = 8;
const EXPANSION_INSTALLER_SECONDS: u32 = 13;
const IMPORT_SECONDS: u32 = 17;
const PROJECT_DIABLO_SECONDS: u32 = 13;
const VERIFY_SECONDS: u32 = 6;
const TIMED_LIMIT: f32 = 0.95;

impl InstallPlan {
    /// The install as `snapshot` stands: the game environment counts while
    /// it is not set up; `in_place` when the player's Diablo II is used
    /// where it is.
    pub(crate) fn for_snapshot(snapshot: &UiSnapshot, in_place: bool) -> Self {
        Self {
            environment: !snapshot.environment_ready,
            import: snapshot.installer_media.existing().is_some() || snapshot.game_folder.is_some(),
            in_place: in_place || snapshot.game_folder.is_some(),
            project_diablo: snapshot
                .installer_media
                .existing()
                .is_some_and(|existing| existing.project_diablo),
        }
    }

    /// The tasks, in order: five with Blizzard's installers, four with the
    /// player's own Diablo II.
    pub(crate) fn tasks(self) -> Vec<InstallTask> {
        let mut tasks = vec![InstallTask::Environment, InstallTask::Game];
        if !self.import {
            tasks.push(InstallTask::Expansion);
        }
        tasks.extend([InstallTask::ProjectDiablo, InstallTask::Verify]);
        tasks
    }

    /// How much `task` weighs in the figure: nothing for a game environment
    /// set up before this install.
    fn weight(self, task: InstallTask) -> u32 {
        match task {
            InstallTask::Environment if self.environment => ENVIRONMENT_SECONDS,
            InstallTask::Environment => 0,
            InstallTask::Game if self.import => IMPORT_SECONDS,
            InstallTask::Game => BASE_INSTALLER_SECONDS,
            InstallTask::Expansion => EXPANSION_INSTALLER_SECONDS,
            InstallTask::ProjectDiablo => PROJECT_DIABLO_SECONDS,
            InstallTask::Verify => VERIFY_SECONDS,
        }
    }

    fn label(self, task: InstallTask) -> &'static str {
        match task {
            InstallTask::Environment => "Set up the game environment",
            InstallTask::Game if self.in_place => "Use your Diablo II where it is",
            InstallTask::Game if self.import => "Copy your Diablo II",
            InstallTask::Game => "Install Diablo II",
            InstallTask::Expansion => "Install Lord of Destruction",
            InstallTask::ProjectDiablo if self.project_diablo => "Update Project Diablo 2",
            InstallTask::ProjectDiablo => "Install Project Diablo 2",
            InstallTask::Verify => "Check the game files",
        }
    }
}

/// The installation's evidence already shows `task` done.
fn task_done(snapshot: &UiSnapshot, task: InstallTask) -> bool {
    use InstallationStatus::{NeedsClient, NeedsExpansion, NeedsRuntime, Ready};
    match task {
        InstallTask::Environment => snapshot.environment_ready,
        InstallTask::Game => matches!(
            snapshot.installation,
            NeedsExpansion | NeedsClient | NeedsRuntime | Ready
        ),
        InstallTask::Expansion => {
            matches!(snapshot.installation, NeedsClient | NeedsRuntime | Ready)
        }
        InstallTask::ProjectDiablo => matches!(snapshot.installation, NeedsRuntime | Ready),
        InstallTask::Verify => snapshot.installation == Ready,
    }
}

/// The task the running install job works on and, when its phase says how
/// far it is regardless of time, that fraction. `None` while no install
/// job runs, and for its start before its first progress names it.
fn running_task(snapshot: &UiSnapshot, plan: InstallPlan) -> Option<(InstallTask, Option<f32>)> {
    if !matches!(
        running_step(snapshot),
        Some(SetupStep::InstallD2 | SetupStep::InstallPd2)
    ) {
        return None;
    }
    let operation = snapshot.operation.as_ref()?;
    let job = operation.job?;
    let last_of_d2 = if plan.import {
        InstallTask::Game
    } else {
        InstallTask::Expansion
    };
    match (job, operation.phase) {
        (_, OperationPhase::RuntimeAcquisition | OperationPhase::PrefixInitialization) => {
            Some((InstallTask::Environment, None))
        }
        (_, OperationPhase::BaseGameInstall | OperationPhase::ImportGame) => {
            Some((InstallTask::Game, None))
        }
        (_, OperationPhase::ExpansionInstall) => Some((InstallTask::Expansion, None)),
        // Install Diablo II's closing check: its last task is done.
        (ActiveJobView::InstallBaseGame, OperationPhase::Verification) => {
            Some((last_of_d2, Some(1.0)))
        }
        (
            ActiveJobView::InstallProjectDiablo,
            OperationPhase::ClientDownload | OperationPhase::Verification,
        ) => Some((InstallTask::ProjectDiablo, None)),
        (ActiveJobView::InstallProjectDiablo, OperationPhase::Promotion) => {
            Some((InstallTask::Verify, None))
        }
        _ => None,
    }
}

/// What a running task does now, in plain words.
fn running_detail(snapshot: &UiSnapshot, plan: InstallPlan, task: InstallTask) -> String {
    let Some(operation) = snapshot.operation.as_ref() else {
        return "Starting…".to_owned();
    };
    if operation.status == OperationStatusView::SafeStopping {
        return "Stopping safely…".to_owned();
    }
    if operation.job.is_none() {
        return "Starting…".to_owned();
    }
    let counted = matches!(operation.progress_unit(), Some("bytes" | "files"));
    match task {
        InstallTask::Environment => operation.environment_line().unwrap_or_else(|| {
            "Setting up what the game and the official PD2 launcher need".to_owned()
        }),
        InstallTask::Game if plan.in_place => "Setting up your Diablo II where it is".to_owned(),
        InstallTask::Game if plan.import => "Copying your Diablo II".to_owned(),
        InstallTask::Game => "Running the Diablo II installer".to_owned(),
        InstallTask::Expansion => "Running the Lord of Destruction installer".to_owned(),
        InstallTask::ProjectDiablo if operation.phase == OperationPhase::Verification => {
            "Checking Project Diablo 2's files".to_owned()
        }
        InstallTask::ProjectDiablo if counted => "Getting Project Diablo 2's files".to_owned(),
        InstallTask::ProjectDiablo => "Running the Project Diablo 2 installer".to_owned(),
        InstallTask::Verify => "Putting the files in place".to_owned(),
    }
}

/// Install's tasks and how each stands: done up to where the install is
/// (the running job's task, else the first one the evidence does not show
/// done), running there, or paused or failed there when the last install
/// was stopped or failed, and waiting after it. Each row says only how its
/// task stands; amounts and times are the figure's.
pub(crate) fn install_tasks(snapshot: &UiSnapshot, plan: InstallPlan) -> Vec<InstallTaskRow> {
    let tasks = plan.tasks();
    let installing = matches!(
        running_step(snapshot),
        Some(SetupStep::InstallD2 | SetupStep::InstallPd2)
    );
    let first_open = tasks
        .iter()
        .copied()
        .find(|task| !task_done(snapshot, *task));
    let at = running_task(snapshot, plan)
        .map(|(task, _)| task)
        .or(first_open.filter(|_| installing));
    let ended = snapshot.operation.as_ref().filter(|operation| {
        matches!(
            operation.job,
            Some(ActiveJobView::InstallBaseGame | ActiveJobView::InstallProjectDiablo)
        ) && operation.status.is_terminal()
    });
    let stopped = !installing
        && (snapshot.stop_result.is_some()
            || ended.is_some_and(|operation| operation.status == OperationStatusView::Cancelled));
    let failed = !installing
        && ended.is_some_and(|operation| {
            matches!(
                operation.status,
                OperationStatusView::Failed | OperationStatusView::Indeterminate
            )
        });
    tasks
        .into_iter()
        .map(|task| {
            let state = if installing && at == Some(task) {
                TaskState::Running
            } else if task_done(snapshot, task) || at.is_some_and(|at| task < at) {
                TaskState::Done
            } else if first_open == Some(task) && stopped {
                TaskState::Paused
            } else if first_open == Some(task) && failed {
                TaskState::Failed
            } else {
                TaskState::Waiting
            };
            let detail = match state {
                TaskState::Running => Some(running_detail(snapshot, plan, task)),
                TaskState::Done => Some("Done".to_owned()),
                TaskState::Paused => Some("Paused".to_owned()),
                TaskState::Failed => Some("Did not finish".to_owned()),
                TaskState::Waiting => None,
            };
            InstallTaskRow {
                task,
                label: plan.label(task),
                detail,
                state,
            }
        })
        .collect()
}

/// The one figure for the whole install, 0 to 100, while an install job
/// runs: the tasks done by their weight, and the running one by the
/// engine's bytes or files when it counts them, else by `elapsed` in its
/// phase against its typical time (the game environment never behind the
/// parts it finished). `None` while no install job runs.
pub(crate) fn install_percent(
    snapshot: &UiSnapshot,
    plan: InstallPlan,
    elapsed: Option<Duration>,
) -> Option<u8> {
    let rows = install_tasks(snapshot, plan);
    let running = rows.iter().find(|row| row.state == TaskState::Running)?;
    let total: u32 = rows.iter().map(|row| plan.weight(row.task)).sum();
    if total == 0 {
        return None;
    }
    let done: u32 = rows
        .iter()
        .filter(|row| row.state == TaskState::Done)
        .map(|row| plan.weight(row.task))
        .sum();
    let weight = plan.weight(running.task);
    let timed = |seconds: u32| {
        elapsed.map_or(0.0, |elapsed| {
            (elapsed.as_secs_f32() / seconds.max(1) as f32).min(TIMED_LIMIT)
        })
    };
    let fraction = match running_task(snapshot, plan) {
        Some((_, Some(fraction))) => fraction,
        // The job's start, before its first progress names its task.
        None => 0.0,
        Some((task, None)) => {
            let operation = snapshot.operation.as_ref();
            let counted = operation
                .filter(|operation| matches!(operation.progress_unit(), Some("bytes" | "files")))
                .and_then(OperationView::progress_percent);
            match (task, counted) {
                (_, Some(percent)) => f32::from(percent.min(100)) / 100.0,
                (InstallTask::Environment, None) => {
                    let parts =
                        operation
                            .and_then(OperationView::environment)
                            .map_or(0.0, |progress| {
                                let planned = progress.planned().collect::<Vec<_>>();
                                let before = planned
                                    .iter()
                                    .position(|part| *part == progress.running())
                                    .unwrap_or(0);
                                before as f32 / planned.len().max(1) as f32
                            });
                    timed(ENVIRONMENT_SECONDS).max(parts)
                }
                (_, None) => timed(weight),
            }
        }
    };
    let percent = (done as f32 + fraction.clamp(0.0, 1.0) * weight as f32) * 100.0 / total as f32;
    Some(percent.clamp(0.0, 100.0) as u8)
}

/// Install's title: the task underway while it runs ("Installing Lord of
/// Destruction"), else how the install stands.
pub(crate) fn install_title(rows: &[InstallTaskRow], snapshot: &UiSnapshot) -> &'static str {
    let stopping = snapshot
        .operation
        .as_ref()
        .is_some_and(|operation| operation.status == OperationStatusView::SafeStopping);
    if stopping {
        return "Stopping safely…";
    }
    match rows
        .iter()
        .find(|row| row.state != TaskState::Done && row.state != TaskState::Waiting)
    {
        Some(row) if row.state == TaskState::Running => match row.task {
            InstallTask::Environment => "Setting up the game environment",
            InstallTask::Game if row.label == "Copy your Diablo II" => "Copying your Diablo II",
            InstallTask::Game if row.label == "Use your Diablo II where it is" => {
                "Setting up your Diablo II"
            }
            InstallTask::Game => "Installing Diablo II",
            InstallTask::Expansion => "Installing Lord of Destruction",
            InstallTask::ProjectDiablo if row.label == "Update Project Diablo 2" => {
                "Updating Project Diablo 2"
            }
            InstallTask::ProjectDiablo => "Installing Project Diablo 2",
            InstallTask::Verify => "Checking the game files",
        },
        Some(row) if row.state == TaskState::Paused => "Install paused",
        Some(_) => "Install failed",
        None => "Install",
    }
}

/// The game folder the player's files are in (saved characters, loot
/// filters, Project Diablo 2's settings), once Diablo II is installed: the
/// Diablo II folder used where it is, or the one in the launcher's game
/// environment. `None` while it isn't installed or can't be reached.
pub(crate) fn game_files_folder(snapshot: &UiSnapshot) -> Option<PathBuf> {
    if !matches!(
        snapshot.installation,
        InstallationStatus::NeedsClient
            | InstallationStatus::NeedsRuntime
            | InstallationStatus::Ready
    ) || snapshot.host != HostView::Supported
    {
        return None;
    }
    match &snapshot.game_folder {
        Some(folder) => folder.available.then(|| folder.path.clone()),
        None => snapshot
            .location
            .as_ref()
            .filter(|location| location.available)
            .map(|location| location.path.join(crate::game_folder::GAME_RELATIVE)),
    }
}

/// One row of Repair: what it is about, how it stands, and its tone.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RepairRowView {
    pub(crate) label: &'static str,
    pub(crate) detail: &'static str,
    pub(crate) tone: EvidenceTone,
}

/// Repair's rows, as the last check found the installation: the games, the
/// updates, and this computer.
pub(crate) fn repair_rows(snapshot: &UiSnapshot) -> Vec<RepairRowView> {
    let checking = snapshot.host == HostView::Checking
        || snapshot.operation.as_ref().is_some_and(|operation| {
            operation.phase == OperationPhase::HostAudit && !operation.status.is_terminal()
        });
    let (installation, tone) = if snapshot.game_folder_unavailable() {
        (
            "The Diablo II folder it uses where it is can't be reached: put it back and check again, or start over",
            EvidenceTone::Failure,
        )
    } else if checking {
        ("Checking…", EvidenceTone::Working)
    } else {
        match snapshot.installation {
            InstallationStatus::Ready => ("Installed and checked", EvidenceTone::Verified),
            InstallationStatus::RepairRequired => (
                "Unfinished work was recorded: Check again finishes or undoes it",
                EvidenceTone::Advisory,
            ),
            InstallationStatus::Indeterminate => (
                "The launcher can't tell what's installed: open Diagnostics, or start over",
                EvidenceTone::Failure,
            ),
            InstallationStatus::StageInterrupted(_) => (
                "An install step was interrupted: Start over resolves it",
                EvidenceTone::Failure,
            ),
            InstallationStatus::NeedsBaseGame
            | InstallationStatus::NeedsExpansion
            | InstallationStatus::NeedsClient
            | InstallationStatus::NeedsRuntime => (
                "Not completely installed: Continue installation goes on where it stopped",
                EvidenceTone::Advisory,
            ),
        }
    };
    let (update, update_tone) = match snapshot.update {
        UpdateStatus::UpToDate => ("Up to date", EvidenceTone::Verified),
        UpdateStatus::Available => ("An update is available", EvidenceTone::Advisory),
        UpdateStatus::Checking => ("Checking for updates…", EvidenceTone::Working),
        UpdateStatus::CheckFailed => ("The update check failed", EvidenceTone::Advisory),
        UpdateStatus::Unknown => ("Not checked yet", EvidenceTone::Working),
    };
    let (host, host_tone) = match snapshot.host {
        HostView::Supported => ("Ready for the game", EvidenceTone::Verified),
        HostView::Checking => ("Checking…", EvidenceTone::Working),
        HostView::Unsupported if service_failure(snapshot).is_some() => (
            "The launcher couldn't check it: open Diagnostics",
            EvidenceTone::Failure,
        ),
        HostView::Unsupported => (
            "Something the game needs is missing: Check again once it's installed",
            EvidenceTone::Failure,
        ),
    };
    vec![
        RepairRowView {
            label: "Diablo II, Lord of Destruction and Project Diablo 2",
            detail: installation,
            tone,
        },
        RepairRowView {
            label: "Project Diablo 2 updates",
            detail: update,
            tone: update_tone,
        },
        RepairRowView {
            label: "This computer",
            detail: host,
            tone: host_tone,
        },
    ]
}

/// Whether Repair's Check again can run now: nothing else runs and the
/// game is closed, as the service requires.
pub(crate) fn check_again_available(snapshot: &UiSnapshot) -> bool {
    snapshot.game == GameView::Idle
        && !snapshot
            .operation
            .as_ref()
            .is_some_and(|operation| !operation.status.is_terminal())
}

/// Settings → About: this launcher's version, with the commit it was built
/// from when its build says (the release and package builds do).
pub(crate) fn about_version() -> String {
    let version = env!("CARGO_PKG_VERSION");
    match option_env!("PD2_LAUNCHER_COMMIT").filter(|commit| !commit.is_empty()) {
        Some(commit) => format!(
            "{version} ({})",
            commit.chars().take(12).collect::<String>()
        ),
        None => version.to_owned(),
    }
}

/// Settings → About: what the launcher runs the game with, as far as this
/// build knows it: the Wine it downloads and, on ARM64, the built-in FEX it
/// can download.
pub(crate) fn about_built_with() -> String {
    let wine = crate::runtime::current().display_name;
    if cfg!(target_arch = "aarch64") {
        format!(
            "{wine} · FEX-Emu {} (the built-in FEX)",
            crate::builtin_fex::fex_version()
        )
    } else {
        wine.to_owned()
    }
}

/// About → How it works: the technical names the setup's plain words stand
/// for (the flow says "Windows support" and "the game environment"; Wine,
/// the prefix and FEX are named here).
pub(crate) fn about_terms() -> Vec<(&'static str, String)> {
    let wine = crate::runtime::current().display_name;
    let mut terms = vec![
        (
            "Windows support",
            format!(
                "Wine ({wine}), with Wine Gecko for web components, and Wine Mono and the .NET Desktop Runtime for .NET"
            ),
        ),
        (
            "Game environment",
            "A Wine prefix the launcher keeps in its install location (its prefix folder), where Diablo II and Project Diablo 2 are installed".to_owned(),
        ),
    ];
    if cfg!(target_arch = "aarch64") {
        terms.push((
            "FEX",
            "FEX-Emu runs the x86 Windows support on this ARM64 computer".to_owned(),
        ));
    }
    terms
}

/// After an import that copied the player's Diablo II (never one used where
/// it is): the folder they chose isn't needed any more, so it can go.
pub(crate) fn copied_folder_note(snapshot: &UiSnapshot) -> Option<String> {
    if snapshot.game_folder.is_some() || standing_step(snapshot) <= SetupStep::InstallD2 {
        return None;
    }
    let existing = snapshot.installer_media.existing()?;
    Some(format!(
        "The launcher has its own copy of Diablo II now, so you can safely delete your folder at {} if you don't need it anymore.",
        existing.path.display()
    ))
}

/// What a safe stop waits for, while it does: a Wine program's window that
/// is stuck is cleared only when the launcher's time limit for it runs out.
pub(crate) const SAFE_STOP_NOTE: &str = "The current step finishes at a safe point first. If an installer window is stuck, it is cleared only when its time limit runs out, which can take up to ten minutes.";

/// Shown for the whole Download or Install Diablo II step whenever that step
/// runs Blizzard's own downloader or installers, whose windows open on top.
pub(crate) const BLIZZARD_WINDOWS_NOTICE: &str = "Blizzard's installer windows may pop up in front of other windows. Don't close them or click anything — the launcher handles them and closes them when it's done.";

/// The notice about Blizzard's windows while a step that runs them is
/// running: Download and Install Diablo II, except for a Diablo II the
/// player already has, which runs none of Blizzard's programs.
pub(crate) fn blizzard_windows_notice(snapshot: &UiSnapshot) -> Option<&'static str> {
    match running_step(snapshot)? {
        SetupStep::Download | SetupStep::InstallD2
            if snapshot.installer_media.existing().is_none() =>
        {
            Some(BLIZZARD_WINDOWS_NOTICE)
        }
        _ => None,
    }
}

/// Why a step cannot be changed right now.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StepLock {
    HostNotReady,
    GameRunning,
    OfficialLauncherOpen,
    WorkRunning,
    RepairPending,
}

impl StepLock {
    pub(crate) const fn reason(self) -> &'static str {
        match self {
            Self::HostNotReady => "The launcher is still checking this device.",
            Self::GameRunning => "Close the game first.",
            Self::OfficialLauncherOpen => "Close the official PD2 launcher first.",
            Self::WorkRunning => "Wait for the current task to finish.",
            Self::RepairPending => "The launcher is finishing a repair first.",
        }
    }
}

/// What the user may do with one setup step now. This is the only source
/// of truth for the clickable Setup rows; the coordinator applies the same
/// guards to the intents a row emits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StepAffordance {
    Locked(StepLock),
    /// Open the step's editor; nothing is running.
    Change,
    /// An install is running: stop it at a safe point, then open the editor.
    StopAndChange,
    /// The step was interrupted and can only be started over (confirmed).
    ResetAvailable,
    /// Nothing to change for this step.
    None,
}

/// The outcome of a user-requested install stop, from the fresh audit that
/// followed it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct StopResultView {
    installation: InstallationStatus,
    recovery: bool,
}

impl StopResultView {
    pub(crate) const fn new(installation: InstallationStatus, recovery: bool) -> Self {
        Self {
            installation,
            recovery,
        }
    }

    /// The installation the audit after the stop found.
    pub(crate) const fn installation(self) -> InstallationStatus {
        self.installation
    }

    /// The audit could not establish one outcome: only Diagnostics (and a
    /// confirmed Start over) remain.
    pub(crate) const fn needs_recovery(self) -> bool {
        self.recovery
    }

    /// The stop left an interrupted stage that only Start over resolves.
    pub(crate) const fn needs_start_over(self) -> bool {
        matches!(self.installation, InstallationStatus::StageInterrupted(_))
    }

    /// What the stop kept and what it did not, in player terms.
    pub(crate) fn lines(self) -> (Vec<&'static str>, Vec<&'static str>) {
        let mut kept = vec![
            "Downloaded files, including unfinished downloads, which continue where they stopped",
        ];
        kept.push("Your CD keys, while the launcher stays open");
        let mut not_kept = Vec::new();
        match self.installation {
            InstallationStatus::NeedsBaseGame => {
                kept.push("The game environment setup, if it was finished");
            }
            InstallationStatus::NeedsExpansion => {
                kept.push("Diablo II");
            }
            InstallationStatus::NeedsClient
            | InstallationStatus::NeedsRuntime
            | InstallationStatus::Ready => {
                kept.pop();
                kept.push("Diablo II and Lord of Destruction");
            }
            InstallationStatus::StageInterrupted(_) => {
                not_kept.push(
                    "The step that was running when it stopped: Start over removes what it left and installs again",
                );
            }
            InstallationStatus::RepairRequired | InstallationStatus::Indeterminate => {
                not_kept.push(
                    "A clear record of the step that was running: open Diagnostics to see what was found",
                );
            }
        }
        (kept, not_kept)
    }
}

pub(crate) fn step_affordance(snapshot: &UiSnapshot, step: SetupStep) -> StepAffordance {
    if !matches!(step, SetupStep::Setup | SetupStep::InstallD2) {
        return StepAffordance::None;
    }
    if snapshot.host != HostView::Supported {
        return StepAffordance::Locked(StepLock::HostNotReady);
    }
    if snapshot.game == GameView::Running {
        return StepAffordance::Locked(StepLock::GameRunning);
    }
    let mutation_active = snapshot
        .operation
        .as_ref()
        .is_some_and(OperationView::owns_mutation);
    match snapshot.active_job {
        Some(ActiveJobView::OfficialLauncher) => {
            return StepAffordance::Locked(StepLock::OfficialLauncherOpen);
        }
        // The installer choice is what Download and Install Diablo II use:
        // it changes only once they stopped.
        Some(ActiveJobView::Download | ActiveJobView::InstallBaseGame) if mutation_active => {
            return match step {
                SetupStep::Setup => StepAffordance::StopAndChange,
                _ => StepAffordance::Locked(StepLock::WorkRunning),
            };
        }
        Some(_) => return StepAffordance::Locked(StepLock::WorkRunning),
        None if mutation_active => return StepAffordance::Locked(StepLock::WorkRunning),
        None => {}
    }
    if snapshot.unresolved_journal || snapshot.installation == InstallationStatus::RepairRequired {
        return StepAffordance::Locked(StepLock::RepairPending);
    }
    match step {
        SetupStep::Setup => StepAffordance::Change,
        SetupStep::InstallD2
            if matches!(
                snapshot.installation,
                InstallationStatus::StageInterrupted(_)
            ) =>
        {
            StepAffordance::ResetAvailable
        }
        _ => StepAffordance::None,
    }
}

/// The evidence row the service publishes for the launcher's own Wine
/// prefix. A verified prefix means an earlier install already started.
pub(crate) const PREFIX_EVIDENCE_LABEL: &str = "Managed prefix";

/// Whether the snapshot is a first run nobody has started installing: it
/// opens at Step 1, whatever the background check already found. Anything
/// else (downloads made, work running or stopped, a result to read, a game
/// partly installed, an unusable location) opens where it stands.
pub(crate) fn first_run(snapshot: &UiSnapshot) -> bool {
    snapshot.installation == InstallationStatus::NeedsBaseGame
        && !snapshot.downloads.complete
        && snapshot.authorization == AuthorizationView::Absent
        && snapshot.active_job.is_none()
        && !snapshot
            .operation
            .as_ref()
            .is_some_and(OperationView::owns_mutation)
        && snapshot.stop_result.is_none()
        && !snapshot.unresolved_journal
        && !snapshot.location_unavailable()
        && !snapshot.game_folder_unavailable()
        && service_failure(snapshot).is_none()
        && result_card(snapshot).is_none()
        && !snapshot.evidence.iter().any(|evidence| {
            evidence.label == PREFIX_EVIDENCE_LABEL && evidence.tone == EvidenceTone::Verified
        })
}

/// Whether the step cursor leads the Overview: not while something stops
/// the whole setup (an unreachable location or game folder, a launcher
/// service that could not start, or evidence that needs repair or a look).
pub(crate) fn guide_applies(snapshot: &UiSnapshot) -> bool {
    !snapshot.location_unavailable()
        && !snapshot.game_folder_unavailable()
        && service_failure(snapshot).is_none()
        && !matches!(
            snapshot.installation,
            InstallationStatus::RepairRequired | InstallationStatus::Indeterminate
        )
}

/// A step as the guide sees it: how it stands, and whether its step mark
/// opens it again.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct GuidedRailRow {
    pub(crate) state: JourneyState,
    pub(crate) reachable: bool,
}

/// Which step the pane shows. View state only: never persisted
/// and never sent to the service. While the first check runs it shows the
/// page the setup record calls for ([`opening_step`]); once the check ends
/// a first run starts at the computer check and anyone else opens where the
/// installation stands. The step moves only when the user presses Continue,
/// Back, or a done step mark, while a setup job runs (the pane shows that
/// job's step), and when a job it showed succeeds: the next step opens.
/// It never passes the step the installation stands at, so no
/// step is skipped. The entered keys and choices live in their own inputs,
/// so moving never discards them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct GuidedCursor {
    current: SetupStep,
    /// The furthest step reached; every step before it was continued past.
    furthest: SetupStep,
    /// Whether the first settled audit placed the cursor yet.
    settled: bool,
    /// The step whose job the pane showed running, until its outcome lands.
    following: Option<SetupStep>,
}

impl Default for GuidedCursor {
    fn default() -> Self {
        Self::new()
    }
}

impl GuidedCursor {
    pub(crate) const fn new() -> Self {
        Self {
            current: SetupStep::Device,
            furthest: SetupStep::Device,
            settled: false,
            following: None,
        }
    }

    /// After Start over: back at Check your computer, with every later step
    /// to be continued through again, whatever the installation keeps
    /// (downloads, say).
    pub(crate) const fn restarted() -> Self {
        Self {
            current: SetupStep::Device,
            furthest: SetupStep::Device,
            settled: true,
            following: None,
        }
    }

    /// A cursor that has reached `step` (debug previews and tests).
    #[cfg(any(test, debug_assertions))]
    pub(crate) const fn at(step: SetupStep) -> Self {
        Self {
            current: step,
            furthest: step,
            settled: true,
            following: None,
        }
    }

    #[cfg(test)]
    pub(crate) const fn current(self) -> SetupStep {
        self.current
    }

    /// Follow the service's evidence: place the cursor once the first check
    /// settled, show a running setup job's step, open the next step once
    /// that job succeeded (a finished download opens Install), and
    /// never stay past the step the installation stands at (after Start
    /// over, say).
    pub(crate) fn settle(&mut self, snapshot: &UiSnapshot) {
        let checking = snapshot.operation.as_ref().is_some_and(|operation| {
            operation.phase == OperationPhase::HostAudit && !operation.status.is_terminal()
        });
        if snapshot.host != HostView::Supported || checking {
            return;
        }
        let standing = standing_step(snapshot);
        if !self.settled {
            self.settled = true;
            if !first_run(snapshot) {
                self.current = standing;
            }
        }
        if let Some(step) = running_step(snapshot) {
            self.current = step;
            self.following = Some(step);
        } else if let Some(step) = self.following {
            // The job ended; its outcome lands with the operation.
            match job_outcome(snapshot, step) {
                Some(OperationStatusView::Succeeded) => {
                    self.following = None;
                    if standing > step {
                        self.current = standing;
                    }
                }
                Some(status) if status.is_terminal() => self.following = None,
                Some(_) => {}
                None => self.following = None,
            }
        }
        if self.current > standing {
            self.current = standing;
        }
        self.furthest = self.furthest.min(standing).max(self.current);
    }

    /// The step shown for `snapshot`: never one behind a gate the snapshot
    /// does not pass (a computer check that failed again), and
    /// during the first check the one the setup record opens on.
    pub(crate) fn shown(self, snapshot: &UiSnapshot) -> SetupStep {
        match snapshot.host {
            HostView::Supported => running_step(snapshot).unwrap_or(self.current),
            HostView::Checking => opening_step(snapshot).unwrap_or(SetupStep::Device),
            HostView::Unsupported => SetupStep::Device,
        }
    }

    /// Why Continue is unavailable on the shown step, if it is.
    pub(crate) fn continue_blocker(
        self,
        snapshot: &UiSnapshot,
        location_pending: bool,
    ) -> Option<&'static str> {
        let standing = standing_step(snapshot);
        match self.shown(snapshot) {
            SetupStep::Device => match snapshot.host {
                HostView::Checking => Some("Wait for the check to finish."),
                HostView::Unsupported => Some("Install what's missing, then choose Check again."),
                HostView::Supported => None,
            },
            SetupStep::Setup => {
                if location_pending {
                    Some("Confirm the new install location first: Install here, or Cancel.")
                } else if !snapshot
                    .location()
                    .is_some_and(|location| location.available)
                {
                    Some("The launcher is still finding the install location.")
                } else {
                    match snapshot.installer_media() {
                        media if media.is_ready() => None,
                        InstallerMediaView::Invalid(_) => Some(
                            "That folder doesn't have both installers. Choose another folder, or download from Blizzard.",
                        ),
                        InstallerMediaView::Existing(_) => Some(
                            "That Diablo II folder can't be used. Choose another, or download from Blizzard.",
                        ),
                        _ => {
                            Some("Choose how to get Diablo II: download it, or use files you have.")
                        }
                    }
                }
            }
            SetupStep::Download if standing <= SetupStep::Download => {
                Some("Download everything first.")
            }
            SetupStep::InstallD2 if standing <= SetupStep::InstallD2 => {
                Some("Install Diablo II first.")
            }
            SetupStep::InstallPd2 if standing <= SetupStep::InstallPd2 => {
                Some("Install Project Diablo 2 first.")
            }
            SetupStep::Play => Some("Everything is ready."),
            SetupStep::Download | SetupStep::InstallD2 | SetupStep::InstallPd2 => None,
        }
    }

    /// Continue: move to the next step when the shown one allows it.
    pub(crate) fn advance(&mut self, snapshot: &UiSnapshot, location_pending: bool) -> bool {
        let shown = self.shown(snapshot);
        let Some(next) = shown.next() else {
            return false;
        };
        if self.continue_blocker(snapshot, location_pending).is_some()
            || next > standing_step(snapshot)
        {
            return false;
        }
        self.current = next;
        self.furthest = self.furthest.max(next);
        true
    }

    /// Back: the previous step. Nothing entered is cleared.
    pub(crate) fn back(&mut self, snapshot: &UiSnapshot) -> bool {
        if running_step(snapshot).is_some() {
            return false;
        }
        match self.shown(snapshot).previous() {
            Some(previous) => {
                self.current = previous;
                true
            }
            None => false,
        }
    }

    /// Open a step from its step mark: only one already reached.
    pub(crate) fn go_to(&mut self, step: SetupStep, snapshot: &UiSnapshot) -> bool {
        if !self.rail(snapshot, step).reachable {
            return false;
        }
        self.current = step;
        true
    }

    /// Show `step` because the service opened its editor (after "Stop and
    /// change"), when the installation has reached it.
    pub(crate) fn open(&mut self, step: SetupStep, snapshot: &UiSnapshot) -> bool {
        if step > standing_step(snapshot) {
            return false;
        }
        self.current = step;
        self.furthest = self.furthest.max(step);
        true
    }

    /// The step a press on `flow`'s mark opens: the latest of its steps
    /// already reached, when one can open now.
    pub(crate) fn mark_target(self, snapshot: &UiSnapshot, flow: FlowStep) -> Option<SetupStep> {
        flow.steps()
            .filter(|step| self.rail(snapshot, *step).reachable)
            .last()
    }

    /// How `step` stands for the guide, and whether it opens again.
    pub(crate) fn rail(self, snapshot: &UiSnapshot, step: SetupStep) -> GuidedRailRow {
        let shown = self.shown(snapshot);
        let standing = standing_step(snapshot);
        let supported = snapshot.host == HostView::Supported;
        // The record's page while the first check runs: the steps before it
        // were done when the record was written, and none opens until the
        // check ends.
        let opening = opening_step(snapshot).is_some();
        let state = if step == shown {
            if step == SetupStep::Device && snapshot.host == HostView::Unsupported {
                JourneyState::Blocked
            } else {
                JourneyState::Active
            }
        } else if (opening && step < shown)
            || (supported
                && match step {
                    // Steps the user continues past are done once passed.
                    SetupStep::Device | SetupStep::Setup => step < self.furthest,
                    // Steps with a result are done when their result holds.
                    _ => step < standing,
                })
        {
            JourneyState::Complete
        } else {
            JourneyState::Pending
        };
        let working = snapshot
            .operation
            .as_ref()
            .is_some_and(OperationView::owns_mutation);
        let reachable = step != shown
            && step <= self.furthest
            && !working
            && !opening
            && (step == SetupStep::Device || supported);
        GuidedRailRow { state, reachable }
    }
}

/// The action bar's main action for a step, and Back.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GuidedActions {
    pub(crate) primary: ActionView,
    pub(crate) back: Option<ActionView>,
}

/// Whether Continue from `from` starts the download: it leaves Location &
/// installers with a usable choice whose downloads are not all there yet,
/// and nothing runs.
pub(crate) fn continue_starts_download(from: SetupStep, snapshot: &UiSnapshot) -> bool {
    from == SetupStep::Setup
        && !snapshot.downloads_ready()
        && snapshot.installer_media.is_ready()
        && standing_step(snapshot) == SetupStep::Download
        && snapshot.game == GameView::Idle
        && !snapshot
            .operation
            .as_ref()
            .is_some_and(OperationView::owns_mutation)
}

/// What the step's main action is: Continue once its result holds, or the
/// step's own work (Download, Install Diablo II, Install Project Diablo 2)
/// with the same guards as the service.
pub(crate) fn guided_actions(
    cursor: GuidedCursor,
    snapshot: &UiSnapshot,
    location_pending: bool,
    keys_complete: bool,
    deck: &DeckPresentation,
) -> GuidedActions {
    let shown = cursor.shown(snapshot);
    let standing = standing_step(snapshot);
    let working = snapshot
        .operation
        .as_ref()
        .is_some_and(OperationView::owns_mutation);
    let blocked = |id, label| ActionView::disabled(id, label);
    let continue_action = || match cursor.continue_blocker(snapshot, location_pending) {
        None => ActionView::enabled(ActionId::GuidedContinue, "Continue"),
        Some(reason) => ActionView::disabled(ActionId::GuidedContinue, reason),
    };
    let opening = opening_step(snapshot).is_some();
    let primary = if working || opening {
        deck.primary.clone()
    } else {
        match shown {
            SetupStep::Device if snapshot.host == HostView::Unsupported => {
                ActionView::enabled(ActionId::CheckAgain, "Check again")
            }
            SetupStep::Device | SetupStep::Setup => continue_action(),
            SetupStep::Download if standing > SetupStep::Download => continue_action(),
            SetupStep::Download => {
                if snapshot.installer_media().is_ready() {
                    ActionView::enabled(ActionId::Download, "Download")
                } else {
                    blocked(ActionId::Download, "Choose how to get Diablo II first.")
                }
            }
            SetupStep::InstallD2 if standing > SetupStep::InstallD2 => continue_action(),
            SetupStep::InstallD2 => match snapshot.installation {
                InstallationStatus::StageInterrupted(_) => {
                    ActionView::enabled(ActionId::StartOver, "Start over")
                }
                InstallationStatus::NeedsBaseGame
                    if snapshot.installer_media().existing().is_some() =>
                {
                    if snapshot.installer_media().is_ready() {
                        ActionView::enabled(ActionId::ImportExisting, "Install Diablo II")
                    } else {
                        blocked(
                            ActionId::ImportExisting,
                            "Choose a usable Diablo II folder first.",
                        )
                    }
                }
                InstallationStatus::NeedsBaseGame | InstallationStatus::NeedsExpansion
                    if keys_complete =>
                {
                    ActionView::enabled(ActionId::AuthorizeAndInstall, "Install Diablo II")
                }
                InstallationStatus::NeedsBaseGame | InstallationStatus::NeedsExpansion => blocked(
                    ActionId::AuthorizeAndInstall,
                    "Enter both CD keys and accept the terms to continue.",
                ),
                _ => deck.primary.clone(),
            },
            SetupStep::InstallPd2 if standing > SetupStep::InstallPd2 => continue_action(),
            SetupStep::InstallPd2 => {
                ActionView::enabled(ActionId::InstallProjectDiablo, "Install Project Diablo 2")
            }
            SetupStep::Play => deck.primary.clone(),
        }
    };
    let back = (!working && !opening && shown != SetupStep::Play)
        .then(|| shown.previous())
        .flatten()
        .map(|_| ActionView::enabled(ActionId::GuidedBack, "Back"));
    GuidedActions { primary, back }
}

fn project_journey(snapshot: &UiSnapshot) -> Vec<JourneyItem> {
    let standing = standing_step(snapshot);
    SetupStep::ALL
        .into_iter()
        .map(|step| {
            // An unreachable location or game folder stops everything,
            // but not because of this computer.
            let elsewhere = snapshot.location_unavailable() || snapshot.game_folder_unavailable();
            let state = if snapshot.host == HostView::Unsupported && !elsewhere {
                JourneyState::Blocked
            } else if snapshot.host == HostView::Checking && step == SetupStep::Device {
                JourneyState::Active
            } else if snapshot.host != HostView::Supported {
                JourneyState::Pending
            } else if step == standing {
                JourneyState::Active
            } else if step < standing {
                JourneyState::Complete
            } else {
                JourneyState::Pending
            };
            let detail =
                if step == SetupStep::Setup && !snapshot.custom_installer_hosts().is_empty() {
                    "Custom download (unverified) or your own files"
                } else {
                    step.hint()
                };
            JourneyItem {
                label: step.name(),
                detail,
                state,
            }
        })
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum DomainEvent {
    FullSnapshot(Box<UiSnapshot>),
    HostChanged(HostView),
    AuthorizationChanged(AuthorizationView),
    InstallerMediaChanged(InstallerMediaView),
    InstallerMediaRejected(Option<&'static str>),
    FexSettingChanged(Option<FexSettingView>),
    InstallationChanged(InstallationStatus),
    UpdateChanged(UpdateStatus),
    OperationChanged(Option<OperationView>),
    GameChanged(GameView),
    GameNoticeChanged(Option<GameNoticeView>),
    JournalChanged(bool),
    LaunchPreflightChanged(bool),
    EvidenceChanged(Vec<EvidenceView>),
    SystemContractChanged(SystemContractView),
    SettingsChanged(LaunchSettingsView),
    DownloadSourcesChanged(Vec<DownloadSourceView>),
    ActiveJobChanged(Option<ActiveJobView>),
    EditingChanged(Option<SetupStep>),
    StopResultChanged(Option<StopResultView>),
    LocationChanged(Option<LocationView>),
    LocationErrorChanged(Option<&'static str>),
    OfficialLauncherTrustChanged(bool),
    GameSoftwareRenderingChanged(bool),
    GameLanguageChanged(Option<crate::game_language::GameLanguage>),
    EnvironmentReadyChanged(bool),
    DownloadsChanged(DownloadsView),
    GameFolderChanged(Option<GameFolderView>),
    ReportChanged(Option<ReportView>),
    SetupRecordChanged(SetupRecord),
    GameFilesOpenChanged(Option<FolderOpenView>),
    SavesPutAsideChanged(Option<SavesPutAsideView>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SequencedEvent {
    sequence: u64,
    event: DomainEvent,
}

impl SequencedEvent {
    pub(crate) fn new(sequence: u64, event: DomainEvent) -> Self {
        Self { sequence, event }
    }

    pub(crate) fn is_full_snapshot(&self) -> bool {
        matches!(self.event, DomainEvent::FullSnapshot(_))
    }

    /// The reason code when this event is the coordinator refusing the
    /// intent it was handling ([`REQUEST_NOT_STARTED`] evidence).
    pub(crate) fn refusal(&self) -> Option<&str> {
        match &self.event {
            DomainEvent::EvidenceChanged(evidence) => match evidence.as_slice() {
                [item] if item.label == REQUEST_NOT_STARTED => Some(&item.detail),
                _ => None,
            },
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReduceOutcome {
    Applied,
    IgnoredStale,
    SnapshotRequired,
}

pub(crate) struct UiReducer {
    snapshot: UiSnapshot,
    snapshot_required: bool,
}

impl UiReducer {
    pub(crate) fn new(snapshot: UiSnapshot) -> Self {
        Self {
            snapshot,
            snapshot_required: false,
        }
    }

    pub(crate) fn snapshot(&self) -> &UiSnapshot {
        &self.snapshot
    }

    pub(crate) fn reduce(&mut self, sequenced: SequencedEvent) -> ReduceOutcome {
        if sequenced.sequence <= self.snapshot.last_event_seq {
            return ReduceOutcome::IgnoredStale;
        }
        let is_full_snapshot = matches!(sequenced.event, DomainEvent::FullSnapshot(_));
        if (!is_full_snapshot && self.snapshot_required)
            || (!is_full_snapshot && sequenced.sequence != self.snapshot.last_event_seq + 1)
        {
            self.snapshot_required = true;
            return ReduceOutcome::SnapshotRequired;
        }

        match sequenced.event {
            DomainEvent::FullSnapshot(mut snapshot) => {
                snapshot.last_event_seq = sequenced.sequence;
                self.snapshot = *snapshot;
                self.snapshot_required = false;
            }
            DomainEvent::HostChanged(value) => self.snapshot.host = value,
            DomainEvent::AuthorizationChanged(value) => self.snapshot.authorization = value,
            DomainEvent::InstallerMediaChanged(value) => self.snapshot.installer_media = value,
            DomainEvent::FexSettingChanged(value) => self.snapshot.fex_setting = value,
            DomainEvent::InstallerMediaRejected(value) => {
                self.snapshot.installer_media_rejection = value;
            }
            DomainEvent::InstallationChanged(value) => self.snapshot.installation = value,
            DomainEvent::UpdateChanged(value) => self.snapshot.update = value,
            DomainEvent::OperationChanged(value) => self.snapshot.operation = value,
            DomainEvent::GameChanged(value) => self.snapshot.game = value,
            DomainEvent::GameNoticeChanged(value) => self.snapshot.game_notice = value,
            DomainEvent::JournalChanged(value) => self.snapshot.unresolved_journal = value,
            DomainEvent::LaunchPreflightChanged(value) => {
                self.snapshot.launch_preflight_passed = value;
            }
            DomainEvent::EvidenceChanged(value) => self.snapshot.evidence = value,
            DomainEvent::SystemContractChanged(value) => self.snapshot.system = value,
            DomainEvent::SettingsChanged(value) => self.snapshot.settings = value,
            DomainEvent::DownloadSourcesChanged(value) => self.snapshot.download_sources = value,
            DomainEvent::ActiveJobChanged(value) => self.snapshot.active_job = value,
            DomainEvent::EditingChanged(value) => self.snapshot.editing = value,
            DomainEvent::StopResultChanged(value) => self.snapshot.stop_result = value,
            DomainEvent::LocationChanged(value) => self.snapshot.location = value,
            DomainEvent::LocationErrorChanged(value) => self.snapshot.location_error = value,
            DomainEvent::DownloadsChanged(value) => self.snapshot.downloads = value,
            DomainEvent::GameFolderChanged(value) => self.snapshot.game_folder = value,
            DomainEvent::ReportChanged(value) => self.snapshot.report = value,
            DomainEvent::SetupRecordChanged(value) => self.snapshot.setup_record = value,
            DomainEvent::GameFilesOpenChanged(value) => self.snapshot.game_files_open = value,
            DomainEvent::SavesPutAsideChanged(value) => self.snapshot.saves_put_aside = value,
            DomainEvent::OfficialLauncherTrustChanged(value) => {
                self.snapshot.official_launcher_unverified = value;
            }
            DomainEvent::GameSoftwareRenderingChanged(value) => {
                self.snapshot.game_software_rendering = value;
            }
            DomainEvent::GameLanguageChanged(value) => self.snapshot.game_language = value,
            DomainEvent::EnvironmentReadyChanged(value) => {
                self.snapshot.environment_ready = value;
            }
        }
        self.snapshot.last_event_seq = sequenced.sequence;
        ReduceOutcome::Applied
    }
}

struct SecretBytes(Box<[u8]>);

impl SecretBytes {
    fn new(mut bytes: Vec<u8>) -> Result<Self, AuthorizationInputError> {
        if bytes.is_empty()
            || bytes.len() > 64
            || !bytes
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
        {
            bytes.zeroize();
            return Err(AuthorizationInputError::InvalidKey);
        }
        Ok(Self(bytes.into_boxed_slice()))
    }

    fn into_string(mut self) -> String {
        let bytes = std::mem::take(&mut self.0).into_vec();
        String::from_utf8(bytes).expect("product keys are constrained to ASCII")
    }
}

impl Drop for SecretBytes {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

pub(crate) struct AuthorizationInput {
    base_key: SecretBytes,
    expansion_key: SecretBytes,
    vendor_terms_accepted: bool,
}

impl AuthorizationInput {
    pub(crate) fn new(
        base_key: String,
        expansion_key: String,
        vendor_terms_accepted: bool,
    ) -> Result<Self, AuthorizationInputError> {
        Self::from_bytes(
            base_key.into_bytes(),
            expansion_key.into_bytes(),
            vendor_terms_accepted,
        )
    }

    /// The keys as raw bytes (the Control channel never makes them a
    /// `String`); each is wiped on every path.
    pub(crate) fn from_bytes(
        base_key: Vec<u8>,
        expansion_key: Vec<u8>,
        vendor_terms_accepted: bool,
    ) -> Result<Self, AuthorizationInputError> {
        // Convert both values before evaluating consent or either format so
        // every error path owns them in a wiping container.
        let base_key = SecretBytes::new(base_key);
        let expansion_key = SecretBytes::new(expansion_key);
        if !vendor_terms_accepted {
            return Err(AuthorizationInputError::ConsentRequired);
        }
        Ok(Self {
            base_key: base_key?,
            expansion_key: expansion_key?,
            vendor_terms_accepted,
        })
    }

    pub(crate) fn into_parts(self) -> (String, String, bool) {
        (
            self.base_key.into_string(),
            self.expansion_key.into_string(),
            self.vendor_terms_accepted,
        )
    }
}

impl fmt::Debug for AuthorizationInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthorizationInput")
            .field("base_key", &RedactedSecret)
            .field("expansion_key", &RedactedSecret)
            .field("vendor_terms_accepted", &self.vendor_terms_accepted)
            .finish()
    }
}

struct RedactedSecret;

impl fmt::Debug for RedactedSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AuthorizationInputError {
    InvalidKey,
    ConsentRequired,
}

pub(crate) enum Intent {
    Navigate(Destination),
    SelectInstallerMedia(PathBuf),
    UseBlizzardDownload,
    /// Open the editor of a step whose affordance is Change.
    EditStep(SetupStep),
    /// "Back to current step".
    CloseEditor,
    /// Stop the running install at a safe point, then open the step's
    /// editor. The row asks for confirmation before emitting it.
    StopAndChange(SetupStep),
    DismissStopResult,
    /// Hide the result card of the last operation that did not succeed.
    DismissResult,
    /// Check this computer again without restarting (missing
    /// prerequisites, or a service that could not establish evidence).
    CheckAgain,
    SaveDownloadSource(DownloadArtifact, String),
    ResetDownloadSource(DownloadArtifact),
    /// Step 4 with both CD keys: install Diablo II.
    AuthorizeAndInstall(AuthorizationInput),
    /// Continue the step the installation stands at, without keys.
    ContinueInstallation,
    /// Step 3: download and verify everything.
    DownloadAll,
    /// Stop the Download job and remove what the Launcher's own downloads
    /// made, for an import (the Android app).
    DiscardDownloads,
    /// The Control channel refused an `authorize` (its key check): keys an
    /// earlier one left waiting go too. Never sent by the app itself.
    ForgetWaitingKeys,
    /// Step 4 for a Diablo II the user already has, of any version.
    ImportExistingInstall(ImportModeView),
    /// Step 5: install Project Diablo 2.
    InstallProjectDiablo,
    /// Choose a Diablo II folder the user already has, of any version.
    SelectExistingInstall(PathBuf),
    /// Use the FEX at the path, or find it automatically (`None`); ARM64.
    SaveFex(Option<PathBuf>),
    /// Use the built-in FEX or a device FEX; ARM64.
    ChooseFexSource(FexSourceView),
    /// Download from Blizzard, unless a choice was already restored.
    UseDefaultInstallerSource,
    RequestSafeStop,
    ApplyUpdate,
    /// Play: the graphics driver, the D2GL graphics profile (the Android
    /// app's; none from the desktop), whether the game gets Box64's strict
    /// x86 memory order (the Android app's Strict CPU emulation), and the
    /// Android app's Detailed logging for its Wine (neither from the
    /// desktop).
    LaunchGame(GraphicsDriver, Option<GraphicsProfile>, bool, bool),
    /// Open the official PD2 launcher as a fallback.
    OpenOfficialLauncher,
    /// Open it after the user confirmed an unverified binary (the Android
    /// app's button is the confirmation), whether a game that it starts gets
    /// Box64's strict x86 memory order (the Android app's Strict CPU
    /// emulation), and the Android app's Detailed logging for its Wine and
    /// the game it starts (neither from the desktop).
    OpenOfficialLauncherConfirmed(bool, bool),
    /// A confirmed Start over.
    StartOver,
    /// Resolve an unavailable install location again ("Check again").
    RetryLocation,
    /// Install into `<parent>/pd2-launcher`, confirmed by the user: a fresh
    /// root, or a move of the current one.
    ChooseInstallLocation(PathBuf),
    /// Unattended `--install-dir`: the same, but only for a fresh root.
    UseFreshInstallLocation(PathBuf),
    /// Remove the installation a move left behind, confirmed by the user.
    RemovePreviousInstallation,
    ChangeSettings(SettingsChange),
    ExportDiagnostics,
    /// Open the saved report's folder again.
    RevealReport,
    /// Open the game folder (Settings → Game files).
    OpenGameFolder,
    DismissReport,
    /// Show the backup of characters put aside (Play's notice).
    OpenSavesBackup,
    DismissSavesPutAside,
    RequestFullSnapshot,
}

impl Intent {
    #[cfg(test)]
    pub(crate) fn history_label(&self) -> &'static str {
        match self {
            Self::Navigate(Destination::Operations) => "Open Operations Deck",
            Self::Navigate(Destination::Settings) => "Open Settings",
            Self::Navigate(Destination::Diagnostics) => "Open Diagnostics",
            Self::Navigate(Destination::About) => "Open About",
            Self::Navigate(Destination::Repair) => "Open Repair",
            Self::SelectInstallerMedia(_) => "Select D2 installer folder",
            Self::UseBlizzardDownload => "Download D2 installers from Blizzard",
            Self::EditStep(_) => "Change a setup step",
            Self::CloseEditor => "Back to current step",
            Self::StopAndChange(_) => "Stop the installation to change a step",
            Self::DismissStopResult => "Dismiss the stop result",
            Self::DismissResult => "Dismiss the result",
            Self::CheckAgain => "Check this computer again",
            Self::SaveDownloadSource(..) => "Save custom download source",
            Self::ResetDownloadSource(_) => "Reset download source to built-in",
            Self::AuthorizeAndInstall(_) => "Authorize and start Managed installation",
            Self::ContinueInstallation => "Continue Managed installation",
            Self::DownloadAll => "Download everything",
            Self::DiscardDownloads => "Remove the downloads for an import",
            Self::ForgetWaitingKeys => "Forget the waiting CD keys",
            Self::ImportExistingInstall(_) => "Import the existing Diablo II",
            Self::InstallProjectDiablo => "Install Project Diablo 2",
            Self::SelectExistingInstall(_) => "Choose an existing Diablo II folder",
            Self::SaveFex(_) => "Set where FEX is",
            Self::ChooseFexSource(_) => "Choose which FEX to use",
            Self::UseDefaultInstallerSource => "Use the default installer source",
            Self::RequestSafeStop => "Request safe stop",
            Self::ApplyUpdate => "Apply Available update",
            Self::LaunchGame(..) => "Launch Game client",
            Self::OpenOfficialLauncher => "Open official PD2 launcher",
            Self::OpenOfficialLauncherConfirmed(..) => "Open the unverified official PD2 launcher",
            Self::StartOver => "Start over",
            Self::RetryLocation => "Check the install location again",
            Self::ChooseInstallLocation(_) => "Choose the install location",
            Self::UseFreshInstallLocation(_) => "Use a fresh install location",
            Self::RemovePreviousInstallation => "Remove the old installation",
            Self::ChangeSettings(SettingsChange::Renderer(_)) => "Change next-launch renderer",
            Self::ChangeSettings(SettingsChange::SkipToBnet(_)) => {
                "Change next-launch online shortcut"
            }
            Self::ChangeSettings(SettingsChange::SoundInBackground(_)) => {
                "Change next-launch background sound"
            }
            Self::ChangeSettings(SettingsChange::DirectDrawRenderer(_)) => {
                "Change how DirectDraw draws"
            }
            Self::ChangeSettings(SettingsChange::OfficialLauncherSoftwareRendering(_)) => {
                "Change official launcher software rendering"
            }
            Self::ChangeSettings(SettingsChange::AppMenuEntry(_)) => "Change the app menu entry",
            Self::ChangeSettings(SettingsChange::UpdateChecks(_)) => {
                "Change Project Diablo 2's update checks"
            }
            Self::ChangeSettings(SettingsChange::InstallSource(_)) => {
                "Change where Blizzard's installers come from"
            }
            Self::ChangeSettings(SettingsChange::CustomArguments(_)) => {
                "Change next-launch custom start options"
            }
            Self::ChangeSettings(SettingsChange::GameProgram(_)) => {
                "Change the program Play starts"
            }
            Self::ChangeSettings(SettingsChange::LauncherProgram(_)) => {
                "Change the program the official PD2 launcher button opens"
            }
            Self::ChangeSettings(SettingsChange::LauncherCustomArguments(_)) => {
                "Change the official PD2 launcher's custom start options"
            }
            Self::ExportDiagnostics => "Save a redacted report",
            Self::RevealReport => "Open the report's folder",
            Self::OpenGameFolder => "Open the game folder",
            Self::DismissReport => "Dismiss the saved report",
            Self::OpenSavesBackup => "Open the backup of saved files",
            Self::DismissSavesPutAside => "Dismiss the saved files put aside",
            Self::RequestFullSnapshot => "Request full UI snapshot",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Destination {
    Operations,
    Settings,
    Diagnostics,
    /// What the launcher is, its version and what it is built with.
    About,
    /// The installation as the last check found it, Check again and Start
    /// over, reachable from Play.
    Repair,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BridgeGeneration(pub(crate) u64);

pub(crate) struct LauncherViewState {
    reducer: UiReducer,
    destination: Destination,
    _bridge_generation: BridgeGeneration,
}

impl LauncherViewState {
    pub(crate) fn new(snapshot: UiSnapshot, bridge_generation: BridgeGeneration) -> Self {
        Self {
            reducer: UiReducer::new(snapshot),
            destination: Destination::Operations,
            _bridge_generation: bridge_generation,
        }
    }

    pub(crate) fn navigate(&mut self, destination: Destination) {
        self.destination = destination;
    }

    pub(crate) fn destination(&self) -> Destination {
        self.destination
    }

    #[cfg(test)]
    pub(crate) fn bridge_generation(&self) -> BridgeGeneration {
        self._bridge_generation
    }

    fn snapshot(&self) -> &UiSnapshot {
        self.reducer.snapshot()
    }

    fn reduce(&mut self, event: SequencedEvent) -> ReduceOutcome {
        self.reducer.reduce(event)
    }
}

const INTENT_CAPACITY: usize = 32;
const EVENT_CAPACITY: usize = 64;

pub(crate) struct PresentationEndpoint {
    initial_snapshot: UiSnapshot,
    intent_sender: SyncSender<Intent>,
    event_receiver: Receiver<SequencedEvent>,
    generation: BridgeGeneration,
}

pub(crate) struct ServiceEndpoint {
    pub(crate) intent_receiver: Receiver<Intent>,
    pub(crate) event_sender: SyncSender<SequencedEvent>,
}

impl PresentationEndpoint {
    /// The initial snapshot and both channel ends, for a presentation
    /// without a window (the Launcher engine).
    pub(crate) fn into_parts(self) -> (UiSnapshot, SyncSender<Intent>, Receiver<SequencedEvent>) {
        (
            self.initial_snapshot,
            self.intent_sender,
            self.event_receiver,
        )
    }
}

pub(crate) fn service_bridge(
    initial_snapshot: UiSnapshot,
) -> (PresentationEndpoint, ServiceEndpoint) {
    let (intent_sender, intent_receiver) = sync_channel(INTENT_CAPACITY);
    let (event_sender, event_receiver) = sync_channel(EVENT_CAPACITY);
    (
        PresentationEndpoint {
            initial_snapshot,
            intent_sender,
            event_receiver,
            generation: BridgeGeneration(1),
        },
        ServiceEndpoint {
            intent_receiver,
            event_sender,
        },
    )
}

#[cfg(feature = "gui")]
actions!(operations_deck, [FocusNext, FocusPrevious]);

/// The most characters a CD key field keeps, so a too-long paste is still
/// reported with its length instead of silently cut to a valid one.
#[cfg(feature = "gui")]
const MAX_KEY_INPUT: usize = 64;
#[cfg(feature = "gui")]
actions!(
    secret_product_key,
    [Backspace, Delete, Left, Right, SelectAll, Home, End, Paste,]
);

#[cfg(feature = "gui")]
struct SecretInput {
    focus_handle: FocusHandle,
    content: Vec<u8>,
    /// What the field holds, for the Show and Paste buttons' names.
    name: SharedString,
    placeholder: SharedString,
    selected_range: Range<usize>,
    marked_range: Option<Range<usize>>,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    selecting: bool,
    selection_anchor: usize,
    /// Show the characters instead of masking them.
    revealed: bool,
    /// Tells this field's Show and Paste buttons apart from the other's.
    tab_order: usize,
}

/// The height of every single-line form control: text fields and the
/// buttons that sit beside them.
pub(crate) const FIELD_HEIGHT: f32 = 32.0;

/// What an empty CD key field asks for.
const KEY_PLACEHOLDER: &str = "26 letters and numbers";

/// Characters per displayed group of a CD key.
const KEY_GROUP: usize = 4;

/// Where content position `index` of a `len`-character key sits in its
/// grouped display (a space after every [`KEY_GROUP`] characters).
fn key_display_index(len: usize, index: usize) -> usize {
    match index {
        0 => 0,
        index if index < len => index + index / KEY_GROUP,
        _ => len + len.saturating_sub(1) / KEY_GROUP,
    }
}

/// The content position of grouped display position `index`.
fn key_content_index(len: usize, index: usize) -> usize {
    (index - index / (KEY_GROUP + 1)).min(len)
}

/// Keep only letters and digits, upper-cased: spaces and dashes people copy
/// with a key are not part of it.
fn normalize_key_input(text: &str) -> Vec<u8> {
    text.bytes()
        .filter(u8::is_ascii_alphanumeric)
        .map(|byte| byte.to_ascii_uppercase())
        .collect()
}

/// An Install Diablo II job failed. A stop is not a failure: the install
/// continues later with the same keys.
pub(crate) fn install_diablo_failed(snapshot: &UiSnapshot) -> bool {
    snapshot.operation.as_ref().is_some_and(|operation| {
        operation.job == Some(ActiveJobView::InstallBaseGame)
            && operation.status == OperationStatusView::Failed
    })
}

/// Whether the entered CD keys are done with after `snapshot`: Diablo II
/// and Lord of Destruction are in, so Blizzard's installers need no key any
/// more, or Install Diablo II has just failed (it had not in the snapshot
/// before, `failed_before`), when a key may have been wrong and a retry
/// asks for both again. Start over and an import forget them too.
pub(crate) fn keys_spent(snapshot: &UiSnapshot, failed_before: bool) -> bool {
    matches!(
        snapshot.installation,
        InstallationStatus::NeedsClient
            | InstallationStatus::NeedsRuntime
            | InstallationStatus::Ready
    ) || (install_diablo_failed(snapshot) && !failed_before)
}

/// Why a typed key cannot be right, in plain words: the installers' check
/// of a key for the `field` of `game`.
pub(crate) fn key_problem(
    game: &str,
    field: crate::blizzard::KeyField,
    typed: &[u8],
) -> Option<String> {
    use crate::blizzard::{KeyProblem, check_key};
    if typed.is_empty() {
        return None;
    }
    match check_key(field, typed) {
        Ok(_) => None,
        Err(KeyProblem::ExpansionKeyInBaseField) => Some(
            "This is a Lord of Destruction CD key. Type it in the Lord of Destruction field."
                .to_owned(),
        ),
        Err(KeyProblem::BaseKeyInExpansionField) => {
            Some("This is a Diablo II CD key. Type it in the Diablo II field.".to_owned())
        }
        Err(KeyProblem::WrongProduct) => Some(format!(
            "This is not a {game} CD key. Check the key: your Blizzard account shows it."
        )),
        Err(KeyProblem::OldKeyLength) => Some(format!(
            "This is a 16-character {game} CD key. Blizzard's installers take only 26-character keys: your Blizzard account shows the 26-character key for it."
        )),
        Err(KeyProblem::InvalidCharacter) => Some(format!(
            "{game} CD keys do not use A, I, L, O, Q, S, U, 0, 1, 3 or 5. Check the key."
        )),
        Err(_) => Some(format!(
            "{game} CD keys are 26 letters and numbers. This one has {}.",
            typed.len()
        )),
    }
}

#[cfg(feature = "gui")]
impl SecretInput {
    fn new(
        name: &'static str,
        tab_order: usize,
        initial_value: String,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut initial_value = initial_value;
        let content = normalize_key_input(&initial_value);
        initial_value.zeroize();
        let cursor = content.len();
        Self {
            // Tab order follows the page: a positive tab index would move
            // the field after every other control.
            focus_handle: cx.focus_handle().tab_stop(true),
            content,
            name: name.into(),
            placeholder: KEY_PLACEHOLDER.into(),
            selected_range: cursor..cursor,
            marked_range: None,
            last_layout: None,
            last_bounds: None,
            selecting: false,
            selection_anchor: 0,
            revealed: false,
            tab_order,
        }
    }

    /// A key the installers take in `field`.
    fn is_valid_for(&self, field: crate::blizzard::KeyField) -> bool {
        crate::blizzard::check_key(field, &self.content).is_ok()
    }

    fn len(&self) -> usize {
        self.content.len()
    }

    fn toggle_revealed(&mut self, cx: &mut Context<Self>) {
        self.revealed = !self.revealed;
        cx.notify();
    }

    /// Replace the whole key with the clipboard's text ("Paste" button).
    fn paste_replacing(&mut self, cx: &mut Context<Self>) {
        if let Some(mut text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let mut replacement = normalize_key_input(&text);
            text.zeroize();
            replacement.truncate(MAX_KEY_INPUT);
            self.content.zeroize();
            self.content = replacement;
            self.selected_range = self.content.len()..self.content.len();
            cx.notify();
        }
    }

    fn copy_value(&self) -> String {
        String::from_utf8(self.content.clone()).expect("product key input accepts ASCII only")
    }

    /// Wipe the key and leave the field empty and masked.
    fn forget(&mut self, cx: &mut Context<Self>) {
        self.content.zeroize();
        self.content = Vec::new();
        self.selected_range = 0..0;
        self.marked_range = None;
        self.revealed = false;
        cx.notify();
    }

    fn restore(&mut self, mut value: String, cx: &mut Context<Self>) {
        self.content.zeroize();
        self.content = normalize_key_input(&value);
        value.zeroize();
        self.selected_range = self.content.len()..self.content.len();
        cx.notify();
    }

    fn cursor(&self) -> usize {
        self.selected_range.end
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let offset = offset.min(self.content.len());
        self.selected_range = offset..offset;
        cx.notify();
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        let target = if self.selected_range.is_empty() {
            self.selected_range.start.saturating_sub(1)
        } else {
            self.selected_range.start
        };
        self.move_to(target, cx);
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        let target = if self.selected_range.is_empty() {
            (self.selected_range.end + 1).min(self.content.len())
        } else {
            self.selected_range.end
        };
        self.move_to(target, cx);
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.selected_range = 0..self.content.len();
        cx.notify();
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() && self.selected_range.start > 0 {
            self.selected_range.start -= 1;
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() && self.selected_range.end < self.content.len() {
            self.selected_range.end += 1;
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(mut text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace_text_in_range(None, &text, window, cx);
            text.zeroize();
        }
    }

    fn index_at(&self, position: Point<Pixels>) -> usize {
        let Some(bounds) = self.last_bounds else {
            return 0;
        };
        let Some(line) = self.last_layout.as_ref() else {
            return 0;
        };
        if position.x <= bounds.left() {
            return 0;
        }
        if position.x >= bounds.right() {
            return self.content.len();
        }
        key_content_index(
            self.content.len(),
            line.closest_index_for_x(position.x - bounds.left()),
        )
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.selecting = true;
        self.selection_anchor = self.index_at(event.position);
        self.move_to(self.selection_anchor, cx);
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.selecting {
            let index = self.index_at(event.position);
            self.selected_range =
                self.selection_anchor.min(index)..self.selection_anchor.max(index);
            cx.notify();
        }
    }

    fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.selecting = false;
    }
}

#[cfg(feature = "gui")]
impl Drop for SecretInput {
    fn drop(&mut self) {
        // `Vec::zeroize` clears the initialized bytes and the spare capacity,
        // including bytes left behind by edits and deletions.
        self.content.zeroize();
    }
}

#[cfg(feature = "gui")]
impl EntityInputHandler for SecretInput {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = range.start.min(self.content.len())..range.end.min(self.content.len());
        actual_range.replace(range.clone());
        Some(String::from_utf8_lossy(&self.content[range]).into_owned())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.selected_range.clone(),
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range.clone()
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range
            .or_else(|| self.marked_range.clone())
            .unwrap_or_else(|| self.selected_range.clone());
        let range = range.start.min(self.content.len())..range.end.min(self.content.len());
        let available = MAX_KEY_INPUT.saturating_sub(self.content.len() - range.len());
        let mut replacement = normalize_key_input(new_text);
        replacement.truncate(available);
        let cursor = range.start + replacement.len();
        self.content.splice(range, replacement);
        self.selected_range = cursor..cursor;
        self.marked_range = None;
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        _: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.replace_text_in_range(range, new_text, window, cx);
        self.marked_range = None;
    }

    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let line = self.last_layout.as_ref()?;
        let len = self.content.len();
        let range = range.start.min(len)..range.end.min(len);
        Some(Bounds::from_corners(
            point(
                bounds.left() + line.x_for_index(key_display_index(len, range.start)),
                bounds.top(),
            ),
            point(
                bounds.left() + line.x_for_index(key_display_index(len, range.end)),
                bounds.bottom(),
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.index_at(point))
    }
}

#[cfg(feature = "gui")]
struct SecretTextElement {
    input: Entity<SecretInput>,
}

#[cfg(feature = "gui")]
struct SecretPrepaint {
    line: Option<ShapedLine>,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
}

#[cfg(feature = "gui")]
impl IntoElement for SecretTextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

#[cfg(feature = "gui")]
impl Element for SecretTextElement {
    type RequestLayoutState = ();
    type PrepaintState = SecretPrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui_kit::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui_kit::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let empty = input.content.is_empty();
        let len = input.content.len();
        // Grouped in fours; masked unless the user chose Show.
        let display: SharedString = if empty {
            input.placeholder.clone()
        } else {
            let mut grouped = String::with_capacity(len + len / KEY_GROUP);
            for (index, byte) in input.content.iter().enumerate() {
                if index > 0 && index % KEY_GROUP == 0 {
                    grouped.push(' ');
                }
                grouped.push(if input.revealed {
                    char::from(*byte)
                } else {
                    '*'
                });
            }
            grouped.into()
        };
        let at = |index: usize| {
            if empty {
                0
            } else {
                key_display_index(len, index)
            }
        };
        let text_color = if empty {
            theme::field_colors().placeholder
        } else {
            window.text_style().color
        };
        let run = TextRun {
            len: display.len(),
            font: window.text_style().font(),
            color: text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let font_size = window.text_style().font_size.to_pixels(window.rem_size());
        let line = window
            .text_system()
            .shape_line(display, font_size, &[run], None);
        let cursor_x = line.x_for_index(at(input.cursor()));
        let (selection, cursor) = if input.selected_range.is_empty() {
            (
                None,
                Some(fill(
                    Bounds::new(
                        point(bounds.left() + cursor_x, bounds.top()),
                        size(px(2.0), bounds.size.height),
                    ),
                    theme::field_colors().focus,
                )),
            )
        } else {
            (
                Some(fill(
                    Bounds::from_corners(
                        point(
                            bounds.left() + line.x_for_index(at(input.selected_range.start)),
                            bounds.top(),
                        ),
                        point(
                            bounds.left() + line.x_for_index(at(input.selected_range.end)),
                            bounds.bottom(),
                        ),
                    ),
                    theme::field_colors().selection,
                )),
                None,
            )
        };
        SecretPrepaint {
            line: Some(line),
            cursor,
            selection,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui_kit::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        if let Some(selection) = prepaint.selection.take() {
            window.paint_quad(selection);
        }
        let line = prepaint.line.take().expect("secret line was shaped");
        line.paint(
            bounds.origin,
            window.line_height(),
            gpui_kit::TextAlign::Left,
            None,
            window,
            cx,
        )
        .expect("secret field text should paint");
        if focus_handle.is_focused(window)
            && let Some(cursor) = prepaint.cursor.take()
        {
            window.paint_quad(cursor);
        }
        self.input.update(cx, |input, _| {
            input.last_layout = Some(line);
            input.last_bounds = Some(bounds);
        });
    }
}

#[cfg(feature = "gui")]
impl Render for SecretInput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme::field_colors();
        div()
            .id(("secret-input", cx.entity_id().as_u64()))
            .key_context("SecretProductKey")
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::paste))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .h(px(FIELD_HEIGHT))
            .w_full()
            .flex()
            .items_center()
            .gap_1()
            .pl_3()
            .pr_1()
            .rounded_md()
            .border_1()
            .border_color(colors.edge)
            .bg(colors.fill)
            .text_sm()
            .text_color(colors.text)
            .focus(|field| field.border_color(colors.focus))
            // The text is clipped to its own box, so a long placeholder or
            // key never runs under the buttons.
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .child(SecretTextElement { input: cx.entity() }),
            )
            .child(
                Button::new(("reveal-key", self.tab_order))
                    .label(if self.revealed { "Hide" } else { "Show" })
                    .accessibility_label(if self.revealed {
                        format!("Hide the {}", self.name)
                    } else {
                        format!("Show the {}", self.name)
                    })
                    .ghost()
                    .small()
                    .on_click(cx.listener(|input, _, _, cx| input.toggle_revealed(cx))),
            )
            .child(
                Button::new(("paste-key", self.tab_order))
                    .label("Paste")
                    .accessibility_label(format!("Paste the {}", self.name))
                    .ghost()
                    .small()
                    .on_click(cx.listener(|input, _, _, cx| input.paste_replacing(cx))),
            )
    }
}

#[cfg(feature = "gui")]
impl Focusable for SecretInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

#[cfg(feature = "gui")]
struct LauncherView {
    state: LauncherViewState,
    intent_sender: SyncSender<Intent>,
    event_receiver: Receiver<SequencedEvent>,
    _event_observer: Task<()>,
    focus_handle: FocusHandle,
    base_key: Entity<SecretInput>,
    expansion_key: Entity<SecretInput>,
    _secret_observers: Vec<Subscription>,
    installer_picker: Option<Task<()>>,
    location_picker: Option<Task<()>>,
    /// The file picker open for Settings → FEX.
    fex_picker: Option<Task<()>>,
    /// Why the FEX file picker could not be used.
    pub(crate) fex_picker_problem: Option<&'static str>,
    /// A picked parent folder awaiting "Install here".
    pending_location: Option<PathBuf>,
    /// Editable URL field per download source, in `DownloadArtifact::ALL`
    /// order, and the effective URL each field was last synchronized to.
    source_inputs: Vec<(DownloadArtifact, Entity<InputState>)>,
    source_synced: Vec<Option<String>>,
    consent: bool,
    /// The last snapshot showed a failed Install Diablo II, whose keys were
    /// forgotten when it first did.
    install_failure_seen: bool,
    /// An Install Diablo II this window started runs: once it succeeds,
    /// Install Project Diablo 2 follows without a press.
    pd2_hand_on: bool,
    /// The install's plan, fixed while its jobs run so its one figure keeps
    /// one scale through both.
    install_plan: Option<InstallPlan>,
    /// The check after a finished update, until it says ([`recheck`]).
    update_recheck: Option<Recheck>,
    /// Show details: the running step's own lines are unfolded.
    details_open: bool,
    /// The graphics card the window runs on, for the facts under Play.
    gpu: Option<String>,
    /// The operation the last snapshot showed, to tell a new outcome.
    seen_operation: Option<OperationView>,
    /// Diagnostics' Advanced group (download sources) is expanded.
    advanced_open: bool,
    /// A "Stop and change" awaiting the user's confirmation.
    confirm_stop: Option<SetupStep>,
    /// A Start over awaiting the user's confirmation.
    confirm_start_over: bool,
    /// Removing the previous installation awaiting confirmation.
    confirm_remove_previous: bool,
    /// Opening an unverified official launcher awaiting confirmation.
    confirm_official_launcher: bool,
    /// Closing the window while work runs, awaiting confirmation.
    confirm_close: bool,
    /// "Stop and close" was chosen: the window closes once work stopped.
    closing: bool,
    /// The left button went down on the title bar's empty area: the next
    /// pointer move hands the window to the compositor to move it.
    title_drag: bool,
    /// Window decorations a test renders with instead of the platform's.
    #[cfg(test)]
    chrome_override: Option<page::WindowChrome>,
    /// The user chose "I already have the installer files" before picking
    /// a folder.
    prefers_folder: bool,
    /// The user chose "I already have Diablo II" before picking a folder.
    prefers_existing: bool,
    existing_picker: Option<Task<()>>,
    /// The progress log version and timer second last drawn.
    progress_log_seen: (u64, Option<u64>),
    /// Settings → FEX: its path, typed by the user.
    fex_input: Entity<InputState>,
    /// How Install Diablo II uses an existing installation.
    import_mode: ImportModeView,
    /// The guided first run's step, until Install is pressed. Never set in
    /// unattended mode, which drives the service directly.
    guide: Option<GuidedCursor>,
    delivery_alert: Option<&'static str>,
    unattended_authorization: Option<AuthorizationInput>,
    /// The setup jobs unattended mode asked for, in order; each at most once.
    unattended_requested: Vec<ActiveJobView>,
    /// `--existing-install-mode`: how an existing installation is used.
    unattended_import_mode: ImportModeView,
    /// A step's job finished but the installation did not move on.
    unattended_stalled: bool,
    /// Started with `--unattended-install`.
    unattended: bool,
    /// `--start-over`: Start over an interrupted installation once.
    unattended_start_over: bool,
    /// `--install-dir`: the parent of the fresh root to install into.
    unattended_install_dir: Option<PathBuf>,
    unattended_location_requested: bool,
    unattended_start_over_requested: bool,
    unattended_after_ready: AfterReady,
    /// `--save-report`: where saving the report and closing stands.
    report_cli: ReportCli,
    snapshot_requested: bool,
    _service_runtime: Box<dyn Send>,
}

/// Where `--save-report` stands.
#[cfg(feature = "gui")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReportCli {
    Off,
    /// Waiting for the startup check to settle.
    Waiting,
    Requested,
    Finished,
}

#[cfg(feature = "gui")]
impl LauncherView {
    fn new(
        endpoint: PresentationEndpoint,
        service_runtime: Box<dyn Send>,
        automation: Option<AutomationRequest>,
        startup: StartupInputs,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (base_value, expansion_value, startup_installer_source) = startup.into_parts();
        let guide = automation.is_none().then(GuidedCursor::new);
        let unattended_start_over = automation
            .as_ref()
            .is_some_and(AutomationRequest::start_over);
        let unattended_install_dir = automation
            .as_ref()
            .and_then(AutomationRequest::install_dir)
            .cloned();
        let unattended = automation.is_some();
        let unattended_import_mode = automation
            .as_ref()
            .map_or(ImportModeView::Copy, AutomationRequest::import_mode);
        let (installer_source, unattended_authorization, unattended_after_ready) = match automation
        {
            Some(request) => {
                let (source, authorization, after_ready) = request.into_parts();
                (Ok(Some(source)), authorization, after_ready)
            }
            None => (startup_installer_source, None, AfterReady::Nothing),
        };
        let source_inputs = DownloadArtifact::ALL
            .into_iter()
            .map(|artifact| {
                let input =
                    cx.new(|cx| InputState::new(window, cx).placeholder(artifact.builtin_url()));
                (artifact, input)
            })
            .collect::<Vec<_>>();
        let fex_input = cx.new(|cx| InputState::new(window, cx).placeholder("/usr/bin/FEX"));
        let base_key = cx.new(move |cx| SecretInput::new("Diablo II CD key", 0, base_value, cx));
        let expansion_key = cx
            .new(move |cx| SecretInput::new("Lord of Destruction CD key", 1, expansion_value, cx));
        let secret_observers = vec![
            cx.observe(&base_key, |_, _, cx| cx.notify()),
            cx.observe(&expansion_key, |_, _, cx| cx.notify()),
        ];
        let event_observer = cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                let keep_observing = view
                    .update(cx, |view, cx| view.drain_event_batch(cx))
                    .unwrap_or(false);
                if !keep_observing {
                    break;
                }
            }
        });
        // Downloading from Blizzard is the default. Unattended mode always
        // names its source explicitly, so only the interactive screen starts
        // with this choice made.
        let delivery_alert = match installer_source {
            // An unknown D2_INSTALLER_SOURCE is reported, never guessed.
            Err(_) => Some(InvalidInstallerSource::MESSAGE),
            Ok(source) => Some(source).and_then(|source| {
                let intent = match source {
                    Some(StartupInstallerSource::Directory(path)) => {
                        Intent::SelectInstallerMedia(path)
                    }
                    Some(StartupInstallerSource::BlizzardDownload) => Intent::UseBlizzardDownload,
                    Some(StartupInstallerSource::ExistingInstall(path)) => {
                        Intent::SelectExistingInstall(path)
                    }
                    // A choice the last Download was made for is kept.
                    None => Intent::UseDefaultInstallerSource,
                };
                match endpoint.intent_sender.try_send(intent) {
                    Ok(()) => None,
                    Err(TrySendError::Full(_)) => {
                        Some("The Launcher is busy. Choose the installer folder manually.")
                    }
                    Err(TrySendError::Disconnected(_)) => Some(
                        "The Launcher service is unavailable. Restart the Launcher to continue.",
                    ),
                }
            }),
        };
        #[allow(unused_mut)]
        let mut view = Self {
            state: LauncherViewState::new(endpoint.initial_snapshot, endpoint.generation),
            intent_sender: endpoint.intent_sender,
            event_receiver: endpoint.event_receiver,
            _event_observer: event_observer,
            focus_handle: cx.focus_handle(),
            base_key,
            expansion_key,
            _secret_observers: secret_observers,
            installer_picker: None,
            location_picker: None,
            fex_picker: None,
            fex_picker_problem: None,
            pending_location: None,
            source_synced: vec![None; source_inputs.len()],
            source_inputs,
            consent: false,
            install_failure_seen: false,
            pd2_hand_on: false,
            install_plan: None,
            update_recheck: None,
            details_open: false,
            gpu: None,
            seen_operation: None,
            advanced_open: false,
            confirm_stop: None,
            confirm_start_over: false,
            confirm_remove_previous: false,
            confirm_official_launcher: false,
            confirm_close: false,
            closing: false,
            title_drag: false,
            #[cfg(test)]
            chrome_override: None,
            prefers_folder: false,
            prefers_existing: false,
            existing_picker: None,
            progress_log_seen: (0, None),
            fex_input,
            import_mode: ImportModeView::Copy,
            guide,
            delivery_alert,
            unattended_authorization,
            unattended_requested: Vec::new(),
            unattended_import_mode,
            unattended_stalled: false,
            unattended,
            unattended_start_over,
            unattended_start_over_requested: false,
            unattended_install_dir,
            unattended_location_requested: false,
            unattended_after_ready,
            report_cli: if crate::automation::report_requested() {
                ReportCli::Waiting
            } else {
                ReportCli::Off
            },
            snapshot_requested: false,
            _service_runtime: service_runtime,
        };
        #[cfg(debug_assertions)]
        view.apply_preview(cx);
        view
    }

    /// Debug builds only: the view-local state of a `PD2_UI_PREVIEW` screen.
    #[cfg(debug_assertions)]
    fn apply_preview(&mut self, cx: &mut Context<Self>) {
        let Some(preview) = preview::view() else {
            return;
        };
        self.base_key.update(cx, |input, cx| {
            input.restore(preview.base_key.to_owned(), cx)
        });
        self.expansion_key.update(cx, |input, cx| {
            input.restore(preview.expansion_key.to_owned(), cx)
        });
        self.consent = preview.consent;
        self.guide = preview.guided.map(GuidedCursor::at);
        self.import_mode = preview.import_mode;
        self.confirm_close = preview.confirm_close;
        self.confirm_start_over = preview.confirm_start_over;
        if preview.rechecking {
            self.update_recheck = Some(Recheck::Checking);
        }
        self.state.navigate(preview.destination);
    }

    fn drain_event_batch(&mut self, cx: &mut Context<Self>) -> bool {
        let mut changed = false;
        let mut connected = true;
        for _ in 0..EVENT_CAPACITY {
            let event = match self.event_receiver.try_recv() {
                Ok(event) => event,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.delivery_alert = Some(
                        "The Launcher service stopped sending status. Restart the Launcher before starting new work.",
                    );
                    changed = true;
                    connected = false;
                    break;
                }
            };
            let full_snapshot = matches!(&event.event, DomainEvent::FullSnapshot(_));
            match self.state.reduce(event) {
                ReduceOutcome::Applied => {
                    if full_snapshot {
                        self.snapshot_requested = false;
                    }
                    changed = true;
                }
                ReduceOutcome::IgnoredStale => {}
                ReduceOutcome::SnapshotRequired if !self.snapshot_requested => {
                    self.snapshot_requested = self.emit(Intent::RequestFullSnapshot);
                    changed = true;
                }
                ReduceOutcome::SnapshotRequired => {}
            }
        }
        if changed {
            self.follow_service_state(cx);
        }
        let automation_changed = self.drive_unattended() | self.drive_report_cli(cx);
        // While a step runs, its log and timer redraw without a service
        // event: when a line arrives, and at least every second.
        let log_changed = self.state.snapshot().active_job.is_some() && {
            let version = crate::activity::recent_version();
            let second = crate::activity::current_phase().map(|(_, elapsed)| elapsed.as_secs());
            let changed = (version, second) != self.progress_log_seen;
            self.progress_log_seen = (version, second);
            changed
        };
        if changed || automation_changed || log_changed {
            cx.notify();
        }
        if self.closing && close_decision(self.state.snapshot(), true) == CloseDecision::Close {
            // "Stop and close": the work stopped at its safe point.
            cx.quit();
            return false;
        }
        if self.unattended_stopped_at_interruption()
            || self.unattended_location_refused()
            || self.unattended_official_launcher_refused()
            || self.unattended_install_failed()
        {
            cx.quit();
            return false;
        }
        connected
    }

    /// `--save-report`: once the startup check settled, save the report the
    /// way Diagnostics → Save report does; print where it is and close a few
    /// seconds after its folder was opened.
    fn drive_report_cli(&mut self, cx: &mut Context<Self>) -> bool {
        let snapshot = self.state.snapshot().clone();
        match self.report_cli {
            ReportCli::Off | ReportCli::Finished => false,
            ReportCli::Waiting => {
                let checking = snapshot.host() == HostView::Checking
                    || snapshot
                        .operation
                        .as_ref()
                        .is_some_and(|operation| operation.status == OperationStatusView::Running);
                if checking {
                    return false;
                }
                self.navigate(Destination::Diagnostics, cx);
                self.activate_action(ActionId::ExportDiagnostics, cx);
                self.report_cli = ReportCli::Requested;
                true
            }
            ReportCli::Requested => {
                match snapshot.report() {
                    Some(ReportView::Saved {
                        path,
                        opened: Some(opened),
                    }) => {
                        println!("{}", path.display());
                        if !opened {
                            eprintln!(
                                "The report's folder could not be opened automatically. The report is at the path above."
                            );
                        }
                    }
                    Some(ReportView::Failed(code)) => {
                        eprintln!("The report could not be saved (code: {code}).");
                        crate::automation::set_unattended_exit_status(
                            crate::automation::REPORT_FAILED_EXIT_STATUS,
                        );
                    }
                    _ => return false,
                }
                self.report_cli = ReportCli::Finished;
                // The confirmation stays in view a moment before closing.
                cx.spawn(async move |_, cx| {
                    cx.background_executor().timer(Duration::from_secs(4)).await;
                    cx.update(|cx| cx.quit());
                })
                .detach();
                true
            }
        }
    }

    /// Keep the step cursor on the service's evidence, open the step the
    /// service asks for after a "Stop and change", and forget the CD keys
    /// once they are done with.
    fn follow_service_state(&mut self, cx: &mut Context<Self>) {
        let snapshot = self.state.snapshot().clone();
        if keys_spent(&snapshot, self.install_failure_seen) {
            self.forget_keys(cx);
        }
        self.install_failure_seen = install_diablo_failed(&snapshot);
        self.hand_on(&snapshot);
        self.follow_install_plan(&snapshot);
        self.update_recheck = recheck(self.update_recheck, self.seen_operation.as_ref(), &snapshot);
        self.seen_operation = snapshot.operation.clone();
        let Some(cursor) = self.guide.as_mut() else {
            return;
        };
        cursor.settle(&snapshot);
        if let Some(step) = snapshot.editing() {
            cursor.open(step, &snapshot);
            self.emit(Intent::CloseEditor);
        }
    }

    /// Install Diablo II hands on to Install Project Diablo 2:
    /// armed while an Install Diablo II runs in this window, it sends
    /// Install Project Diablo 2 once, when that job succeeded. A stop or a
    /// failure disarms it, and a restart never starts anything. Unattended
    /// mode runs the steps itself.
    fn hand_on(&mut self, snapshot: &UiSnapshot) {
        if self.unattended {
            return;
        }
        if running_step(snapshot) == Some(SetupStep::InstallD2) {
            self.pd2_hand_on = true;
            return;
        }
        let ended = snapshot.operation.as_ref().is_some_and(|operation| {
            operation.job == Some(ActiveJobView::InstallBaseGame) && operation.status.is_terminal()
        });
        if !self.pd2_hand_on || !ended {
            return;
        }
        self.pd2_hand_on = false;
        if hands_on_to_project_diablo(snapshot) {
            self.emit(Intent::InstallProjectDiablo);
        }
    }

    /// Fix the install's plan when one of its jobs starts, keep it through
    /// the hand-on from Install Diablo II to Install Project Diablo 2, and
    /// let it go once nothing of the install runs.
    fn follow_install_plan(&mut self, snapshot: &UiSnapshot) {
        if matches!(
            running_step(snapshot),
            Some(SetupStep::InstallD2 | SetupStep::InstallPd2)
        ) {
            let in_place = self.import_mode == ImportModeView::InPlace;
            self.install_plan
                .get_or_insert_with(|| InstallPlan::for_snapshot(snapshot, in_place));
        } else if snapshot.active_job.is_none()
            && !(self.pd2_hand_on || hands_on_to_project_diablo(snapshot))
        {
            self.install_plan = None;
        }
    }

    /// `--open-official-launcher-after-ready` never runs an official
    /// launcher that is not verified: running it needs the user's
    /// confirmation, which has no unattended form.
    fn unattended_official_launcher_refused(&mut self) -> bool {
        if after_ready_step(self.unattended_after_ready, self.state.snapshot())
            != AfterReadyStep::RefuseUnverifiedOfficialLauncher
        {
            return false;
        }
        eprintln!(
            "The official PD2 launcher was not opened: PD2Launcher.exe matches neither the file list this launcher accepted nor a copy it recorded after the official launcher last ran, so running it needs your confirmation. Open the Launcher and choose \"Open official PD2 launcher\" to review and confirm it. The installation is Ready and was not changed."
        );
        crate::automation::set_unattended_exit_status(
            crate::automation::OFFICIAL_LAUNCHER_EXIT_STATUS,
        );
        self.unattended_after_ready = AfterReady::Nothing;
        true
    }

    /// Unattended mode has no back navigation: an interrupted installation
    /// without `--start-over` ends the run with instructions.
    fn unattended_stopped_at_interruption(&mut self) -> bool {
        let snapshot = self.state.snapshot();
        let InstallationStatus::StageInterrupted(stage) = snapshot.installation() else {
            return false;
        };
        if self.unattended_authorization.is_none() && self.unattended_requested.is_empty() {
            return false;
        }
        if self.unattended_start_over
            || snapshot
                .operation
                .as_ref()
                .is_some_and(OperationView::owns_mutation)
        {
            return false;
        }
        let step = match stage {
            InterruptedStage::Prefix => "while the Wine prefix was being created",
            InterruptedStage::BaseGame => "while the Diablo II installer ran",
            InterruptedStage::Expansion => "while the Lord of Destruction installer ran",
        };
        eprintln!(
            "The installation was interrupted {step}. Start over removes the Managed prefix and installs again; your characters, downloads and the current Wine runtime are kept. Open the Launcher and choose Start over, or run unattended again with --start-over."
        );
        crate::automation::set_unattended_exit_status(crate::automation::INTERRUPTED_EXIT_STATUS);
        true
    }

    /// An unattended install that failed ends the run with the same plain
    /// explanation the result card shows, and its stable code.
    fn unattended_install_failed(&mut self) -> bool {
        // The Diablo II folder used in place is gone: nothing can run.
        if self.unattended
            && let Some(folder) = self
                .state
                .snapshot()
                .game_folder()
                .filter(|folder| !folder.available)
        {
            eprintln!(
                "The Diablo II folder this installation uses where it is can't be found at {} or isn't the same folder anymore. Put it back, or open the Launcher and choose Start over. (code: game-folder-unavailable)",
                folder.path.display()
            );
            crate::automation::set_unattended_exit_status(
                crate::automation::INSTALL_FAILED_EXIT_STATUS,
            );
            return true;
        }
        // `--existing-install` named a folder that can't be used: say why.
        if self.unattended
            && let Some(problem) = self
                .state
                .snapshot()
                .installer_media()
                .existing()
                .and_then(|existing| existing.problem)
        {
            eprintln!("{} (code: existing-install-invalid)", problem.message());
            crate::automation::set_unattended_exit_status(
                crate::automation::INSTALL_FAILED_EXIT_STATUS,
            );
            return true;
        }
        if self.unattended_requested.is_empty() {
            return false;
        }
        if self.unattended_stalled {
            eprintln!(
                "A setup step finished, but the installation did not move on. Open the Launcher and choose Diagnostics → Save report to ask for help. (code: step-did-not-advance)"
            );
            crate::automation::set_unattended_exit_status(
                crate::automation::INSTALL_FAILED_EXIT_STATUS,
            );
            return true;
        }
        let snapshot = self.state.snapshot();
        let Some(operation) = snapshot.operation.as_ref().filter(|operation| {
            operation.job.is_some_and(ActiveJobView::is_setup)
                && operation.status == OperationStatusView::Failed
        }) else {
            return false;
        };
        let code = operation.failure.unwrap_or("managed-install-failed");
        let message = result_card(snapshot).map_or(
            "The installation stopped. Open the Launcher and choose Diagnostics → Save report to ask for help.",
            |card| card.message,
        );
        eprintln!("{message} (code: {code})");
        crate::automation::set_unattended_exit_status(
            crate::automation::INSTALL_FAILED_EXIT_STATUS,
        );
        true
    }

    /// `--install-dir` names a root that is not (yet) the current one.
    fn unattended_location_pending(&self) -> bool {
        self.unattended_install_dir.as_ref().is_some_and(|parent| {
            !self.state.snapshot().location().is_some_and(|location| {
                location.available
                    && location.path == parent.join(crate::location::INSTALL_DIRECTORY)
            })
        })
    }

    /// The fresh location `--install-dir` asked for was refused: the run
    /// ends instead of installing anywhere else.
    fn unattended_location_refused(&mut self) -> bool {
        let snapshot = self.state.snapshot();
        let Some(code) = snapshot.location_error() else {
            return false;
        };
        if !self.unattended_location_requested
            || !self.unattended_location_pending()
            || snapshot
                .operation
                .as_ref()
                .is_some_and(OperationView::owns_mutation)
        {
            return false;
        }
        eprintln!(
            "--install-dir was not used ({code}). It must name a folder where the launcher can create an empty pd2-launcher folder, and the current install location must not hold an installation."
        );
        crate::automation::set_unattended_exit_status(crate::automation::LOCATION_EXIT_STATUS);
        true
    }

    fn drive_unattended(&mut self) -> bool {
        let mut changed = false;
        if self.unattended_location_pending() {
            let snapshot = self.state.snapshot();
            let settled = snapshot.location().is_some()
                && (snapshot.host() != HostView::Checking || snapshot.location_unavailable())
                && !snapshot
                    .operation
                    .as_ref()
                    .is_some_and(OperationView::owns_mutation);
            if settled
                && !self.unattended_location_requested
                && let Some(parent) = self.unattended_install_dir.clone()
                && self.emit(Intent::UseFreshInstallLocation(parent))
            {
                self.unattended_location_requested = true;
                changed = true;
            }
            // Nothing is installed until the fresh root is the current one.
            return changed;
        }
        if self.unattended_start_over
            && !self.unattended_start_over_requested
            && matches!(
                self.state.snapshot().installation(),
                InstallationStatus::StageInterrupted(_)
            )
            && start_over_available(self.state.snapshot())
            && self.emit(Intent::StartOver)
        {
            self.unattended_start_over_requested = true;
            changed = true;
        }
        // Download, then Install Diablo II, then Install Project Diablo 2,
        // each once the one before it finished.
        let snapshot = self.state.snapshot();
        let idle = self.unattended
            && snapshot.host() == HostView::Supported
            && !snapshot
                .operation
                .as_ref()
                .is_some_and(OperationView::owns_mutation)
            && !snapshot.operation.as_ref().is_some_and(|operation| {
                operation.job.is_some_and(ActiveJobView::is_setup)
                    && operation.status != OperationStatusView::Succeeded
            });
        let next = match snapshot.installation() {
            _ if !idle => None,
            InstallationStatus::NeedsBaseGame
            | InstallationStatus::NeedsExpansion
            | InstallationStatus::NeedsClient
                if !snapshot.downloads_ready() && snapshot.installer_media().is_ready() =>
            {
                Some(ActiveJobView::Download)
            }
            InstallationStatus::NeedsBaseGame
                if snapshot.downloads_ready()
                    && snapshot
                        .installer_media()
                        .existing()
                        .is_some_and(|existing| existing.problem.is_none()) =>
            {
                Some(ActiveJobView::InstallBaseGame)
            }
            InstallationStatus::NeedsBaseGame | InstallationStatus::NeedsExpansion
                if snapshot.downloads_ready()
                    && snapshot.installer_media().is_ready()
                    && self.unattended_authorization.is_some() =>
            {
                Some(ActiveJobView::InstallBaseGame)
            }
            InstallationStatus::NeedsClient | InstallationStatus::NeedsRuntime => {
                Some(ActiveJobView::InstallProjectDiablo)
            }
            _ => None,
        };
        if let Some(job) = next {
            if self.unattended_requested.contains(&job) {
                self.unattended_stalled = true;
                return true;
            }
            let existing = snapshot.installer_media().existing().is_some();
            let sent = match job {
                ActiveJobView::Download => self.emit(Intent::DownloadAll),
                ActiveJobView::InstallBaseGame if existing => {
                    self.emit(Intent::ImportExistingInstall(self.unattended_import_mode))
                }
                ActiveJobView::InstallBaseGame => self.send_unattended_authorization(),
                _ => self.emit(Intent::InstallProjectDiablo),
            };
            if sent {
                self.unattended_requested.push(job);
                changed = true;
            }
        }

        let intent = match after_ready_step(self.unattended_after_ready, self.state.snapshot()) {
            AfterReadyStep::LaunchGame => Some(Intent::LaunchGame(
                GraphicsDriver::Standard,
                None,
                false,
                false,
            )),
            AfterReadyStep::OpenOfficialLauncher => Some(Intent::OpenOfficialLauncher),
            AfterReadyStep::Wait | AfterReadyStep::RefuseUnverifiedOfficialLauncher => None,
        };
        if let Some(intent) = intent
            && self.emit(intent)
        {
            self.unattended_after_ready = AfterReady::Nothing;
            changed = true;
        }
        changed
    }

    /// Hand the unattended CD keys to the service exactly once.
    fn send_unattended_authorization(&mut self) -> bool {
        let Some(authorization) = self.unattended_authorization.take() else {
            return false;
        };
        match self
            .intent_sender
            .try_send(Intent::AuthorizeAndInstall(authorization))
        {
            Ok(()) => {
                self.delivery_alert = None;
                true
            }
            Err(TrySendError::Full(Intent::AuthorizeAndInstall(value))) => {
                self.unattended_authorization = Some(value);
                self.delivery_alert =
                    Some("The Launcher is busy. Unattended installation will retry.");
                false
            }
            Err(TrySendError::Disconnected(Intent::AuthorizeAndInstall(value))) => {
                self.unattended_authorization = Some(value);
                self.delivery_alert =
                    Some("The Launcher service is unavailable. Restart the Launcher to continue.");
                false
            }
            Err(_) => unreachable!("only the submitted authorization can be returned"),
        }
    }

    /// Whether the window may close now; asks first while work runs.
    fn should_close(&mut self, cx: &mut Context<Self>) -> bool {
        match close_decision(self.state.snapshot(), self.closing) {
            CloseDecision::Close => true,
            CloseDecision::Confirm => {
                self.confirm_close = true;
                cx.notify();
                false
            }
            CloseDecision::WaitForStop => false,
        }
    }

    /// The title bar's Close: the same decision as the compositor's close
    /// request, so work in progress asks first instead of being cut off.
    fn close_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.should_close(cx) {
            window.remove_window();
        }
    }

    /// How the window is decorated; a test may stand in for the platform.
    fn window_chrome(&self, window: &Window) -> page::WindowChrome {
        #[cfg(test)]
        if let Some(chrome) = self.chrome_override {
            return chrome;
        }
        page::WindowChrome::detect(window)
    }

    /// "Stop and close": stop at a safe point, then close. The window stays
    /// responsive meanwhile; nothing blocks on the worker.
    fn stop_and_close(&mut self, cx: &mut Context<Self>) {
        self.confirm_close = false;
        if self.emit(Intent::RequestSafeStop) {
            self.closing = true;
        }
        cx.notify();
    }

    fn authorization_complete(&self, cx: &App) -> bool {
        self.consent
            && self
                .base_key
                .read(cx)
                .is_valid_for(crate::blizzard::KeyField::BaseGame)
            && self
                .expansion_key
                .read(cx)
                .is_valid_for(crate::blizzard::KeyField::Expansion)
    }

    fn choose_installer_media(&mut self, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Use installer folder".into()),
        });
        self.installer_picker = Some(cx.spawn(async move |view, cx| {
            let result = prompt.await;
            let _ = view.update(cx, |this, cx| {
                match result {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.into_iter().next() {
                            this.emit(Intent::SelectInstallerMedia(path));
                        }
                    }
                    Ok(Ok(None)) => {}
                    Ok(Err(_)) | Err(_) => {
                        this.delivery_alert = Some(
                            "The system file picker is unavailable. Install and enable an XDG desktop portal, then try again, or use Download from Blizzard instead.",
                        );
                    }
                }
                cx.notify();
            });
        }));
    }

    /// Pick a Diablo II folder the user already has, of any version.
    fn choose_existing_install(&mut self, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Use this Diablo II folder".into()),
        });
        self.existing_picker = Some(cx.spawn(async move |view, cx| {
            let result = prompt.await;
            let _ = view.update(cx, |this, cx| {
                match result {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.into_iter().next() {
                            this.emit(Intent::SelectExistingInstall(path));
                        }
                    }
                    Ok(Ok(None)) => {}
                    Ok(Err(_)) | Err(_) => {
                        this.delivery_alert = Some(
                            "The system file picker is unavailable. Install and enable an XDG desktop portal, then try again, or use Download from Blizzard instead.",
                        );
                    }
                }
                cx.notify();
            });
        }));
    }

    /// Choose the device's FEX program with the system file picker; where
    /// there is no picker (no desktop portal) the path is typed instead.
    fn choose_fex_file(&mut self, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Use this FEX".into()),
        });
        self.fex_picker = Some(cx.spawn(async move |view, cx| {
            let result = prompt.await;
            let _ = view.update(cx, |this, cx| {
                let answer = match result {
                    Ok(Ok(paths)) => Some(paths),
                    // No portal, or the picker failed: the path is typed.
                    Ok(Err(_)) | Err(_) => None,
                };
                this.chose_fex_file(answer, cx);
            });
        }));
    }

    /// What the FEX file picker answered: a file (saved), nothing (closed),
    /// or `None` when there is no picker.
    fn chose_fex_file(&mut self, result: Option<Option<Vec<PathBuf>>>, cx: &mut Context<Self>) {
        match result {
            Some(Some(paths)) => {
                self.fex_picker_problem = None;
                if let Some(path) = paths.into_iter().next() {
                    self.emit(Intent::SaveFex(Some(path)));
                }
            }
            Some(None) => {}
            None => {
                self.fex_picker_problem = Some(FEX_PICKER_UNAVAILABLE);
            }
        }
        cx.notify();
    }

    /// Save the FEX path that was typed; an empty field finds FEX again.
    fn save_typed_fex(&mut self, cx: &mut Context<Self>) {
        let typed = self.fex_input.read(cx).value().trim().to_owned();
        self.emit(Intent::SaveFex(
            (!typed.is_empty()).then(|| PathBuf::from(typed)),
        ));
        cx.notify();
    }

    /// Pick the parent folder the launcher creates `pd2-launcher` in. The
    /// choice is confirmed before it is emitted.
    fn choose_install_location(&mut self, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Install here".into()),
        });
        self.location_picker = Some(cx.spawn(async move |view, cx| {
            let result = prompt.await;
            let _ = view.update(cx, |this, cx| {
                match result {
                    Ok(Ok(Some(paths))) => {
                        this.pending_location = paths.into_iter().next();
                    }
                    Ok(Ok(None)) => {}
                    Ok(Err(_)) | Err(_) => {
                        this.delivery_alert = Some(
                            "The system file picker is unavailable. Install and enable an XDG desktop portal, then try again.",
                        );
                    }
                }
                cx.notify();
            });
        }));
    }

    fn use_blizzard_download(&mut self, cx: &mut Context<Self>) {
        self.emit(Intent::UseBlizzardDownload);
        cx.notify();
    }

    /// Keep each URL field showing its effective source after the service
    /// publishes new download-source state, without clobbering an edit the
    /// user has not saved yet for an unchanged row.
    fn sync_source_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let sources = self.state.snapshot().download_sources().to_vec();
        for (index, (artifact, input)) in self.source_inputs.iter().enumerate() {
            let Some(source) = sources.iter().find(|source| source.artifact == *artifact) else {
                continue;
            };
            if self.source_synced[index].as_deref() != Some(source.effective_url.as_str()) {
                let value = source.effective_url.clone();
                input.update(cx, |state, cx| state.set_value(value, window, cx));
                self.source_synced[index] = Some(source.effective_url.clone());
            }
        }
    }

    fn save_download_source(&mut self, artifact: DownloadArtifact, cx: &mut Context<Self>) {
        let Some((_, input)) = self
            .source_inputs
            .iter()
            .find(|(candidate, _)| *candidate == artifact)
        else {
            return;
        };
        let value = input.read(cx).value().to_string();
        self.emit(Intent::SaveDownloadSource(artifact, value));
        cx.notify();
    }

    fn reset_download_source(&mut self, artifact: DownloadArtifact, cx: &mut Context<Self>) {
        if let Some(index) = self
            .source_inputs
            .iter()
            .position(|(candidate, _)| *candidate == artifact)
        {
            // Force the field back to the service's effective URL on the next
            // render even when that URL did not change.
            self.source_synced[index] = None;
        }
        self.emit(Intent::ResetDownloadSource(artifact));
        cx.notify();
    }

    fn focus_next(&mut self, _: &FocusNext, window: &mut Window, cx: &mut Context<Self>) {
        window.focus_next(cx);
    }

    fn focus_previous(&mut self, _: &FocusPrevious, window: &mut Window, cx: &mut Context<Self>) {
        window.focus_prev(cx);
    }

    fn navigate(&mut self, destination: Destination, cx: &mut Context<Self>) {
        self.state.navigate(destination);
        self.emit(Intent::Navigate(destination));
        cx.notify();
    }

    /// The confirmed Start over: the setup begins again at Check your
    /// computer, guided as on a first start, even when the steps had been
    /// following the installation since Install was pressed. The CD keys
    /// entered so far are forgotten.
    pub(crate) fn start_over_confirmed(&mut self, cx: &mut Context<Self>) -> bool {
        let sent = self.emit(Intent::StartOver);
        if sent {
            self.forget_keys(cx);
            if !self.unattended {
                self.guide = Some(GuidedCursor::restarted());
            }
        }
        sent
    }

    /// Wipe both CD key fields: nothing keeps the keys afterwards.
    fn forget_keys(&mut self, cx: &mut Context<Self>) {
        for input in [self.base_key.clone(), self.expansion_key.clone()] {
            if input.read(cx).len() > 0 {
                input.update(cx, |input, cx| input.forget(cx));
            }
        }
    }

    fn emit(&mut self, intent: Intent) -> bool {
        match self.intent_sender.try_send(intent) {
            Ok(()) => {
                self.delivery_alert = None;
                true
            }
            Err(TrySendError::Full(_)) => {
                self.delivery_alert =
                    Some("The Launcher is busy. Wait for the active request and try again.");
                false
            }
            Err(TrySendError::Disconnected(_)) => {
                self.delivery_alert =
                    Some("The Launcher service is unavailable. Restart the Launcher to continue.");
                false
            }
        }
    }

    fn activate_action(&mut self, action: ActionId, cx: &mut Context<Self>) {
        let intent = match action {
            ActionId::AuthorizeAndInstall => {
                self.dispatch_authorization(cx);
                None
            }
            ActionId::ContinueInstallation => Some(Intent::ContinueInstallation),
            ActionId::Play => Some(Intent::LaunchGame(
                GraphicsDriver::Standard,
                None,
                false,
                false,
            )),
            ActionId::Update => Some(Intent::ApplyUpdate),
            ActionId::SafeStop => Some(Intent::RequestSafeStop),
            ActionId::OpenDiagnostics => {
                self.navigate(Destination::Diagnostics, cx);
                None
            }
            ActionId::ExportDiagnostics => Some(Intent::ExportDiagnostics),
            ActionId::OpenOfficialLauncher
                if self.state.snapshot().official_launcher_unverified() =>
            {
                self.confirm_official_launcher = true;
                cx.notify();
                None
            }
            ActionId::OpenOfficialLauncher => Some(Intent::OpenOfficialLauncher),
            ActionId::StartOver => {
                self.confirm_start_over = true;
                cx.notify();
                None
            }
            ActionId::RetryLocation => Some(Intent::RetryLocation),
            ActionId::CheckAgain => Some(Intent::CheckAgain),
            ActionId::GuidedContinue => {
                self.guided_continue(cx);
                None
            }
            ActionId::GuidedBack => {
                self.guided_back(cx);
                None
            }
            ActionId::Download => Some(Intent::DownloadAll),
            ActionId::ImportExisting => {
                // An imported Diablo II needs no keys: none stay behind.
                self.forget_keys(cx);
                Some(Intent::ImportExistingInstall(self.import_mode))
            }
            ActionId::InstallProjectDiablo => Some(Intent::InstallProjectDiablo),
            ActionId::ReturnToOverview => {
                self.navigate(Destination::Operations, cx);
                None
            }
            ActionId::None | ActionId::Repairing => None,
        };
        if let Some(intent) = intent {
            self.emit(intent);
            cx.notify();
        }
    }

    /// The step the pane shows. Unattended mode has no guide.
    fn guided_step(&self, snapshot: &UiSnapshot) -> Option<SetupStep> {
        self.guide
            .filter(|_| guide_applies(snapshot))
            .map(|cursor| cursor.shown(snapshot))
    }

    /// Continue: the next step. Continue on Location & installers starts
    /// the download it chose.
    fn guided_continue(&mut self, cx: &mut Context<Self>) {
        let pending = self.pending_location.is_some();
        let snapshot = self.state.snapshot().clone();
        let Some(cursor) = self.guide.as_mut() else {
            return;
        };
        let from = cursor.shown(&snapshot);
        if cursor.advance(&snapshot, pending) {
            if continue_starts_download(from, &snapshot) {
                self.emit(Intent::DownloadAll);
            }
            cx.notify();
        }
    }

    fn guided_back(&mut self, cx: &mut Context<Self>) {
        let snapshot = self.state.snapshot();
        if let Some(cursor) = self.guide.as_mut()
            && cursor.back(snapshot)
        {
            cx.notify();
        }
    }

    fn guided_go_to(&mut self, step: SetupStep, cx: &mut Context<Self>) {
        let snapshot = self.state.snapshot();
        if let Some(cursor) = self.guide.as_mut()
            && cursor.go_to(step, snapshot)
        {
            cx.notify();
        }
    }

    fn dispatch_authorization(&mut self, cx: &mut Context<Self>) {
        if !self.state.snapshot().installer_media().is_ready() {
            self.delivery_alert =
                Some("Choose how to get Diablo II first: download it, or use files you have.");
            cx.notify();
            return;
        }
        if !self.authorization_complete(cx) {
            self.delivery_alert = Some(
                "Enter both CD keys (26 letters and numbers each) and accept Blizzard's license terms to continue.",
            );
            cx.notify();
            return;
        }

        let base_key = self.base_key.read(cx).copy_value();
        let expansion_key = self.expansion_key.read(cx).copy_value();
        let authorization = match AuthorizationInput::new(base_key, expansion_key, self.consent) {
            Ok(authorization) => authorization,
            Err(_) => {
                self.delivery_alert =
                    Some("A CD key can only have letters and numbers. Check both CD keys.");
                cx.notify();
                return;
            }
        };

        match self
            .intent_sender
            .try_send(Intent::AuthorizeAndInstall(authorization))
        {
            Ok(()) => {
                self.consent = false;
                self.delivery_alert = None;
                // From here on the steps follow the installation itself.
                self.guide = None;
            }
            Err(error) => {
                let (authorization, alert) = match error {
                    TrySendError::Full(Intent::AuthorizeAndInstall(value)) => (
                        value,
                        "The Launcher is busy. Wait for the active request and try again.",
                    ),
                    TrySendError::Disconnected(Intent::AuthorizeAndInstall(value)) => (
                        value,
                        "The Launcher service is unavailable. Restart the Launcher to continue.",
                    ),
                    _ => unreachable!("only the submitted authorization intent can be returned"),
                };
                let (base_key, expansion_key, _) = authorization.into_parts();
                self.base_key
                    .update(cx, |input, cx| input.restore(base_key, cx));
                self.expansion_key
                    .update(cx, |input, cx| input.restore(expansion_key, cx));
                self.delivery_alert = Some(alert);
            }
        }
        cx.notify();
    }
}

#[cfg(feature = "gui")]
impl Focusable for LauncherView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// GPUI-neutral renderer evidence emitted once the native window exists.
#[cfg(feature = "gui")]
pub(crate) struct GraphicsSnapshot {
    pub(crate) is_software_emulated: bool,
    pub(crate) device_name: String,
    pub(crate) driver_name: String,
    pub(crate) driver_info: String,
}

#[cfg(feature = "gui")]
pub(crate) fn run(
    endpoint: PresentationEndpoint,
    automation: Option<AutomationRequest>,
    mode: LaunchMode,
    start_service: impl FnOnce(Option<GraphicsSnapshot>) -> Box<dyn Send> + 'static,
) {
    let interactive = automation.is_none();
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx: &mut App| {
            gpui_kit::init(cx);
            theme::apply(cx);
            cx.bind_keys([
                KeyBinding::new("tab", FocusNext, Some("OperationsDeck")),
                KeyBinding::new("shift-tab", FocusPrevious, Some("OperationsDeck")),
                KeyBinding::new("backspace", Backspace, Some("SecretProductKey")),
                KeyBinding::new("delete", Delete, Some("SecretProductKey")),
                KeyBinding::new("left", Left, Some("SecretProductKey")),
                KeyBinding::new("right", Right, Some("SecretProductKey")),
                KeyBinding::new("home", Home, Some("SecretProductKey")),
                KeyBinding::new("end", End, Some("SecretProductKey")),
                KeyBinding::new("ctrl-a", SelectAll, Some("SecretProductKey")),
                KeyBinding::new("ctrl-v", Paste, Some("SecretProductKey")),
            ]);
            // A compact launcher window, centered: everything fits at the
            // minimum size. A larger or maximized window fills edge to edge;
            // only the content column keeps its readable width.
            let bounds = Bounds::centered(None, size(px(960.0), px(600.0)), cx);
            let opened = cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(800.0), px(520.0))),
                    titlebar: Some(gpui_kit::TitlebarOptions {
                        title: Some("PD2 Launcher".into()),
                        ..Default::default()
                    }),
                    // The desktop file's name, so docks show its icon.
                    app_id: Some("io.github.kapdon.pd2-launcher".into()),
                    // The header is the title bar, with its own window
                    // buttons: ask for client-side decorations so a
                    // compositor that draws title bars never adds a second
                    // one, and one that draws none (Hyprland, Sway) still
                    // leaves the user a way to minimize, maximize and close.
                    window_decorations: Some(WindowDecorations::Client),
                    ..Default::default()
                },
                move |window, cx| {
                    let graphics = window.gpu_specs().map(GraphicsSnapshot::from);
                    // A window drawn on the CPU (a software Vulkan or OpenGL
                    // driver) keeps busy indicators still: an animation
                    // redraws every frame, and on a slow computer those frames
                    // take CPU time from the Wine programs the step runs.
                    if graphics
                        .as_ref()
                        .is_some_and(|graphics| graphics.is_software_emulated)
                    {
                        cx.set_reduce_motion(true);
                    }
                    let gpu = graphics
                        .as_ref()
                        .map(|graphics| graphics.device_name.clone());
                    let service_runtime = start_service(graphics);
                    let view = cx.new(|cx| {
                        let mut view = LauncherView::new(
                            endpoint,
                            service_runtime,
                            automation,
                            StartupInputs::load(mode),
                            window,
                            cx,
                        );
                        view.gpu = gpu;
                        view
                    });
                    let closing = view.downgrade();
                    window.on_window_should_close(cx, move |_, cx| {
                        closing
                            .update(cx, |view, cx| view.should_close(cx))
                            .unwrap_or(true)
                    });
                    // The shell paints every pixel of the window: no
                    // transparent shadow margin, which some compositors show
                    // as a black band, and no second frame over the
                    // compositor's own border.
                    window.set_client_inset(px(0.0));
                    cx.new(|cx| gpui_kit::component::Root::new(view, window, cx).bordered(false))
                },
            );
            if let Err(error) = opened {
                // Without a window the user would see nothing: save the
                // details and say so with a desktop dialog.
                crate::startup_failure::report_system(&format!(
                    "The window could not be opened.\n\n{error:#}"
                ));
                crate::automation::set_unattended_exit_status(
                    crate::automation::STARTUP_FAILURE_EXIT_STATUS,
                );
                quit_once_running(cx);
                return;
            }
            // Without WAYLAND_DISPLAY or DISPLAY, GPUI runs a headless
            // window nobody sees: for the interactive Launcher that is the
            // same silent failure.
            if interactive && cx.compositor_name() == "headless" {
                crate::startup_failure::report_system(
                    "No desktop display was found: WAYLAND_DISPLAY and DISPLAY are both unset.",
                );
                crate::automation::set_unattended_exit_status(
                    crate::automation::STARTUP_FAILURE_EXIT_STATUS,
                );
                quit_once_running(cx);
                return;
            }
            crate::startup_failure::window_opened();
            // Closing the only window ends the Launcher. Its view owned the
            // service runtime, whose drop already requested a safe stop and
            // joined any running work, so nothing is detached.
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            cx.activate(true);
        });
}

/// Quit from inside the launch callback. A quit requested before the event
/// loop runs is lost on some platforms, so it waits for the loop.
#[cfg(feature = "gui")]
fn quit_once_running(cx: &mut App) {
    cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(Duration::from_millis(50))
            .await;
        cx.update(|cx| cx.quit());
    })
    .detach();
}

#[cfg(feature = "gui")]
impl From<GpuSpecs> for GraphicsSnapshot {
    fn from(graphics: GpuSpecs) -> Self {
        Self {
            is_software_emulated: graphics.is_software_emulated,
            device_name: graphics.device_name,
            driver_name: graphics.driver_name,
            driver_info: graphics.driver_info,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InstallationStatus, InterruptedStage, UpdateStatus};

    fn ready(update: UpdateStatus) -> UiSnapshot {
        UiSnapshot::initial()
            .with_host(HostView::Supported)
            .with_installation(InstallationStatus::Ready)
            .with_update(update)
            .with_launch_preflight(true)
    }

    #[test]
    fn blizzard_windows_notice_covers_only_steps_that_run_blizzards_programs() {
        let running = |job, phase, media| {
            UiSnapshot::initial()
                .with_installer_media(media)
                .with_active_job(Some(job))
                .with_operation(Some(
                    OperationView::mutating(phase, OperationStatusView::Running, "Working")
                        .with_job(job),
                ))
        };
        let folder = InstallerMediaView::Validated(std::path::PathBuf::from("/media/installers"));
        for (job, phase, media) in [
            (
                ActiveJobView::Download,
                OperationPhase::RuntimeAcquisition,
                InstallerMediaView::BlizzardDownload,
            ),
            (
                ActiveJobView::Download,
                OperationPhase::ExpansionDownload,
                folder.clone(),
            ),
            (
                ActiveJobView::InstallBaseGame,
                OperationPhase::PrefixInitialization,
                folder.clone(),
            ),
            (
                ActiveJobView::InstallBaseGame,
                OperationPhase::ExpansionInstall,
                InstallerMediaView::BlizzardDownload,
            ),
        ] {
            assert_eq!(
                blizzard_windows_notice(&running(job, phase, media)),
                Some(BLIZZARD_WINDOWS_NOTICE),
                "{job:?} {phase:?}"
            );
        }
        let existing = InstallerMediaView::Existing(ExistingInstallView {
            path: std::path::PathBuf::from("/games/Diablo II"),
            problem: None,
            copy_bytes: 1,
            in_place_possible: true,
            project_diablo: false,
        });
        for (job, phase, media) in [
            (
                ActiveJobView::InstallBaseGame,
                OperationPhase::ImportGame,
                existing,
            ),
            (
                ActiveJobView::InstallProjectDiablo,
                OperationPhase::ClientDownload,
                folder,
            ),
        ] {
            assert_eq!(blizzard_windows_notice(&running(job, phase, media)), None);
        }
        assert_eq!(blizzard_windows_notice(&UiSnapshot::initial()), None);
    }

    #[test]
    fn ready_available_update_keeps_play_primary_and_offers_update_separately() {
        let deck = DeckPresentation::project(&ready(UpdateStatus::Available), false);

        assert_eq!(deck.mode, DeckMode::Ready);
        assert_eq!(deck.primary, ActionView::enabled(ActionId::Play, "Play"));
        assert_eq!(
            deck.secondary,
            vec![ActionView::enabled(ActionId::Update, "Update")]
        );
        assert_eq!(
            deck.advisory.as_deref(),
            Some(
                "An update is available. Online play may require it; single-player remains available."
            )
        );
        assert_eq!(
            deck.focus_order,
            vec![
                FocusTarget::Diagnostics,
                FocusTarget::Settings,
                FocusTarget::Primary,
                FocusTarget::Update,
            ]
        );
    }

    #[test]
    fn ready_projection_quiets_completed_setup_history() {
        let deck = DeckPresentation::project(&ready(UpdateStatus::UpToDate), false);
        let visible = deck.visible_text();

        assert_eq!(deck.mode, DeckMode::Ready);
        assert!(!visible.contains(&"Check your computer"));
        assert!(!visible.contains(&"Location & installers"));
        assert!(!visible.contains(&"Install Diablo II"));
        assert!(visible.contains(&"Ready to play"));
        assert!(visible.contains(&"Play"));
    }

    #[test]
    fn incomplete_projection_remains_in_the_setup_funnel() {
        let snapshot = UiSnapshot::initial()
            .with_host(HostView::Supported)
            .with_installation(InstallationStatus::NeedsBaseGame);
        let deck = DeckPresentation::project(&snapshot, false);
        let visible = deck.visible_text();

        assert_eq!(deck.mode, DeckMode::Setup);
        for step in SetupStep::ALL {
            assert!(visible.contains(&step.name()), "{step:?}");
        }
    }

    #[test]
    fn host_audit_is_the_only_current_setup_checkpoint_while_checking() {
        let snapshot = UiSnapshot::initial()
            .with_host(HostView::Checking)
            .with_installation(InstallationStatus::NeedsBaseGame);
        let deck = DeckPresentation::project(&snapshot, false);
        let active = deck
            .journey
            .iter()
            .filter(|item| item.state == JourneyState::Active)
            .collect::<Vec<_>>();

        assert_eq!(active.len(), 1);
        assert_eq!(active[0].label, "Check your computer");
        assert!(
            deck.journey
                .iter()
                .skip(1)
                .all(|item| item.state == JourneyState::Pending)
        );
    }

    #[test]
    fn failed_update_check_does_not_regress_ready_play() {
        let deck = DeckPresentation::project(&ready(UpdateStatus::CheckFailed), false);

        assert_eq!(deck.primary.id, ActionId::Play);
        assert!(deck.primary.enabled);
        assert!(deck.secondary.is_empty());
        assert_eq!(
            deck.advisory.as_deref(),
            Some(
                "Update status is unavailable. Your verified local installation can still be played."
            )
        );
    }

    #[test]
    fn first_d2gl_launch_notice_shows_before_during_and_after_that_launch() {
        let notice = Some(GameNoticeView::FirstD2glLaunch);
        // Before Play (a fresh installation), while the game runs, and after
        // it exited: the player sees the tip whenever it matters.
        for game in [GameView::Idle, GameView::Running] {
            let deck = DeckPresentation::project(
                &ready(UpdateStatus::UpToDate)
                    .with_game(game)
                    .with_game_notice(notice),
                false,
            );
            assert_eq!(deck.game_notice, notice, "{game:?}");
        }
        let quiet = DeckPresentation::project(
            &ready(UpdateStatus::UpToDate).with_game(GameView::Running),
            false,
        );
        assert_eq!(quiet.game_notice, None);

        // It is about D2GL only, and never gates Play.
        let directdraw = DeckPresentation::project(
            &ready(UpdateStatus::UpToDate)
                .with_game_notice(notice)
                .with_settings(LaunchSettingsView::new(
                    RendererPreferenceView::DirectDraw,
                    true,
                    false,
                )),
            false,
        );
        assert_eq!(directdraw.game_notice, None);
        assert!(directdraw.primary.enabled);
    }

    #[test]
    fn game_and_mutation_guards_are_visible_and_not_only_hidden() {
        let game = DeckPresentation::project(
            &ready(UpdateStatus::Available).with_game(GameView::Running),
            false,
        );
        assert!(!game.primary.enabled);
        assert_eq!(game.primary.label, "Game is running");
        assert_eq!(game.secondary[0].id, ActionId::Update);
        assert!(!game.secondary[0].enabled);

        let mutation = DeckPresentation::project(
            &ready(UpdateStatus::Available).with_operation(Some(OperationView::mutating(
                OperationPhase::ClientDownload,
                OperationStatusView::Running,
                "Downloading verified PD2 files",
            ))),
            false,
        );
        assert!(!mutation.primary.enabled);
        assert_eq!(mutation.primary.label, "Play unavailable during update");
        assert_eq!(
            mutation.secondary,
            vec![
                ActionView::disabled(ActionId::Update, "Update unavailable while busy"),
                ActionView::enabled(ActionId::SafeStop, "Request safe stop"),
            ]
        );
    }

    #[test]
    fn after_start_over_the_setup_begins_again_at_check_your_computer() {
        // Downloads are kept, so the installation stands at Install Diablo II.
        let kept = UiSnapshot::initial()
            .with_host(HostView::Supported)
            .with_installation(InstallationStatus::NeedsBaseGame)
            .with_downloads(DownloadsView {
                complete: true,
                installers: true,
                total_bytes: 1,
            })
            .with_operation(None);
        assert_eq!(standing_step(&kept), SetupStep::InstallD2);
        let mut cursor = GuidedCursor::restarted();
        cursor.settle(&kept);
        assert_eq!(cursor.shown(&kept), SetupStep::Device);
        assert!(cursor.rail(&kept, SetupStep::Device).state == JourneyState::Active);
        // Later steps open only by continuing through each one again.
        assert!(!cursor.rail(&kept, SetupStep::Download).reachable);
        assert!(cursor.advance(&kept, false));
        assert_eq!(cursor.shown(&kept), SetupStep::Setup);
        assert!(cursor.rail(&kept, SetupStep::Device).reachable);
    }

    #[test]
    fn authorization_requires_both_keys_and_explicit_consent_in_focus_order() {
        // Install Diablo II comes after Download.
        let snapshot = UiSnapshot::initial()
            .with_host(HostView::Supported)
            .with_installation(InstallationStatus::NeedsBaseGame)
            .with_downloads(DownloadsView {
                complete: true,
                installers: true,
                total_bytes: 1,
            });

        let incomplete = DeckPresentation::project(&snapshot, false);
        assert_eq!(incomplete.primary.id, ActionId::AuthorizeAndInstall);
        assert!(!incomplete.primary.enabled);
        assert_eq!(
            incomplete.focus_order,
            vec![
                FocusTarget::Diagnostics,
                FocusTarget::Settings,
                FocusTarget::BaseGameKey,
                FocusTarget::ExpansionKey,
                FocusTarget::Consent,
                FocusTarget::Primary,
            ]
        );

        let complete = DeckPresentation::project(
            &snapshot
                .clone()
                .with_installer_media(InstallerMediaView::Validated(PathBuf::from(
                    "/tmp/pd2-launcher-test-installers",
                ))),
            true,
        );
        assert!(complete.primary.enabled);

        // Choosing the Blizzard download satisfies the same installer step.
        let downloading = snapshot.with_installer_media(InstallerMediaView::BlizzardDownload);
        assert!(
            DeckPresentation::project(&downloading, true)
                .primary
                .enabled
        );
        assert!(
            !DeckPresentation::project(&downloading, false)
                .primary
                .enabled
        );
        // The downloads are verified: Install Diablo II is the current step.
        assert_eq!(
            project_journey(&downloading)
                .iter()
                .position(|item| item.state == JourneyState::Active),
            Some(3)
        );
        assert!(downloading.custom_installer_hosts().is_empty());
        assert!(!downloading.custom_sources_active());
        // Before the downloads, the Download step names where they come from.
        let downloading = downloading.with_downloads(DownloadsView::default());
        assert!(
            DeckPresentation::project(&downloading, false)
                .description
                .contains("from Blizzard")
        );

        // With an overridden D2 or LoD source the copy names the custom,
        // unverified hosts instead of Blizzard.
        let row = |artifact: DownloadArtifact, badge, url: &str| DownloadSourceView {
            artifact,
            effective_url: url.to_owned(),
            badge,
            error: None,
            installed: None,
        };
        let overridden = downloading.with_download_sources(vec![
            row(
                DownloadArtifact::D2Installer,
                SourceBadgeView::Custom,
                "https://user:secret@Mirror.Example/d2.exe?token=abc",
            ),
            row(
                DownloadArtifact::LodInstaller,
                SourceBadgeView::BuiltInVerified,
                &DownloadArtifact::LodInstaller.builtin_url(),
            ),
            row(
                DownloadArtifact::Wine,
                SourceBadgeView::CustomFromEnvironment,
                "https://other.example/wine.tar.xz",
            ),
        ]);
        assert_eq!(overridden.custom_installer_hosts(), ["mirror.example"]);
        // Any custom source, installer or not, raises the main-screen banner.
        assert!(overridden.custom_sources_active());
        let deck = DeckPresentation::project(&overridden, false);
        assert!(!deck.description.contains("Blizzard"));
        assert!(deck.description.contains("custom sources (unverified)"));
        assert_eq!(
            deck.journey[1].detail,
            "Custom download (unverified) or your own files"
        );
    }

    #[test]
    fn proven_vendor_stages_continue_after_restart_without_collecting_keys_again() {
        for (installation, primary) in [
            (
                InstallationStatus::NeedsClient,
                ActionView::enabled(ActionId::InstallProjectDiablo, "Install Project Diablo 2"),
            ),
            (
                InstallationStatus::NeedsRuntime,
                ActionView::enabled(ActionId::InstallProjectDiablo, "Continue installation"),
            ),
        ] {
            let snapshot = UiSnapshot::initial()
                .with_host(HostView::Supported)
                .with_installation(installation)
                .with_authorization(AuthorizationView::Absent)
                .with_downloads(DownloadsView {
                    complete: true,
                    installers: true,
                    total_bytes: 1,
                });

            let deck = DeckPresentation::project(&snapshot, false);

            assert_eq!(deck.primary, primary);
            assert!(!deck.focus_order.contains(&FocusTarget::BaseGameKey));
            assert!(!deck.focus_order.contains(&FocusTarget::ExpansionKey));
        }
    }

    #[test]
    fn the_official_launcher_is_offered_for_an_installed_game_and_opens_only_when_idle() {
        let ready = ready(UpdateStatus::UpToDate);
        assert!(official_launcher_offered(&ready));
        assert!(official_launcher_available(&ready));
        let client = ready
            .clone()
            .with_installation(InstallationStatus::NeedsClient);
        // During Install Project Diablo 2 there is no official launcher yet.
        assert!(!official_launcher_offered(&client));
        assert!(official_launcher_available(&client));
        for installation in [
            InstallationStatus::NeedsBaseGame,
            InstallationStatus::NeedsExpansion,
            InstallationStatus::NeedsRuntime,
            InstallationStatus::Indeterminate,
        ] {
            let snapshot = ready.clone().with_installation(installation);
            assert!(!official_launcher_offered(&snapshot), "{installation:?}");
            assert!(!official_launcher_available(&snapshot), "{installation:?}");
        }
        assert!(!official_launcher_available(
            &ready.clone().with_game(GameView::Running)
        ));
        assert!(!official_launcher_available(
            &ready.clone().with_journal(true)
        ));
        let updating = ready.clone().with_operation(Some(OperationView::mutating(
            OperationPhase::ClientDownload,
            OperationStatusView::Running,
            "Updating",
        )));
        assert!(!official_launcher_available(&updating));
        let open = ready.with_operation(Some(OperationView::mutating(
            OperationPhase::OfficialLauncher,
            OperationStatusView::Running,
            "The official PD2 launcher is open",
        )));
        assert!(official_launcher_offered(&open));
        assert!(!official_launcher_available(&open));
        let deck = DeckPresentation::project(&open, false);
        assert!(deck.visible_text().contains(&"Official launcher is open"));
        assert!(deck.visible_text().contains(&"Close official launcher"));
    }

    #[test]
    fn unattended_mode_never_opens_an_unverified_official_launcher() {
        let ready = ready(UpdateStatus::UpToDate);
        assert_eq!(
            after_ready_step(AfterReady::OpenOfficialLauncher, &ready),
            AfterReadyStep::OpenOfficialLauncher
        );
        let unverified = ready.clone().with_official_launcher_unverified(true);
        assert_eq!(
            after_ready_step(AfterReady::OpenOfficialLauncher, &unverified),
            AfterReadyStep::RefuseUnverifiedOfficialLauncher
        );
        // Nothing is decided before the installation is Ready and idle.
        assert_eq!(
            after_ready_step(
                AfterReady::OpenOfficialLauncher,
                &unverified
                    .clone()
                    .with_installation(InstallationStatus::NeedsExpansion)
            ),
            AfterReadyStep::Wait
        );
        assert_eq!(
            after_ready_step(AfterReady::LaunchGame, &unverified),
            AfterReadyStep::LaunchGame
        );
        assert_eq!(
            after_ready_step(AfterReady::Nothing, &unverified),
            AfterReadyStep::Wait
        );
    }

    #[test]
    fn step_affordances_follow_one_table() {
        use StepAffordance::{Change, Locked, None as Nothing, ResetAvailable, StopAndChange};
        let idle = |installation| {
            UiSnapshot::initial()
                .with_host(HostView::Supported)
                .with_installation(installation)
                .with_operation(None)
        };
        let running = |installation, job, phase| {
            idle(installation)
                .with_active_job(Some(job))
                .with_operation(Some(OperationView::mutating(
                    phase,
                    OperationStatusView::Running,
                    "Working",
                )))
        };
        let locked = Locked;
        let work = || locked(StepLock::WorkRunning);
        // Rows: Device, Setup, Download, InstallD2, InstallPd2, Play. Only the
        // installer choice (Setup) and an interrupted Install Diablo II have
        // affordances of their own; every other row just navigates.
        let cases: Vec<(&str, UiSnapshot, [StepAffordance; 6])> = vec![
            (
                "checking host",
                UiSnapshot::initial(),
                [
                    Nothing,
                    locked(StepLock::HostNotReady),
                    Nothing,
                    locked(StepLock::HostNotReady),
                    Nothing,
                    Nothing,
                ],
            ),
            (
                "fresh, idle",
                idle(InstallationStatus::NeedsBaseGame),
                [Nothing, Change, Nothing, Nothing, Nothing, Nothing],
            ),
            (
                "expansion pending, idle",
                idle(InstallationStatus::NeedsExpansion),
                [Nothing, Change, Nothing, Nothing, Nothing, Nothing],
            ),
            (
                "ready",
                idle(InstallationStatus::Ready),
                [Nothing, Change, Nothing, Nothing, Nothing, Nothing],
            ),
            (
                "interrupted",
                idle(InstallationStatus::StageInterrupted(
                    InterruptedStage::Expansion,
                )),
                [Nothing, Change, Nothing, ResetAvailable, Nothing, Nothing],
            ),
            (
                "downloading",
                running(
                    InstallationStatus::NeedsBaseGame,
                    ActiveJobView::Download,
                    OperationPhase::RuntimeAcquisition,
                ),
                [Nothing, StopAndChange, Nothing, work(), Nothing, Nothing],
            ),
            (
                "installing Diablo II",
                running(
                    InstallationStatus::NeedsBaseGame,
                    ActiveJobView::InstallBaseGame,
                    OperationPhase::BaseGameInstall,
                ),
                [Nothing, StopAndChange, Nothing, work(), Nothing, Nothing],
            ),
            (
                "installing Project Diablo 2",
                running(
                    InstallationStatus::NeedsClient,
                    ActiveJobView::InstallProjectDiablo,
                    OperationPhase::ClientDownload,
                ),
                [Nothing, work(), Nothing, work(), Nothing, Nothing],
            ),
            (
                "update running",
                running(
                    InstallationStatus::Ready,
                    ActiveJobView::Update,
                    OperationPhase::ClientDownload,
                ),
                [Nothing, work(), Nothing, work(), Nothing, Nothing],
            ),
            (
                "official launcher open",
                running(
                    InstallationStatus::Ready,
                    ActiveJobView::OfficialLauncher,
                    OperationPhase::OfficialLauncher,
                ),
                [
                    Nothing,
                    locked(StepLock::OfficialLauncherOpen),
                    Nothing,
                    locked(StepLock::OfficialLauncherOpen),
                    Nothing,
                    Nothing,
                ],
            ),
            (
                "game running",
                idle(InstallationStatus::Ready).with_game(GameView::Running),
                [
                    Nothing,
                    locked(StepLock::GameRunning),
                    Nothing,
                    locked(StepLock::GameRunning),
                    Nothing,
                    Nothing,
                ],
            ),
            (
                "unresolved journal",
                idle(InstallationStatus::RepairRequired).with_journal(true),
                [
                    Nothing,
                    locked(StepLock::RepairPending),
                    Nothing,
                    locked(StepLock::RepairPending),
                    Nothing,
                    Nothing,
                ],
            ),
        ];
        for (name, snapshot, expected) in cases {
            for (step, want) in SetupStep::ALL.into_iter().zip(expected) {
                assert_eq!(step_affordance(&snapshot, step), want, "{name}: {step:?}");
            }
        }
    }

    #[test]
    fn a_stop_result_names_what_was_kept_and_what_was_not() {
        let (kept, not_kept) =
            StopResultView::new(InstallationStatus::NeedsBaseGame, false).lines();
        assert!(
            kept.iter()
                .any(|line| line.contains("unfinished downloads"))
        );
        // The keys stay in their masked inputs for a retry in this session.
        assert!(kept.iter().any(|line| line.contains("CD keys")));
        assert!(not_kept.is_empty());
        let (kept, not_kept) =
            StopResultView::new(InstallationStatus::NeedsExpansion, false).lines();
        assert!(kept.contains(&"Diablo II"));
        assert!(kept.iter().any(|line| line.contains("CD keys")));
        assert!(not_kept.is_empty());
        let (kept, not_kept) = StopResultView::new(InstallationStatus::NeedsClient, false).lines();
        assert!(kept.contains(&"Diablo II and Lord of Destruction"));
        assert!(not_kept.is_empty());
        let recovery = StopResultView::new(InstallationStatus::Indeterminate, true);
        assert!(recovery.needs_recovery());
        assert!(
            recovery
                .lines()
                .1
                .iter()
                .any(|line| line.contains("Diagnostics"))
        );
    }

    #[test]
    fn repair_and_indeterminate_states_never_offer_play() {
        let repairing = DeckPresentation::project(
            &ready(UpdateStatus::Available)
                .with_installation(InstallationStatus::RepairRequired)
                .with_journal(true),
            false,
        );
        assert_eq!(repairing.primary.id, ActionId::Repairing);
        assert!(!repairing.primary.enabled);
        assert!(!repairing.available_actions().contains(&ActionId::Play));

        let indeterminate = DeckPresentation::project(
            &ready(UpdateStatus::Available).with_installation(InstallationStatus::Indeterminate),
            false,
        );
        assert_eq!(indeterminate.primary.id, ActionId::OpenDiagnostics);
        assert!(!indeterminate.available_actions().contains(&ActionId::Play));
    }

    #[test]
    fn sequenced_reducer_ignores_stale_events_and_requests_snapshot_for_a_gap() {
        let mut reducer = UiReducer::new(ready(UpdateStatus::Unknown).with_sequence(40));

        assert_eq!(
            reducer.reduce(SequencedEvent::new(
                41,
                DomainEvent::UpdateChanged(UpdateStatus::Checking),
            )),
            ReduceOutcome::Applied
        );
        assert_eq!(reducer.snapshot().update, UpdateStatus::Checking);
        assert_eq!(
            reducer.reduce(SequencedEvent::new(
                40,
                DomainEvent::UpdateChanged(UpdateStatus::Available),
            )),
            ReduceOutcome::IgnoredStale
        );
        assert_eq!(reducer.snapshot().update, UpdateStatus::Checking);
        assert_eq!(
            reducer.reduce(SequencedEvent::new(
                43,
                DomainEvent::UpdateChanged(UpdateStatus::Available),
            )),
            ReduceOutcome::SnapshotRequired
        );
        assert_eq!(reducer.snapshot().update, UpdateStatus::Checking);
    }

    #[test]
    fn installation_authorization_debug_and_history_never_contain_keys() {
        let authorization = AuthorizationInput::new(
            "D2-SECRET-CANARY".to_owned(),
            "LOD-SECRET-CANARY".to_owned(),
            true,
        )
        .expect("valid authorization");

        let debug = format!("{authorization:?}");
        assert_eq!(
            debug,
            "AuthorizationInput { base_key: [REDACTED], expansion_key: [REDACTED], vendor_terms_accepted: true }"
        );
        assert!(!debug.contains("SECRET-CANARY"));
        let intent = Intent::AuthorizeAndInstall(authorization);
        assert_eq!(
            intent.history_label(),
            "Authorize and start Managed installation"
        );
    }

    #[test]
    fn navigation_does_not_replace_the_retained_service_bridge() {
        let mut state = LauncherViewState::new(ready(UpdateStatus::Unknown), BridgeGeneration(7));

        state.navigate(Destination::Diagnostics);
        state.navigate(Destination::Settings);

        assert_eq!(state.destination(), Destination::Settings);
        assert_eq!(state.bridge_generation(), BridgeGeneration(7));
    }

    #[test]
    fn settings_state_reduces_as_one_immutable_sequenced_event() {
        let mut reducer = UiReducer::new(ready(UpdateStatus::Unknown).with_sequence(9));
        let settings = LaunchSettingsView::new(RendererPreferenceView::DirectDraw, false, true);

        assert_eq!(
            reducer.reduce(SequencedEvent::new(
                10,
                DomainEvent::SettingsChanged(settings.clone()),
            )),
            ReduceOutcome::Applied
        );
        assert_eq!(reducer.snapshot().settings(), settings);
        assert_eq!(settings.renderer(), RendererPreferenceView::DirectDraw);
        assert_eq!(settings.persistence(), SettingsPersistenceView::Saved);
    }

    #[test]
    fn play_is_labelled_in_the_history() {
        assert_eq!(
            Intent::LaunchGame(GraphicsDriver::Standard, None, false, false).history_label(),
            "Launch Game client"
        );
    }

    #[test]
    fn settings_intents_are_typed_and_preserve_explicit_focus_order() {
        assert_eq!(
            Intent::ChangeSettings(SettingsChange::Renderer(RendererPreferenceView::Glide))
                .history_label(),
            "Change next-launch renderer"
        );
        assert_eq!(
            Intent::ChangeSettings(SettingsChange::SkipToBnet(false)).history_label(),
            "Change next-launch online shortcut"
        );
        assert_eq!(
            Intent::ChangeSettings(SettingsChange::SoundInBackground(true)).history_label(),
            "Change next-launch background sound"
        );
    }

    #[test]
    fn production_projection_contains_no_prototype_controls_or_fake_evidence() {
        let deck = DeckPresentation::project(&ready(UpdateStatus::Unknown), false);
        let copy = deck.visible_text().join(" ").to_lowercase();

        for forbidden in [
            "simulated state",
            "prototype variant",
            "fake progress",
            "last checked just now",
            "about 90 seconds",
        ] {
            assert!(
                !copy.contains(forbidden),
                "found prototype copy: {forbidden}"
            );
        }
    }

    fn failed(job: ActiveJobView, code: &'static str) -> UiSnapshot {
        UiSnapshot::initial()
            .with_host(HostView::Supported)
            .with_installation(InstallationStatus::NeedsBaseGame)
            .with_operation(Some(
                OperationView::mutating(
                    OperationPhase::Verification,
                    OperationStatusView::Failed,
                    "Operation complete",
                )
                .with_job(job)
                .with_failure(code),
            ))
    }

    #[test]
    fn result_cards_name_the_step_and_one_next_action() {
        let card = |job, code| result_card(&failed(job, code)).expect("card");
        let key = card(ActiveJobView::InstallBaseGame, "base-game-key-rejected");
        assert_eq!(key.title, "Diablo II's installer rejected the CD key");
        assert_eq!(
            key.message,
            "Diablo II's installer rejected the CD key. Check the Diablo II CD key and try again. Your downloads are kept."
        );
        assert_eq!(key.next, Some(ResultNextStep::TryAgain));
        for code in ["base-game-installer-menu", "expansion-installer-menu"] {
            let menu = card(ActiveJobView::InstallBaseGame, code);
            assert_eq!(
                menu.message,
                "Blizzard's installer opened its menu instead of installing. Try again, or choose Download from Blizzard."
            );
            assert_eq!(menu.next, Some(ResultNextStep::TryAgain));
        }
        let expansion = card(ActiveJobView::InstallBaseGame, "expansion-key-rejected");
        assert!(
            expansion
                .message
                .starts_with("Lord of Destruction's installer rejected the CD key.")
        );
        assert!(
            expansion
                .message
                .contains("Diablo II and your downloads are kept")
        );
        let generic = card(ActiveJobView::InstallBaseGame, "base-game-install-failed");
        assert_eq!(
            generic.message,
            "Installation stopped while installing Diablo II. Your downloads are kept. Try again, or open Diagnostics and choose Save report to ask for help."
        );
        for (code, step) in [
            ("expansion-install-failed", "installing Lord of Destruction"),
            ("runtime-setup-failed", "setting up the game environment"),
            ("client-install-failed", "installing Project Diablo 2"),
        ] {
            assert!(
                card(ActiveJobView::InstallBaseGame, code)
                    .message
                    .contains(step),
                "{code}"
            );
        }
        assert_eq!(
            card(ActiveJobView::InstallBaseGame, "stage-interrupted").next,
            Some(ResultNextStep::StartOver)
        );
        assert!(
            card(ActiveJobView::InstallBaseGame, "installer-download-failed")
                .message
                .contains("internet connection")
        );
        // Each way Blizzard's downloader fails has its own card; only these
        // two are the connection.
        assert!(
            card(ActiveJobView::InstallBaseGame, "installer-download-stalled")
                .message
                .contains("internet connection")
        );
        for code in [
            "installer-downloader-error",
            "installer-download-incomplete",
            "installer-download-rejected",
            "installer-downloader-failed",
        ] {
            let card = card(ActiveJobView::InstallBaseGame, code);
            assert!(!card.message.contains("internet connection"), "{code}");
            assert!(card.message.contains("Save report"), "{code}");
        }
        assert!(
            card(
                ActiveJobView::InstallBaseGame,
                "installer-payload-store-failed"
            )
            .message
            .contains("free space")
        );
        // An unknown code still says what to do, in plain words.
        let unknown = card(ActiveJobView::InstallBaseGame, "managed-install-failed");
        assert!(unknown.message.contains("Save report"));
        let update = card(ActiveJobView::Update, "update-failed");
        assert_eq!(update.title, "The update didn't finish");

        // No jargon reaches a card.
        for code in [
            "base-game-key-rejected",
            "expansion-key-rejected",
            "base-game-installer-error",
            "expansion-installer-error",
            "base-game-installer-menu",
            "expansion-installer-menu",
            "base-game-install-failed",
            "stage-interrupted",
            "download-failed",
            "download-cache-unusable",
            "managed-install-failed",
        ] {
            let text = format!("{:?}", card(ActiveJobView::InstallBaseGame, code)).to_lowercase();
            for word in [
                "prefix",
                "managed",
                "audit",
                "artifact",
                "receipt",
                "evidence",
                "provision",
            ] {
                assert!(!text.contains(word), "{code}: {word}");
            }
        }
    }

    #[test]
    fn a_start_over_that_could_not_keep_the_characters_says_nothing_was_removed() {
        let card = result_card(&failed(ActiveJobView::Reset, "saves-not-kept")).expect("a card");
        assert_eq!(card.title, "Start over didn't run");
        assert!(card.message.contains("nothing was removed"));
    }

    #[test]
    fn a_missing_official_launcher_says_which_file_and_what_to_check() {
        let card = result_card(&failed(
            ActiveJobView::OfficialLauncher,
            "official-launcher-missing",
        ))
        .expect("a card");
        assert_eq!(
            card.title,
            "The official launcher isn't in your game folder"
        );
        assert!(
            card.message
                .starts_with("ProjectD2\\PD2Launcher.exe is missing.")
        );
        assert_eq!(card.next, None);
    }

    #[test]
    fn a_game_folder_left_without_its_characters_is_why_the_game_did_not_start() {
        let play = UiSnapshot::initial()
            .with_host(HostView::Supported)
            .with_installation(InstallationStatus::Ready)
            .with_operation(Some(
                OperationView::read_only(
                    OperationPhase::GameExecution,
                    OperationStatusView::Failed,
                    "The game didn't start",
                )
                .with_failure("saves-unavailable"),
            ));
        let card = result_card(&play).expect("a card");
        assert_eq!(card.title, "The game didn't start");
        assert_eq!(card.message, SAVES_UNAVAILABLE);
        let official = result_card(&failed(
            ActiveJobView::OfficialLauncher,
            "saves-unavailable",
        ))
        .expect("a card");
        assert_eq!(official.title, "The official launcher didn't open");
        assert_eq!(official.message, SAVES_UNAVAILABLE);
    }

    #[test]
    fn saves_put_aside_say_how_many_in_plain_words_and_add_up() {
        let notice = |characters, files, others, folder: &str| SavesPutAsideView {
            characters,
            files,
            others,
            folder: PathBuf::from(folder),
            open: None,
        };
        assert_eq!(
            notice(1, 0, 0, "/b").message(),
            "1 character had another copy that differed: the one saved last is kept and the other is in backups."
        );
        // The shared stash, or a file of Save/ProjectD2, is no character.
        assert_eq!(
            notice(0, 1, 0, "/b").message(),
            "1 saved file had another copy that differed: the one saved last is kept and the other is in backups."
        );
        assert_eq!(
            notice(0, 4, 0, "/b").message(),
            "4 saved files had another copy that differed: the ones saved last are kept and the others are in backups."
        );
        assert_eq!(
            notice(1, 1, 0, "/b").message(),
            "1 character and 1 other saved file had another copy that differed: the ones saved last are kept and the others are in backups."
        );
        assert_eq!(
            notice(2, 0, 3, "/b").message(),
            "2 characters had another copy that differed: the ones saved last are kept and the others are in backups. 3 other items from the game's Save folder are in backups."
        );
        assert_eq!(
            notice(0, 0, 1, "/b").message(),
            "1 other item from the game's Save folder is in backups."
        );
        // A later keep adds to the notice, in its own folder.
        let snapshot = UiSnapshot::initial().with_saves_put_aside(Some(SavesPutAsideView {
            open: Some(FolderOpenView::Opened),
            ..notice(1, 1, 0, "/b/backup-1")
        }));
        assert_eq!(
            snapshot.saves_put_aside_with(notice(2, 3, 1, "/b/backup-2")),
            notice(3, 4, 1, "/b/backup-2")
        );
        let mut reducer = UiReducer::new(snapshot);
        reducer.reduce(SequencedEvent::new(
            1,
            DomainEvent::SavesPutAsideChanged(None),
        ));
        assert_eq!(reducer.snapshot().saves_put_aside(), None);
    }

    #[test]
    fn an_unusable_download_cache_is_not_blamed_on_the_network() {
        let card =
            result_card(&failed(ActiveJobView::Download, "download-cache-unusable")).expect("card");
        let text = format!("{card:?}");
        assert!(text.contains("download cache"), "{text}");
        assert!(!text.to_lowercase().contains("internet"), "{text}");
    }

    #[test]
    fn only_unsuccessful_outcomes_show_a_result_card() {
        let succeeded = failed(ActiveJobView::InstallBaseGame, "x").with_operation(Some(
            OperationView::mutating(
                OperationPhase::Verification,
                OperationStatusView::Succeeded,
                "Operation complete",
            ),
        ));
        assert_eq!(result_card(&succeeded), None);
        let running = failed(ActiveJobView::InstallBaseGame, "x").with_operation(Some(
            OperationView::mutating(
                OperationPhase::BaseGameInstall,
                OperationStatusView::Running,
                "Installing Diablo II",
            ),
        ));
        assert_eq!(result_card(&running), None);
        // A stop the user asked for has its own card.
        let stopped = UiSnapshot::initial()
            .with_operation(Some(
                OperationView::mutating(
                    OperationPhase::Verification,
                    OperationStatusView::Cancelled,
                    "Operation complete",
                )
                .with_job(ActiveJobView::InstallBaseGame),
            ))
            .with_stop_result(Some(StopResultView::new(
                InstallationStatus::NeedsBaseGame,
                false,
            )));
        assert_eq!(result_card(&stopped), None);
        assert!(result_card(&stopped.clone().with_stop_result(None)).is_some());
        let unclear = failed(ActiveJobView::InstallBaseGame, "operation-indeterminate")
            .with_operation(Some(
                OperationView::mutating(
                    OperationPhase::Verification,
                    OperationStatusView::Indeterminate,
                    "Operation complete",
                )
                .with_failure("operation-indeterminate"),
            ));
        assert_eq!(
            result_card(&unclear).map(|card| card.next),
            Some(Some(ResultNextStep::OpenDiagnostics))
        );
    }

    #[test]
    fn a_launcher_service_failure_has_its_own_view_and_retry() {
        let snapshot = UiSnapshot::initial()
            .with_host(HostView::Unsupported)
            .with_installation(InstallationStatus::Indeterminate)
            .with_operation(Some(
                OperationView::read_only(
                    OperationPhase::HostAudit,
                    OperationStatusView::Failed,
                    "Launcher service could not establish safe evidence",
                )
                .with_failure("paths-unavailable"),
            ));
        assert_eq!(service_failure(&snapshot), Some("paths-unavailable"));
        let deck = DeckPresentation::project(&snapshot, false);
        assert_eq!(
            deck.primary,
            ActionView::enabled(ActionId::CheckAgain, "Try again")
        );
        assert_eq!(result_card(&snapshot), None);
        // A service thread that never started cannot be retried.
        let dead = snapshot.with_operation(Some(
            OperationView::read_only(
                OperationPhase::HostAudit,
                OperationStatusView::Failed,
                "Launcher service could not start",
            )
            .with_failure(SERVICE_RUNTIME_UNAVAILABLE),
        ));
        assert_eq!(
            DeckPresentation::project(&dead, false).primary.id,
            ActionId::OpenDiagnostics
        );
    }

    #[test]
    fn an_installation_held_by_a_running_game_or_program_says_so() {
        for (code, title) in [
            ("game-still-running", "Project Diablo 2 is still running"),
            ("installation-in-use", "The installation is in use"),
        ] {
            let snapshot = UiSnapshot::initial()
                .with_host(HostView::Unsupported)
                .with_installation(InstallationStatus::Indeterminate)
                .with_operation(Some(
                    OperationView::read_only(
                        OperationPhase::HostAudit,
                        OperationStatusView::Failed,
                        "Launcher service could not establish safe evidence",
                    )
                    .with_failure(code),
                ));
            let deck = DeckPresentation::project(&snapshot, false);
            assert_eq!(deck.title, title);
            assert!(deck.description.contains("Check again"), "{code}");
            assert!(!deck.description.contains("background service"), "{code}");
            assert_eq!(
                deck.primary,
                ActionView::enabled(ActionId::CheckAgain, "Check again")
            );
        }
        assert_eq!(installation_busy_message("paths-unavailable"), None);
    }

    #[test]
    fn cd_keys_are_normalized_grouped_and_validated() {
        assert_eq!(
            normalize_key_input(" abcd-efgh ijkl-mnop "),
            b"ABCDEFGHIJKLMNOP".to_vec()
        );
        assert_eq!(normalize_key_input("a\tb_c.d\u{e9}"), b"ABCD".to_vec());
        use crate::blizzard::{KeyField, TEST_BASE_KEY, TEST_EXPANSION_KEY};
        let base = KeyField::BaseGame;
        assert!(
            key_problem("Diablo II", base, &[b'2'; 17]).is_some(),
            "only 26-character keys are accepted"
        );
        assert_eq!(key_problem("Diablo II", base, b""), None);
        assert_eq!(
            key_problem("Diablo II", base, TEST_BASE_KEY.as_bytes()),
            None
        );
        assert_eq!(
            key_problem(
                "Lord of Destruction",
                KeyField::Expansion,
                TEST_EXPANSION_KEY.as_bytes()
            ),
            None
        );
        assert_eq!(
            key_problem("Diablo II", base, &[b'2'; 15]).as_deref(),
            Some("Diablo II CD keys are 26 letters and numbers. This one has 15.")
        );
        assert!(
            key_problem("Diablo II", base, &[b'2'; 16])
                .is_some_and(|problem| problem.contains("16-character"))
        );
        let mut letter = [b'2'; 26];
        letter[3] = b'O';
        assert!(
            key_problem("Diablo II", base, &letter)
                .is_some_and(|problem| problem.contains("do not use"))
        );
        // Of the form, but no key of the product.
        assert_eq!(
            key_problem("Diablo II", base, &[b'2'; 26]).as_deref(),
            Some("This is not a Diablo II CD key. Check the key: your Blizzard account shows it.")
        );
        // "ABCD EFGH": positions around the group gap map both ways.
        assert_eq!(key_display_index(8, 0), 0);
        assert_eq!(key_display_index(8, 3), 3);
        assert_eq!(key_display_index(8, 4), 5);
        assert_eq!(key_display_index(8, 8), 9);
        assert_eq!(key_display_index(9, 9), 11);
        assert_eq!(key_content_index(8, 4), 4);
        assert_eq!(key_content_index(8, 5), 4);
        assert_eq!(key_content_index(8, 9), 8);
        assert_eq!(key_content_index(8, 40), 8);
        for len in 0..30 {
            for index in 0..=len {
                assert_eq!(
                    key_content_index(len, key_display_index(len, index)),
                    index,
                    "{len} {index}"
                );
            }
        }
    }

    #[test]
    fn missing_prerequisites_get_commands_for_the_distribution() {
        let os = |id: &str, like: &str| {
            DistroFamily::from_os_release(&format!("NAME=x\nID={id}\nID_LIKE=\"{like}\"\n"))
        };
        assert_eq!(os("ubuntu", "debian"), DistroFamily::Debian);
        assert_eq!(os("linuxmint", "ubuntu debian"), DistroFamily::Debian);
        assert_eq!(os("fedora", ""), DistroFamily::Fedora);
        assert_eq!(os("\"rocky\"", "rhel centos fedora"), DistroFamily::Fedora);
        assert_eq!(os("cachyos", "arch"), DistroFamily::Arch);
        assert_eq!(os("someos", "arch"), DistroFamily::Arch);
        assert_eq!(
            os("opensuse-tumbleweed", "opensuse suse"),
            DistroFamily::OpenSuse
        );
        assert_eq!(os("nixos", ""), DistroFamily::Unknown);

        let missing = [
            Prerequisite::Libunwind,
            Prerequisite::XWayland,
            Prerequisite::VulkanDriver,
        ];
        assert_eq!(
            install_command(DistroFamily::Debian, &missing).as_deref(),
            Some("sudo apt install libunwind8 xwayland mesa-vulkan-drivers")
        );
        assert_eq!(
            install_command(DistroFamily::Fedora, &missing).as_deref(),
            Some("sudo dnf install libunwind xorg-x11-server-Xwayland mesa-vulkan-drivers")
        );
        assert_eq!(
            install_command(DistroFamily::Arch, &missing).as_deref(),
            Some("sudo pacman -S --needed libunwind xorg-xwayland vulkan-radeon vulkan-intel")
        );
        assert_eq!(
            install_command(DistroFamily::OpenSuse, &missing).as_deref(),
            Some("sudo zypper install libunwind xwayland libvulkan_radeon libvulkan_intel")
        );
        // Unknown distributions and non-package gaps get words, no command.
        assert_eq!(install_command(DistroFamily::Unknown, &missing), None);
        assert_eq!(
            install_command(DistroFamily::Debian, &[Prerequisite::DisplaySession]),
            None
        );
        // One package is named once.
        assert_eq!(
            install_command(
                DistroFamily::Debian,
                &[Prerequisite::VulkanDriver, Prerequisite::HardwareGraphics]
            )
            .as_deref(),
            Some("sudo apt install mesa-vulkan-drivers")
        );
        // Each 64-bit library is named on its own with only its package;
        // Wine's own unnamed missing library offers no command.
        let wine_libraries = WineLibrary::ALL.map(Prerequisite::WineLibrary);
        assert_eq!(
            install_command(DistroFamily::Debian, &wine_libraries).as_deref(),
            Some("sudo apt install libx11-6 libxext6 libfreetype6 libfontconfig1 libegl1")
        );
        assert_eq!(
            install_command(DistroFamily::Arch, &wine_libraries).as_deref(),
            Some("sudo pacman -S --needed libx11 libxext freetype2 fontconfig libglvnd mesa")
        );
        assert_eq!(
            install_command(
                DistroFamily::Debian,
                &[Prerequisite::WineLibrary(WineLibrary::OpenGl)]
            )
            .as_deref(),
            Some("sudo apt install libegl1")
        );
        assert_eq!(
            install_command(DistroFamily::Debian, &[Prerequisite::Libraries]),
            None
        );
        for prerequisite in missing {
            let plain = prerequisite.plain().to_lowercase();
            assert!(!plain.contains("host") && !plain.contains("capabilit"));
        }
    }

    #[test]
    fn closing_asks_first_while_work_runs_and_waits_for_the_stop() {
        let idle = ready(UpdateStatus::UpToDate);
        assert_eq!(close_decision(&idle, false), CloseDecision::Close);
        let installing = UiSnapshot::initial()
            .with_host(HostView::Supported)
            .with_active_job(Some(ActiveJobView::InstallBaseGame))
            .with_operation(Some(OperationView::mutating(
                OperationPhase::BaseGameInstall,
                OperationStatusView::Running,
                "Installing Diablo II",
            )));
        assert_eq!(close_decision(&installing, false), CloseDecision::Confirm);
        assert_eq!(
            close_warning(&installing),
            "Installation is still running. Closing stops it at a safe point; your downloads are kept, but you'll need to enter your CD keys again."
        );
        // After "Stop and close" the window waits for the safe point...
        assert_eq!(
            close_decision(&installing, true),
            CloseDecision::WaitForStop
        );
        let stopping = installing
            .clone()
            .with_operation(Some(OperationView::mutating(
                OperationPhase::Verification,
                OperationStatusView::SafeStopping,
                "Stopping at a safe point",
            )));
        assert_eq!(close_decision(&stopping, true), CloseDecision::WaitForStop);
        // ...and closes once the work stopped.
        let stopped = installing.with_operation(Some(OperationView::mutating(
            OperationPhase::Verification,
            OperationStatusView::Cancelled,
            "Operation complete",
        )));
        assert_eq!(close_decision(&stopped, true), CloseDecision::Close);
    }

    #[test]
    fn sizes_and_times_read_like_a_download_page() {
        assert_eq!(human_bytes(512), "512 bytes");
        assert_eq!(human_bytes(4_096), "4.1 KB");
        assert_eq!(human_bytes(1_551_865_181), "1.6 GB");
        assert_eq!(human_bytes(5_300_000), "5.3 MB");
        assert_eq!(human_duration(30), "less than a minute");
        assert_eq!(human_duration(60), "1 minute");
        assert_eq!(human_duration(61), "2 minutes");
        assert_eq!(human_duration(20 * 60), "20 minutes");
        assert_eq!(human_duration(2 * 3600), "2 hours");
        assert_eq!(human_duration(65 * 60), "65 minutes");
        assert_eq!(human_duration(95 * 60), "1 hour 35 minutes");
    }

    fn fresh() -> UiSnapshot {
        UiSnapshot::initial()
            .with_host(HostView::Supported)
            .with_installation(InstallationStatus::NeedsBaseGame)
            .with_installer_media(InstallerMediaView::BlizzardDownload)
            .with_operation(None)
            .with_location(Some(LocationView {
                path: PathBuf::from("/home/deck/.pd2launcher/game"),
                default: true,
                available: true,
                previous: None,
            }))
    }

    fn downloaded() -> UiSnapshot {
        fresh().with_downloads(DownloadsView {
            complete: true,
            installers: true,
            total_bytes: 2_600_000_000,
        })
    }

    fn running(snapshot: UiSnapshot, job: ActiveJobView) -> UiSnapshot {
        snapshot.with_active_job(Some(job)).with_operation(Some(
            OperationView::mutating(
                OperationPhase::RuntimeAcquisition,
                OperationStatusView::Running,
                "Working",
            )
            .with_job(job),
        ))
    }

    fn succeeded(snapshot: UiSnapshot, job: ActiveJobView) -> UiSnapshot {
        snapshot.with_operation(Some(
            OperationView::mutating(
                OperationPhase::Verification,
                OperationStatusView::Succeeded,
                "Operation complete",
            )
            .with_job(job),
        ))
    }

    #[test]
    fn the_steps_stand_where_the_evidence_is() {
        assert_eq!(
            standing_step(&fresh().with_host(HostView::Checking)),
            SetupStep::Device
        );
        assert_eq!(standing_step(&fresh()), SetupStep::Download);
        assert_eq!(standing_step(&downloaded()), SetupStep::InstallD2);
        // Interrupted while the downloads set up the prefix: Start over is
        // at Install Diablo II, never behind an unusable Download button.
        let interrupted = fresh().with_installation(InstallationStatus::StageInterrupted(
            InterruptedStage::Prefix,
        ));
        assert_eq!(standing_step(&interrupted), SetupStep::InstallD2);
        let mut cursor = GuidedCursor::new();
        cursor.settle(&interrupted);
        assert_eq!(cursor.shown(&interrupted), SetupStep::InstallD2);
        let deck = DeckPresentation::project(&interrupted, false);
        assert_eq!(
            guided_actions(cursor, &interrupted, false, false, &deck).primary,
            ActionView::enabled(ActionId::StartOver, "Start over")
        );
        // Downloads for an existing installation serve only that choice.
        let existing_downloads = fresh().with_downloads(DownloadsView {
            complete: true,
            installers: false,
            total_bytes: 1,
        });
        assert_eq!(standing_step(&existing_downloads), SetupStep::Download);
        let existing = InstallerMediaView::Existing(ExistingInstallView {
            path: PathBuf::from("/games/Diablo II"),
            problem: None,
            copy_bytes: 1,
            in_place_possible: true,
            project_diablo: false,
        });
        assert_eq!(
            standing_step(&existing_downloads.with_installer_media(existing)),
            SetupStep::InstallD2
        );
        for (installation, step) in [
            (InstallationStatus::NeedsExpansion, SetupStep::InstallD2),
            (
                InstallationStatus::StageInterrupted(InterruptedStage::BaseGame),
                SetupStep::InstallD2,
            ),
            (InstallationStatus::NeedsClient, SetupStep::InstallPd2),
            (InstallationStatus::NeedsRuntime, SetupStep::InstallPd2),
            (InstallationStatus::Ready, SetupStep::Play),
        ] {
            assert_eq!(
                standing_step(&downloaded().with_installation(installation)),
                step,
                "{installation:?}"
            );
        }
    }

    #[test]
    fn a_first_run_starts_at_the_computer_check_even_when_the_audit_is_green() {
        let mut cursor = GuidedCursor::new();
        cursor.settle(&fresh());
        assert!(first_run(&fresh()));
        assert_eq!(cursor.shown(&fresh()), SetupStep::Device);
        // A green audit alone moves nothing: step 1 stays current and the
        // later steps can't be opened until the user continues.
        let row = |cursor: GuidedCursor, step| cursor.rail(&fresh(), step);
        assert_eq!(
            row(cursor, SetupStep::Device),
            GuidedRailRow {
                state: JourneyState::Active,
                reachable: false
            }
        );
        for step in SetupStep::ALL.into_iter().skip(1) {
            assert_eq!(
                row(cursor, step),
                GuidedRailRow {
                    state: JourneyState::Pending,
                    reachable: false
                },
                "{step:?} waits for Continue"
            );
        }
        assert!(cursor.advance(&fresh(), false));
        assert_eq!(cursor.current(), SetupStep::Setup);
        assert_eq!(row(cursor, SetupStep::Device).state, JourneyState::Complete);
        assert!(row(cursor, SetupStep::Device).reachable);
        assert_eq!(row(cursor, SetupStep::Setup).state, JourneyState::Active);
        // An unfinished check keeps the cursor unplaced.
        let mut early = GuidedCursor::new();
        early.settle(&downloaded().with_host(HostView::Checking));
        early.settle(&downloaded());
        assert_eq!(early.current(), SetupStep::InstallD2, "not a first run");
    }

    #[test]
    fn each_step_ends_with_its_result_and_the_next_opens_by_itself() {
        let mut cursor = GuidedCursor::new();
        cursor.settle(&fresh());
        assert!(cursor.advance(&fresh(), false));
        // Continue on Location & installers starts the download it chose.
        assert!(continue_starts_download(SetupStep::Setup, &fresh()));
        assert!(!continue_starts_download(SetupStep::Device, &fresh()));
        assert!(
            !continue_starts_download(SetupStep::Setup, &downloaded()),
            "already downloaded"
        );
        assert!(!continue_starts_download(
            SetupStep::Setup,
            &fresh().with_installer_media(InstallerMediaView::NotSelected)
        ));
        assert!(!continue_starts_download(
            SetupStep::Setup,
            &running(fresh(), ActiveJobView::Download)
        ));
        assert!(cursor.advance(&fresh(), false));
        assert_eq!(cursor.current(), SetupStep::Download);
        // Download is the step's own work; Continue waits for its result.
        assert_eq!(
            cursor.continue_blocker(&fresh(), false),
            Some("Download everything first.")
        );
        assert!(!cursor.advance(&fresh(), false));
        let deck = DeckPresentation::project(&fresh(), false);
        assert_eq!(
            guided_actions(cursor, &fresh(), false, false, &deck).primary,
            ActionView::enabled(ActionId::Download, "Download")
        );
        // While it runs the pane follows the job; nothing else opens.
        let downloading = running(fresh(), ActiveJobView::Download);
        cursor.settle(&downloading);
        assert_eq!(cursor.shown(&downloading), SetupStep::Download);
        assert!(!cursor.back(&downloading));
        assert!(!cursor.rail(&downloading, SetupStep::Setup).reachable);
        // The job's end lands before its outcome: the cursor waits for it.
        let ending = downloading.clone().with_active_job(None);
        cursor.settle(&ending);
        assert_eq!(cursor.shown(&ending), SetupStep::Download);
        // A finished download opens Install Diablo II.
        let done = succeeded(downloaded(), ActiveJobView::Download);
        cursor.settle(&done);
        assert_eq!(cursor.shown(&done), SetupStep::InstallD2);
        assert_eq!(
            cursor.rail(&done, SetupStep::Download).state,
            JourneyState::Complete
        );
        // Install Diablo II keeps its own guards: both keys first.
        let deck = DeckPresentation::project(&done, false);
        let actions = guided_actions(cursor, &done, false, false, &deck);
        assert_eq!(actions.primary.id, ActionId::AuthorizeAndInstall);
        assert!(!actions.primary.enabled);
        assert!(
            guided_actions(cursor, &done, false, true, &deck)
                .primary
                .enabled
        );
        assert!(!cursor.advance(&done, false), "Install Diablo II first");
        // Back still shows the download's result, with Continue.
        assert!(cursor.back(&done));
        assert_eq!(cursor.shown(&done), SetupStep::Download);
        assert_eq!(
            guided_actions(cursor, &done, false, false, &deck).primary,
            ActionView::enabled(ActionId::GuidedContinue, "Continue")
        );
        assert!(cursor.advance(&done, false));
        // Install Diablo II, then Install Project Diablo 2 opens by itself.
        cursor.settle(&running(downloaded(), ActiveJobView::InstallBaseGame));
        let base = succeeded(
            downloaded().with_installation(InstallationStatus::NeedsClient),
            ActiveJobView::InstallBaseGame,
        );
        cursor.settle(&base);
        assert_eq!(cursor.shown(&base), SetupStep::InstallPd2);
        let deck = DeckPresentation::project(&base, false);
        assert_eq!(
            guided_actions(cursor, &base, false, false, &deck).primary,
            ActionView::enabled(ActionId::InstallProjectDiablo, "Install Project Diablo 2")
        );
        // Then Play.
        cursor.settle(&running(
            downloaded().with_installation(InstallationStatus::NeedsClient),
            ActiveJobView::InstallProjectDiablo,
        ));
        let ready = succeeded(
            downloaded()
                .with_installation(InstallationStatus::Ready)
                .with_launch_preflight(true),
            ActiveJobView::InstallProjectDiablo,
        );
        cursor.settle(&ready);
        assert_eq!(cursor.shown(&ready), SetupStep::Play);
        let deck = DeckPresentation::project(&ready, false);
        let play = guided_actions(cursor, &ready, false, false, &deck);
        assert_eq!(play.primary.id, ActionId::Play);
        assert_eq!(play.back, None);
    }

    #[test]
    fn only_a_job_the_pane_followed_and_that_succeeded_opens_the_next_step() {
        // A stopped download stays where it is, with what it kept.
        let mut cursor = GuidedCursor::at(SetupStep::Download);
        cursor.settle(&running(fresh(), ActiveJobView::Download));
        let stopped = fresh().with_operation(Some(
            OperationView::mutating(
                OperationPhase::Verification,
                OperationStatusView::Cancelled,
                "Operation complete",
            )
            .with_job(ActiveJobView::Download),
        ));
        cursor.settle(&stopped);
        assert_eq!(cursor.shown(&stopped), SetupStep::Download);
        // A later success the pane did not follow (another window's, or one
        // from before a restart) moves nothing.
        let mut cursor = GuidedCursor::at(SetupStep::Download);
        let done = succeeded(downloaded(), ActiveJobView::Download);
        cursor.settle(&done);
        assert_eq!(cursor.shown(&done), SetupStep::Download);
        // A failed Install Diablo II stays on Install Diablo II.
        let mut cursor = GuidedCursor::at(SetupStep::InstallD2);
        cursor.settle(&running(downloaded(), ActiveJobView::InstallBaseGame));
        let failed = downloaded().with_operation(Some(
            OperationView::mutating(
                OperationPhase::Verification,
                OperationStatusView::Failed,
                "Operation complete",
            )
            .with_job(ActiveJobView::InstallBaseGame)
            .with_failure("base-game-key-rejected"),
        ));
        cursor.settle(&failed);
        assert_eq!(cursor.shown(&failed), SetupStep::InstallD2);
    }

    #[test]
    fn install_diablo_ii_hands_on_to_install_project_diablo_2() {
        let base = succeeded(
            downloaded().with_installation(InstallationStatus::NeedsClient),
            ActiveJobView::InstallBaseGame,
        );
        assert!(hands_on_to_project_diablo(&base));
        // A game missing only Windows components goes on the same way.
        assert!(hands_on_to_project_diablo(
            &base
                .clone()
                .with_installation(InstallationStatus::NeedsRuntime)
        ));
        // Not while anything runs, after a stop or a failure, or for any
        // other job's success.
        assert!(!hands_on_to_project_diablo(
            &base
                .clone()
                .with_active_job(Some(ActiveJobView::InstallProjectDiablo))
        ));
        let stopped = downloaded()
            .with_installation(InstallationStatus::NeedsExpansion)
            .with_operation(Some(
                OperationView::mutating(
                    OperationPhase::Verification,
                    OperationStatusView::Cancelled,
                    "Operation complete",
                )
                .with_job(ActiveJobView::InstallBaseGame),
            ));
        assert!(!hands_on_to_project_diablo(&stopped));
        assert!(!hands_on_to_project_diablo(&succeeded(
            downloaded().with_installation(InstallationStatus::NeedsClient),
            ActiveJobView::Download,
        )));
        assert!(!hands_on_to_project_diablo(
            &base.clone().with_game(GameView::Running)
        ));
    }

    #[test]
    fn guided_continue_and_back_follow_each_steps_guard() {
        let mut cursor = GuidedCursor::new();
        // Step 1: Continue waits for a passed check.
        let checking = fresh().with_host(HostView::Checking);
        assert_eq!(
            cursor.continue_blocker(&checking, false),
            Some("Wait for the check to finish.")
        );
        assert!(!cursor.advance(&checking, false));
        let missing = fresh().with_host(HostView::Unsupported);
        assert!(cursor.continue_blocker(&missing, false).is_some());
        assert!(!cursor.advance(&missing, false));
        assert_eq!(
            cursor.rail(&missing, SetupStep::Device).state,
            JourneyState::Blocked
        );
        assert!(!cursor.back(&fresh()), "no step before the first");

        // Step 2: a usable location and a way to get the game.
        assert!(cursor.advance(&fresh(), false));
        assert!(
            !cursor.advance(&fresh(), true),
            "a picked folder waits for Install here"
        );
        for media in [
            InstallerMediaView::NotSelected,
            InstallerMediaView::Invalid("installer-media-invalid"),
            InstallerMediaView::Existing(ExistingInstallView {
                path: PathBuf::from("/games/classic"),
                problem: Some(ExistingProblemView::NoExpansion),
                copy_bytes: 0,
                in_place_possible: false,
                project_diablo: false,
            }),
        ] {
            let snapshot = fresh().with_installer_media(media);
            assert!(cursor.continue_blocker(&snapshot, false).is_some());
            assert!(!cursor.advance(&snapshot, false));
        }
        assert!(!cursor.advance(&fresh().with_location(None), false));
        let folder = fresh().with_installer_media(InstallerMediaView::Validated(PathBuf::from(
            "/media/installers",
        )));
        assert!(cursor.advance(&folder, false));
        assert_eq!(cursor.current(), SetupStep::Download);
        assert!(cursor.back(&folder));
        assert_eq!(cursor.current(), SetupStep::Setup);
        assert!(cursor.back(&folder));
        assert_eq!(cursor.current(), SetupStep::Device);

        // A check that fails again brings the pane back to step 1.
        let mut ahead = GuidedCursor::at(SetupStep::Download);
        assert_eq!(ahead.shown(&missing), SetupStep::Device);
        assert!(!ahead.go_to(SetupStep::Setup, &missing));
        assert_eq!(ahead.shown(&fresh()), SetupStep::Download);
    }

    fn installing(snapshot: UiSnapshot, job: ActiveJobView, phase: OperationPhase) -> UiSnapshot {
        snapshot.with_active_job(Some(job)).with_operation(Some(
            OperationView::mutating(phase, OperationStatusView::Running, "Installing")
                .with_job(job),
        ))
    }

    fn states(rows: &[InstallTaskRow]) -> Vec<TaskState> {
        rows.iter().map(|row| row.state).collect()
    }

    #[test]
    fn install_lists_its_tasks_each_with_its_own_state() {
        use TaskState::{Done, Failed, Paused, Running, Waiting};
        let blizzard = InstallPlan::for_snapshot(&downloaded(), false);
        assert_eq!(
            blizzard.tasks(),
            [
                InstallTask::Environment,
                InstallTask::Game,
                InstallTask::Expansion,
                InstallTask::ProjectDiablo,
                InstallTask::Verify
            ]
        );
        // Before the install nothing runs: what the evidence has is done.
        assert_eq!(
            states(&install_tasks(&downloaded(), blizzard)),
            [Waiting; 5]
        );
        let set_up = downloaded().with_environment_ready(true);
        assert_eq!(
            states(&install_tasks(&set_up, blizzard)),
            [Done, Waiting, Waiting, Waiting, Waiting]
        );
        // Lord of Destruction's installer runs: everything before it is done.
        let lod = installing(
            downloaded(),
            ActiveJobView::InstallBaseGame,
            OperationPhase::ExpansionInstall,
        );
        let rows = install_tasks(&lod, blizzard);
        assert_eq!(states(&rows), [Done, Done, Running, Waiting, Waiting]);
        assert_eq!(
            rows[2].detail.as_deref(),
            Some("Running the Lord of Destruction installer")
        );
        assert_eq!(install_title(&rows, &lod), "Installing Lord of Destruction");
        // The game environment names its running part.
        let environment = downloaded()
            .with_active_job(Some(ActiveJobView::InstallBaseGame))
            .with_operation(Some(
                OperationView::mutating(
                    OperationPhase::PrefixInitialization,
                    OperationStatusView::Running,
                    "Installing",
                )
                .with_job(ActiveJobView::InstallBaseGame)
                .with_environment(EnvironmentProgress::new(
                    EnvironmentPart::Dotnet,
                    &EnvironmentPart::ALL,
                )),
            ));
        let rows = install_tasks(&environment, blizzard);
        assert_eq!(rows[0].state, Running);
        assert_eq!(rows[0].detail.as_deref(), Some("Adding .NET (3 of 4)"));
        // Project Diablo 2 after the hand-on, by its files.
        let pd2 = installing(
            downloaded()
                .with_installation(InstallationStatus::NeedsClient)
                .with_environment_ready(true),
            ActiveJobView::InstallProjectDiablo,
            OperationPhase::ClientDownload,
        );
        assert_eq!(
            states(&install_tasks(&pd2, blizzard)),
            [Done, Done, Done, Running, Waiting]
        );
        // A stop leaves a Paused row where it stopped; a failure a failed one.
        let paused = downloaded()
            .with_installation(InstallationStatus::NeedsExpansion)
            .with_environment_ready(true)
            .with_operation(Some(
                OperationView::mutating(
                    OperationPhase::Verification,
                    OperationStatusView::Cancelled,
                    "Operation complete",
                )
                .with_job(ActiveJobView::InstallBaseGame),
            ));
        let rows = install_tasks(&paused, blizzard);
        assert_eq!(states(&rows), [Done, Done, Paused, Waiting, Waiting]);
        assert_eq!(rows[2].detail.as_deref(), Some("Paused"));
        assert_eq!(install_title(&rows, &paused), "Install paused");
        let failed = downloaded()
            .with_installation(InstallationStatus::NeedsClient)
            .with_environment_ready(true)
            .with_operation(Some(
                OperationView::mutating(
                    OperationPhase::Verification,
                    OperationStatusView::Failed,
                    "Operation complete",
                )
                .with_job(ActiveJobView::InstallProjectDiablo)
                .with_failure("client-install-failed"),
            ));
        let rows = install_tasks(&failed, blizzard);
        assert_eq!(states(&rows), [Done, Done, Done, Failed, Waiting]);
        assert_eq!(install_title(&rows, &failed), "Install failed");
        // The player's own Diablo II: four tasks, copied or used in place.
        let existing =
            fresh().with_installer_media(InstallerMediaView::Existing(ExistingInstallView {
                path: PathBuf::from("/games/Diablo II"),
                problem: None,
                copy_bytes: 1,
                in_place_possible: true,
                project_diablo: false,
            }));
        let copy = InstallPlan::for_snapshot(&existing, false);
        let labels = |plan| {
            install_tasks(&existing, plan)
                .into_iter()
                .map(|row| row.label)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            labels(copy),
            [
                "Set up the game environment",
                "Copy your Diablo II",
                "Install Project Diablo 2",
                "Check the game files"
            ]
        );
        assert_eq!(
            labels(InstallPlan::for_snapshot(&existing, true))[1],
            "Use your Diablo II where it is"
        );
    }

    #[test]
    fn a_folder_with_both_games_says_so_and_install_updates_project_diablo_2() {
        let found = |project_diablo| ExistingInstallView {
            path: PathBuf::from("/games/Diablo II"),
            problem: None,
            copy_bytes: 1,
            in_place_possible: true,
            project_diablo,
        };
        assert_eq!(
            existing_found_line(&found(false)),
            "Diablo II found in /games/Diablo II"
        );
        assert_eq!(
            existing_found_line(&found(true)),
            "Diablo II and Project Diablo 2 found in /games/Diablo II. Install brings Project Diablo 2 up to date."
        );
        let snapshot = fresh().with_installer_media(InstallerMediaView::Existing(found(true)));
        let rows = install_tasks(&snapshot, InstallPlan::for_snapshot(&snapshot, false));
        assert_eq!(
            rows.iter().map(|row| row.label).collect::<Vec<_>>(),
            [
                "Set up the game environment",
                "Copy your Diablo II",
                "Update Project Diablo 2",
                "Check the game files"
            ]
        );
        let updating = snapshot
            .with_installation(InstallationStatus::NeedsClient)
            .with_environment_ready(true)
            .with_active_job(Some(ActiveJobView::InstallProjectDiablo))
            .with_operation(Some(
                OperationView::mutating(
                    OperationPhase::ClientDownload,
                    OperationStatusView::Running,
                    "Installing",
                )
                .with_job(ActiveJobView::InstallProjectDiablo),
            ));
        let plan = InstallPlan::for_snapshot(&updating, false);
        assert_eq!(
            install_title(&install_tasks(&updating, plan), &updating),
            "Updating Project Diablo 2"
        );
    }

    #[test]
    fn install_has_one_figure_weighted_by_desktop_timings() {
        let blizzard = InstallPlan::for_snapshot(&downloaded(), false);
        // Nothing while no install job runs.
        assert_eq!(install_percent(&downloaded(), blizzard, None), None);
        // The game environment set up in this install weighs 40 of 80 s.
        let d2 = installing(
            downloaded(),
            ActiveJobView::InstallBaseGame,
            OperationPhase::BaseGameInstall,
        );
        assert_eq!(install_percent(&d2, blizzard, None), Some(50));
        // A part reported nothing for fills by its typical time, up to 95 %.
        assert_eq!(
            install_percent(&d2, blizzard, Some(Duration::from_secs(4))),
            Some(55)
        );
        assert_eq!(
            install_percent(&d2, blizzard, Some(Duration::from_secs(600))),
            Some(59)
        );
        // Install Diablo II's closing check counts its last task done.
        let closing = installing(
            downloaded(),
            ActiveJobView::InstallBaseGame,
            OperationPhase::Verification,
        );
        assert_eq!(install_percent(&closing, blizzard, None), Some(76));
        // Install Project Diablo 2 goes on from there, by its files.
        let pd2 = downloaded()
            .with_installation(InstallationStatus::NeedsClient)
            .with_environment_ready(true)
            .with_active_job(Some(ActiveJobView::InstallProjectDiablo))
            .with_operation(Some(
                OperationView::mutating(
                    OperationPhase::ClientDownload,
                    OperationStatusView::Running,
                    "Installing",
                )
                .with_job(ActiveJobView::InstallProjectDiablo)
                .with_typed_progress(1, Some(2), "files"),
            ));
        assert_eq!(install_percent(&pd2, blizzard, None), Some(84));
        // With the environment set up in Download it weighs nothing: the
        // figure starts at 0 with Diablo II's installer.
        let ready = InstallPlan::for_snapshot(&downloaded().with_environment_ready(true), false);
        let d2_ready = installing(
            downloaded().with_environment_ready(true),
            ActiveJobView::InstallBaseGame,
            OperationPhase::BaseGameInstall,
        );
        assert_eq!(install_percent(&d2_ready, ready, None), Some(0));
        // The game environment never stands behind the parts it finished.
        let environment = downloaded()
            .with_active_job(Some(ActiveJobView::InstallBaseGame))
            .with_operation(Some(
                OperationView::mutating(
                    OperationPhase::PrefixInitialization,
                    OperationStatusView::Running,
                    "Installing",
                )
                .with_job(ActiveJobView::InstallBaseGame)
                .with_environment(EnvironmentProgress::new(
                    EnvironmentPart::Dotnet,
                    &EnvironmentPart::ALL,
                )),
            ));
        assert_eq!(install_percent(&environment, blizzard, None), Some(25));
        // The job's start, before its first progress names it, is not ahead.
        let start = downloaded()
            .with_active_job(Some(ActiveJobView::InstallBaseGame))
            .with_operation(Some(OperationView::mutating(
                OperationPhase::BaseGameInstall,
                OperationStatusView::Running,
                "Installing Diablo II",
            )));
        assert_eq!(install_percent(&start, blizzard, None), Some(0));
    }

    fn update_outcome(status: OperationStatusView) -> OperationView {
        OperationView::mutating(OperationPhase::Verification, status, "Operation complete")
            .with_job(ActiveJobView::Update)
    }

    #[test]
    fn update_takes_plays_main_button_and_play_stays_possible() {
        let ready = downloaded()
            .with_installation(InstallationStatus::Ready)
            .with_launch_preflight(true)
            .with_update(UpdateStatus::UpToDate);
        let main = |snapshot: &UiSnapshot, recheck| {
            let deck = DeckPresentation::project(snapshot, false);
            let update = play_update(snapshot, recheck);
            (update, play_main(snapshot, &deck, update))
        };
        // Nothing to offer: Play.
        let (update, action) = main(&ready, None);
        assert_eq!(update, PlayUpdate::None);
        assert_eq!(action, ActionView::enabled(ActionId::Play, "Play"));
        // Offered: Update takes the main button; Play stays possible.
        let available = ready.clone().with_update(UpdateStatus::Available);
        let (update, action) = main(&available, None);
        assert_eq!(update, PlayUpdate::Offered);
        assert_eq!(action, ActionView::enabled(ActionId::Update, "Update"));
        assert!(plays_without_updating(update));
        assert!(
            DeckPresentation::project(&available, false).primary.enabled,
            "Play stays available"
        );
        // Running: its figure and Stop; then stopping.
        let running = available
            .clone()
            .with_active_job(Some(ActiveJobView::Update))
            .with_operation(Some(
                OperationView::mutating(
                    OperationPhase::ClientDownload,
                    OperationStatusView::Running,
                    "Updating",
                )
                .with_job(ActiveJobView::Update),
            ));
        let (update, action) = main(&running, None);
        assert_eq!(update, PlayUpdate::Running);
        assert_eq!(action, ActionView::enabled(ActionId::SafeStop, "Stop"));
        assert!(!plays_without_updating(update));
        let stopping = running.clone().with_operation(Some(
            OperationView::mutating(
                OperationPhase::ClientDownload,
                OperationStatusView::SafeStopping,
                "Updating",
            )
            .with_job(ActiveJobView::Update),
        ));
        assert_eq!(main(&stopping, None).0, PlayUpdate::Stopping);
        assert!(!main(&stopping, None).1.enabled);
        // Stopped: Resume; failed: Try again; Play stays possible beside both.
        let stopped = available
            .clone()
            .with_operation(Some(update_outcome(OperationStatusView::Cancelled)));
        assert_eq!(
            main(&stopped, None),
            (
                PlayUpdate::Stopped,
                ActionView::enabled(ActionId::Update, "Resume")
            )
        );
        let failed = available
            .clone()
            .with_operation(Some(update_outcome(OperationStatusView::Failed)));
        assert_eq!(
            main(&failed, None),
            (
                PlayUpdate::Failed,
                ActionView::enabled(ActionId::Update, "Try again")
            )
        );
        // Not while the game or the official launcher runs.
        assert_eq!(
            main(&available.clone().with_game(GameView::Running), None).0,
            PlayUpdate::None
        );
        // A failed check never stands in Play's way.
        assert_eq!(
            main(&ready.clone().with_update(UpdateStatus::CheckFailed), None).0,
            PlayUpdate::None
        );
    }

    #[test]
    fn play_has_one_line_of_facts() {
        let ready = downloaded()
            .with_installation(InstallationStatus::Ready)
            .with_update(UpdateStatus::UpToDate);
        assert_eq!(
            play_facts(&ready, Some("AMD Radeon RX 7900 XTX ")),
            ["Glide", "AMD Radeon RX 7900 XTX", "Up to date"]
        );
        // Only what is known: no card, nothing checked yet.
        let unknown = ready
            .clone()
            .with_update(UpdateStatus::Unknown)
            .with_settings(LaunchSettingsView::new(
                RendererPreferenceView::DirectDraw,
                true,
                false,
            ));
        assert_eq!(play_facts(&unknown, Some(" ")), ["DirectDraw"]);
        for (update, fact) in [
            (UpdateStatus::Available, "Update available"),
            (UpdateStatus::Checking, "Checking for updates"),
            (UpdateStatus::CheckFailed, "Update check failed"),
        ] {
            assert_eq!(
                play_facts(&ready.clone().with_update(update), None)[1],
                fact
            );
        }
    }

    #[test]
    fn a_finished_update_is_checked_again_before_it_is_offered_again() {
        let ready = downloaded()
            .with_installation(InstallationStatus::Ready)
            .with_launch_preflight(true);
        let running = OperationView::mutating(
            OperationPhase::Promotion,
            OperationStatusView::Running,
            "Updating",
        )
        .with_job(ActiveJobView::Update);
        let done = update_outcome(OperationStatusView::Succeeded);
        // The success lands while the update still says Available.
        let landed = ready
            .clone()
            .with_update(UpdateStatus::Available)
            .with_operation(Some(done.clone()));
        let state = recheck(None, Some(&running), &landed);
        assert_eq!(state, Some(Recheck::Waiting));
        assert_eq!(play_update(&landed, state), PlayUpdate::Rechecking);
        // The check runs.
        let checking = landed.clone().with_update(UpdateStatus::Checking);
        let state = recheck(state, Some(&done), &checking);
        assert_eq!(state, Some(Recheck::Checking));
        assert_eq!(play_update(&checking, state), PlayUpdate::Rechecking);
        // Up to date: done.
        let current = landed.clone().with_update(UpdateStatus::UpToDate);
        assert_eq!(recheck(state, Some(&done), &current), None);
        // A newer update after the check is offered again.
        let newer = landed.clone().with_update(UpdateStatus::Available);
        let state = recheck(Some(Recheck::Checking), Some(&done), &newer);
        assert_eq!(state, None);
        assert_eq!(play_update(&newer, state), PlayUpdate::Offered);
        // Another operation ends it.
        assert_eq!(recheck(Some(Recheck::Waiting), Some(&done), &ready), None);
    }

    #[test]
    fn repair_says_how_the_installation_stands() {
        let ready = downloaded()
            .with_installation(InstallationStatus::Ready)
            .with_update(UpdateStatus::UpToDate);
        let rows = repair_rows(&ready);
        assert_eq!(
            rows.iter().map(|row| row.label).collect::<Vec<_>>(),
            [
                "Diablo II, Lord of Destruction and Project Diablo 2",
                "Project Diablo 2 updates",
                "This computer"
            ]
        );
        assert!(rows.iter().all(|row| row.tone == EvidenceTone::Verified));
        assert_eq!(rows[0].detail, "Installed and checked");
        for (installation, detail) in [
            (
                InstallationStatus::RepairRequired,
                "Unfinished work was recorded: Check again finishes or undoes it",
            ),
            (
                InstallationStatus::StageInterrupted(InterruptedStage::Prefix),
                "An install step was interrupted: Start over resolves it",
            ),
            (
                InstallationStatus::NeedsClient,
                "Not completely installed: Continue installation goes on where it stopped",
            ),
        ] {
            let row = &repair_rows(&ready.clone().with_installation(installation))[0];
            assert_eq!(row.detail, detail, "{installation:?}");
            assert_ne!(row.tone, EvidenceTone::Verified);
        }
        let update = &repair_rows(&ready.clone().with_update(UpdateStatus::Available))[1];
        assert_eq!(update.detail, "An update is available");
        let gone = ready.clone().with_game_folder(Some(GameFolderView {
            path: PathBuf::from("/games/Diablo II"),
            available: false,
        }));
        assert_eq!(repair_rows(&gone)[0].tone, EvidenceTone::Failure);
        let missing = ready.clone().with_host(HostView::Unsupported);
        assert_eq!(repair_rows(&missing)[2].tone, EvidenceTone::Failure);
        // Check again waits for anything that runs, and for the game.
        assert!(check_again_available(&ready));
        assert!(!check_again_available(
            &ready.clone().with_game(GameView::Running)
        ));
        assert!(!check_again_available(&running(
            ready,
            ActiveJobView::Update
        )));
    }

    #[test]
    fn four_step_marks_stand_for_the_six_steps() {
        use FlowStep::{Check, Download, Install, Play};
        let flows = SetupStep::ALL.map(FlowStep::of);
        assert_eq!(flows, [Check, Download, Download, Install, Install, Play]);
        assert_eq!(
            FlowStep::ALL.map(FlowStep::label),
            ["Check", "Download", "Install", "Play"]
        );
        assert_eq!(
            Install.steps().collect::<Vec<_>>(),
            [SetupStep::InstallD2, SetupStep::InstallPd2]
        );
        // ✓ done, the current one, ! when it stopped, a ring ahead.
        assert_eq!(step_mark(Check, Install, false), StepMark::Done);
        assert_eq!(step_mark(Install, Install, false), StepMark::Current);
        assert_eq!(step_mark(Install, Install, true), StepMark::Failed);
        assert_eq!(step_mark(Play, Install, true), StepMark::Ahead);

        let deck = |snapshot: &UiSnapshot| DeckPresentation::project(snapshot, false);
        // A computer that misses something stops Check.
        let missing = fresh().with_host(HostView::Unsupported);
        assert!(flow_failed(&missing, Check, &deck(&missing)));
        assert!(!flow_failed(&fresh(), Check, &deck(&fresh())));
        // A failed job stops its own mark only; a stop is no failure.
        let failed = |job, status| {
            downloaded().with_operation(Some(
                OperationView::mutating(OperationPhase::Verification, status, "Operation complete")
                    .with_job(job),
            ))
        };
        let install_failed = failed(ActiveJobView::InstallBaseGame, OperationStatusView::Failed);
        assert!(flow_failed(
            &install_failed,
            Install,
            &deck(&install_failed)
        ));
        assert!(!flow_failed(
            &install_failed,
            Download,
            &deck(&install_failed)
        ));
        let stopped = failed(ActiveJobView::Download, OperationStatusView::Cancelled);
        assert!(!flow_failed(&stopped, Download, &deck(&stopped)));
        let interrupted = downloaded().with_installation(InstallationStatus::StageInterrupted(
            InterruptedStage::BaseGame,
        ));
        assert!(flow_failed(&interrupted, Install, &deck(&interrupted)));

        // The running job's percent, and nothing while nothing runs.
        assert_eq!(flow_progress(&downloaded()), None);
        let downloading = fresh()
            .with_active_job(Some(ActiveJobView::Download))
            .with_operation(Some(
                OperationView::mutating(
                    OperationPhase::BaseGameDownload,
                    OperationStatusView::Running,
                    "Downloading",
                )
                .with_job(ActiveJobView::Download)
                .with_typed_progress(3, Some(4), "bytes"),
            ));
        assert_eq!(flow_progress(&downloading), Some(75));
        let unmeasured = running(fresh(), ActiveJobView::Download);
        assert_eq!(flow_progress(&unmeasured), None);
    }

    #[test]
    fn a_done_mark_opens_the_latest_of_its_steps_already_reached() {
        let snapshot = downloaded();
        let cursor = GuidedCursor::at(SetupStep::InstallD2);
        assert_eq!(
            cursor.mark_target(&snapshot, FlowStep::Check),
            Some(SetupStep::Device)
        );
        assert_eq!(
            cursor.mark_target(&snapshot, FlowStep::Download),
            Some(SetupStep::Download)
        );
        // The current step opens nothing, nor one not reached yet.
        assert_eq!(cursor.mark_target(&snapshot, FlowStep::Install), None);
        assert_eq!(cursor.mark_target(&snapshot, FlowStep::Play), None);
        // Nothing opens while a job runs.
        let busy = running(snapshot, ActiveJobView::InstallBaseGame);
        assert_eq!(cursor.mark_target(&busy, FlowStep::Check), None);
    }

    #[test]
    fn the_step_marks_open_only_steps_already_reached() {
        let snapshot = downloaded();
        let mut cursor = GuidedCursor::at(SetupStep::InstallD2);
        assert!(cursor.go_to(SetupStep::Device, &snapshot));
        assert_eq!(cursor.current(), SetupStep::Device);
        let row = |step| cursor.rail(&snapshot, step);
        assert_eq!(
            row(SetupStep::Download),
            GuidedRailRow {
                state: JourneyState::Complete,
                reachable: true
            }
        );
        assert_eq!(
            row(SetupStep::InstallD2),
            GuidedRailRow {
                state: JourneyState::Pending,
                reachable: true
            },
            "reached, not finished"
        );
        for step in [SetupStep::InstallPd2, SetupStep::Play] {
            assert!(!row(step).reachable, "{step:?} is not reached yet");
        }
        assert!(!row(SetupStep::Device).reachable, "the current step");
        assert!(cursor.go_to(SetupStep::InstallD2, &snapshot));

        // A fresh cursor can't jump ahead.
        let mut fresh_cursor = GuidedCursor::new();
        assert!(!fresh_cursor.go_to(SetupStep::Setup, &fresh()));
        assert!(!fresh_cursor.go_to(SetupStep::Download, &fresh()));
    }

    #[test]
    fn start_over_brings_the_cursor_back_to_where_the_installation_stands() {
        let mut cursor = GuidedCursor::at(SetupStep::Play);
        let ready = downloaded()
            .with_installation(InstallationStatus::Ready)
            .with_launch_preflight(true);
        cursor.settle(&ready);
        assert_eq!(cursor.shown(&ready), SetupStep::Play);
        // Start over removed the game; the client prefetch is gone, so the
        // downloads are incomplete again.
        cursor.settle(&fresh());
        assert_eq!(cursor.current(), SetupStep::Download);
        assert!(!cursor.rail(&fresh(), SetupStep::InstallD2).reachable);
        // The service reopens a step after "Stop and change".
        let mut stopped = GuidedCursor::at(SetupStep::Download);
        assert!(stopped.open(SetupStep::Setup, &fresh()));
        assert_eq!(stopped.current(), SetupStep::Setup);
        assert!(!stopped.open(SetupStep::InstallPd2, &fresh()));
    }

    #[test]
    fn returning_users_open_where_they_stand() {
        let failed_install = downloaded().with_operation(Some(
            OperationView::mutating(
                OperationPhase::BaseGameInstall,
                OperationStatusView::Failed,
                "Operation complete",
            )
            .with_job(ActiveJobView::InstallBaseGame)
            .with_failure("base-game-key-rejected"),
        ));
        let returning = [
            ("downloads done", downloaded(), SetupStep::InstallD2),
            (
                "downloading",
                running(fresh(), ActiveJobView::Download),
                SetupStep::Download,
            ),
            ("failed", failed_install, SetupStep::InstallD2),
            (
                "stopped",
                fresh().with_stop_result(Some(StopResultView::new(
                    InstallationStatus::NeedsBaseGame,
                    false,
                ))),
                SetupStep::Download,
            ),
            (
                "interrupted",
                downloaded().with_installation(InstallationStatus::StageInterrupted(
                    InterruptedStage::Expansion,
                )),
                SetupStep::InstallD2,
            ),
            (
                "client",
                fresh().with_installation(InstallationStatus::NeedsClient),
                SetupStep::InstallPd2,
            ),
            (
                "runtime",
                fresh().with_installation(InstallationStatus::NeedsRuntime),
                SetupStep::InstallPd2,
            ),
            (
                "ready",
                fresh().with_installation(InstallationStatus::Ready),
                SetupStep::Play,
            ),
            (
                "prefix already made",
                fresh().with_evidence(vec![EvidenceView::new(
                    PREFIX_EVIDENCE_LABEL,
                    "Launcher-owned Wine prefix verified",
                    EvidenceTone::Verified,
                )]),
                SetupStep::Download,
            ),
        ];
        for (name, snapshot, step) in returning {
            assert!(!first_run(&snapshot), "{name} is not a first run");
            let mut cursor = GuidedCursor::new();
            cursor.settle(&snapshot);
            assert_eq!(cursor.shown(&snapshot), step, "{name}");
        }
        // A prefix not made yet is still a first run.
        assert!(first_run(&fresh().with_evidence(vec![EvidenceView::new(
            PREFIX_EVIDENCE_LABEL,
            "Not created yet",
            EvidenceTone::Advisory,
        )])));
        // What stops the whole setup has its own view, not a step.
        for snapshot in [
            fresh().with_installation(InstallationStatus::RepairRequired),
            fresh().with_location(Some(LocationView {
                path: PathBuf::from("/run/media/deck/card/pd2-launcher"),
                default: false,
                available: false,
                previous: None,
            })),
            fresh().with_game_folder(Some(GameFolderView {
                path: PathBuf::from("/games/Diablo II"),
                available: false,
            })),
            fresh().with_operation(Some(
                OperationView::read_only(
                    OperationPhase::HostAudit,
                    OperationStatusView::Failed,
                    "Launcher service could not establish safe evidence",
                )
                .with_failure("paths-unavailable"),
            )),
        ] {
            assert!(!guide_applies(&snapshot));
        }
        assert!(guide_applies(&fresh()));
    }

    #[test]
    fn the_game_folder_is_the_managed_one_or_the_one_used_in_place() {
        assert_eq!(game_files_folder(&fresh()), None, "not installed yet");
        let installed = fresh().with_installation(InstallationStatus::Ready);
        assert_eq!(
            game_files_folder(&installed),
            Some(PathBuf::from(
                "/home/deck/.pd2launcher/game/prefix/drive_c/Program Files (x86)/Diablo II"
            ))
        );
        assert!(
            game_files_folder(&fresh().with_installation(InstallationStatus::NeedsClient))
                .is_some()
        );
        let in_place = |available| {
            installed.clone().with_game_folder(Some(GameFolderView {
                path: PathBuf::from("/games/Diablo II"),
                available,
            }))
        };
        assert_eq!(
            game_files_folder(&in_place(true)),
            Some(PathBuf::from("/games/Diablo II"))
        );
        assert_eq!(game_files_folder(&in_place(false)), None);
        assert_eq!(
            game_files_folder(&installed.with_host(HostView::Checking)),
            None
        );
    }

    #[test]
    fn about_names_the_version_and_what_the_game_runs_with() {
        assert!(about_version().starts_with(env!("CARGO_PKG_VERSION")));
        let built_with = about_built_with();
        assert!(built_with.starts_with(crate::runtime::current().display_name));
        assert_eq!(
            built_with.contains("FEX-Emu"),
            cfg!(target_arch = "aarch64"),
            "{built_with}"
        );
    }

    #[test]
    fn only_a_copied_import_says_the_original_folder_can_go() {
        let existing =
            fresh().with_installer_media(InstallerMediaView::Existing(ExistingInstallView {
                path: PathBuf::from("/games/Diablo II"),
                problem: None,
                copy_bytes: 1,
                in_place_possible: true,
                project_diablo: false,
            }));
        // Not before the import has finished.
        assert_eq!(copied_folder_note(&existing), None);
        let imported = existing.with_installation(InstallationStatus::NeedsClient);
        assert_eq!(
            copied_folder_note(&imported).as_deref(),
            Some(
                "The launcher has its own copy of Diablo II now, so you can safely delete your folder at /games/Diablo II if you don't need it anymore."
            )
        );
        assert!(
            copied_folder_note(
                &imported
                    .clone()
                    .with_installation(InstallationStatus::Ready)
            )
            .is_some()
        );
        // Used where it is: that folder is the game.
        assert_eq!(
            copied_folder_note(&imported.with_game_folder(Some(GameFolderView {
                path: PathBuf::from("/games/Diablo II"),
                available: true,
            }))),
            None
        );
        // Installed from Blizzard's installers: no folder of the player's.
        assert_eq!(
            copied_folder_note(&fresh().with_installation(InstallationStatus::NeedsClient)),
            None
        );
    }

    #[test]
    fn the_cd_keys_are_done_with_once_both_games_are_in_or_their_install_failed() {
        assert!(!keys_spent(&fresh(), false));
        assert!(!keys_spent(
            &fresh().with_installation(InstallationStatus::NeedsExpansion),
            false
        ));
        for installed in [
            InstallationStatus::NeedsClient,
            InstallationStatus::NeedsRuntime,
            InstallationStatus::Ready,
        ] {
            assert!(keys_spent(&fresh().with_installation(installed), false));
        }
        let ended = |status| {
            downloaded().with_operation(Some(
                OperationView::mutating(
                    OperationPhase::BaseGameInstall,
                    status,
                    "Operation complete",
                )
                .with_job(ActiveJobView::InstallBaseGame),
            ))
        };
        let failed = ended(OperationStatusView::Failed);
        assert!(install_diablo_failed(&failed));
        assert!(keys_spent(&failed, false), "a failure that just happened");
        assert!(!keys_spent(&failed, true), "the same failure again");
        assert!(!keys_spent(&ended(OperationStatusView::Cancelled), false));
        assert!(!install_diablo_failed(&ended(
            OperationStatusView::Cancelled
        )));
    }

    #[test]
    fn download_lists_its_items_each_with_its_own_state() {
        use DownloadItem::{Diablo, Environment, ProjectDiablo, Windows};
        use DownloadItemState::{Done, NotNeeded, Running, Waiting};
        let rows = |snapshot: &UiSnapshot| {
            download_rows(snapshot)
                .into_iter()
                .map(|row| (row.item, row.state))
                .collect::<Vec<_>>()
        };
        let at = |snapshot: UiSnapshot, phase| {
            snapshot
                .with_active_job(Some(ActiveJobView::Download))
                .with_operation(Some(
                    OperationView::mutating(phase, OperationStatusView::Running, "Downloading")
                        .with_job(ActiveJobView::Download),
                ))
        };

        // Before the start: what comes down, the game environment only for
        // Blizzard's downloader, which runs in it.
        assert_eq!(
            rows(&fresh()),
            [
                (Windows, Waiting),
                (Environment, Waiting),
                (Diablo, Waiting),
                (ProjectDiablo, Waiting)
            ]
        );
        assert_eq!(
            rows(&fresh().with_environment_ready(true))[1],
            (Environment, Done)
        );
        // While it runs: done up to the item its phase works on.
        assert_eq!(
            rows(&at(fresh(), OperationPhase::RuntimeAcquisition)),
            [
                (Windows, Running),
                (Environment, Waiting),
                (Diablo, Waiting),
                (ProjectDiablo, Waiting)
            ]
        );
        let environment = at(fresh(), OperationPhase::PrefixInitialization);
        let environment = environment.clone().with_operation(Some(
            environment
                .operation
                .clone()
                .unwrap()
                .with_environment(EnvironmentProgress::new(
                    EnvironmentPart::WebComponents,
                    &EnvironmentPart::ALL,
                )),
        ));
        let shown = download_rows(&environment);
        assert_eq!(shown[0].state, Done);
        assert_eq!(shown[1].state, Running);
        assert_eq!(shown[1].detail, "Adding web components (2 of 4)");
        assert_eq!(
            rows(&at(fresh(), OperationPhase::ExpansionDownload)),
            [
                (Windows, Done),
                (Environment, Done),
                (Diablo, Running),
                (ProjectDiablo, Waiting)
            ]
        );
        let checking = download_rows(&at(fresh(), OperationPhase::Verification));
        assert_eq!(checking[3].state, Running);
        assert_eq!(checking[3].detail, "Checking the downloads…");
        // Verified: all done, and the step's result holds.
        assert_eq!(
            rows(&downloaded()),
            [
                (Windows, Done),
                (Environment, Done),
                (Diablo, Done),
                (ProjectDiablo, Done)
            ]
        );
        assert_eq!(
            download_rows(&downloaded())[2].detail,
            "Downloaded and checked"
        );

        // Installer files from a folder are copied; no downloader runs.
        let folder = fresh().with_installer_media(InstallerMediaView::Validated(PathBuf::from(
            "/media/installers",
        )));
        assert_eq!(
            rows(&folder),
            [
                (Windows, Waiting),
                (Diablo, Waiting),
                (ProjectDiablo, Waiting)
            ]
        );
        assert_eq!(
            download_rows(&at(folder, OperationPhase::BaseGameDownload))[1].detail,
            "Copying from your installer files…"
        );
        // The player's own Diablo II needs no download.
        let existing =
            fresh().with_installer_media(InstallerMediaView::Existing(ExistingInstallView {
                path: PathBuf::from("/games/Diablo II"),
                problem: None,
                copy_bytes: 1,
                in_place_possible: true,
                project_diablo: false,
            }));
        let shown = download_rows(&existing);
        assert_eq!(
            rows(&existing),
            [
                (Windows, Waiting),
                (Diablo, NotNeeded),
                (ProjectDiablo, Waiting)
            ]
        );
        assert_eq!(
            shown[1].detail,
            "Not needed: you're using your own at /games/Diablo II"
        );
    }

    #[test]
    fn the_game_environment_names_its_running_part() {
        let all = EnvironmentPart::ALL;
        for (part, line) in [
            (EnvironmentPart::Windows, "Setting up Windows (1 of 4)"),
            (
                EnvironmentPart::WebComponents,
                "Adding web components (2 of 4)",
            ),
            (EnvironmentPart::Dotnet, "Adding .NET (3 of 4)"),
            (
                EnvironmentPart::LauncherSupport,
                "Adding official launcher support (4 of 4)",
            ),
        ] {
            assert_eq!(
                environment_part_line(EnvironmentProgress::new(part, &all)),
                line
            );
        }
        // An environment that lacks only some parts runs just those.
        let missing = [EnvironmentPart::Dotnet, EnvironmentPart::LauncherSupport];
        assert_eq!(
            environment_part_line(EnvironmentProgress::new(
                EnvironmentPart::LauncherSupport,
                &missing
            )),
            "Adding official launcher support (2 of 2)"
        );
        let operation = OperationView::mutating(
            OperationPhase::PrefixInitialization,
            OperationStatusView::Running,
            "Setting up the game environment",
        );
        assert_eq!(operation.environment_line(), None);
        assert_eq!(
            operation
                .with_environment(EnvironmentProgress::new(EnvironmentPart::Dotnet, &missing))
                .environment_line()
                .as_deref(),
            Some("Adding .NET (1 of 2)")
        );
    }

    #[test]
    fn the_setup_record_opens_its_page_until_the_check_ends() {
        let checking = |record| UiSnapshot::initial().with_setup_record(record);
        let cursor = GuidedCursor::new();

        // No record (or none since Start over): Check your computer.
        let none = checking(SetupRecord::None);
        assert_eq!(opening_step(&none), None);
        assert_eq!(cursor.shown(&none), SetupStep::Device);
        assert_eq!(
            DeckPresentation::project(&none, false).mode,
            DeckMode::Setup
        );

        // Installed: Play from the first frame, waiting until the check ends.
        let installed = checking(SetupRecord::Installed);
        assert_eq!(opening_step(&installed), Some(SetupStep::Play));
        assert_eq!(cursor.shown(&installed), SetupStep::Play);
        let deck = DeckPresentation::project(&installed, false);
        assert_eq!(deck.mode, DeckMode::Ready);
        assert_eq!(deck.title, "Getting ready…");
        assert_eq!(deck.primary.id, ActionId::Play);
        assert!(!deck.primary.enabled);
        let actions = guided_actions(cursor, &installed, false, false, &deck);
        assert_eq!(actions.primary, deck.primary);
        assert_eq!(actions.back, None);
        for step in [SetupStep::Device, SetupStep::InstallPd2] {
            let row = cursor.rail(&installed, step);
            assert_eq!(row.state, JourneyState::Complete, "{step:?}");
            assert!(!row.reachable, "nothing opens before the check ends");
        }

        // Started: the first install step, never the key form before the
        // check says where the installation stands.
        let started = checking(SetupRecord::Started);
        assert_eq!(cursor.shown(&started), SetupStep::InstallD2);
        let deck = DeckPresentation::project(&started, true);
        assert_eq!(deck.mode, DeckMode::Setup);
        let actions = guided_actions(cursor, &started, false, true, &deck);
        assert!(!actions.primary.enabled);
        assert_ne!(actions.primary.id, ActionId::AuthorizeAndInstall);
        assert_eq!(actions.back, None);
        assert_eq!(
            cursor.rail(&started, SetupStep::Download).state,
            JourneyState::Complete
        );
        assert_eq!(
            cursor.rail(&started, SetupStep::InstallPd2).state,
            JourneyState::Pending
        );

        // The check's evidence decides once it ends: Play, the desktop's
        // Continue installation, or Start over for an interrupted step.
        for (record, evidence, step) in [
            (
                SetupRecord::Installed,
                fresh().with_installation(InstallationStatus::Ready),
                SetupStep::Play,
            ),
            (
                SetupRecord::Installed,
                downloaded().with_installation(InstallationStatus::NeedsClient),
                SetupStep::InstallPd2,
            ),
            (
                SetupRecord::Installed,
                downloaded().with_installation(InstallationStatus::StageInterrupted(
                    InterruptedStage::BaseGame,
                )),
                SetupStep::InstallD2,
            ),
            (
                SetupRecord::Started,
                downloaded().with_installation(InstallationStatus::NeedsClient),
                SetupStep::InstallPd2,
            ),
            (SetupRecord::Started, downloaded(), SetupStep::InstallD2),
        ] {
            let mut cursor = GuidedCursor::new();
            cursor.settle(&checking(record));
            let evidence = evidence.with_setup_record(record);
            assert_eq!(opening_step(&evidence), None);
            cursor.settle(&evidence);
            assert_eq!(cursor.shown(&evidence), step, "{record:?} {evidence:?}");
        }
        // A check that finds this computer can't run the game says so.
        let unsupported = checking(SetupRecord::Installed).with_host(HostView::Unsupported);
        assert_eq!(cursor.shown(&unsupported), SetupStep::Device);
    }

    #[test]
    fn install_diablo_ii_asks_for_keys_only_when_they_are_needed() {
        let deck = |snapshot: &UiSnapshot| DeckPresentation::project(snapshot, true);
        let cursor = GuidedCursor::at(SetupStep::InstallD2);
        // Blizzard's installers: both keys.
        let keys = downloaded();
        assert_eq!(
            guided_actions(cursor, &keys, false, true, &deck(&keys)).primary,
            ActionView::enabled(ActionId::AuthorizeAndInstall, "Install Diablo II")
        );
        // Lord of Destruction left: its key is still needed.
        let expansion = downloaded().with_installation(InstallationStatus::NeedsExpansion);
        assert_eq!(
            guided_actions(cursor, &expansion, false, true, &deck(&expansion)).primary,
            ActionView::enabled(ActionId::AuthorizeAndInstall, "Install Diablo II")
        );
        // An existing installation: no keys, and only a usable folder.
        let existing = |problem| {
            fresh()
                .with_installer_media(InstallerMediaView::Existing(ExistingInstallView {
                    path: PathBuf::from("/games/Diablo II"),
                    problem,
                    copy_bytes: 1,
                    in_place_possible: true,
                    project_diablo: false,
                }))
                .with_downloads(DownloadsView {
                    complete: true,
                    installers: false,
                    total_bytes: 1,
                })
        };
        let usable = existing(None);
        assert_eq!(
            guided_actions(cursor, &usable, false, false, &deck(&usable)).primary,
            ActionView::enabled(ActionId::ImportExisting, "Install Diablo II")
        );
        let unusable = existing(Some(ExistingProblemView::NoExpansion));
        assert!(
            !guided_actions(cursor, &unusable, false, false, &deck(&unusable))
                .primary
                .enabled
        );
        // Interrupted: Start over.
        let interrupted = downloaded().with_installation(InstallationStatus::StageInterrupted(
            InterruptedStage::BaseGame,
        ));
        assert_eq!(
            guided_actions(cursor, &interrupted, false, true, &deck(&interrupted)).primary,
            ActionView::enabled(ActionId::StartOver, "Start over")
        );
    }

    #[test]
    fn existing_install_problems_are_named_in_player_terms() {
        for (problem, words) in [
            (
                ExistingProblemView::NoExpansion,
                "Lord of Destruction isn't installed here",
            ),
            (
                ExistingProblemView::MissingFile("d2music.mpq"),
                "d2music.mpq",
            ),
            (ExistingProblemView::Links, "links to other places"),
            (ExistingProblemView::NotDiablo, "Game.exe"),
        ] {
            assert!(problem.message().contains(words), "{problem:?}");
        }
    }
}
