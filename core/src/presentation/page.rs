//! The launcher's game page, rendered with GPUI Kit components.
//!
//! A shell that fills the window: a header that
//! is also the title bar, with the launcher's mark, the four step marks
//! (Check · Download · Install · Play) during setup and the window buttons;
//! a task pane that shows only the current task; and no footer: Back sits in
//! the bottom-left corner and the page's one action in the bottom-right.
//! Once the game is installed, Play stands alone in the middle with the
//! destinations along its foot. Every action and its gating comes from
//! [`DeckPresentation`]; this module only restates it in plain player
//! language and emits the same intents.

use gpui_kit::{
    AnyElement, App, Context, CursorStyle, Decorations, Div, ElementId, Hsla, IntoElement,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Render, ResizeEdge,
    SharedString, Styled, Tiling, Window, WindowControls,
    base::TestSupportExt,
    component::{
        ActiveTheme, Disableable, IconName, Sizable, StyledExt,
        accordion::Accordion,
        alert::Alert,
        button::{Button, ButtonCustomVariant, ButtonVariants},
        checkbox::Checkbox,
        h_flex,
        input::Input,
        progress::Progress,
        radio::{Radio, RadioGroup},
        scroll::ScrollableElement,
        switch::Switch,
        tag::Tag,
        tooltip::Tooltip,
        v_flex,
    },
    div,
    prelude::*,
    px,
};

use super::{
    ActionId, ActionView, ActiveJobView, AuthorizationView, DeckPresentation, Destination,
    DownloadItemState, DownloadRowView, EvidenceTone, FIELD_HEIGHT, FlowStep, FolderOpenView,
    GameView, HostView, ImportModeView, InstallPlan, InstallerMediaView, LauncherView,
    OperationPhase, OperationStatusView, OperationView, RendererPreferenceView, ReportView,
    ResultCardView, ResultNextStep, SAFE_STOP_NOTE, SavesPutAsideView, SettingsChange,
    SettingsPersistenceView, SetupStep, SourceBadgeView, StepAffordance, StepMark, StopResultView,
    TaskState, UiSnapshot, about_built_with, about_version, blizzard_windows_notice,
    check_again_available, close_warning, copied_folder_note, download_rows, existing_found_line,
    flow_failed, flow_progress, game_files_folder, guide_applies, guided_actions, human_bytes,
    install_command, install_percent, install_tasks, install_title, key_problem,
    official_launcher_available, official_launcher_offered, opening_step, repair_rows, result_card,
    running_step, service_failure, standing_step, start_over_available, step_affordance, step_mark,
    theme::palette,
};
use super::{PlayUpdate, about_terms, play_facts, play_main, play_update, plays_without_updating};
use crate::sources::DownloadArtifact;
use crate::{InstallationStatus, InterruptedStage, UpdateStatus};

/// Diagnostics → Report a problem.
const REPORT_COPY: &str = "Creates a file you can send to the PD2 Launcher testers to help fix problems. It doesn't include your CD keys.";

/// `path` for display: under the home it starts with `~`.
fn display_path(path: &std::path::Path) -> String {
    let text = path.to_string_lossy().into_owned();
    match std::env::var("HOME") {
        Ok(home) if home.len() > 1 => match text.strip_prefix(home.trim_end_matches('/')) {
            Some(rest) if rest.starts_with('/') => format!("~{rest}"),
            _ => text,
        },
        _ => text,
    }
}

/// The title bar's brand mark: the launcher's icon (the gold numeral II in
/// the teal waypoint ring), from the same SVG the app menu entry installs.
fn brand_mark() -> std::sync::Arc<gpui_kit::Image> {
    static MARK: std::sync::LazyLock<std::sync::Arc<gpui_kit::Image>> =
        std::sync::LazyLock::new(|| {
            std::sync::Arc::new(gpui_kit::Image::from_bytes(
                gpui_kit::ImageFormat::Svg,
                crate::app_menu::ICON.to_vec(),
            ))
        });
    MARK.clone()
}

/// Shown on the first setup screen and in Settings → About.
const NOT_AFFILIATED: &str = "PD2 Launcher is an unofficial, community-made tool. It is not affiliated with or endorsed by the Project Diablo 2 team or Blizzard Entertainment.";

const HEADER_HEIGHT: f32 = 44.0;
/// A title bar window button's hit area.
const WINDOW_BUTTON_WIDTH: f32 = 36.0;
const WINDOW_BUTTON_HEIGHT: f32 = 30.0;
/// How far inside the window edge a press resizes it, and the size of the
/// square that resizes from a corner.
const RESIZE_EDGE: f32 = 5.0;
const RESIZE_CORNER: f32 = 12.0;
/// Below this window width the header keeps only the current mark's name,
/// and shorter rules between the marks: every name, with the current one's
/// percent, fits beside the launcher's name and the links only above it.
const COMPACT_HEADER_WIDTH: f32 = 940.0;
/// The corner pills: their height, their margin above the window's bottom,
/// and how far above them the page fades into the canvas.
const PILL_HEIGHT: f32 = 40.0;
const PILL_MARGIN: f32 = 16.0;
const CORNER_FADE: f32 = 28.0;
/// What every page keeps below its end so its last row scrolls clear of the
/// corner pills: the pills, their margin and some air.
const CORNER_ROOM: f32 = PILL_HEIGHT + PILL_MARGIN + 20.0;
/// Play's own button, and the column of what stands around it.
const PLAY_WIDTH: f32 = 300.0;
const PLAY_HEIGHT: f32 = 56.0;
const PLAY_COLUMN: f32 = 460.0;
/// The widest the task pane's column grows, so forms never stretch.
const PANE_MAX_WIDTH: f32 = 720.0;
/// Every form label sits in one column: this share of the row, between the
/// two widths below, so a narrow window keeps room for the fields.
const LABEL_SHARE: f32 = 0.3;
const LABEL_WIDTH: f32 = 200.0;
const LABEL_MIN_WIDTH: f32 = 140.0;
/// Lines a label up with the text of a [`FIELD_HEIGHT`] control.
const LABEL_OFFSET: f32 = 6.0;
/// Lines content up with a radio button's label.
const RADIO_INDENT: f32 = 22.0;

/// The pages the header links to; Back returns to the Overview.
const SECTIONS: [Destination; 2] = [Destination::Settings, Destination::Diagnostics];

fn section_label(destination: Destination) -> &'static str {
    match destination {
        Destination::Operations => "Overview",
        Destination::Settings => "Settings",
        Destination::Diagnostics => "Diagnostics",
        Destination::About => "About",
        Destination::Repair => "Repair",
    }
}

/// What Repair's two actions do.
const REPAIR_INTRO: &str = "Check again looks the installation over again and finishes or undoes recorded work. Start over removes it so you can install again; your characters and downloads are kept.";

/// About's first line: what the launcher is.
const ABOUT_END: &str = "qbzWmZ79geA";
const ABOUT_WHAT: &str = "PD2 Launcher installs Diablo II, Lord of Destruction and Project Diablo 2 on Linux and keeps Project Diablo 2 up to date, so you can play with one button.";

#[derive(Clone, Copy, Eq, PartialEq)]
enum Tone {
    Working,
    Waiting,
    Problem,
    Ready,
}

/// The one-line status a page leads with, in player terms.
struct Status {
    tone: Tone,
    headline: &'static str,
    detail: SharedString,
}

fn mutation_active(snapshot: &UiSnapshot) -> bool {
    snapshot
        .operation
        .as_ref()
        .is_some_and(OperationView::owns_mutation)
}

fn phase_headline(phase: OperationPhase) -> &'static str {
    match phase {
        OperationPhase::HostAudit => "Checking your computer",
        OperationPhase::RuntimeAcquisition => "Downloading Windows support",
        OperationPhase::PrefixInitialization => "Setting up the game environment",
        OperationPhase::BaseGameInstall => "Installing Diablo II",
        OperationPhase::ExpansionInstall => "Installing Lord of Destruction",
        OperationPhase::BaseGameDownload => "Downloading Diablo II from Blizzard",
        OperationPhase::ExpansionDownload => "Downloading Lord of Destruction from Blizzard",
        OperationPhase::ImportGame => "Setting up your Diablo II",
        OperationPhase::ClientDownload => "Downloading Project Diablo 2 files",
        OperationPhase::Verification => "Checking the installation",
        OperationPhase::Promotion => "Installing Project Diablo 2",
        OperationPhase::Repair => "Repairing the last update",
        OperationPhase::GameExecution => "Game is running",
        OperationPhase::OfficialLauncher => "Official PD2 launcher is open",
        OperationPhase::Reset => "Starting over",
        OperationPhase::Relocation => "Changing the install location",
    }
}

/// The headline and the one line under it while work runs. Blizzard's
/// downloader and installers report no progress of their own, so their
/// steps say how long they take; the game environment names its part, and
/// an update says it is updating whatever phase it is in.
fn working_status(
    operation: &OperationView,
    snapshot: &UiSnapshot,
) -> (&'static str, SharedString) {
    if operation.status == OperationStatusView::SafeStopping {
        return ("Stopping safely…", SAFE_STOP_NOTE.into());
    }
    // Its job is on the operation from its first progress, and in the
    // snapshot from its start.
    if operation.job().or(snapshot.active_job()) == Some(ActiveJobView::Update) {
        let detail = match operation.phase {
            OperationPhase::ClientDownload => {
                "Downloading the files that changed. Keep this window open."
            }
            OperationPhase::Verification => "Checking the new files. Keep this window open.",
            OperationPhase::Promotion => "Putting the new files in place. Keep this window open.",
            _ => "Keep this window open until it finishes.",
        };
        return ("Updating Project Diablo 2", detail.into());
    }
    let media = &snapshot.installer_media;
    if let Some(part) = operation.environment_line() {
        return (
            phase_headline(OperationPhase::PrefixInitialization),
            format!("{part}. Keep this window open until it finishes.").into(),
        );
    }
    let measured = operation.progress_percent.is_some();
    let from_folder = matches!(media, InstallerMediaView::Validated(_));
    let (headline, detail) = match operation.phase {
        // Installer files the player already has are copied, not downloaded.
        OperationPhase::BaseGameDownload if from_folder => (
            "Copying the Diablo II installer",
            "From your installer files. Keep this window open.",
        ),
        OperationPhase::ExpansionDownload if from_folder => (
            "Copying the Lord of Destruction installer",
            "From your installer files. Keep this window open.",
        ),
        OperationPhase::BaseGameDownload if measured => (
            "Downloading Diablo II from Blizzard",
            "About 1.6 GB. Keep this window open.",
        ),
        OperationPhase::BaseGameDownload => (
            "Downloading Diablo II from Blizzard",
            "Blizzard's downloader is starting. This can look stuck for a minute. Keep this window open.",
        ),
        OperationPhase::ExpansionDownload if measured => (
            "Downloading Lord of Destruction from Blizzard",
            "About 0.5 GB. Keep this window open.",
        ),
        OperationPhase::ExpansionDownload => (
            "Downloading Lord of Destruction from Blizzard",
            "Blizzard's downloader is starting. This can look stuck for a minute. Keep this window open.",
        ),
        OperationPhase::BaseGameInstall | OperationPhase::ExpansionInstall => (
            phase_headline(operation.phase),
            "Blizzard's installer is running from the verified download. This can take a few minutes and may look stuck. Keep this window open.",
        ),
        OperationPhase::ImportGame if measured => (
            "Copying your Diablo II",
            "Your own folder is only read, never changed. Keep this window open.",
        ),
        OperationPhase::GameExecution => (
            "Game is running",
            "Close the game to update or change the installation.",
        ),
        phase => (
            phase_headline(phase),
            "Keep this window open until it finishes.",
        ),
    };
    (headline, detail.into())
}

fn status(snapshot: &UiSnapshot, deck: &DeckPresentation) -> Status {
    let plain = |tone, headline, detail: &'static str| Status {
        tone,
        headline,
        detail: detail.into(),
    };
    if let Some((headline, detail)) =
        service_failure(snapshot).and_then(super::installation_busy_message)
    {
        return Status {
            tone: Tone::Problem,
            headline,
            detail: detail.into(),
        };
    }
    if let Some(code) = service_failure(snapshot) {
        return Status {
            tone: Tone::Problem,
            headline: "The launcher couldn't start",
            detail: format!(
                "The launcher couldn't start its background service (code: {code}). Open Diagnostics and choose Save report to ask for help."
            )
            .into(),
        };
    }
    // The setup record's page while the first check runs.
    if opening_step(snapshot).is_some() {
        return plain(Tone::Working, deck.title, deck.description);
    }
    if let Some(operation) = snapshot
        .operation
        .as_ref()
        .filter(|op| !op.status.is_terminal())
    {
        let (headline, detail) = working_status(operation, snapshot);
        return Status {
            tone: Tone::Working,
            headline,
            detail,
        };
    }
    if let Some(folder) = snapshot.game_folder().filter(|folder| !folder.available) {
        return Status {
            tone: Tone::Problem,
            headline: "Diablo II folder unavailable",
            detail: format!(
                "{} can't be reached or isn't the same folder. Put it back, then check again.",
                folder.path.display()
            )
            .into(),
        };
    }
    if let Some(location) = snapshot.location().filter(|location| !location.available) {
        return Status {
            tone: Tone::Problem,
            headline: "Install location unavailable",
            detail: format!(
                "{} can't be reached or isn't the launcher's folder. Reconnect the drive and check again.",
                location.path.display()
            )
            .into(),
        };
    }
    match snapshot.host {
        HostView::Checking => plain(
            Tone::Working,
            "Checking your computer",
            "This takes a few seconds. Nothing is installed or changed yet.",
        ),
        HostView::Unsupported => Status {
            tone: Tone::Problem,
            headline: "Your computer needs a few things first",
            detail: deck.description.into(),
        },
        HostView::Supported => match snapshot.installation {
            InstallationStatus::RepairRequired => plain(
                Tone::Problem,
                "Repairing the last update",
                "The launcher is finishing or undoing unfinished work. You can play once it's done.",
            ),
            InstallationStatus::Indeterminate => plain(
                Tone::Problem,
                "The launcher can't tell what's installed",
                "Nothing will be guessed or deleted. Open Diagnostics to see what was found.",
            ),
            InstallationStatus::StageInterrupted(_) => plain(
                Tone::Problem,
                "The installation was interrupted",
                "A step that installs the game stopped part way. Start over to install Diablo II again; your downloads are kept.",
            ),
            InstallationStatus::Ready if snapshot.game == GameView::Running => plain(
                Tone::Working,
                "Game is running",
                "Close the game to update or change the installation.",
            ),
            InstallationStatus::Ready => plain(
                Tone::Ready,
                "Ready to play",
                "Project Diablo 2 is installed.",
            ),
            _ if standing_step(snapshot) == SetupStep::Download => plain(
                Tone::Waiting,
                "Download everything first",
                "No CD keys are needed yet.",
            ),
            InstallationStatus::NeedsBaseGame | InstallationStatus::NeedsExpansion
                if snapshot.authorization == AuthorizationView::Absent =>
            {
                plain(
                    Tone::Waiting,
                    "Everything is downloaded",
                    "Install Diablo II next.",
                )
            }
            _ if standing_step(snapshot) == SetupStep::InstallPd2 => plain(
                Tone::Waiting,
                "Diablo II is installed",
                "Install Project Diablo 2 next. Its files are already downloaded.",
            ),
            _ => plain(
                Tone::Waiting,
                "Ready to continue installing",
                "The next steps need no input from you.",
            ),
        },
    }
}

/// The step's title and one-line introduction.
fn guided_copy(step: SetupStep) -> (&'static str, &'static str) {
    match step {
        SetupStep::Device => (
            "Check your computer",
            "First, let's make sure this computer can run the game.",
        ),
        SetupStep::Setup => (
            "Location & installers",
            "Choose where to install and how to get Diablo II. Continue starts the download.",
        ),
        SetupStep::Download => (
            "Download",
            "Everything is downloaded and checked before anything is installed.",
        ),
        SetupStep::InstallD2 => (
            "Install Diablo II",
            "Diablo II and Lord of Destruction, from the downloads.",
        ),
        SetupStep::InstallPd2 => (
            "Install Project Diablo 2",
            "Project Diablo 2 goes on top of Diablo II, from the downloads.",
        ),
        SetupStep::Play => ("Play", "Everything is ready."),
    }
}

/// A plain button label, whether it shows a loading spinner, and why it is
/// unavailable when it is.
fn present_action(action: &ActionView) -> (&'static str, bool, Option<&'static str>) {
    match (action.id, action.enabled) {
        (ActionId::None, _) => ("Checking", true, None),
        (ActionId::AuthorizeAndInstall, true) => (action.label, false, None),
        (ActionId::AuthorizeAndInstall, false) => (
            "Install Diablo II",
            false,
            Some(if action.label == "Install Diablo II" {
                INSTALL_HINT
            } else {
                action.label
            }),
        ),
        (ActionId::Download, true) => ("Download", false, None),
        (ActionId::Download, false) => (
            "Download",
            false,
            Some(if action.label == "Download" {
                "Choose how to get Diablo II first."
            } else {
                action.label
            }),
        ),
        (ActionId::ImportExisting, true) => ("Install Diablo II", false, None),
        (ActionId::ImportExisting, false) => (
            "Install Diablo II",
            false,
            Some("Choose a usable Diablo II folder first."),
        ),
        (ActionId::InstallProjectDiablo, _) => (action.label, false, None),
        (ActionId::ContinueInstallation, true) => (action.label, false, None),
        (ActionId::ContinueInstallation, false) => ("Installing", true, None),
        (ActionId::Play, true) => ("Play", false, None),
        (ActionId::Play, false) => (
            "Play",
            false,
            Some(match action.label {
                "Game is running" => "The game is already running.",
                "Official launcher is open" => "Close the official PD2 launcher first.",
                "Play unavailable during update" => "Wait for the update to finish.",
                _ => "The launcher is still checking the game before it can start.",
            }),
        ),
        (ActionId::Update, true) => ("Update", false, None),
        (ActionId::Update, false) => (
            "Update",
            false,
            Some("Wait for the current task to finish."),
        ),
        (ActionId::SafeStop, _) if action.label == "Close official launcher" => {
            ("Close official launcher", false, None)
        }
        (ActionId::SafeStop, _) => ("Stop safely", false, None),
        (ActionId::Repairing, _) => ("Repairing", true, None),
        (ActionId::OpenDiagnostics, _) => ("Open Diagnostics", false, None),
        (ActionId::ExportDiagnostics, _) => ("Save report", false, None),
        (ActionId::OpenOfficialLauncher, _) => ("Open official launcher", false, None),
        (ActionId::RetryLocation, _) => ("Check again", false, None),
        (ActionId::StartOver, true) => ("Start over…", false, None),
        (ActionId::StartOver, false) => ("Starting over", true, None),
        (ActionId::GuidedContinue, true) => ("Continue", false, None),
        // A disabled Continue carries its reason as its label.
        (ActionId::GuidedContinue, false) => ("Continue", false, Some(action.label)),
        (ActionId::GuidedBack | ActionId::ReturnToOverview, _) => ("Back", false, None),
        (ActionId::CheckAgain, _) => (
            if action.label == "Try again" {
                "Try again"
            } else {
                "Check again"
            },
            false,
            None,
        ),
    }
}

/// Why Install Diablo II is not available yet, in neutral words.
const INSTALL_HINT: &str = "Enter both CD keys and accept the terms to continue.";

fn game_label(snapshot: &UiSnapshot) -> &'static str {
    if snapshot.host != HostView::Supported {
        return "Not checked yet";
    }
    match snapshot.installation {
        InstallationStatus::NeedsBaseGame => "Not installed",
        InstallationStatus::NeedsExpansion => "Diablo II partly installed",
        InstallationStatus::NeedsClient => "Project Diablo 2 not installed",
        InstallationStatus::NeedsRuntime => "Installed; Wine components pending",
        InstallationStatus::StageInterrupted(InterruptedStage::Prefix) => "Wine setup interrupted",
        InstallationStatus::StageInterrupted(InterruptedStage::BaseGame) => {
            "Diablo II install interrupted"
        }
        InstallationStatus::StageInterrupted(InterruptedStage::Expansion) => {
            "Lord of Destruction install interrupted"
        }
        InstallationStatus::RepairRequired => "Needs repair",
        InstallationStatus::Indeterminate => "Unknown",
        InstallationStatus::Ready => "Installed",
    }
}

fn location_label(snapshot: &UiSnapshot) -> SharedString {
    match snapshot.location() {
        None => "Not checked yet".into(),
        Some(location) if !location.available => {
            format!("Unavailable: {}", location.path.display()).into()
        }
        Some(location) if location.default => {
            format!("{} (default)", location.path.display()).into()
        }
        Some(location) => location.path.display().to_string().into(),
    }
}

/// Why a location was refused, in player terms.
fn location_error_message(code: &str) -> &'static str {
    match code {
        "location-parent-invalid" => {
            "That folder can't be used: it must be a reachable folder whose path has no shortcuts (symbolic links)."
        }
        "location-target-not-empty" => {
            "That folder already has a pd2-launcher folder with files in it. Pick another folder, or empty that one first."
        }
        "location-noexec" => "That drive is mounted without permission to run programs.",
        "location-free-space" => "That drive needs at least 8 GiB free.",
        "location-not-writable" => "The launcher can't create files in that folder.",
        "location-rename-unsupported"
        | "location-locking-unsupported"
        | "location-symlinks-unsupported" => {
            "That drive lacks a file-system feature the launcher needs to keep the installation safe."
        }
        "location-case-insensitive" => {
            "That drive ignores upper and lower case in file names, which Windows programs need."
        }
        "install-location-busy" => {
            "Another launcher window is using the installation. Close it and try again."
        }
        "location-unchanged" => "That is already the install location.",
        "location-not-fresh" => {
            "The current location already holds an installation, so it isn't replaced here."
        }
        "location-previous-pending" => {
            "Remove the old installation (in Diagnostics) before moving to another drive again."
        }
        "location-previous-unavailable" => {
            "The old installation is no longer where it was, or is in use, so it was not removed."
        }
        "saves-not-kept" => {
            "The old installation was not removed: its characters couldn't be moved to their own folder first. Check that your home folder has free space, then try again."
        }
        _ => "The install location couldn't be changed. Nothing was moved.",
    }
}

fn update_label(update: UpdateStatus) -> &'static str {
    match update {
        UpdateStatus::Unknown => "Not checked yet",
        UpdateStatus::Checking => "Checking",
        UpdateStatus::UpToDate => "Latest version",
        UpdateStatus::Available => "Update available",
        UpdateStatus::CheckFailed => "Couldn't check",
    }
}

fn host_label(host: HostView) -> &'static str {
    match host {
        HostView::Checking => "Checking",
        HostView::Supported => "Supported",
        HostView::Unsupported => "Needs setup",
    }
}

fn evidence_word(tone: &EvidenceTone) -> &'static str {
    match tone {
        EvidenceTone::Verified => "Verified",
        EvidenceTone::Working => "In progress",
        EvidenceTone::Advisory => "Note",
        EvidenceTone::Failure => "Problem",
    }
}

impl LauncherView {
    /// A button for `action`; with `explain`, a disabled one also says why in
    /// a tooltip.
    fn action_button(
        &self,
        id: impl Into<gpui_kit::ElementId>,
        action: &ActionView,
        explain: bool,
        cx: &mut Context<Self>,
    ) -> Button {
        let (label, loading, reason) = present_action(action);
        let id_action = action.id;
        Button::new(id)
            .label(label)
            .loading(loading)
            .disabled(!action.enabled || loading || reason.is_some())
            .when_some(reason.filter(|_| explain), |button, reason| {
                button.tooltip(reason)
            })
            .on_click(cx.listener(move |this, _, _, cx| this.activate_action(id_action, cx)))
    }

    /// The header, which is also the window's title bar: identity on the
    /// left; during setup the four step marks; Settings and Diagnostics
    /// (except beside Play, whose foot has them); then the window buttons.
    /// Under it a rule that carries a running download's or install's
    /// progress. Its empty area moves the window, a double click there
    /// maximizes or restores it, and a right click opens the compositor's
    /// window menu.
    #[allow(clippy::too_many_arguments)]
    fn render_header(
        &mut self,
        destination: Destination,
        chrome: WindowChrome,
        snapshot: &UiSnapshot,
        deck: &DeckPresentation,
        play: bool,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Stateful<Div> {
        let controls = self.render_window_controls(chrome, cx);
        let marks = (destination == Destination::Operations && !play)
            .then(|| self.render_marks(snapshot, deck, compact, cx));
        let progress = marks.as_ref().and(self.flow_percent(snapshot));
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let foreground = theme.foreground;
        let accent = theme.primary;
        let border = theme.border;
        let links = (!play).then(|| {
            SECTIONS
                .into_iter()
                .enumerate()
                .map(|(index, section)| {
                    let selected = section == destination;
                    div()
                        .relative()
                        .h_full()
                        .flex()
                        .items_center()
                        .child(
                            Button::new(("section", index))
                                .label(section_label(section))
                                .ghost()
                                .small()
                                .text_color(if selected { foreground } else { muted })
                                .toggled(selected)
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.navigate(section, cx)),
                                ),
                        )
                        .when(selected, |link| {
                            link.child(
                                div()
                                    .absolute()
                                    .bottom_0()
                                    .left_2()
                                    .right_2()
                                    .h(px(2.0))
                                    .rounded_full()
                                    .bg(accent),
                            )
                        })
                })
                .collect::<Vec<_>>()
        });
        let identity = h_flex()
            .id("title-drag")
            .flex_1()
            .min_w_0()
            .h_full()
            .pl_4()
            .gap_2()
            .child(
                div()
                    .id("brand-mark")
                    .flex_none()
                    .size(px(24.0))
                    .child(gpui_kit::img(brand_mark()).size_full())
                    .test_support(),
            )
            .child(
                div()
                    .min_w_0()
                    .text_sm()
                    .font_semibold()
                    .truncate()
                    .child("PD2 Launcher"),
            )
            .when(chrome.draws_controls, |identity| {
                identity
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, _| {
                            if event.click_count == 2 {
                                this.title_drag = false;
                                if chrome.controls.maximize {
                                    window.zoom_window();
                                }
                            } else {
                                this.title_drag = true;
                            }
                        }),
                    )
                    // Moving hands the pointer to the compositor, so it waits
                    // for the first move: a plain click or a double click
                    // stays with the launcher.
                    .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, _| {
                        if this.title_drag && event.pressed_button == Some(MouseButton::Left) {
                            this.title_drag = false;
                            window.start_window_move();
                        }
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseUpEvent, _, _| this.title_drag = false),
                    )
                    .on_mouse_down_out(
                        cx.listener(|this, _: &MouseDownEvent, _, _| this.title_drag = false),
                    )
                    .when(chrome.controls.window_menu, |identity| {
                        identity.on_mouse_down(
                            MouseButton::Right,
                            |event: &MouseDownEvent, window, _| {
                                window.show_window_menu(event.position);
                            },
                        )
                    })
            });
        // Both sides share the room around the marks, which keeps them in
        // the middle; this side never shrinks below what it holds, so in a
        // narrow window the name gives way instead of the links running
        // under the marks.
        let trailing = h_flex()
            .flex_1()
            .h_full()
            .justify_end()
            .gap_2()
            .when_some(links, |trailing, links| {
                trailing.child(
                    h_flex()
                        .id("sections")
                        .flex_none()
                        .h_full()
                        .gap_1()
                        .children(links)
                        .test_support(),
                )
            })
            .when_some(controls, |trailing, controls| {
                trailing
                    .child(div().flex_none().w(px(1.0)).h(px(20.0)).mx_1().bg(border))
                    .child(controls)
            });
        let rule = div()
            .id("header-progress")
            .flex_none()
            .w_full()
            .h(px(if progress.is_some() { 2.0 } else { 1.0 }))
            .bg(border)
            .when_some(progress, |rule, percent| {
                rule.child(
                    div()
                        .h_full()
                        .w(gpui_kit::relative(f32::from(percent) / 100.0))
                        .bg(accent),
                )
            })
            .test_support();
        v_flex()
            .id("header")
            .flex_none()
            .bg(theme.title_bar)
            .child(
                h_flex()
                    .h(px(HEADER_HEIGHT))
                    .pr(if chrome.draws_controls {
                        px(6.0)
                    } else {
                        px(12.0)
                    })
                    .gap_3()
                    .child(identity)
                    .children(marks)
                    .child(trailing),
            )
            .child(rule)
    }

    /// Minimize, maximize or restore, and close, when the launcher draws
    /// its own title bar. A button the compositor cannot honor is left out;
    /// Close is always offered and asks first while work runs.
    fn render_window_controls(
        &mut self,
        chrome: WindowChrome,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !chrome.draws_controls {
            return None;
        }
        let (muted, foreground) = (cx.theme().muted_foreground, cx.theme().foreground);
        let quiet = ButtonCustomVariant::new(cx)
            .foreground(muted)
            .hover(palette().control_hover)
            .active(palette().control_pressed);
        let close = ButtonCustomVariant::new(cx)
            .foreground(foreground)
            .hover(palette().close_hover)
            .active(palette().close_pressed);
        let control = |id: &'static str, icon: IconName, tip: &'static str, variant| {
            Button::new(id)
                .icon(icon)
                .custom(variant)
                .tooltip(tip)
                .flex_none()
                .w(px(WINDOW_BUTTON_WIDTH))
                .h(px(WINDOW_BUTTON_HEIGHT))
        };
        let (zoom_id, zoom_icon, zoom_tip) = match chrome.maximized {
            Some(true) => ("window-restore", IconName::WindowRestore, "Restore"),
            Some(false) => ("window-maximize", IconName::WindowMaximize, "Maximize"),
            // The compositor toggles, but does not say which way.
            None => (
                "window-maximize",
                IconName::WindowMaximize,
                "Maximize or restore",
            ),
        };
        Some(
            h_flex()
                .id("window-controls")
                .flex_none()
                .gap_1()
                .when(chrome.controls.minimize, |controls| {
                    controls.child(
                        control(
                            "window-minimize",
                            IconName::WindowMinimize,
                            "Minimize",
                            quiet,
                        )
                        .on_click(|_, window, _| window.minimize_window()),
                    )
                })
                .when(chrome.controls.maximize, |controls| {
                    controls.child(
                        control(zoom_id, zoom_icon, zoom_tip, quiet)
                            .on_click(|_, window, _| window.zoom_window()),
                    )
                })
                .child(
                    control("window-close", IconName::WindowClose, "Close", close)
                        .on_click(cx.listener(|this, _, window, cx| this.close_window(window, cx))),
                )
                .test_support()
                .into_any_element(),
        )
    }

    /// The step the pane shows: the guide's, else the running job's or the
    /// one the installation stands at, and during the first check the one
    /// the setup record opens on.
    fn shown_step(&self, snapshot: &UiSnapshot) -> SetupStep {
        opening_step(snapshot)
            .or_else(|| self.guided_step(snapshot))
            .or_else(|| running_step(snapshot))
            .unwrap_or_else(|| standing_step(snapshot))
    }

    /// Whether the Overview shows Play: the game is installed and Play is
    /// the step shown, unless a job other than the game or the official
    /// launcher runs, or something stops the whole setup.
    fn play_shown(&self, snapshot: &UiSnapshot, deck: &DeckPresentation) -> bool {
        // An update runs on Play itself, its figure where "Ready to play"
        // goes.
        let updating = snapshot.active_job == Some(ActiveJobView::Update)
            || snapshot
                .operation
                .as_ref()
                .is_some_and(|operation| operation.job() == Some(ActiveJobView::Update));
        let working = !updating
            && snapshot.operation.as_ref().is_some_and(|operation| {
                operation.owns_mutation()
                    && !matches!(
                        operation.phase,
                        OperationPhase::GameExecution | OperationPhase::OfficialLauncher
                    )
            });
        let unsupported =
            snapshot.host == HostView::Unsupported && service_failure(snapshot).is_none();
        deck.mode == super::DeckMode::Ready
            && !working
            && !unsupported
            && !snapshot.location_unavailable()
            && !snapshot.game_folder_unavailable()
            && self
                .guide
                .filter(|_| guide_applies(snapshot))
                .is_none_or(|cursor| cursor.shown(snapshot) == SetupStep::Play)
    }

    /// The four step marks, Check · Download · Install · Play: ✓ done, a
    /// filled number for the current one with its percent while its work
    /// runs, ! when it stopped on a problem, a ring for those ahead. A done
    /// mark opens the latest of its steps already reached. `compact` keeps
    /// only the current mark's name.
    fn render_marks(
        &mut self,
        snapshot: &UiSnapshot,
        deck: &DeckPresentation,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current = FlowStep::of(self.shown_step(snapshot));
        let failed = flow_failed(snapshot, current, deck);
        let percent = self.flow_percent(snapshot);
        let guide = self.guide.filter(|_| guide_applies(snapshot));
        let theme = cx.theme();
        let (foreground, muted, danger, primary, on_primary) = (
            theme.foreground,
            theme.muted_foreground,
            theme.danger,
            theme.primary,
            theme.primary_foreground,
        );
        let (teal, faint, rule) = (palette().teal, palette().faint, theme.border);
        let mut marks = h_flex().id("step-marks").flex_none().items_center();
        for (index, step) in FlowStep::ALL.into_iter().enumerate() {
            let mark = step_mark(step, current, failed);
            if index > 0 {
                let (margin, length) = if compact { (6.0, 10.0) } else { (8.0, 16.0) };
                marks = marks.child(
                    div()
                        .flex_none()
                        .mx(px(margin))
                        .w(px(length))
                        .h(px(1.0))
                        .bg(if step <= current {
                            teal.opacity(0.5)
                        } else {
                            rule
                        }),
                );
            }
            let now = matches!(mark, StepMark::Current | StepMark::Failed);
            let (glyph, glyph_color, fill, ring) = match mark {
                StepMark::Done => ("✓".to_owned(), teal, gpui_kit::transparent_black(), teal),
                StepMark::Current => ((index + 1).to_string(), on_primary, primary, primary),
                StepMark::Failed => ("!".to_owned(), on_primary, danger, danger),
                StepMark::Ahead => (
                    (index + 1).to_string(),
                    faint,
                    gpui_kit::transparent_black(),
                    rule,
                ),
            };
            let label_color = match mark {
                StepMark::Current | StepMark::Failed => foreground,
                StepMark::Done => muted,
                StepMark::Ahead => faint,
            };
            let content = h_flex()
                .gap_1p5()
                .items_center()
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .size(px(20.0))
                        .rounded_full()
                        .border_1()
                        .border_color(ring)
                        .bg(fill)
                        .text_xs()
                        .font_semibold()
                        .text_color(glyph_color)
                        .child(glyph),
                )
                .when(!compact || now, |content| {
                    content.child(
                        div()
                            .flex_none()
                            .text_sm()
                            .when(now, |label| label.font_medium())
                            .text_color(label_color)
                            .child(step.label()),
                    )
                })
                .when_some(percent.filter(|_| now), |content, percent| {
                    content.child(
                        div()
                            .id("step-percent")
                            .flex_none()
                            .text_sm()
                            .text_color(muted)
                            .child(format!("· {percent}%"))
                            .test_support(),
                    )
                });
            let target = guide
                .filter(|_| mark == StepMark::Done)
                .and_then(|cursor| cursor.mark_target(snapshot, step));
            let mark = match target {
                Some(target) => Button::new(("open-step-mark", index))
                    .ghost()
                    .compact()
                    .h_auto()
                    .px_1()
                    .accessibility_label(format!("{}: done, open it again", step.label()))
                    .tooltip(format!("Open {} again", step.label()))
                    .child(content)
                    .on_click(cx.listener(move |this, _, _, cx| this.guided_go_to(target, cx)))
                    .into_any_element(),
                None => div().px_1().child(content).into_any_element(),
            };
            marks = marks.child(
                div()
                    .id(("step-mark", index))
                    .flex_none()
                    .child(mark)
                    .test_support(),
            );
        }
        marks.test_support().into_any_element()
    }

    /// What the bottom corners hold now: Back (or the way back from a page)
    /// on the left, and on the right the page's one action, or Stop while a
    /// job runs.
    fn corner_actions(
        &self,
        destination: Destination,
        snapshot: &UiSnapshot,
        deck: &DeckPresentation,
        cx: &App,
    ) -> (Option<ActionView>, Option<ActionView>) {
        if destination != Destination::Operations {
            // Repair's one action is Check again; the other pages have none.
            let action = (destination == Destination::Repair).then(|| {
                if check_again_available(snapshot) {
                    ActionView::enabled(ActionId::CheckAgain, "Check again")
                } else {
                    ActionView::disabled(
                        ActionId::CheckAgain,
                        "Wait for the current task to finish.",
                    )
                }
            });
            return (
                Some(ActionView::enabled(ActionId::ReturnToOverview, "Back")),
                action,
            );
        }
        let stop = deck
            .secondary
            .iter()
            .find(|action| action.id == ActionId::SafeStop)
            .filter(|_| mutation_active(snapshot))
            .cloned();
        let guided = self
            .guided_step(snapshot)
            .zip(self.guide)
            .map(|(_, cursor)| {
                guided_actions(
                    cursor,
                    snapshot,
                    self.pending_location.is_some(),
                    self.authorization_complete(cx),
                    deck,
                )
            });
        match guided {
            Some(actions) => (actions.back, Some(stop.unwrap_or(actions.primary))),
            None => (None, Some(stop.unwrap_or_else(|| deck.primary.clone()))),
        }
    }

    /// Back in the bottom-left corner and the page's action in the
    /// bottom-right, over a fade into the canvas, in the same places on
    /// every page. Every page keeps [`CORNER_ROOM`] below its end so its
    /// last row scrolls clear of them.
    fn render_corners(
        &mut self,
        back: Option<ActionView>,
        action: Option<ActionView>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let background = theme.background;
        let foreground = theme.foreground;
        let quiet = ButtonCustomVariant::new(cx)
            .color(palette().surface)
            .foreground(foreground)
            .hover(palette().raised)
            .active(palette().raised);
        let pill = |button: Button, wide: bool| {
            button
                .rounded(px(PILL_HEIGHT / 2.0))
                .h(px(PILL_HEIGHT))
                .px_6()
                .min_w(px(if wide { 132.0 } else { 104.0 }))
        };
        // No tooltip: the line under the page says why an action waits. The
        // window's single tooltip overlay is dismissed only when its trigger
        // sees the pointer leave, which a touch screen never sends.
        let back = back.map(|back| {
            pill(
                self.action_button("back-action", &back, false, cx)
                    .custom(quiet)
                    .border_1()
                    .border_color(palette().edge),
                false,
            )
        });
        let action = action.map(|action| {
            let main = !matches!(action.id, ActionId::SafeStop);
            let id = if main {
                "primary-action"
            } else {
                "stop-action"
            };
            let button = self.action_button(id, &action, false, cx);
            if main {
                pill(button.primary(), true)
            } else {
                pill(
                    button.custom(quiet).border_1().border_color(palette().edge),
                    false,
                )
            }
        });
        div()
            .id("corner-actions")
            .absolute()
            .bottom_0()
            .left_0()
            .right_0()
            .h(px(CORNER_FADE + PILL_HEIGHT + PILL_MARGIN))
            .bg(gpui_kit::linear_gradient(
                180.0,
                gpui_kit::linear_color_stop(background.opacity(0.0), 0.0),
                gpui_kit::linear_color_stop(background, CORNER_FADE / (CORNER_FADE + PILL_HEIGHT)),
            ))
            .child(
                h_flex()
                    .absolute()
                    .bottom(px(PILL_MARGIN))
                    .left_0()
                    .right_0()
                    .px_6()
                    .gap_3()
                    .items_center()
                    .children(back)
                    .child(div().flex_1())
                    .children(action),
            )
            .test_support()
            .into_any_element()
    }

    /// Why the page's action waits, under its cause: amber when it waits on
    /// the player (keys to type, a choice to make), muted when it waits on
    /// the launcher.
    fn wait_note(
        &self,
        action: Option<&ActionView>,
        snapshot: &UiSnapshot,
        cx: &App,
    ) -> Option<AnyElement> {
        let action = action.filter(|action| !action.enabled)?;
        let (_, _, reason) = present_action(action);
        let reason = reason?;
        let on_launcher = snapshot.host == HostView::Checking
            || matches!(
                action.id,
                ActionId::Play | ActionId::Update | ActionId::None
            );
        let color = if on_launcher {
            cx.theme().muted_foreground
        } else {
            cx.theme().warning
        };
        Some(
            div()
                .id("wait-note")
                .pt_4()
                .text_sm()
                .text_color(color)
                .child(reason)
                .test_support()
                .into_any_element(),
        )
    }

    /// Play: alone in the middle of the page with what stands above it, the
    /// update and the official launcher's quieter buttons under it, and the
    /// destinations along the foot.
    fn render_play_page(
        &mut self,
        snapshot: &UiSnapshot,
        deck: &DeckPresentation,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let (muted, danger, border) = (theme.muted_foreground, theme.danger, theme.border);
        let foreground = theme.foreground;
        let status = status(snapshot, deck);
        let custom_sources = snapshot.custom_sources_active().then(|| {
            Alert::warning("custom-sources-active", CUSTOM_SOURCES_BANNER)
                .small()
                .into_any_element()
        });
        let update = play_update(snapshot, self.update_recheck);
        let main = play_main(snapshot, deck, update);
        let working = matches!(update, PlayUpdate::Running | PlayUpdate::Stopping);
        let official_open = snapshot.operation.as_ref().is_some_and(|operation| {
            operation.owns_mutation() && operation.phase == OperationPhase::OfficialLauncher
        });
        // A failed update's card tries the update again, not Play.
        let mut card_deck = deck.clone();
        if update == PlayUpdate::Failed {
            card_deck.primary = main.clone();
        }
        // A stopped update says so above Resume; its card would repeat it.
        let result = result_card(snapshot)
            .filter(|_| !mutation_active(snapshot) && update != PlayUpdate::Stopped)
            .map(|card| self.render_result_card(card, snapshot, &card_deck, cx));
        let quiet = ButtonCustomVariant::new(cx)
            .color(palette().surface)
            .foreground(foreground)
            .hover(palette().raised)
            .active(palette().raised);
        let (label, loading, reason) = match main.id {
            ActionId::Update | ActionId::SafeStop => (main.label, false, None),
            _ => present_action(&main),
        };
        let main_id = main.id;
        let play = Button::new("primary-action")
            .when(working, |button| {
                button.custom(quiet).border_1().border_color(palette().edge)
            })
            .when(!working, |button| button.primary())
            .loading(loading)
            // No tooltip: what stands above Play already says why it waits,
            // and a tooltip stays on a touch screen once the button changed.
            .disabled(!main.enabled || loading || reason.is_some())
            .accessibility_label(label)
            .rounded(px(PLAY_HEIGHT / 2.0))
            .w(px(PLAY_WIDTH))
            .h(px(PLAY_HEIGHT))
            .child(div().text_xl().font_bold().child(label))
            .on_click(cx.listener(move |this, _, _, cx| this.activate_action(main_id, cx)));
        // Play stays possible beside an update that does not run.
        let play_without = (plays_without_updating(update) && deck.primary.enabled).then(|| {
            Button::new("play-without-updating")
                .label("Play without updating")
                .ghost()
                .small()
                .text_color(palette().teal)
                .on_click(cx.listener(|this, _, _, cx| this.activate_action(ActionId::Play, cx)))
        });
        // What stands above Play: how it stands, or the update's figure.
        let above = if working {
            let operation = snapshot.operation.as_ref();
            let percent = operation.and_then(OperationView::progress_percent);
            let caption = operation.and_then(|operation| {
                operation.progress_summary().map(str::to_owned).or_else(|| {
                    match operation.phase() {
                        OperationPhase::Verification => Some("Checking the new files…".to_owned()),
                        OperationPhase::Promotion => {
                            Some("Putting the new files in place…".to_owned())
                        }
                        _ => Some("Checking which files changed…".to_owned()),
                    }
                })
            });
            let bar = match percent {
                Some(percent) => Progress::new("update-progress")
                    .value(f32::from(percent))
                    .accessibility_label(format!("{percent} percent")),
                None => Progress::new("update-progress")
                    .loading(true)
                    .accessibility_label("Updating"),
            };
            v_flex()
                .id("update-figure")
                .w_full()
                .max_w(px(PLAY_COLUMN - 60.0))
                .gap_1p5()
                .child(
                    h_flex()
                        .items_baseline()
                        .gap_3()
                        .child(
                            div().h(px(40.0)).text_3xl().font_light().child(
                                percent
                                    .map(|percent| format!("{percent}%"))
                                    .unwrap_or_default(),
                            ),
                        )
                        .child(div().flex_1())
                        .child(div().text_sm().text_color(muted).child(
                            if update == PlayUpdate::Stopping {
                                "Stopping safely"
                            } else {
                                "Updating Project Diablo 2"
                            },
                        )),
                )
                .child(bar)
                .children(
                    caption
                        .map(|caption| div().text_xs().text_color(muted).truncate().child(caption)),
                )
                .test_support()
                .into_any_element()
        } else {
            let headline = match update {
                PlayUpdate::Offered | PlayUpdate::Failed => "Project Diablo 2 has an update",
                PlayUpdate::Stopped => "Update stopped",
                PlayUpdate::Rechecking => "Checking for updates…",
                PlayUpdate::None | PlayUpdate::Running | PlayUpdate::Stopping => status.headline,
            };
            div()
                .id("play-headline")
                .text_base()
                .font_semibold()
                .text_color(
                    if status.tone == Tone::Problem && update == PlayUpdate::None {
                        danger
                    } else {
                        muted
                    },
                )
                .child(headline)
                .test_support()
                .into_any_element()
        };
        let secondary = deck
            .secondary
            .iter()
            // The update's own steps are the main button's now, and closing
            // the official launcher is its own button's.
            .filter(|action| {
                action.id != ActionId::Update
                    && !(working && action.id == ActionId::SafeStop)
                    && !(official_open && action.id == ActionId::SafeStop)
            })
            .enumerate()
            .map(|(index, action)| {
                let id = if index == 0 {
                    "secondary-action"
                } else {
                    "tertiary-action"
                };
                self.action_button(id, action, true, cx)
                    .custom(quiet)
                    .border_1()
                    .border_color(palette().edge)
                    .rounded(px(PILL_HEIGHT / 2.0))
                    .w(px(PLAY_WIDTH))
                    .h(px(PILL_HEIGHT))
            })
            .collect::<Vec<_>>();
        // Project Diablo 2's own launcher, for loot filters: Play's second
        // button, which closes it again while it is open.
        let official = official_launcher_offered(snapshot).then(|| {
            let available = official_launcher_available(snapshot);
            v_flex()
                .items_center()
                .gap_1()
                .child(
                    Button::new("official-launcher")
                        .label(if official_open {
                            "Close official launcher"
                        } else {
                            "Official PD2 launcher"
                        })
                        .custom(quiet)
                        .border_1()
                        .border_color(palette().edge)
                        .rounded(px(PILL_HEIGHT / 2.0))
                        .w(px(PLAY_WIDTH))
                        .h(px(PILL_HEIGHT))
                        .disabled(!official_open && !available)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.activate_action(
                                if official_open {
                                    ActionId::SafeStop
                                } else {
                                    ActionId::OpenOfficialLauncher
                                },
                                cx,
                            )
                        })),
                )
                .child(
                    div()
                        .id("official-launcher-line")
                        .text_xs()
                        .text_color(muted)
                        .child(OFFICIAL_LAUNCHER_SHORT)
                        .test_support(),
                )
        });
        let official_confirmation = self.official_launcher_confirmation(snapshot, 0, cx);
        let put_aside = snapshot
            .saves_put_aside()
            .map(|put_aside| self.render_saves_put_aside(put_aside, cx));
        let notices = v_flex()
            .w_full()
            .max_w(px(PLAY_COLUMN))
            .gap_3()
            .children(put_aside)
            .when_some(deck.game_notice, |notices, notice| {
                // A warning, not a tip: the first launch really does open at
                // the wrong size until the player fixes it in the game.
                notices.child(
                    div()
                        .id("game-notice")
                        .child(
                            Alert::warning("game-notice-alert", notice.message())
                                .title(notice.title())
                                .small(),
                        )
                        .test_support(),
                )
            })
            .when(snapshot.game_software_rendering(), |notices| {
                // Information, never a gate: the game still plays.
                notices.child(
                    div()
                        .id("software-rendering")
                        .child(
                            Alert::warning(
                                "software-rendering-alert",
                                "The game will use software rendering and may be slow. Installing your graphics card's driver usually fixes this.",
                            )
                            .title("No hardware graphics acceleration was found")
                            .small(),
                        )
                        .test_support(),
                )
            })
            .when_some(snapshot.game_not_english(), |notices, language| {
                // A warning, never a gate: the player can still play.
                notices.child(
                    div()
                        .id("game-language")
                        .child(
                            Alert::warning(
                                "game-language-alert",
                                format!(
                                    "This Diablo II is in {}. Project Diablo 2 supports only an English Diablo II: the game can stop with an error. Install Diablo II and Lord of Destruction in English to play without this problem.",
                                    language.name()
                                ),
                            )
                            .title("Diablo II is not in English")
                            .small(),
                        )
                        .test_support(),
                )
            });
        let content = v_flex()
            .id("play-surface")
            .flex_none()
            .w_full()
            .items_center()
            .gap_3()
            .children(
                custom_sources.map(|banner| div().w_full().max_w(px(PLAY_COLUMN)).child(banner)),
            )
            .children(result.map(|card| div().w_full().max_w(px(PLAY_COLUMN)).child(card)))
            .child(above)
            .child(play)
            .children(official)
            .children(secondary)
            .children(play_without)
            .children(
                official_confirmation
                    .map(|confirmation| div().w_full().max_w(px(PLAY_COLUMN)).child(confirmation)),
            )
            .child(
                div()
                    .id("play-facts")
                    .max_w(px(PLAY_COLUMN))
                    .text_sm()
                    .text_center()
                    .text_color(muted)
                    .child(play_facts(snapshot, self.gpu.as_deref()).join("  ·  "))
                    .test_support(),
            )
            .when_some(deck.advisory.clone(), |content, advisory| {
                content.child(
                    div()
                        .id("advisory")
                        .max_w(px(PLAY_COLUMN))
                        .text_xs()
                        .text_center()
                        .text_color(muted)
                        .child(advisory)
                        .test_support(),
                )
            })
            .child(notices)
            .test_support();
        let destinations = [
            ("play-settings", "Settings", Destination::Settings),
            ("play-repair", "Repair", Destination::Repair),
            ("play-diagnostics", "Diagnostics", Destination::Diagnostics),
            ("play-about", "About", Destination::About),
        ]
        .map(|(id, label, destination)| {
            Button::new(id)
                .label(label)
                .ghost()
                .flex_1()
                .h(px(PILL_HEIGHT))
                .text_color(muted)
                .on_click(cx.listener(move |this, _, _, cx| this.navigate(destination, cx)))
        });
        v_flex()
            .id("play-page")
            .size_full()
            .child(
                div()
                    .id("play-stage")
                    .flex()
                    .flex_col()
                    .items_center()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .px_6()
                    .overflow_y_scrollbar()
                    // The spacers centre what stands there; once it is
                    // taller than the room they shrink to nothing and it
                    // scrolls from its top.
                    .child(div().flex_1().min_h(px(16.0)))
                    .child(content)
                    .child(div().flex_1().min_h(px(16.0))),
            )
            .child(
                v_flex()
                    .id("play-foot")
                    .flex_none()
                    .w_full()
                    .items_center()
                    .px_6()
                    .pb_2()
                    .child(div().w_full().max_w(px(PLAY_COLUMN)).h(px(1.0)).bg(border))
                    .child(
                        h_flex()
                            .w_full()
                            .max_w(px(PLAY_COLUMN))
                            .pt_1()
                            .gap_1()
                            .children(destinations),
                    )
                    .test_support(),
            )
            .into_any_element()
    }

    /// Repair, from Play: the installation as the last check found it, Check
    /// again (in the corner) and Start over, which asks first; and Continue
    /// installation when the game is not completely installed.
    fn render_repair(
        &mut self,
        snapshot: &UiSnapshot,
        deck: &DeckPresentation,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let (muted, border) = (theme.muted_foreground, theme.border);
        let rows = repair_rows(snapshot);
        let count = rows.len();
        let rows = rows
            .into_iter()
            .enumerate()
            .map(|(index, row)| {
                let mark = match row.tone {
                    EvidenceTone::Verified => CheckMark::Passed,
                    EvidenceTone::Working => CheckMark::Pending,
                    EvidenceTone::Advisory | EvidenceTone::Failure => CheckMark::Attention,
                };
                h_flex()
                    .id(("repair-row", index))
                    .items_start()
                    .gap_3()
                    .py_2()
                    .when(index + 1 < count, |line| {
                        line.border_b_1().border_color(border)
                    })
                    .child(check_mark(mark, cx))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().font_medium().child(row.label))
                            .child(div().text_xs().text_color(muted).child(row.detail)),
                    )
                    .test_support()
            })
            .collect::<Vec<_>>();
        let available = start_over_available(snapshot);
        let confirm = (self.confirm_start_over && available)
            .then(|| self.render_start_over_confirmation("repair", snapshot, cx));
        // An installed game the check found broken goes on where it stopped.
        let continues = deck.primary.enabled
            && matches!(
                deck.primary.id,
                ActionId::InstallProjectDiablo | ActionId::ContinueInstallation
            );
        let continue_id = deck.primary.id;
        v_flex()
            .id("repair-page")
            .gap_4()
            .child(heading(
                "Your installation",
                Some(REPAIR_INTRO.into()),
                muted,
            ))
            // The question comes first, so its answers stay in view.
            .children(confirm)
            .child(v_flex().id("repair-rows").children(rows).test_support())
            .when(!self.confirm_start_over || !available, |page| {
                page.child(
                    h_flex()
                        .gap_2()
                        .flex_wrap()
                        .when(continues, |buttons| {
                            buttons.child(
                                Button::new("repair-continue")
                                    .label("Continue installation")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.navigate(Destination::Operations, cx);
                                        this.activate_action(continue_id, cx);
                                    })),
                            )
                        })
                        .child(
                            Button::new("repair-start-over")
                                .label("Start over…")
                                .danger()
                                .outline()
                                .disabled(!available)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.confirm_start_over = true;
                                    cx.notify();
                                })),
                        ),
                )
            })
            .test_support()
            .into_any_element()
    }

    /// About: what PD2 Launcher is, its version, what it is built with, and
    /// the non-affiliation notice.
    fn render_about(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        v_flex()
            .id("about-page")
            .gap_6()
            .child(
                page_section("about", "About", None)
                    .child(div().text_sm().child(ABOUT_WHAT))
                    .child(
                        facts(
                            [
                                ("Version", about_version().into()),
                                ("Built with", about_built_with().into()),
                            ],
                            muted,
                        )
                        .id("about-facts")
                        .test_support(),
                    ),
            )
            .child(
                page_section("about-terms", "How it works", Some(border)).child(
                    v_flex()
                        .id("about-terms-list")
                        .gap_2()
                        .text_sm()
                        .children(about_terms().into_iter().map(|(term, meaning)| {
                            h_flex()
                                .items_start()
                                .gap_4()
                                .child(
                                    div()
                                        .w(gpui_kit::relative(LABEL_SHARE))
                                        .max_w(px(LABEL_WIDTH))
                                        .min_w(px(LABEL_MIN_WIDTH))
                                        .flex_none()
                                        .text_color(muted)
                                        .child(term),
                                )
                                .child(div().flex_1().min_w_0().child(meaning))
                        }))
                        .test_support(),
                ),
            )
            .child(
                page_section("about-notice", "Not affiliated", Some(border))
                    .child(div().text_sm().text_color(muted).child(NOT_AFFILIATED)),
            )
            .child(
                div()
                    .id("about-end")
                    .text_xs()
                    .text_color(muted)
                    .child(ABOUT_END)
                    .test_support(),
            )
            .test_support()
            .into_any_element()
    }

    /// Questions that must stay in view whatever the page shows, just under
    /// the header: closing while work runs, and a request the service did
    /// not take.
    fn render_interrupts(
        &mut self,
        snapshot: &UiSnapshot,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let confirm_close = (self.confirm_close && !self.closing)
            .then(|| self.render_close_confirmation(snapshot, cx));
        let delivery = self
            .delivery_alert
            .map(|alert| Alert::error("delivery", alert).small());
        if confirm_close.is_none() && delivery.is_none() {
            return None;
        }
        let theme = cx.theme();
        Some(
            v_flex()
                .id("interrupts")
                .flex_none()
                .px_4()
                .py_3()
                .gap_2()
                .bg(theme.group_box)
                .border_b_1()
                .border_color(theme.border)
                .children(confirm_close)
                .children(delivery)
                .into_any_element(),
        )
    }

    /// What this computer is missing, in plain words, with a copyable
    /// install command for the distributions the launcher recognizes.
    fn render_prerequisites(&mut self, snapshot: &UiSnapshot, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let system = &snapshot.system;
        let command = install_command(system.distro, &system.missing);
        let mut list = v_flex().id("missing-prerequisites").gap_1().children(
            system
                .missing
                .iter()
                .map(|item| div().text_sm().child(format!("• {}", item.plain()))),
        );
        if system.missing.is_empty() {
            list = list.child(
                div()
                    .text_sm()
                    .child("• Something the launcher checks for (open Diagnostics for details)"),
            );
        }
        let mut section = v_flex().gap_3().child(list.test_support());
        if let Some(command) = command {
            let copied = command.clone();
            section = section.child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .text_color(muted)
                            .child("To install them, run this in a terminal:"),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .id("install-command")
                                    .flex_1()
                                    .min_w_0()
                                    .h(px(FIELD_HEIGHT))
                                    .flex()
                                    .items_center()
                                    .px_3()
                                    .rounded_md()
                                    .border_1()
                                    .border_color(border)
                                    .font_family("monospace")
                                    .text_sm()
                                    .overflow_x_hidden()
                                    .child(div().truncate().child(command))
                                    .test_support(),
                            )
                            .child(
                                Button::new("copy-install-command")
                                    .label("Copy")
                                    .h(px(FIELD_HEIGHT))
                                    .on_click(cx.listener(move |_, _, _, cx| {
                                        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(
                                            copied.clone(),
                                        ));
                                    })),
                            ),
                    ),
            );
        }
        section.child(
            div()
                .text_sm()
                .text_color(muted)
                .child("When you've installed them, choose Check again."),
        )
    }

    /// The last operation that did not succeed, until dismissed.
    fn render_result_card(
        &mut self,
        card: ResultCardView,
        snapshot: &UiSnapshot,
        deck: &DeckPresentation,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let retry = matches!(
            deck.primary.id,
            ActionId::ContinueInstallation
                | ActionId::Update
                | ActionId::Play
                | ActionId::Download
                | ActionId::InstallProjectDiablo
                | ActionId::ImportExisting
                | ActionId::AuthorizeAndInstall
        ) && deck.primary.enabled;
        let mut buttons = h_flex().gap_2().flex_wrap();
        match card.next {
            Some(ResultNextStep::TryAgain) if retry => {
                let action = deck.primary.id;
                buttons =
                    buttons.child(Button::new("result-try-again").label("Try again").on_click(
                        cx.listener(move |this, _, _, cx| this.activate_action(action, cx)),
                    ));
            }
            Some(ResultNextStep::StartOver) => {
                buttons = buttons.child(
                    Button::new("result-start-over")
                        .label("Start over…")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.confirm_start_over = true;
                            cx.notify();
                        })),
                );
            }
            _ => {}
        }
        let creating = snapshot.report() == Some(&ReportView::Creating);
        buttons = buttons.child(
            Button::new("result-save-report")
                .label(if creating {
                    "Creating report…"
                } else {
                    "Save report"
                })
                .loading(creating)
                .disabled(creating)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.activate_action(ActionId::ExportDiagnostics, cx)
                })),
        );
        if matches!(
            card.next,
            Some(ResultNextStep::OpenDiagnostics | ResultNextStep::TryAgain)
        ) {
            buttons = buttons.child(
                Button::new("result-diagnostics")
                    .label("Open Diagnostics")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.activate_action(ActionId::OpenDiagnostics, cx)
                    })),
            );
        }
        buttons = buttons.child(
            Button::new("result-dismiss")
                .label("Dismiss")
                .ghost()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.emit(super::Intent::DismissResult);
                    cx.notify();
                })),
        );
        v_flex()
            .id("result-card")
            .gap_2()
            .child(
                Alert::error("result-alert", card.message)
                    .title(card.title)
                    .small(),
            )
            .child(buttons)
            .children(self.render_report_status(snapshot, "result", cx))
            .test_support()
            .into_any_element()
    }

    /// Play's notice of what keeping the characters put in a backup,
    /// with the way to it.
    fn render_saves_put_aside(
        &self,
        put_aside: &SavesPutAsideView,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let danger = cx.theme().danger;
        let opening = put_aside.open == Some(FolderOpenView::Opening);
        let unopened = (put_aside.open == Some(FolderOpenView::Failed))
            .then(|| display_path(&put_aside.folder));
        v_flex()
            .id("saves-put-aside")
            .gap_2()
            .child(
                Alert::info("saves-put-aside-alert", put_aside.message())
                    .title(SavesPutAsideView::TITLE)
                    .small(),
            )
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        Button::new("open-saves-backup")
                            .label("Open backups")
                            .small()
                            .loading(opening)
                            .disabled(opening)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.emit(super::Intent::OpenSavesBackup);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("dismiss-saves-put-aside")
                            .label("Dismiss")
                            .small()
                            .ghost()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.emit(super::Intent::DismissSavesPutAside);
                                cx.notify();
                            })),
                    ),
            )
            .when_some(unopened, |notice, shown| {
                notice.child(div().text_xs().text_color(danger).child(format!(
                    "The folder couldn't be opened automatically: the backups are in {shown}."
                )))
            })
            .test_support()
            .into_any_element()
    }

    /// What Save report did: creating, where the file is, or why it failed.
    fn render_report_status(
        &self,
        snapshot: &UiSnapshot,
        id: &'static str,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let muted = cx.theme().muted_foreground;
        let n = usize::from(id == "result");
        let status = match snapshot.report()? {
            ReportView::Creating => div()
                .text_sm()
                .text_color(muted)
                .child("Creating report… This takes a few seconds.")
                .into_any_element(),
            ReportView::Saved { path, opened } => {
                let shown = display_path(path);
                let copied = path.to_string_lossy().into_owned();
                let opened_note = match opened {
                    None => " Opening its folder…",
                    Some(true) => " Its folder is open in your file manager.",
                    Some(false) => {
                        " The folder couldn't be opened automatically: use Copy path to find it."
                    }
                };
                v_flex()
                    .gap_2()
                    .child(
                        Alert::success(
                            ("report-saved", n),
                            format!(
                                "Report saved to {shown} — send this file to the PD2 Launcher testers.{opened_note}"
                            ),
                        )
                        .title("Report saved")
                        .small(),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .child(
                                Button::new(("report-open-folder", n))
                                    .label("Open folder")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.emit(super::Intent::RevealReport);
                                        cx.notify();
                                    })),
                            )
                            .child(Button::new(("report-copy-path", n)).label("Copy path").on_click(cx.listener(
                                move |_, _, _, cx| {
                                    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(
                                        copied.clone(),
                                    ));
                                },
                            )))
                            .child(
                                Button::new(("report-dismiss", n))
                                    .label("Dismiss")
                                    .ghost()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.emit(super::Intent::DismissReport);
                                        cx.notify();
                                    })),
                            ),
                    )
                    .into_any_element()
            }
            ReportView::Failed(code) => Alert::error(
                ("report-failed", n),
                format!(
                    "The report couldn't be saved (code: {code}). Check that your home folder has free space, then choose Save report again."
                ),
            )
            .title("Report not saved")
            .small()
            .into_any_element(),
        };
        Some(
            div()
                .id(("report-status", n))
                .child(status)
                .test_support()
                .into_any_element(),
        )
    }

    /// Asked when the window is closed while work runs.
    fn render_close_confirmation(
        &mut self,
        snapshot: &UiSnapshot,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let keep = match snapshot.active_job {
            Some(ActiveJobView::Download) => "Keep downloading",
            Some(ActiveJobView::InstallBaseGame | ActiveJobView::InstallProjectDiablo) => {
                "Keep installing"
            }
            Some(super::ActiveJobView::Update) => "Keep updating",
            _ => "Keep working",
        };
        h_flex()
            .id("confirm-close")
            .gap_3()
            .items_center()
            .child(
                div().flex_1().min_w_0().child(
                    Alert::warning("confirm-close-alert", close_warning(snapshot))
                        .title("Close the launcher?")
                        .small(),
                ),
            )
            .child(
                Button::new("keep-running")
                    .label(keep)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.confirm_close = false;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("stop-and-close")
                    .label("Stop and close")
                    .warning()
                    .on_click(cx.listener(|this, _, _, cx| this.stop_and_close(cx))),
            )
            .test_support()
            .into_any_element()
    }

    /// The Overview's task pane: the step the cursor shows, or what stops
    /// the setup (an unavailable folder, a service failure, repair).
    fn render_overview(
        &mut self,
        snapshot: &UiSnapshot,
        deck: &DeckPresentation,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let operation = snapshot
            .operation
            .clone()
            .filter(|operation| !operation.status.is_terminal());
        let custom_sources = snapshot.custom_sources_active().then(|| {
            Alert::warning("custom-sources-active", CUSTOM_SOURCES_BANNER)
                .small()
                .into_any_element()
        });
        let unavailable = snapshot
            .location_unavailable()
            .then(|| self.render_location_unavailable(snapshot, cx));
        let start_over = (self.confirm_start_over && start_over_available(snapshot))
            .then(|| self.render_start_over_confirmation("overview", snapshot, cx));
        let confirm_stop = self
            .confirm_stop
            .filter(|step| step_affordance(snapshot, *step) == StepAffordance::StopAndChange)
            .map(|step| self.render_stop_confirmation(step, cx));
        if confirm_stop.is_none() {
            self.confirm_stop = None;
        }
        let stop_result = snapshot
            .stop_result()
            .filter(|_| !mutation_active(snapshot))
            .map(|result| self.render_stop_result(result, deck, cx));
        let result = result_card(snapshot)
            .filter(|_| operation.is_none())
            .map(|card| self.render_result_card(card, snapshot, deck, cx));
        let interrupted = unavailable.is_some() || stop_result.is_some() || result.is_some();
        let working = operation.as_ref().filter(|operation| {
            !matches!(
                operation.phase,
                OperationPhase::GameExecution | OperationPhase::OfficialLauncher
            )
        });
        let unsupported =
            snapshot.host == HostView::Unsupported && service_failure(snapshot).is_none();
        let ready = deck.mode == super::DeckMode::Ready;
        let guided = self.guided_step(snapshot);
        let main = if let Some(step) = guided {
            // A stop's own card says what was kept and what's next; an
            // interruption's card offers Start over itself. Beside any other
            // result card the step keeps its form, without repeating its
            // heading, so both fit.
            let interrupted_card = result.is_some()
                && matches!(
                    snapshot.installation,
                    InstallationStatus::StageInterrupted(_)
                );
            if stop_result.is_some() && matches!(step, SetupStep::InstallD2 | SetupStep::InstallPd2)
            {
                // A stopped install shows where it paused.
                Some(self.render_install_tasks(snapshot, cx))
            } else {
                (stop_result.is_none() && !interrupted_card)
                    .then(|| self.render_guided(step, snapshot, deck, result.is_none(), cx))
            }
        } else if snapshot.location_unavailable() {
            // Nothing else runs until the location is back or replaced.
            None
        } else if snapshot.game_folder_unavailable() {
            Some(self.render_game_folder_unavailable(snapshot, cx))
        } else if let Some(operation) = working {
            Some(self.render_progress(operation, snapshot, deck, cx))
        } else if unsupported {
            let muted = cx.theme().muted_foreground;
            Some(
                v_flex()
                    .id("prerequisites")
                    .gap_4()
                    .child(heading(deck.title, Some(deck.description.into()), muted))
                    .child(self.render_prerequisites(snapshot, cx))
                    .into_any_element(),
            )
        } else if stop_result.is_some()
            && matches!(
                standing_step(snapshot),
                SetupStep::InstallD2 | SetupStep::InstallPd2
            )
        {
            Some(self.render_install_tasks(snapshot, cx))
        } else if interrupted && !ready {
            None
        } else if standing_step(snapshot) == SetupStep::InstallPd2 {
            // After the guided steps, step 5 keeps its own screen rather
            // than a generic status.
            Some(self.render_guided(SetupStep::InstallPd2, snapshot, deck, true, cx))
        } else {
            let status = status(snapshot, deck);
            let muted = cx.theme().muted_foreground;
            Some(
                heading(status.headline, Some(status.detail), muted)
                    .id("status-view")
                    .test_support()
                    .into_any_element(),
            )
        };
        v_flex()
            .id("overview-page")
            .gap_5()
            .children(custom_sources)
            .children(unavailable)
            .children(start_over)
            .children(confirm_stop)
            .children(stop_result)
            .children(result)
            .children(main)
            .test_support()
            .into_any_element()
    }

    /// The running step: which one, what it is doing, how far along.
    fn render_progress(
        &mut self,
        operation: &OperationView,
        snapshot: &UiSnapshot,
        deck: &DeckPresentation,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if matches!(
            running_step(snapshot),
            Some(SetupStep::InstallD2 | SetupStep::InstallPd2)
        ) {
            return self.render_install_progress(operation, snapshot, cx);
        }
        let muted = cx.theme().muted_foreground;
        let (headline, detail) = working_status(operation, snapshot);
        let _ = deck;
        let eyebrow = running_step(snapshot).map(|step| step.name());
        let bar = match operation.progress_percent {
            Some(percent) => Progress::new("operation-progress")
                .value(f32::from(percent))
                .accessibility_label(format!("{percent} percent")),
            None => Progress::new("operation-progress")
                .loading(true)
                .accessibility_label("Working"),
        };
        let caption = progress_caption(operation);
        let downloading = running_step(snapshot) == Some(SetupStep::Download);
        // Blizzard's windows open on top of everything while its programs
        // run; the notice stays for the whole step.
        let blizzard_notice = blizzard_windows_notice(snapshot).map(|notice| {
            div()
                .id("blizzard-windows-notice")
                .child(
                    Alert::warning("blizzard-windows-notice-alert", notice)
                        .title("Leave Blizzard's windows alone")
                        .small(),
                )
                .test_support()
        });
        v_flex()
            .id("progress-view")
            .gap_3()
            .child(
                v_flex()
                    .gap_1()
                    .children(
                        eyebrow.map(|eyebrow| div().text_xs().text_color(muted).child(eyebrow)),
                    )
                    .child(div().text_lg().font_semibold().child(headline))
                    .child(div().text_sm().text_color(muted).child(detail)),
            )
            .child(
                v_flex().gap_2().child(bar).child(
                    h_flex()
                        .justify_between()
                        .gap_4()
                        .text_xs()
                        .text_color(muted)
                        .child(
                            div().min_w_0().truncate().child(
                                operation
                                    .progress_summary
                                    .clone()
                                    .or(caption)
                                    .unwrap_or_else(|| "Working…".to_owned()),
                            ),
                        )
                        .children(
                            operation
                                .progress_percent
                                .map(|percent| div().flex_none().child(format!("{percent}%"))),
                        )
                        .child(self.details_toggle(cx)),
                ),
            )
            .children(downloading.then(|| download_list(&download_rows(snapshot), true, cx)))
            .children(blizzard_notice)
            .children(self.stop_and_change(snapshot, cx))
            // The programs' own lines are details: behind Show details, and
            // in Diagnostics.
            .children(
                self.details_open
                    .then(|| progress_log(muted, cx.theme().border))
                    .flatten(),
            )
            .test_support()
            .into_any_element()
    }

    /// Show details: the running step's own lines (the programs' output, in
    /// the engine's words), folded away by default.
    fn details_toggle(&self, cx: &mut Context<Self>) -> Button {
        Button::new("progress-details")
            .label(if self.details_open {
                "Hide details"
            } else {
                "Show details"
            })
            .ghost()
            .xsmall()
            .flex_none()
            .on_click(cx.listener(|this, _, _, cx| {
                this.details_open = !this.details_open;
                cx.notify();
            }))
    }

    /// The installer choice changes only once Download or Install Diablo II
    /// stopped: asked first, then stopped at a safe point.
    fn stop_and_change(&mut self, snapshot: &UiSnapshot, cx: &mut Context<Self>) -> Option<Div> {
        (step_affordance(snapshot, SetupStep::Setup) == StepAffordance::StopAndChange
            && self.confirm_stop.is_none())
        .then(|| {
            h_flex().child(
                Button::new("stop-and-change")
                    .label("Stop and change how to get Diablo II…")
                    .ghost()
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.confirm_stop = Some(SetupStep::Setup);
                        cx.notify();
                    })),
            )
        })
    }

    /// The install as it stands now (fixed while it runs, so its figure
    /// keeps one scale through both install jobs).
    fn install_plan(&self, snapshot: &UiSnapshot) -> InstallPlan {
        self.install_plan.unwrap_or_else(|| {
            InstallPlan::for_snapshot(snapshot, self.import_mode == ImportModeView::InPlace)
        })
    }

    /// The running download's percent, or the whole install's one figure,
    /// for the current step mark and the line along the header.
    fn flow_percent(&self, snapshot: &UiSnapshot) -> Option<u8> {
        if matches!(
            running_step(snapshot),
            Some(SetupStep::InstallD2 | SetupStep::InstallPd2)
        ) {
            let elapsed = crate::activity::current_phase().map(|(_, elapsed)| elapsed);
            install_percent(snapshot, self.install_plan(snapshot), elapsed)
        } else {
            flow_progress(snapshot)
        }
    }

    /// Install's tasks, one row each with its state's mark, name and line.
    fn render_install_tasks(&self, snapshot: &UiSnapshot, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let (muted, border) = (theme.muted_foreground, theme.border);
        let rows = install_tasks(snapshot, self.install_plan(snapshot));
        let count = rows.len();
        v_flex()
            .id("install-tasks")
            .children(rows.into_iter().enumerate().map(|(index, row)| {
                let mark = match row.state {
                    TaskState::Waiting => CheckMark::Pending,
                    TaskState::Running => CheckMark::Working,
                    TaskState::Done => CheckMark::Passed,
                    TaskState::Paused => CheckMark::Attention,
                    TaskState::Failed => CheckMark::Failed,
                };
                h_flex()
                    .id(("install-task", index))
                    .items_center()
                    .gap_3()
                    .py_1()
                    .when(index + 1 < count, |line| {
                        line.border_b_1().border_color(border)
                    })
                    .child(check_mark(mark, cx))
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .items_baseline()
                            .gap_2()
                            .child(
                                div()
                                    .flex_none()
                                    .text_sm()
                                    .font_medium()
                                    .text_color(if row.state == TaskState::Waiting {
                                        palette().faint
                                    } else {
                                        theme.foreground
                                    })
                                    .child(row.label),
                            )
                            .children(row.detail.map(|detail| {
                                div()
                                    .min_w_0()
                                    .text_xs()
                                    .text_color(match row.state {
                                        TaskState::Paused => theme.warning,
                                        TaskState::Failed => theme.danger,
                                        _ => muted,
                                    })
                                    .truncate()
                                    .child(detail)
                            })),
                    )
                    .test_support()
            }))
            .test_support()
            .into_any_element()
    }

    /// Install while it runs: what runs now as the title, the one figure for
    /// the whole install with the running part's amount or time under it,
    /// and the tasks where other steps show their log.
    fn render_install_progress(
        &mut self,
        operation: &OperationView,
        snapshot: &UiSnapshot,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let plan = self.install_plan(snapshot);
        let rows = install_tasks(snapshot, plan);
        let title = install_title(&rows, snapshot);
        let elapsed = crate::activity::current_phase().map(|(_, elapsed)| elapsed);
        let percent = install_percent(snapshot, plan, elapsed);
        let stopping = operation.status == OperationStatusView::SafeStopping;
        let caption = operation.progress_summary.clone().or_else(|| {
            elapsed.map(|elapsed| {
                let seconds = elapsed.as_secs();
                format!("{}:{:02} elapsed", seconds / 60, seconds % 60)
            })
        });
        let bar = match percent {
            Some(percent) => Progress::new("operation-progress")
                .value(f32::from(percent))
                .accessibility_label(format!("{percent} percent")),
            None => Progress::new("operation-progress")
                .loading(true)
                .accessibility_label("Working"),
        };
        let blizzard_notice = blizzard_windows_notice(snapshot).map(|notice| {
            div()
                .id("blizzard-windows-notice")
                .child(
                    Alert::warning("blizzard-windows-notice-alert", notice)
                        .title("Leave Blizzard's windows alone")
                        .small(),
                )
                .test_support()
        });
        let tasks = self.render_install_tasks(snapshot, cx);
        v_flex()
            .id("progress-view")
            .gap_2p5()
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_xl().font_semibold().child(title))
                    .child(div().text_sm().text_color(muted).child(if stopping {
                        SAFE_STOP_NOTE
                    } else {
                        "Nothing needs your input while it runs. Keep this window open."
                    })),
            )
            .child(
                v_flex()
                    .id("install-figure")
                    .gap_1p5()
                    .child(
                        div().h(px(36.0)).text_3xl().font_light().child(
                            percent
                                .map(|percent| format!("{percent}%"))
                                .unwrap_or_default(),
                        ),
                    )
                    .child(bar)
                    .child(
                        h_flex()
                            .gap_4()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_xs()
                                    .text_color(muted)
                                    .truncate()
                                    .child(caption.unwrap_or_default()),
                            )
                            .child(self.details_toggle(cx)),
                    )
                    .test_support(),
            )
            .child(tasks)
            .children(blizzard_notice)
            .children(self.stop_and_change(snapshot, cx))
            .children(
                self.details_open
                    .then(|| progress_log(muted, cx.theme().border))
                    .flatten(),
            )
            .test_support()
            .into_any_element()
    }

    /// One setup step: its number, title, and what it needs now.
    /// The step's work runs from the action bar; while it runs, the pane
    /// shows its progress, and afterwards its result.
    fn render_guided(
        &mut self,
        step: SetupStep,
        snapshot: &UiSnapshot,
        deck: &DeckPresentation,
        with_heading: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if let Some(operation) = snapshot
            .operation
            .clone()
            .filter(|operation| operation.owns_mutation() && !operation.status.is_terminal())
        {
            return self.render_progress(&operation, snapshot, deck, cx);
        }
        if step == SetupStep::Play {
            // Play has a page of its own; this stands in while something
            // keeps it from showing.
            let status = status(snapshot, deck);
            let muted = cx.theme().muted_foreground;
            return heading(status.headline, Some(status.detail), muted)
                .id("status-view")
                .test_support()
                .into_any_element();
        }
        let muted = cx.theme().muted_foreground;
        let (title, intro) = guided_copy(step);
        let header = v_flex()
            .gap_1()
            .child(div().text_xl().font_semibold().child(title))
            .child(div().text_sm().text_color(muted).child(intro));
        let (id, body) = match step {
            // The setup record's page waits for the first check.
            _ if opening_step(snapshot).is_some() => (
                "guided-opening",
                v_flex()
                    .gap_2()
                    .child(
                        Progress::new("opening-progress")
                            .loading(true)
                            .accessibility_label("Getting ready"),
                    )
                    .child(div().text_sm().text_color(muted).child(deck.description)),
            ),
            SetupStep::Device => ("guided-check", self.guided_check(snapshot, cx)),
            SetupStep::Setup => {
                let location = self.location_row(snapshot, cx);
                let installers = self.installer_section(cx);
                (
                    "guided-location",
                    v_flex().gap_4().child(location).child(installers),
                )
            }
            SetupStep::Download => ("guided-download", self.download_body(snapshot, cx)),
            SetupStep::InstallD2 => ("guided-install-d2", self.install_d2_body(snapshot, cx)),
            SetupStep::InstallPd2 => ("guided-install-pd2", self.install_pd2_body(snapshot, cx)),
            SetupStep::Play => unreachable!("Play shows the Play surface"),
        };
        v_flex()
            .id(id)
            .gap_4()
            .when(with_heading, |step| step.child(header))
            .child(body)
            .test_support()
            .into_any_element()
    }

    /// A step's result: a check mark and one line, and what was kept.
    fn step_result(&self, title: String, detail: impl Into<SharedString>, cx: &App) -> AnyElement {
        let detail = detail.into();
        let muted = cx.theme().muted_foreground;
        h_flex()
            .id("step-result")
            .items_start()
            .gap_3()
            .child(check_mark(CheckMark::Passed, cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(div().text_sm().font_semibold().child(title))
                    .child(div().text_xs().text_color(muted).child(detail)),
            )
            .test_support()
            .into_any_element()
    }

    /// Step 3: what is downloaded, each with its state, or that it all is.
    fn download_body(&mut self, snapshot: &UiSnapshot, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        let list = download_list(&download_rows(snapshot), false, cx);
        if standing_step(snapshot) > SetupStep::Download {
            let total = snapshot.downloads().total_bytes;
            return v_flex()
                .gap_3()
                .child(self.step_result(
                    format!("All downloads verified ({})", human_bytes(total)),
                    "Kept in the launcher's folder. Nothing is installed yet, and no CD keys were needed.",
                    cx,
                ))
                .child(list);
        }
        v_flex()
            .gap_3()
            .child(list)
            .child(div().text_xs().text_color(muted).child(
                "Every file is checked before anything is installed. Stopping keeps what's done; downloading again continues where it stopped.",
            ))
    }

    /// Step 4: the CD keys, the existing installation, or the result.
    fn install_d2_body(&mut self, snapshot: &UiSnapshot, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        if standing_step(snapshot) > SetupStep::InstallD2 {
            let detail: SharedString = match snapshot.game_folder() {
                Some(_) => "Your own Diablo II folder is used where it is.".into(),
                // After a copy, the player's own folder can go.
                None => copied_folder_note(snapshot).map_or(
                    "Installed in the launcher's own game folder.".into(),
                    Into::into,
                ),
            };
            return v_flex().gap_3().child(self.step_result(
                "Diablo II and Lord of Destruction are installed".to_owned(),
                detail,
                cx,
            ));
        }
        match snapshot.installation {
            InstallationStatus::StageInterrupted(_) => v_flex().gap_3().child(Alert::warning(
                "install-d2-interrupted",
                "A step stopped part way. Start over removes what it left and installs Diablo II again; your downloads are kept.",
            )),
            _ => match snapshot.installer_media().existing().cloned() {
                Some(existing) => self.existing_section(&existing, cx),
                None => v_flex()
                    .gap_3()
                    .children(self.keys_section(cx))
                    .child(div().text_xs().text_color(muted).child(
                        "Everything was downloaded and checked in the step before, so this runs from those files.",
                    )),
            },
        }
    }

    /// Step 4 with a Diablo II the user already has: the check's
    /// result, and whether to copy it or use it where it is.
    fn existing_section(
        &mut self,
        existing: &super::ExistingInstallView,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = cx.theme();
        let (muted, danger, warning) = (theme.muted_foreground, theme.danger, theme.warning);
        let verdict = match existing.problem {
            None => h_flex()
                .gap_2()
                .items_start()
                .child(check_mark(CheckMark::Passed, cx))
                .child(
                    v_flex()
                        .min_w_0()
                        .child(div().text_sm().child(if existing.project_diablo {
                            "Diablo II, Lord of Destruction and Project Diablo 2 found"
                        } else {
                            "Diablo II + Lord of Destruction, used as it is"
                        }))
                        .when(existing.project_diablo, |verdict| {
                            verdict.child(div().text_xs().text_color(muted).child(
                                "Install brings Project Diablo 2 up to date instead of installing it again.",
                            ))
                        })
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .truncate()
                                .child(existing.path.display().to_string()),
                        ),
                ),
            Some(problem) => h_flex()
                .gap_2()
                .items_start()
                .child(check_mark(CheckMark::Attention, cx))
                .child(div().text_sm().text_color(danger).child(problem.message())),
        };
        let mode = self.import_mode;
        let modes = existing.problem.is_none().then(|| {
            let in_place_possible = existing.in_place_possible;
            RadioGroup::vertical("import-mode")
                .selected_index(Some(match mode {
                    ImportModeView::Copy => 0,
                    ImportModeView::InPlace => 1,
                }))
                .on_change(cx.listener(move |this, index: &usize, _, cx| {
                    this.import_mode = if *index == 1 && in_place_possible {
                        ImportModeView::InPlace
                    } else {
                        ImportModeView::Copy
                    };
                    cx.notify();
                }))
                .child(
                    Radio::new("import-copy")
                        .small()
                        .label("Copy into the launcher's game folder (recommended)")
                        .child(div().text_xs().text_color(muted).child(format!(
                            "Copies about {}{}. Your folder stays as it is and isn't needed afterwards.",
                            human_bytes(existing.copy_bytes),
                            if existing.project_diablo {
                                ", its Project Diablo 2 included"
                            } else {
                                ""
                            }
                        ))),
                )
                .child(
                    Radio::new("import-in-place")
                        .small()
                        .disabled(!in_place_possible)
                        .label("Use it where it is")
                        .child(div().text_xs().text_color(warning).child(
                            if in_place_possible && existing.project_diablo {
                                "Its Project Diablo 2 is brought up to date there. Moving or deleting the folder breaks this installation."
                            } else if in_place_possible {
                                "Project Diablo 2's files are added to that folder. Moving or deleting it breaks this installation."
                            } else {
                                "Not possible for this folder: its path has characters Windows programs can't use, or its Project Diablo 2 folder isn't named ProjectD2. Copy it instead."
                            },
                        )),
                )
        });
        v_flex()
            .gap_3()
            .child(form_row("Your Diablo II", 0.0, verdict))
            .when_some(modes, |section, modes| {
                section.child(form_row("How to use it", 0.0, modes))
            })
            .child(form_row(
                "",
                0.0,
                h_flex().child(
                    Button::new("choose-existing-again")
                        .label("Choose another folder")
                        .small()
                        .on_click(cx.listener(|this, _, _, cx| this.choose_existing_install(cx))),
                ),
            ))
    }

    /// Step 5: installs from the downloads, or its result.
    fn install_pd2_body(&mut self, snapshot: &UiSnapshot, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let (muted, warning) = (theme.muted_foreground, theme.warning);
        if standing_step(snapshot) > SetupStep::InstallPd2 {
            return v_flex().gap_3().child(self.step_result(
                "Project Diablo 2 is installed and checked".to_owned(),
                "Everything is ready to play.",
                cx,
            ));
        }
        let adopts = snapshot
            .installer_media()
            .existing()
            .is_some_and(|existing| existing.project_diablo);
        let text = if snapshot.installation == InstallationStatus::NeedsRuntime {
            "The game is installed. A few Windows components are still missing; Continue adds them without touching the game."
        } else if adopts {
            "Brings the Project Diablo 2 your Diablo II came with up to date from the verified downloads: only the files that differ are fetched, and its installer doesn't run again. No CD keys are needed."
        } else {
            "Installs Project Diablo 2 and its files from the verified downloads. Files that changed since then are fetched again. No CD keys are needed."
        };
        v_flex()
            .gap_3()
            .child(self.render_install_tasks(snapshot, cx))
            .child(div().text_sm().child(text))
            .when_some(snapshot.game_folder().cloned(), |section, folder| {
                section.child(div().text_xs().text_color(warning).child(format!(
                    "Project Diablo 2's files go into your Diablo II folder: {}",
                    folder.path.display()
                )))
            })
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("This takes a few minutes; keep this window open."),
            )
    }

    /// A folder used in place can't be reached: check again, or start over
    /// (which forgets the folder and never deletes it).
    fn render_game_folder_unavailable(
        &mut self,
        snapshot: &UiSnapshot,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let path = snapshot
            .game_folder()
            .map(|folder| folder.path.display().to_string())
            .unwrap_or_default();
        v_flex()
            .id("game-folder-unavailable")
            .gap_4()
            .child(heading("Diablo II folder unavailable", None, muted))
            .child(Alert::warning(
                "game-folder-unavailable-alert",
                format!(
                    "The launcher uses your Diablo II where it is, at {path}, but it isn't there, or it isn't the same folder anymore. If it's on a drive that isn't connected, connect it and choose Check again. Nothing runs until it's back."
                ),
            ))
            .child(
                h_flex().gap_2().child(
                    Button::new("game-folder-start-over")
                        .label("Start over…")
                        .small()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.confirm_start_over = true;
                            cx.notify();
                        })),
                ),
            )
            .child(div().text_xs().text_color(muted).child(
                "Start over forgets that folder and removes only the launcher's own game environment; it never deletes your folder.",
            ))
            .test_support()
            .into_any_element()
    }

    /// The computer check in plain words: each thing checked with ✓ or !,
    /// or what is missing and how to install it.
    /// FEX on ARM64: the built-in FEX the launcher downloads, or
    /// one on this device; in Settings and in step 1.
    fn fex_choice(
        &mut self,
        snapshot: &UiSnapshot,
        fex: super::FexSettingView,
        busy: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        let device = fex.source == super::FexSourceView::Device;
        let in_use: SharedString = match &fex.in_use {
            Some(path) => format!("In use: {}", path.display()).into(),
            None => {
                "No FEX found on this device. The launcher needs it to run Windows programs.".into()
            }
        };
        let restart = fex.saved.is_some() && fex.saved != fex.in_use;
        let builtin_available = fex.builtin_available;
        let picker_problem = self.fex_picker_problem;
        let builtin = fex.source == super::FexSourceView::Builtin;
        let size = fex.builtin_download_bytes.div_ceil(1_000_000);
        let builtin_status: SharedString = if fex.builtin_installed {
            format!("Installed: FEX {}.", crate::builtin_fex::fex_version()).into()
        } else {
            format!("Downloads in step 3, Download: about {size} MB, and about 2 GB once unpacked.")
                .into()
        };
        // An installed game gets the built-in FEX without the setup
        // steps.
        let download_now = builtin
            && !fex.builtin_installed
            && matches!(
                snapshot.installation(),
                InstallationStatus::Ready | InstallationStatus::NeedsRuntime
            );
        v_flex()
            .gap_2()
            .child(div().text_xs().text_color(muted).child(
                "FEX runs the launcher's Windows programs on ARM64. The built-in FEX is the one \
                     the launcher is tested with.",
            ))
            .child(
                RadioGroup::vertical("fex-source")
                    .selected_index(Some(usize::from(device)))
                    .on_change(cx.listener(move |this, index: &usize, _, cx| {
                        let source = if *index == 0 {
                            super::FexSourceView::Builtin
                        } else {
                            super::FexSourceView::Device
                        };
                        if source == super::FexSourceView::Device || builtin_available {
                            this.emit(super::Intent::ChooseFexSource(source));
                        }
                        cx.notify();
                    }))
                    .child(
                        Radio::new("fex-builtin")
                            .small()
                            .disabled(!builtin_available)
                            .label("Built-in FEX (recommended)")
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(if builtin_available {
                                        "The FEX the launcher downloads and keeps in its own \
                                         folder, with an Ubuntu 24.04 RootFS."
                                    } else {
                                        "Not available on this computer."
                                    }),
                            ),
                    )
                    .child(Radio::new("fex-device").small().label("Device FEX").child(
                        div().text_xs().text_color(muted).child(
                            "A FEX on this device: found by itself in the usual places \
                                     (and DroidDeck's), or the one you choose.",
                        ),
                    )),
            )
            .when(builtin, |section| {
                section
                    .child(
                        div()
                            .id("fex-builtin-status")
                            .text_sm()
                            .child(builtin_status)
                            .test_support(),
                    )
                    .when(download_now, |section| {
                        section.child(
                            h_flex().gap_2().child(
                                Button::new("download-builtin-fex")
                                    .label(format!("Download it ({size} MB)"))
                                    .small()
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.emit(super::Intent::DownloadAll);
                                        cx.notify();
                                    })),
                            ),
                        )
                    })
            })
            .when(device, |section| {
                section
                    .child(
                        div()
                            .id("fex-in-use")
                            .text_sm()
                            .child(in_use)
                            .test_support(),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().flex_1().min_w_0().child(Input::new(&self.fex_input)))
                            .child(
                                Button::new("save-fex")
                                    .label("Use")
                                    .h(px(FIELD_HEIGHT))
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.save_typed_fex(cx)),
                                    ),
                            )
                            .child(
                                Button::new("choose-fex-file")
                                    .label("Choose file…")
                                    .h(px(FIELD_HEIGHT))
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.choose_fex_file(cx)),
                                    ),
                            ),
                    )
                    .child(
                        h_flex().child(
                            Button::new("reset-fex")
                                .label("Find automatically")
                                .ghost()
                                .small()
                                .disabled(fex.saved.is_none())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.emit(super::Intent::SaveFex(None));
                                    cx.notify();
                                })),
                        ),
                    )
                    .when_some(fex.saved.clone(), |section, saved| {
                        section.child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(format!("Chosen: {}", saved.display())),
                        )
                    })
                    .when(restart, |section| {
                        section.child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child("Restart the launcher to use this FEX."),
                        )
                    })
            })
            .when_some(fex.problem.or(picker_problem), |section, problem| {
                section.child(
                    div()
                        .id("fex-problem")
                        .text_xs()
                        .text_color(danger)
                        .child(problem)
                        .test_support(),
                )
            })
    }

    fn guided_check(&mut self, snapshot: &UiSnapshot, cx: &mut Context<Self>) -> Div {
        let busy = snapshot
            .operation
            .as_ref()
            .is_some_and(OperationView::owns_mutation);
        // ARM64: which FEX runs the launcher's Wine, chosen here first;
        // the built-in one also stands in for a missing FEX.
        let fex = snapshot.fex_setting().cloned().map(|fex| {
            let border = cx.theme().border;
            v_flex()
                .id("guided-fex")
                .gap_2()
                .pt_2()
                .border_t_1()
                .border_color(border)
                .child(div().text_sm().font_medium().child("FEX"))
                .child(self.fex_choice(snapshot, fex, busy, cx))
                .test_support()
        });
        if snapshot.host == HostView::Unsupported {
            let muted = cx.theme().muted_foreground;
            return v_flex()
                .gap_3()
                .child(
                    div()
                        .text_sm()
                        .child("Your computer is missing a few things the game needs:"),
                )
                .child(self.render_prerequisites(snapshot, cx))
                .children(fex)
                .child(
                    h_flex()
                        .gap_3()
                        .child(
                            Button::new("guided-check-again")
                                .label("Check again")
                                .small()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.activate_action(ActionId::CheckAgain, cx)
                                })),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child("Continue opens once everything is found."),
                        ),
                );
        }
        let theme = cx.theme();
        let (muted, border) = (theme.muted_foreground, theme.border);
        let checking = snapshot.host == HostView::Checking;
        let items = check_items(snapshot);
        let count = items.len();
        let rows = items
            .into_iter()
            .enumerate()
            .map(|(index, (label, detail, mark))| {
                h_flex()
                    .id(("check-item", index))
                    .items_start()
                    .gap_3()
                    .py_1p5()
                    .when(index + 1 < count, |row| {
                        row.border_b_1().border_color(border)
                    })
                    .child(check_mark(mark, cx))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().font_medium().child(label))
                            .child(div().text_xs().text_color(muted).child(detail)),
                    )
            })
            .collect::<Vec<_>>();
        v_flex()
            .gap_3()
            .when(checking, |check| {
                check.child(
                    v_flex()
                        .id("guided-checking")
                        .gap_2()
                        .child(div().text_sm().child("Checking your computer…"))
                        .child(
                            Progress::new("guided-check-progress")
                                .loading(true)
                                .accessibility_label("Checking your computer"),
                        )
                        .test_support(),
                )
            })
            .child(v_flex().id("check-list").children(rows).test_support())
            .children(fex)
            .child(
                div()
                    .id("not-affiliated")
                    .pt_2()
                    .text_xs()
                    .text_color(muted)
                    .child(NOT_AFFILIATED)
                    .test_support(),
            )
    }

    /// The install location as one form row; its full editor while a new
    /// folder waits for confirmation or was refused.
    fn location_row(&mut self, snapshot: &UiSnapshot, cx: &mut Context<Self>) -> Div {
        if self.pending_location.is_some() || snapshot.location_error().is_some() {
            let body = self.location_body(snapshot, None, cx);
            return form_row("Install location", 0.0, body);
        }
        let changeable = step_affordance(snapshot, SetupStep::Setup) == StepAffordance::Change;
        form_row(
            "Install location",
            LABEL_OFFSET,
            h_flex()
                .h(px(FIELD_HEIGHT))
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .truncate()
                        .child(location_label(snapshot)),
                )
                .when(changeable, |row| {
                    row.child(
                        Button::new("change-location")
                            .label("Change…")
                            .ghost()
                            .small()
                            .on_click(
                                cx.listener(|this, _, _, cx| this.choose_install_location(cx)),
                            ),
                    )
                }),
        )
    }

    /// How to get Diablo II: Blizzard's download (default and recommended),
    /// installer files the user has, or a Diablo II the user already has
    /// installed, of any version.
    fn installer_section(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let danger = theme.danger;
        let warning = theme.warning;
        let success = theme.success;
        // D2/LoD downloads use the effective installer sources: Blizzard's
        // built-in URLs (size, PE, and file-name checks), or the user's own,
        // unverified sources.
        let custom_hosts = self.state.snapshot().custom_installer_hosts();
        let media = self.state.snapshot().installer_media().clone();
        let folder_chosen = matches!(
            media,
            InstallerMediaView::Validated(_) | InstallerMediaView::Invalid(_)
        );
        let existing_chosen = matches!(media, InstallerMediaView::Existing(_));
        let selected = if existing_chosen || (self.prefers_existing && !folder_chosen) {
            2
        } else if folder_chosen || self.prefers_folder {
            1
        } else {
            0
        };
        let download_label: SharedString = if custom_hosts.is_empty() {
            "Download from Blizzard (recommended)".into()
        } else {
            "Download from your custom sources (unverified)".into()
        };
        let download_detail: SharedString = if custom_hosts.is_empty() {
            "About 2.1 GB · usually 10–20 minutes".into()
        } else {
            format!(
                "Downloads both installers from {}. These files are not checked.",
                custom_hosts.join(", ")
            )
            .into()
        };
        let status_line = |text: SharedString, color: Hsla| {
            div()
                .flex_1()
                .min_w_0()
                .text_xs()
                .text_color(color)
                .truncate()
                .child(text)
        };
        let folder_status: Option<(SharedString, Hsla)> = match &media {
            InstallerMediaView::Validated(path) => Some((
                format!("Found both installers in {}", path.display()).into(),
                success,
            )),
            InstallerMediaView::Invalid(_) => Some((
                "That folder is missing an installer, or a file isn't a valid installer.".into(),
                danger,
            )),
            _ if self.prefers_folder => Some(("No folder chosen yet".into(), muted)),
            _ => None,
        };
        let folder = (selected == 1).then(|| {
            h_flex()
                .gap_3()
                .pl(px(RADIO_INDENT))
                .child(
                    Button::new("choose-installers")
                        .label(if folder_chosen {
                            "Choose a different folder"
                        } else {
                            "Choose folder"
                        })
                        .small()
                        .on_click(cx.listener(|this, _, _, cx| this.choose_installer_media(cx))),
                )
                .when_some(folder_status, |row, (text, color)| {
                    row.child(status_line(text, color))
                })
        });
        let existing_status: Option<(SharedString, Hsla)> = match &media {
            InstallerMediaView::Existing(existing) => Some(match existing.problem {
                None => (existing_found_line(existing).into(), success),
                Some(problem) => (problem.message().into(), danger),
            }),
            _ if self.prefers_existing => Some(("No folder chosen yet".into(), muted)),
            _ => None,
        };
        // A reason the folder can't be used is read in full, on its own line.
        let existing = (selected == 2).then(|| {
            v_flex()
                .gap_1p5()
                .pl(px(RADIO_INDENT))
                .child(
                    h_flex().child(
                        Button::new("choose-existing")
                            .label(if existing_chosen {
                                "Choose a different folder"
                            } else {
                                "Choose its game folder"
                            })
                            .small()
                            .on_click(
                                cx.listener(|this, _, _, cx| this.choose_existing_install(cx)),
                            ),
                    ),
                )
                .when_some(existing_status, |row, (text, color)| {
                    row.child(div().text_xs().text_color(color).child(text))
                })
        });
        let rejection = self.state.snapshot().installer_media_rejection().map(|_| {
            div().text_xs().text_color(danger).child(
                "That folder wasn't used: it needs both Downloader_Diablo2_enUS.exe and Downloader_Diablo2_Lord_of_Destruction_enUS.exe from Blizzard (other files in it are ignored). Your earlier choice is kept.",
            )
        });
        form_row(
            "Get Diablo II",
            0.0,
            v_flex()
                .gap_2()
                .child(
                    RadioGroup::vertical("installer-source")
                        .selected_index(Some(selected))
                        .on_change(cx.listener(|this, index: &usize, _, cx| {
                            match *index {
                                0 => {
                                    this.prefers_folder = false;
                                    this.prefers_existing = false;
                                    this.use_blizzard_download(cx);
                                }
                                1 => {
                                    this.prefers_folder = true;
                                    this.prefers_existing = false;
                                    this.choose_installer_media(cx);
                                }
                                _ => {
                                    this.prefers_existing = true;
                                    this.prefers_folder = false;
                                    this.choose_existing_install(cx);
                                }
                            }
                            cx.notify();
                        }))
                        .child(
                            Radio::new("installer-download")
                                .small()
                                .label(download_label)
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(if custom_hosts.is_empty() {
                                            muted
                                        } else {
                                            warning
                                        })
                                        .child(download_detail),
                                ),
                        )
                        .child(
                            Radio::new("installer-folder")
                                .small()
                                .label("I already have the installer files")
                                .when(selected == 1, |radio| {
                                    radio.child(div().text_xs().text_color(muted).child(
                                        "Downloader_Diablo2_enUS.exe and Downloader_Diablo2_Lord_of_Destruction_enUS.exe",
                                    ))
                                }),
                        )
                        .child(
                            Radio::new("installer-existing")
                                .small()
                                .label("I already have Diablo II + Lord of Destruction installed")
                                .when(selected == 2, |radio| {
                                    radio.child(div().text_xs().text_color(muted).child(
                                        "Choose the folder that contains Game.exe. No CD keys needed.",
                                    ))
                                }),
                        ),
                )
                .children(folder)
                .children(existing)
                .children(rejection),
        )
    }

    /// Both CD keys and the license consent, as form rows.
    fn keys_section(&mut self, cx: &mut Context<Self>) -> Vec<Div> {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let danger = theme.danger;
        let fields = [
            (
                "Diablo II CD key",
                "Diablo II",
                crate::blizzard::KeyField::BaseGame,
                self.base_key.clone(),
            ),
            (
                "Lord of Destruction CD key",
                "Lord of Destruction",
                crate::blizzard::KeyField::Expansion,
                self.expansion_key.clone(),
            ),
        ]
        .map(|(label, game, field, input)| {
            let problem = key_problem(game, field, &input.read(cx).content);
            form_row(
                label,
                LABEL_OFFSET,
                v_flex()
                    .gap_1()
                    .child(input.into_any_element())
                    .when_some(problem, |field, problem| {
                        field.child(div().text_xs().text_color(danger).child(problem))
                    }),
            )
        });
        let [base, expansion] = fields;
        vec![
            base,
            expansion,
            form_row(
                "",
                0.0,
                div().text_xs().text_color(muted).child(
                    "Find them in your Blizzard account. Have an older 16-character key from the box? Add it to your Blizzard account to see its 26-character version. Lord of Destruction has its own key.",
                ),
            ),
            form_row(
                "",
                0.0,
                v_flex()
                    .gap_1p5()
                    .child(
                        Checkbox::new("consent")
                            .small()
                            .checked(self.consent)
                            .label("I agree to Blizzard's license terms for Diablo II and Lord of Destruction, and let the launcher accept them for me.")
                            .on_change(cx.listener(|this, checked: &bool, _, cx| {
                                this.consent = *checked;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .id("key-privacy")
                            .text_xs()
                            .text_color(muted)
                            .child(KEY_PRIVACY)
                            .tooltip(|window, cx| Tooltip::new(KEY_RETRY).build(window, cx)),
                    ),
            ),
        ]
    }

    /// Start over removes the Managed prefix, so it always asks first and
    /// names what is lost and what is kept.
    fn render_start_over_confirmation(
        &mut self,
        id: &'static str,
        snapshot: &UiSnapshot,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let index = match id {
            "overview" => 0_usize,
            "diagnostics" => 1,
            _ => 2,
        };
        let from_repair = id == "repair";
        let warning = start_over_warning(snapshot);
        v_flex()
            .id(("confirm-start-over", index))
            .gap_2()
            .child(
                Alert::error(("start-over-warning", index), warning)
                    .title("Remove the installation and start over?"),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new(("confirm-start-over-button", index))
                            .label("Remove and start over")
                            .danger()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.confirm_start_over = false;
                                // From Repair the setup begins again in view.
                                if this.start_over_confirmed(cx) && from_repair {
                                    this.navigate(Destination::Operations, cx);
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new(("cancel-start-over", index))
                            .label("Keep the installation")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.confirm_start_over = false;
                                cx.notify();
                            })),
                    ),
            )
            .test_support()
            .into_any_element()
    }

    /// "Stop and change" asks before it stops a running install.
    fn render_stop_confirmation(&mut self, step: SetupStep, cx: &mut Context<Self>) -> AnyElement {
        let what = match step {
            SetupStep::Setup => "how to get Diablo II",
            _ => "this step",
        };
        v_flex()
            .gap_2()
            .child(Alert::warning(
                "confirm-stop",
                format!(
                    "Stop at a safe point to change {what}? Downloaded files are kept and unfinished downloads continue later. Your CD keys stay filled in."
                ),
            ))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("confirm-stop-and-change")
                            .label("Stop and change")
                            .warning()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.confirm_stop = None;
                                this.emit(super::Intent::StopAndChange(step));
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("keep-installing")
                            .label("Keep going")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.confirm_stop = None;
                                cx.notify();
                            })),
                    ),
            )
            .into_any_element()
    }

    /// What a stopped install kept, and where to go from here.
    fn render_stop_result(
        &mut self,
        result: StopResultView,
        deck: &DeckPresentation,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let (kept, not_kept) = result.lines();
        let list =
            |heading: &'static str, lines: Vec<&'static str>| {
                (!lines.is_empty()).then(|| {
                    v_flex()
                        .gap_1()
                        .child(div().text_sm().font_semibold().child(heading))
                        .children(lines.into_iter().map(|line| {
                            div().text_sm().text_color(muted).child(format!("• {line}"))
                        }))
                })
            };
        let continues = deck
            .primary
            .enabled
            .then_some(deck.primary.id)
            .filter(|id| {
                matches!(
                    id,
                    ActionId::ContinueInstallation
                        | ActionId::Download
                        | ActionId::InstallProjectDiablo
                        | ActionId::ImportExisting
                )
            });
        let mut buttons = h_flex().gap_2().flex_wrap();
        if result.needs_start_over() {
            buttons = buttons.child(
                Button::new("stop-result-start-over")
                    .label("Start over…")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.confirm_start_over = true;
                        cx.notify();
                    })),
            );
        } else if result.needs_recovery() {
            buttons = buttons.child(
                Button::new("stop-result-diagnostics")
                    .label("Open Diagnostics")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.activate_action(ActionId::OpenDiagnostics, cx)
                    })),
            );
        } else {
            buttons = buttons
                .child(
                    Button::new("stop-result-continue")
                        .label("Continue")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.emit(super::Intent::DismissStopResult);
                            if let Some(action) = continues {
                                this.activate_action(action, cx);
                            }
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("stop-result-installers")
                        .label("Change installers")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.emit(super::Intent::EditStep(SetupStep::Setup));
                            cx.notify();
                        })),
                );
        }
        v_flex()
            .id("stop-result")
            .gap_4()
            .child(heading(
                if result.needs_recovery() {
                    "The installation stopped, but its state is unclear"
                } else {
                    "Stopped at a safe point"
                },
                None,
                muted,
            ))
            .child(
                v_flex()
                    .gap_3()
                    .children(list("Kept", kept))
                    .children(list("Not kept", not_kept)),
            )
            .child(buttons)
            .test_support()
            .into_any_element()
    }

    /// An unreachable location: check again, or start fresh elsewhere.
    fn render_location_unavailable(
        &mut self,
        snapshot: &UiSnapshot,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let location = self.location_body(snapshot, None, cx);
        v_flex()
            .id("location-unavailable")
            .gap_4()
            .child(heading("Install location unavailable", None, muted))
            .child(Alert::warning(
                "location-unavailable-alert",
                "The launcher can't find its installation folder, or the folder isn't the one it created. If it is on a removable drive, connect it and choose Check again. Nothing is installed anywhere else until you decide.",
            ))
            .child(location)
            .into_any_element()
    }

    /// Where the installation lives, and how to change it.
    /// `locked` names why it can't be changed right now.
    fn location_body(
        &mut self,
        snapshot: &UiSnapshot,
        locked: Option<&'static str>,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let danger = theme.danger;
        let installed = !matches!(snapshot.installation, InstallationStatus::NeedsBaseGame)
            || snapshot.location_unavailable();
        let pending = self.pending_location.clone();
        let mut section = v_flex()
            .gap_1()
            .child(div().text_sm().child(location_label(snapshot)))
            .child(div().text_xs().text_color(muted).child(
                "Pick a folder on a drive with at least 8 GiB free. The launcher creates a pd2-launcher folder inside it and never uses files that are already there.",
            ));
        if let Some(code) = snapshot.location_error() {
            section = section.child(
                div()
                    .text_xs()
                    .text_color(danger)
                    .child(location_error_message(code)),
            );
        }
        match pending {
            Some(parent) => {
                let target = parent.join("pd2-launcher");
                let consequence = if snapshot.location_unavailable() {
                    "The launcher starts a fresh installation there. The unreachable folder is left untouched."
                } else if installed {
                    "On the same drive the installation moves there as it is. On another drive the launcher installs again there (you'll need your CD keys) and keeps the current folder until the new one is ready; then you can remove it."
                } else {
                    "Nothing is installed yet; any downloads already made go there too."
                };
                section
                    .child(div().pt_2().child(Alert::info(
                        "confirm-location",
                        format!("Install into {}? {consequence}", target.display()),
                    )))
                    .child(
                        h_flex()
                            .pt_1()
                            .gap_2()
                            .child(
                                Button::new("confirm-location-button")
                                    .label("Install here")
                                    .small()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.pending_location = None;
                                        this.emit(super::Intent::ChooseInstallLocation(
                                            parent.clone(),
                                        ));
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("cancel-location")
                                    .label("Cancel")
                                    .small()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.pending_location = None;
                                        cx.notify();
                                    })),
                            ),
                    )
            }
            None => section.child(
                h_flex().pt_2().child(
                    Button::new("change-location")
                        .label("Change location…")
                        .small()
                        .disabled(locked.is_some())
                        .when_some(locked, |button, reason| button.tooltip(reason))
                        .on_click(cx.listener(|this, _, _, cx| this.choose_install_location(cx))),
                ),
            ),
        }
    }

    fn render_settings(&mut self, snapshot: &UiSnapshot, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let border = theme.border;
        let settings = snapshot.settings();
        let (saved_text, saved_color): (SharedString, Hsla) = match settings.persistence() {
            SettingsPersistenceView::Loading => ("Loading your settings…".into(), muted),
            SettingsPersistenceView::Saved => (
                "Saved. Changes apply the next time you press Play.".into(),
                muted,
            ),
            SettingsPersistenceView::Failed(code) => (
                format!("Your last change couldn't be saved ({code}).").into(),
                theme.danger,
            ),
        };
        let busy = settings.persistence() == SettingsPersistenceView::Loading;
        let location_lock = match step_affordance(snapshot, SetupStep::Setup) {
            StepAffordance::Change => None,
            StepAffordance::Locked(lock) => Some(lock.reason()),
            _ => Some("The install location can't be changed right now."),
        };

        let game = page_section("settings", "Game settings", None)
            .child(div().text_xs().text_color(saved_color).child(saved_text))
            .child(
                v_flex()
                    .gap_4()
                    .child(form_row(
                        "Graphics mode",
                        0.0,
                        v_flex()
                            .gap_1()
                            .child(
                                RadioGroup::horizontal("graphics-mode")
                                    .disabled(busy)
                                    .selected_index(Some(match settings.renderer() {
                                        RendererPreferenceView::Glide => 0,
                                        RendererPreferenceView::DirectDraw => 1,
                                    }))
                                    .on_change(cx.listener(|this, index: &usize, _, cx| {
                                        let renderer = if *index == 0 {
                                            RendererPreferenceView::Glide
                                        } else {
                                            RendererPreferenceView::DirectDraw
                                        };
                                        this.emit(super::Intent::ChangeSettings(
                                            SettingsChange::Renderer(renderer),
                                        ));
                                        cx.notify();
                                    }))
                                    .child(Radio::new("glide").small().label("Glide (recommended)"))
                                    .child(Radio::new("directdraw").small().label("DirectDraw")),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child("Try DirectDraw if the game shows a black screen."),
                            )
                            .when(
                                settings.renderer() == RendererPreferenceView::DirectDraw,
                                |column| {
                                    column.child(self.direct_draw_renderer_choice(
                                        settings.clone(),
                                        busy,
                                        cx,
                                    ))
                                },
                            ),
                    ))
                    .child(setting_switch(
                        "skip-to-battle-net",
                        "Skip to Battle.net",
                        "Go straight to the online login when the game starts.",
                        settings.skip_to_bnet(),
                        busy,
                        muted,
                        cx.listener(|this, checked: &bool, _, cx| {
                            this.emit(super::Intent::ChangeSettings(SettingsChange::SkipToBnet(
                                *checked,
                            )));
                            cx.notify();
                        }),
                    ))
                    .child(setting_switch(
                        "background-sound",
                        "Sound in the background",
                        "Keep game sound on when the window isn't focused.",
                        settings.sound_in_background(),
                        busy,
                        muted,
                        cx.listener(|this, checked: &bool, _, cx| {
                            this.emit(super::Intent::ChangeSettings(
                                SettingsChange::SoundInBackground(*checked),
                            ));
                            cx.notify();
                        }),
                    )),
            );
        let danger = cx.theme().danger;
        let game_files = game_files_folder(snapshot).map(|folder| {
            let opening = snapshot.game_files_open() == Some(FolderOpenView::Opening);
            let shown = display_path(&folder);
            let copied = folder.to_string_lossy().into_owned();
            page_section("game-files", "Game files", Some(border))
                .child(div().text_sm().child(shown))
                .child(div().text_xs().text_color(muted).child(
                    "Your loot filters and Project Diablo 2's settings are in this folder, and your characters in its Save, which the launcher keeps in a folder of its own so Start over never removes them.",
                ))
                .child(
                    h_flex()
                        .gap_2()
                        .flex_wrap()
                        .child(
                            Button::new("open-game-folder")
                                .label("Open game folder")
                                .small()
                                .loading(opening)
                                .disabled(opening)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.emit(super::Intent::OpenGameFolder);
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("copy-game-folder")
                                .label("Copy path")
                                .small()
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(
                                        copied.clone(),
                                    ));
                                })),
                        ),
                )
                .when(
                    snapshot.game_files_open() == Some(FolderOpenView::Failed),
                    |section| {
                        section.child(div().text_xs().text_color(danger).child(
                            "The folder couldn't be opened automatically: use Copy path to find it.",
                        ))
                    },
                )
                .test_support()
        });
        let official = page_section(
            "official-launcher-settings",
            "Official PD2 launcher",
            Some(border),
        )
        .child(
            div()
                .text_xs()
                .text_color(muted)
                .child("Changes apply the next time you open the official PD2 launcher."),
        )
        .child(setting_switch(
            "official-launcher-software-rendering",
            "Software rendering",
            "Draw the official PD2 launcher without the graphics card, so its window \
             isn't black. Keep it on for ARM64. It needs nothing extra installed.",
            settings.official_launcher_software_rendering(),
            busy,
            muted,
            cx.listener(|this, checked: &bool, _, cx| {
                this.emit(super::Intent::ChangeSettings(
                    SettingsChange::OfficialLauncherSoftwareRendering(*checked),
                ));
                cx.notify();
            }),
        ));
        let app_menu =
            page_section("app-menu-settings", "App menu", Some(border)).child(setting_switch(
                "app-menu-entry",
                "Add to the app menu",
                "Keep PD2 Launcher in your desktop's app menu, pointing at this copy of it. \
                 Turning it off removes the entry the launcher added.",
                settings.app_menu_entry(),
                busy,
                muted,
                cx.listener(|this, checked: &bool, _, cx| {
                    this.emit(super::Intent::ChangeSettings(SettingsChange::AppMenuEntry(
                        *checked,
                    )));
                    cx.notify();
                }),
            ));
        let fex = snapshot.fex_setting().cloned().map(|fex| {
            page_section("fex-settings", "FEX", Some(border))
                .child(self.fex_choice(snapshot, fex, busy, cx))
                .test_support()
        });
        let location = self.location_body(snapshot, location_lock, cx);
        let location =
            page_section("install-location", "Install location", Some(border)).child(location);

        let about = page_section("settings-about", "About", Some(border)).child(
            h_flex()
                .gap_4()
                .items_center()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .text_color(muted)
                        .child(format!("PD2 Launcher {}", about_version())),
                )
                .child(
                    Button::new("open-about")
                        .label("About PD2 Launcher")
                        .small()
                        .on_click(
                            cx.listener(|this, _, _, cx| this.navigate(Destination::About, cx)),
                        ),
                ),
        );

        v_flex()
            .id("settings-page")
            .gap_6()
            .child(game)
            .children(game_files)
            .child(official)
            .child(app_menu)
            .children(fex)
            .child(location)
            .child(about)
            .test_support()
            .into_any_element()
    }

    fn render_diagnostics(&mut self, snapshot: &UiSnapshot, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let border = theme.border;
        let system = &snapshot.system;
        let installation = facts(
            [
                ("Game", game_label(snapshot).into()),
                ("Updates", update_label(snapshot.update()).into()),
                ("Location", location_label(snapshot)),
            ],
            muted,
        );
        let device = facts(
            [
                ("Support", host_label(snapshot.host).into()),
                ("Checks", system.host.clone().into()),
                ("Runtime", system.runtime.clone().into()),
                ("Environment", system.prefix.clone().into()),
                ("Downloads", system.artifact_health.clone().into()),
            ],
            muted,
        );
        let evidence = v_flex().gap_2().children(if snapshot.evidence.is_empty() {
            vec![
                div()
                    .text_sm()
                    .text_color(muted)
                    .child("Findings appear here after the launcher checks your installation."),
            ]
        } else {
            snapshot
                .evidence
                .iter()
                .map(|item| {
                    h_flex()
                        .items_start()
                        .gap_4()
                        .text_sm()
                        .child(
                            div()
                                .w(gpui_kit::relative(LABEL_SHARE))
                                .max_w(px(LABEL_WIDTH))
                                .min_w(px(LABEL_MIN_WIDTH))
                                .flex_none()
                                .text_color(muted)
                                .child(item.label.clone()),
                        )
                        .child(div().flex_1().min_w_0().child(item.detail.clone()))
                        .child(evidence_tag(&item.tone))
                })
                .collect()
        });

        let official = official_launcher_offered(snapshot)
            .then(|| self.render_official_launcher(snapshot, cx));
        let start_over = self.render_start_over_section(snapshot, cx);
        let previous = self.render_previous_installation(snapshot, cx);
        let advanced_open = self.advanced_open;
        let sources = self.render_download_sources(snapshot, cx);
        let advanced = page_section("advanced", "Advanced", Some(border)).child(
            Accordion::new("advanced-sections")
                .h_auto()
                .on_toggle_click(cx.listener(|this, open: &[usize], _, cx| {
                    this.advanced_open = !open.is_empty();
                    cx.notify();
                }))
                .item(|item| {
                    item.open(advanced_open)
                        .title("Download sources")
                        .child(sources)
                }),
        );
        let creating = snapshot.report() == Some(&ReportView::Creating);
        let report_status = self.render_report_status(snapshot, "diagnostics", cx);
        // Start over comes first: it is what a stuck setup needs.
        let content = v_flex()
            .gap_6()
            .child(start_over)
            .child(
                page_section("report", "Report a problem", Some(border))
                    .child(div().text_sm().text_color(muted).child(REPORT_COPY))
                    .child(
                        h_flex().child(
                            Button::new("save-report")
                                .primary()
                                .label(if creating {
                                    "Creating report…"
                                } else {
                                    "Save report"
                                })
                                .loading(creating)
                                .disabled(creating)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.activate_action(ActionId::ExportDiagnostics, cx)
                                })),
                        ),
                    )
                    .children(report_status),
            )
            // What the running step's programs said, in their own words.
            .child(
                page_section("diagnostics-activity", "Activity", Some(border)).child(
                    progress_log(muted, border)
                        .map(IntoElement::into_any_element)
                        .unwrap_or_else(|| {
                            div()
                                .text_sm()
                                .text_color(muted)
                                .child("Nothing runs now. While a step runs, its latest lines show here.")
                                .into_any_element()
                        }),
                )
                .test_support(),
            )
            .child(page_section("installation", "Installation", Some(border)).child(installation))
            .child(page_section("your-device", "Your device", Some(border)).child(device))
            .child(
                page_section("findings", "Findings", Some(border))
                    .child(evidence.id("tech-checks").test_support()),
            )
            .children(official)
            .children(previous)
            .child(advanced);

        div()
            .id("diagnostics-page")
            .child(content)
            .test_support()
            .into_any_element()
    }
}

impl LauncherView {
    /// Diagnostics' section for Project Diablo 2's own launcher, for loot
    /// filters and its other settings (Play's second button opens it too).
    fn render_official_launcher(
        &self,
        snapshot: &UiSnapshot,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let border = theme.border;
        let open = snapshot.operation.as_ref().is_some_and(|operation| {
            operation.owns_mutation() && operation.phase == OperationPhase::OfficialLauncher
        });
        let available = official_launcher_available(snapshot);
        page_section(
            "diagnostics-official-launcher",
            "Official PD2 launcher",
            Some(border),
        )
        .child(
            h_flex()
                .items_start()
                .gap_4()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(muted)
                        .child(OFFICIAL_LAUNCHER_COPY),
                )
                .child(
                    Button::new(("open-official-launcher", 1_usize))
                        .label(if open {
                            "Official launcher is open"
                        } else {
                            "Open official PD2 launcher"
                        })
                        .small()
                        .disabled(!available)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.activate_action(ActionId::OpenOfficialLauncher, cx)
                        })),
                ),
        )
        .children(self.official_launcher_confirmation(snapshot, 1, cx))
        .into_any_element()
    }

    /// Opening a PD2Launcher.exe this launcher has not verified asks first.
    fn official_launcher_confirmation(
        &self,
        snapshot: &UiSnapshot,
        index: usize,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        (self.confirm_official_launcher
            && official_launcher_available(snapshot)
            && snapshot.official_launcher_unverified())
        .then(|| {
            v_flex()
                .gap_2()
                .child(Alert::warning(
                            ("confirm-official-launcher", index),
                            "PD2Launcher.exe matches neither the file list this launcher accepted nor the copy it recorded after the official launcher last ran. It may have updated itself, or been replaced. Run it anyway?",
                        ))
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    Button::new(("confirm-official-launcher-button", index))
                                        .label("Run it")
                                        .warning()
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.confirm_official_launcher = false;
                                            this.emit(super::Intent::OpenOfficialLauncherConfirmed(false, false));
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Button::new(("cancel-official-launcher", index))
                                        .label("Don't run it")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.confirm_official_launcher = false;
                                            cx.notify();
                                        })),
                                ),
                        )
        })
    }

    /// The installation a move to another drive left behind:
    /// removable, with confirmation, once the new one is Ready.
    fn render_previous_installation(
        &mut self,
        snapshot: &UiSnapshot,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let previous = snapshot.location()?.previous.clone()?;
        let theme = cx.theme();
        let (muted, border) = (theme.muted_foreground, theme.border);
        let removable = snapshot.installation == InstallationStatus::Ready
            && snapshot.game == GameView::Idle
            && !mutation_active(snapshot);
        let mut group = page_section("previous-installation", "Old installation", Some(border))
            .child(div().text_sm().text_color(muted).child(format!(
                "Moving to another drive left the old installation at {}. It is kept until the new one is ready; then you can remove it to free space.",
                previous.display()
            )));
        if self.confirm_remove_previous && removable {
            group = group
                .child(Alert::warning(
                    "confirm-remove-previous",
                    format!(
                        "Remove {} and everything in it? Its characters are moved to their own folder first, where they are kept. The current installation is not affected.",
                        previous.display()
                    ),
                ))
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("confirm-remove-previous-button")
                                .label("Remove old installation")
                                .danger()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.confirm_remove_previous = false;
                                    this.emit(super::Intent::RemovePreviousInstallation);
                                    cx.notify();
                                })),
                        )
                        .child(Button::new("keep-previous").label("Keep it").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.confirm_remove_previous = false;
                                cx.notify();
                            }),
                        )),
                );
        } else {
            group = group.child(
                h_flex().child(
                    Button::new("remove-previous")
                        .label("Remove old installation…")
                        .disabled(!removable)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.confirm_remove_previous = true;
                            cx.notify();
                        })),
                ),
            );
        }
        Some(group.into_any_element())
    }

    /// Diagnostics' Start over: the only way to reinstall a completed
    /// Diablo II.
    /// How DirectDraw mode draws: PD2's own setting (OpenGL), or Direct3D 9
    /// or software where OpenGL inside Wine shows nothing.
    fn direct_draw_renderer_choice(
        &mut self,
        settings: super::LaunchSettingsView,
        busy: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        use super::DirectDrawRendererView as View;
        let muted = cx.theme().muted_foreground;
        let choices = [View::Pd2, View::Direct3d9, View::Software, View::OpenGl];
        let selected = choices
            .iter()
            .position(|choice| *choice == settings.direct_draw_renderer());
        v_flex()
            .gap_1()
            .pt_2()
            .child(div().text_sm().font_medium().child("DirectDraw draws with"))
            .child(
                RadioGroup::vertical("direct-draw-renderer")
                    .selected_index(selected)
                    .disabled(busy)
                    .on_change(cx.listener(move |this, index: &usize, _, cx| {
                        this.emit(super::Intent::ChangeSettings(
                            SettingsChange::DirectDrawRenderer(choices[*index]),
                        ));
                        cx.notify();
                    }))
                    .child(Radio::new("ddraw-pd2").small().label("PD2's setting"))
                    .child(Radio::new("ddraw-d3d9").small().label("Direct3D 9"))
                    .child(Radio::new("ddraw-gdi").small().label("Software"))
                    .child(Radio::new("ddraw-opengl").small().label("OpenGL")),
            )
            .child(
                div().text_xs().text_color(muted).child(
                    "If the game window stays white or black, try Direct3D 9, then Software.",
                ),
            )
    }

    fn render_start_over_section(
        &mut self,
        snapshot: &UiSnapshot,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let available = start_over_available(snapshot);
        let confirm = (self.confirm_start_over && available)
            .then(|| self.render_start_over_confirmation("diagnostics", snapshot, cx));
        page_section("start-over", "Start over", None)
            .child(div().text_sm().text_color(muted).child(
                "Removes the Launcher's game environment and installs Diablo II, Lord of Destruction, and Project Diablo 2 again. Use it when an installation was interrupted or is damaged. Your characters are kept in a folder of their own, and everything already downloaded is kept, including Windows support and the Project Diablo 2 files, so reinstalling doesn't download it again.",
            ))
            .when_some(confirm, |group, confirm| group.child(confirm))
            .when(!self.confirm_start_over || !available, |group| {
                group.child(
                    h_flex().child(
                        Button::new("start-over-diagnostics")
                            .label("Start over…")
                            .disabled(!available)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.confirm_start_over = true;
                                cx.notify();
                            })),
                    ),
                )
            })
            .into_any_element()
    }

    fn render_download_sources(
        &mut self,
        snapshot: &UiSnapshot,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Stateful<Div> {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let danger = theme.danger;
        let warning = theme.warning;
        let rows = self
            .source_inputs
            .iter()
            .map(|(artifact, input)| {
                let artifact = *artifact;
                let source = snapshot
                    .download_sources()
                    .iter()
                    .find(|source| source.artifact == artifact);
                let badge = source.map_or(SourceBadgeView::BuiltInVerified, |source| source.badge);
                let tag = match badge {
                    SourceBadgeView::BuiltInVerified => Tag::success(),
                    SourceBadgeView::CustomFromEnvironment | SourceBadgeView::Custom => {
                        Tag::warning()
                    }
                }
                .small()
                .outline()
                .child(badge.label());
                let hint: Option<SharedString> = match badge {
                    SourceBadgeView::CustomFromEnvironment => Some(
                        format!(
                            "Set by {} in the launcher's environment. Unset it to return to the built-in source.",
                            artifact.environment_name()
                        )
                        .into(),
                    ),
                    SourceBadgeView::Custom => {
                        Some("Saved in the launcher's settings. Not verified.".into())
                    }
                    SourceBadgeView::BuiltInVerified => None,
                };
                let error = source.and_then(|source| source.error);
                let installed = source.and_then(|source| source.installed.clone());
                v_flex()
                    .gap_1()
                    .child(
                        h_flex()
                            .justify_between()
                            .gap_3()
                            .child(div().text_sm().font_semibold().child(artifact.label()))
                            .child(tag),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().flex_1().min_w_0().child(Input::new(input)))
                            .child(
                                Button::new(("save-source", source_index(artifact)))
                                    .label("Save")
                                    .h(px(FIELD_HEIGHT))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.save_download_source(artifact, cx)
                                    })),
                            )
                            .child(
                                Button::new(("reset-source", source_index(artifact)))
                                    .label("Reset to built-in")
                                    .h(px(FIELD_HEIGHT))
                                    .disabled(badge != SourceBadgeView::Custom)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.reset_download_source(artifact, cx)
                                    })),
                            ),
                    )
                    .when_some(hint, |row, hint| {
                        row.child(div().text_xs().text_color(muted).child(hint))
                    })
                    .when_some(installed, |row, installed| {
                        row.child(div().text_xs().text_color(warning).child(installed))
                    })
                    .when_some(error, |row, code| {
                        row.child(
                            div()
                                .text_xs()
                                .text_color(danger)
                                .child(format!("Not saved: {code}")),
                        )
                    })
            })
            .collect::<Vec<_>>();

        v_flex()
            .id("download-sources")
            .gap_4()
            .child(Alert::warning(
                "custom-sources-unverified",
                "Custom download sources are not verified. Built-in sources are verified before use: reviewed checksums and exact sizes for Wine, Wine Gecko, Wine Mono, the .NET Desktop Runtime, and the Project Diablo 2 installer; size, file-type, and file-name checks for Blizzard's installer downloads; the manifest's checksums for Project Diablo 2 client files. A file from a URL you enter is used as-is, at your own risk.",
            ))
            .child(div().text_xs().text_color(muted).child(
                "Changes apply to future downloads. A custom source never replaces a verified component that is already installed. With the built-in source selected, a Wine runtime, Wine Gecko, Wine Mono, or .NET Desktop Runtime installed from a custom source is no longer used and is replaced with the verified built-in during installation. Precedence: a source saved here, then a PD2_URL_* environment variable, then the built-in source.",
            ))
            .children(rows)
    }
}

/// What Start over's confirmation names: what is lost, and what is kept,
/// the characters first. A folder used in place is never
/// deleted: Start over only stops using it.
fn start_over_warning(snapshot: &UiSnapshot) -> SharedString {
    match snapshot.game_folder() {
        Some(folder) => format!(
            "Start over removes the Launcher's game environment and forgets your Diablo II folder. Your folder at {} is not deleted; the Project Diablo 2 files and the characters in it stay there. Everything already downloaded is kept, including Windows support and the Project Diablo 2 files, so reinstalling doesn't download it again.",
            folder.path.display()
        )
        .into(),
        None => "Start over removes the Launcher's game environment: the installed Diablo II, Lord of Destruction, and Project Diablo 2 files and their settings. Your characters are kept: they live in a folder of their own, outside the installation. Everything already downloaded is kept, including Windows support and the Project Diablo 2 files, so reinstalling doesn't download it again. You'll need your CD keys again to reinstall.".into(),
    }
}

/// Shown above the main actions whenever any download source is custom.
pub(super) const CUSTOM_SOURCES_BANNER: &str =
    "Custom download sources are active (unverified). Manage in Diagnostics → Download sources.";

/// What happens to the CD keys, in one line under the form.
const KEY_PRIVACY: &str = "Keys are never saved or shared. They stay only in the launcher's memory and are forgotten once Diablo II is installed, if installing it fails, on Start over, and when you close the launcher.";
/// The rest of the key handling, on hover over [`KEY_PRIVACY`].
const KEY_RETRY: &str = "Until then they stay filled in, so an install you stop continues without asking for them again.";

/// What Project Diablo 2's own launcher is for, under Play's second button.
const OFFICIAL_LAUNCHER_SHORT: &str = "For loot filters and its other settings.";

/// What Project Diablo 2's own launcher is for, and what happens while it
/// is open, in Diagnostics.
const OFFICIAL_LAUNCHER_COPY: &str = "Project Diablo 2's own launcher, for loot filters and its other settings; it runs in this launcher's game environment. While it is open, Play, updates, and installs here wait. When you close it, this launcher checks the installation again and keeps any game update it made.";

fn source_index(artifact: DownloadArtifact) -> usize {
    DownloadArtifact::ALL
        .iter()
        .position(|candidate| *candidate == artifact)
        .unwrap_or_default()
}

impl Render for LauncherView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_source_inputs(window, cx);
        let snapshot = self.state.snapshot().clone();
        let deck = DeckPresentation::project(&snapshot, self.authorization_complete(cx));
        let destination = self.state.destination();

        let chrome = self.window_chrome(window);
        let play = destination == Destination::Operations && self.play_shown(&snapshot, &deck);
        // A narrow window keeps only the current mark's name.
        let compact = window.viewport_size().width < px(COMPACT_HEADER_WIDTH);
        let header = self.render_header(destination, chrome, &snapshot, &deck, play, compact, cx);
        let interrupts = self.render_interrupts(&snapshot, cx);
        let body = if play {
            self.render_play_page(&snapshot, &deck, cx)
        } else {
            let content = match destination {
                Destination::Operations => self.render_overview(&snapshot, &deck, cx),
                Destination::Settings => self.render_settings(&snapshot, cx),
                Destination::Diagnostics => self.render_diagnostics(&snapshot, cx),
                Destination::About => self.render_about(cx),
                Destination::Repair => self.render_repair(&snapshot, &deck, cx),
            };
            let (back, action) = self.corner_actions(destination, &snapshot, &deck, cx);
            let wait = self.wait_note(action.as_ref(), &snapshot, cx);
            let corners = self.render_corners(back, action, cx);
            div()
                .id("body")
                .relative()
                .flex_1()
                .min_h_0()
                .w_full()
                .child(
                    div()
                        .id("page-scroll")
                        .size_full()
                        .overflow_y_scrollbar()
                        .child(
                            h_flex().w_full().justify_center().child(
                                v_flex()
                                    .id("pane")
                                    .w_full()
                                    .max_w(px(PANE_MAX_WIDTH))
                                    .px_6()
                                    .pt_5()
                                    .pb(px(CORNER_ROOM))
                                    .child(content)
                                    .children(wait)
                                    .test_support(),
                            ),
                        ),
                )
                .child(corners)
                .into_any_element()
        };

        let theme = cx.theme();
        // The shell fills the window edge to edge: the header is full-bleed,
        // the page's content column keeps a readable width in the middle,
        // and Back and the page's action sit in the bottom corners.
        let resize = chrome.resize_zones();
        let shell = v_flex()
            .id("shell")
            .size_full()
            .overflow_hidden()
            .bg(theme.background)
            .child(header.test_support())
            .children(interrupts)
            .child(body);

        v_flex()
            .id("operations-deck")
            .key_context("OperationsDeck")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::focus_next))
            .on_action(cx.listener(Self::focus_previous))
            .relative()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(shell.test_support())
            .children(resize.into_iter().map(resize_zone))
    }
}

/// How the window is decorated and placed, read from the platform each
/// frame. The launcher draws its own window buttons when it decorates the
/// window, or when the compositor claims to but draws none; resize edges
/// only in the first case, where the compositor also reports the window's
/// state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct WindowChrome {
    /// The header shows window buttons and moves the window.
    pub(super) draws_controls: bool,
    /// Edges held against a screen edge or a neighbor: no resize handle.
    pub(super) tiling: Tiling,
    /// `None` when the compositor's report is meaningless: Hyprland marks
    /// every window maximized, floating or not.
    pub(super) maximized: Option<bool>,
    pub(super) fullscreen: bool,
    pub(super) resizable: bool,
    /// What the compositor says it can do.
    pub(super) controls: WindowControls,
}

impl WindowChrome {
    pub(super) fn detect(window: &Window) -> Self {
        let (draws_controls, tiling, maximized) = match window.window_decorations() {
            Decorations::Client { tiling } => (true, tiling, Some(window.is_maximized())),
            // Some compositors answer the request for client decorations
            // with server-side ones and then draw no title bar at all.
            Decorations::Server => (
                std::env::var("XDG_CURRENT_DESKTOP")
                    .is_ok_and(|desktop| draws_no_title_bars(&desktop)),
                Tiling::default(),
                None,
            ),
        };
        Self {
            draws_controls,
            tiling,
            maximized,
            fullscreen: window.is_fullscreen(),
            resizable: window.is_resizable(),
            controls: window.window_controls(),
        }
    }

    /// The edges and corners that resize the window: every free edge of a
    /// client-decorated window that is not maximized. Where the compositor
    /// keeps the decorations it also keeps resizing.
    fn resize_zones(self) -> Vec<ResizeEdge> {
        if self.maximized != Some(false) || !self.resizable || self.fullscreen {
            return Vec::new();
        }
        let Tiling {
            top,
            left,
            right,
            bottom,
        } = self.tiling;
        [
            (ResizeEdge::Top, !top),
            (ResizeEdge::Bottom, !bottom),
            (ResizeEdge::Left, !left),
            (ResizeEdge::Right, !right),
            // Corners after the edges, so they win where they overlap.
            (ResizeEdge::TopLeft, !top && !left),
            (ResizeEdge::TopRight, !top && !right),
            (ResizeEdge::BottomLeft, !bottom && !left),
            (ResizeEdge::BottomRight, !bottom && !right),
        ]
        .into_iter()
        .filter_map(|(edge, free)| free.then_some(edge))
        .collect()
    }
}

/// Desktops that insist on server-side decorations yet draw no title bar
/// or window buttons, so the launcher must offer its own.
fn draws_no_title_bars(current_desktop: &str) -> bool {
    current_desktop.split(':').any(|desktop| {
        ["Hyprland", "niri"]
            .iter()
            .any(|known| desktop.eq_ignore_ascii_case(known))
    })
}

/// A thin band just inside a window edge (a square at a corner) that shows
/// the resize cursor and hands a press to the compositor to resize.
fn resize_zone(edge: ResizeEdge) -> AnyElement {
    let (band, corner) = (px(RESIZE_EDGE), px(RESIZE_CORNER));
    let zone = div()
        .id(ElementId::Name(format!("resize-{edge:?}").into()))
        .absolute()
        .cursor(match edge {
            ResizeEdge::Top | ResizeEdge::Bottom => CursorStyle::ResizeUpDown,
            ResizeEdge::Left | ResizeEdge::Right => CursorStyle::ResizeLeftRight,
            ResizeEdge::TopLeft | ResizeEdge::BottomRight => CursorStyle::ResizeUpLeftDownRight,
            ResizeEdge::TopRight | ResizeEdge::BottomLeft => CursorStyle::ResizeUpRightDownLeft,
        })
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            cx.stop_propagation();
            window.start_window_resize(edge);
        });
    match edge {
        ResizeEdge::Top => zone.top_0().left_0().right_0().h(band),
        ResizeEdge::Bottom => zone.bottom_0().left_0().right_0().h(band),
        ResizeEdge::Left => zone.top_0().bottom_0().left_0().w(band),
        ResizeEdge::Right => zone.top_0().bottom_0().right_0().w(band),
        ResizeEdge::TopLeft => zone.top_0().left_0().size(corner),
        ResizeEdge::TopRight => zone.top_0().right_0().size(corner),
        ResizeEdge::BottomLeft => zone.bottom_0().left_0().size(corner),
        ResizeEdge::BottomRight => zone.bottom_0().right_0().size(corner),
    }
    .test_support()
    .into_any_element()
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum CheckMark {
    Passed,
    Attention,
    Pending,
    /// Running now.
    Working,
    /// Not needed.
    Skipped,
    /// Did not finish.
    Failed,
}

/// What the computer check looked at, in plain words: display, Windows
/// support, free space when known, and the install location. Graphics is
/// not listed: the open window already proves the Launcher's own, and the
/// game's is checked once it is installed (the Play screen says when it
/// renders in software).
fn check_items(snapshot: &UiSnapshot) -> Vec<(&'static str, SharedString, CheckMark)> {
    let checking = snapshot.host != HostView::Supported;
    let audited = |label, pending: &'static str, passed: &'static str| {
        if checking {
            (label, SharedString::from(pending), CheckMark::Pending)
        } else {
            (label, SharedString::from(passed), CheckMark::Passed)
        }
    };
    let mut items = vec![
        audited(
            "Display",
            "Checking your desktop session…",
            "Your desktop can open the game's window.",
        ),
        audited(
            "Windows support",
            "Waiting for the check…",
            "Ready to download. The launcher sets it up for 32- and 64-bit programs.",
        ),
    ];
    if let Some(space) = snapshot
        .evidence
        .iter()
        .find(|evidence| evidence.label.to_ascii_lowercase().contains("space"))
    {
        items.push((
            "Disk space",
            space.detail.clone().into(),
            match space.tone {
                EvidenceTone::Verified => CheckMark::Passed,
                EvidenceTone::Working => CheckMark::Pending,
                EvidenceTone::Advisory | EvidenceTone::Failure => CheckMark::Attention,
            },
        ));
    }
    items.push(match snapshot.location() {
        None => (
            "Install location",
            "Finding the install folder…".into(),
            CheckMark::Pending,
        ),
        Some(location) if !location.available => (
            "Install location",
            format!("{} can't be reached.", location.path.display()).into(),
            CheckMark::Attention,
        ),
        Some(_) => (
            "Install location",
            format!("{} · you can change it next", location_label(snapshot)).into(),
            CheckMark::Passed,
        ),
    });
    items
}

/// A check item's mark: ✓ passed, ! needs a look, a quiet ring while checking.
fn check_mark(mark: CheckMark, cx: &App) -> Div {
    let theme = cx.theme();
    let (text, color, fill, ring) = match mark {
        CheckMark::Passed => (
            "✓",
            palette().teal,
            palette().active_fill,
            palette().active_fill,
        ),
        CheckMark::Attention => (
            "!",
            theme.warning,
            gpui_kit::transparent_black(),
            theme.warning,
        ),
        CheckMark::Pending => (
            "",
            theme.muted_foreground,
            gpui_kit::transparent_black(),
            theme.input,
        ),
        CheckMark::Working => (
            "•",
            palette().teal,
            gpui_kit::transparent_black(),
            theme.primary,
        ),
        CheckMark::Skipped => (
            "–",
            theme.muted_foreground,
            gpui_kit::transparent_black(),
            theme.input,
        ),
        CheckMark::Failed => (
            "!",
            theme.danger,
            gpui_kit::transparent_black(),
            theme.danger,
        ),
    };
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(20.0))
        .mt(px(1.0))
        .rounded_full()
        .border_1()
        .border_color(ring)
        .bg(fill)
        .text_xs()
        .font_semibold()
        .text_color(color)
        .child(text)
}

/// Step 3's items, one row each with its state's mark, name and line;
/// `compact` puts the line beside the name, one row a line (while it runs).
fn download_list(rows: &[DownloadRowView], compact: bool, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let (muted, border) = (theme.muted_foreground, theme.border);
    let count = rows.len();
    v_flex()
        .id("download-list")
        .children(rows.iter().enumerate().map(|(index, row)| {
            let mark = match row.state {
                DownloadItemState::Waiting => CheckMark::Pending,
                DownloadItemState::Running => CheckMark::Working,
                DownloadItemState::Done => CheckMark::Passed,
                DownloadItemState::NotNeeded => CheckMark::Skipped,
            };
            h_flex()
                .id(("download-item", index))
                .items_start()
                .gap_3()
                .py_1p5()
                .when(index + 1 < count, |item| {
                    item.border_b_1().border_color(border)
                })
                .when(compact, |item| item.py_1().items_center())
                .child(check_mark(mark, cx))
                .child(
                    div()
                        .flex()
                        .flex_1()
                        .min_w_0()
                        .when(compact, |text| text.flex_row().items_baseline().gap_2())
                        .when(!compact, |text| text.flex_col())
                        .child(div().flex_none().text_sm().font_medium().child(row.label))
                        .child(
                            div()
                                .min_w_0()
                                .text_xs()
                                .text_color(muted)
                                .truncate()
                                .child(row.detail.clone()),
                        ),
                )
        }))
        .test_support()
        .into_any_element()
}

/// A progress caption: the service's own summary, else "done of total".
fn progress_caption(operation: &OperationView) -> Option<String> {
    operation.progress_summary.clone().or_else(|| {
        match (operation.progress_completed, operation.progress_total) {
            (Some(done), Some(total)) => Some(format!(
                "{done} of {total} {}",
                operation.progress_unit.as_deref().unwrap_or("")
            )),
            _ => None,
        }
    })
}

/// A pane heading with its one-line explanation.
fn heading(title: impl Into<SharedString>, detail: Option<SharedString>, muted: Hsla) -> Div {
    v_flex()
        .gap_1()
        .child(div().text_lg().font_semibold().child(title.into()))
        .when_some(detail, |heading, detail| {
            heading.child(div().text_sm().text_color(muted).child(detail))
        })
}

/// A titled section of Settings or Diagnostics, set apart from the one
/// above it by a rule rather than a card.
fn page_section(
    id: &'static str,
    title: &'static str,
    rule: Option<Hsla>,
) -> gpui_kit::Stateful<Div> {
    v_flex()
        .id(id)
        .gap_3()
        .when_some(rule, |section, rule| {
            section.pt_6().border_t_1().border_color(rule)
        })
        .child(div().text_base().font_semibold().child(title))
}

/// One labelled row of a form: a fixed label column, then the control.
/// `label_offset` lines the label up with a 32px control's text.
/// What the running step did most recently, under its progress bar, so a
/// slow step visibly moves (and a failed one shows why).
fn progress_log(muted: Hsla, border: Hsla) -> Option<impl IntoElement> {
    let phase = crate::activity::current_phase().map(|(phase, elapsed)| {
        let seconds = elapsed.as_secs();
        format!("{phase} · {}:{:02}", seconds / 60, seconds % 60)
    });
    // Six lines in all, the phase and its timer first.
    let lines = crate::activity::recent_lines(PROGRESS_LOG_LINES - usize::from(phase.is_some()));
    (!lines.is_empty() || phase.is_some()).then(|| {
        v_flex()
            .id("progress-log")
            .p_2()
            .gap_0p5()
            .border_1()
            .border_color(border)
            .rounded_md()
            .text_xs()
            .text_color(muted)
            .children(phase.map(|phase| div().font_medium().child(phase)))
            .children(
                lines
                    .into_iter()
                    .map(|line| div().min_w_0().truncate().child(line)),
            )
            .test_support()
    })
}

/// Lines of the progress log; the step fits 960×600 with them.
const PROGRESS_LOG_LINES: usize = 6;

fn form_row(label: &'static str, label_offset: f32, content: impl IntoElement) -> Div {
    h_flex()
        .items_start()
        .gap_4()
        .child(
            div()
                .w(gpui_kit::relative(LABEL_SHARE))
                .max_w(px(LABEL_WIDTH))
                .min_w(px(LABEL_MIN_WIDTH))
                .flex_none()
                .pt(px(label_offset))
                .text_sm()
                .font_medium()
                .child(label),
        )
        .child(div().flex_1().min_w_0().child(content))
}

fn evidence_tag(tone: &EvidenceTone) -> Tag {
    let tag = match tone {
        EvidenceTone::Verified => Tag::success(),
        EvidenceTone::Working => Tag::primary(),
        EvidenceTone::Advisory => Tag::warning(),
        EvidenceTone::Failure => Tag::danger(),
    };
    tag.small().outline().child(evidence_word(tone))
}

#[allow(clippy::too_many_arguments)]
fn setting_switch(
    id: &'static str,
    title: &'static str,
    detail: &'static str,
    checked: bool,
    disabled: bool,
    muted: Hsla,
    on_change: impl Fn(&bool, &mut Window, &mut App) + 'static,
) -> Div {
    form_row(
        title,
        0.0,
        h_flex()
            .items_start()
            .gap_3()
            .child(
                Switch::new(id)
                    .checked(checked)
                    .disabled(disabled)
                    .on_change(on_change),
            )
            // The description wraps in what is left of the row instead of
            // running off its right edge.
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .text_color(muted)
                    .child(detail),
            ),
    )
}

/// Label/value rows in the form's label column. Values wrap instead of
/// clipping, which the kit's DescriptionList does not do.
fn facts<const N: usize>(rows: [(&'static str, SharedString); N], muted: Hsla) -> Div {
    v_flex()
        .gap_2()
        .text_sm()
        .children(rows.into_iter().map(move |(label, value)| {
            h_flex()
                .items_start()
                .gap_4()
                .child(
                    div()
                        .w(gpui_kit::relative(LABEL_SHARE))
                        .max_w(px(LABEL_WIDTH))
                        .min_w(px(LABEL_MIN_WIDTH))
                        .flex_none()
                        .text_color(muted)
                        .child(label),
                )
                .child(div().flex_1().min_w_0().child(value))
        }))
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use gpui_kit::{AppContext as _, Entity, TestAppContext};

    use super::super::{
        ActiveJobView, Destination, DistroFamily, DomainEvent, DownloadSourceView, HostView,
        InstallerMediaView, Intent, LauncherView, OperationPhase, OperationStatusView,
        OperationView, Prerequisite, SequencedEvent, ServiceEndpoint, SourceBadgeView,
        SystemContractView, UiSnapshot, service_bridge, theme,
    };
    use crate::{
        InstallationStatus, sources::DownloadArtifact, startup_credentials::StartupInputs,
    };

    #[gpui_kit::test]
    fn setup_and_diagnostics_render_installer_download_and_download_sources(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            theme::apply(cx);
        });
        let (endpoint, _service) = service_bridge(UiSnapshot::initial());
        let captured: Rc<RefCell<Option<Entity<LauncherView>>>> = Rc::default();
        let capture = captured.clone();
        let (_root, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| {
                LauncherView::new(
                    endpoint,
                    Box::new(()),
                    None,
                    StartupInputs::for_tests(None, None, false),
                    window,
                    cx,
                )
            });
            *capture.borrow_mut() = Some(view.clone());
            gpui_kit::component::Root::new(view, window, cx)
        });
        let view = captured.borrow().clone().expect("launcher view");
        let sources = DownloadArtifact::ALL
            .into_iter()
            .map(|artifact| DownloadSourceView {
                artifact,
                effective_url: format!("https://mirror.example/{artifact:?}"),
                badge: match artifact {
                    DownloadArtifact::Wine => SourceBadgeView::Custom,
                    DownloadArtifact::Gecko => SourceBadgeView::CustomFromEnvironment,
                    _ => SourceBadgeView::BuiltInVerified,
                },
                error: (artifact == DownloadArtifact::Pd2Installer).then_some("source-url-invalid"),
                installed: (artifact == DownloadArtifact::Gecko)
                    .then(|| "Installed: custom (unverified) from mirror.example".to_owned()),
            })
            .collect::<Vec<_>>();
        let snapshot = UiSnapshot::initial()
            .with_host(HostView::Supported)
            .with_installation(InstallationStatus::NeedsBaseGame)
            .with_installer_media(InstallerMediaView::BlizzardDownload)
            .with_download_sources(sources);

        // Overview setup step with the Blizzard download chosen.
        view.update(cx, |view, cx| {
            view.state.reduce(SequencedEvent::new(
                1,
                DomainEvent::FullSnapshot(Box::new(snapshot)),
            ));
            cx.notify();
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        // Each step editor opened from its row.
        for step in [
            super::super::SetupStep::Setup,
            super::super::SetupStep::InstallD2,
        ] {
            view.update(cx, |view, cx| {
                let snapshot = view.state.snapshot().clone().with_editing(Some(step));
                view.state.reduce(SequencedEvent::new(
                    view.state.snapshot().last_event_seq + 1,
                    DomainEvent::FullSnapshot(Box::new(snapshot)),
                ));
                cx.notify();
            });
            cx.update(|window, cx| {
                let _ = window.draw(cx);
            });
        }

        // The result card after a stopped install, and its recovery form.
        for recovery in [false, true] {
            view.update(cx, |view, cx| {
                let snapshot = view
                    .state
                    .snapshot()
                    .clone()
                    .with_editing(None)
                    .with_stop_result(Some(super::super::StopResultView::new(
                        InstallationStatus::NeedsBaseGame,
                        recovery,
                    )));
                view.state.reduce(SequencedEvent::new(
                    view.state.snapshot().last_event_seq + 1,
                    DomainEvent::FullSnapshot(Box::new(snapshot)),
                ));
                cx.notify();
            });
            cx.update(|window, cx| {
                let _ = window.draw(cx);
            });
        }

        // The location editor with a pending choice and an error, and the
        // unavailable-location card.
        for available in [true, false] {
            view.update(cx, |view, cx| {
                view.pending_location = Some(std::path::PathBuf::from("/home/deck/Games"));
                let snapshot = view
                    .state
                    .snapshot()
                    .clone()
                    .with_stop_result(None)
                    .with_editing(Some(super::super::SetupStep::Setup))
                    .with_location_error(Some("location-free-space"))
                    .with_location(Some(super::super::LocationView {
                        path: std::path::PathBuf::from("/run/media/deck/card/pd2-launcher"),
                        default: false,
                        available,
                        previous: Some(std::path::PathBuf::from(
                            "/home/deck/.local/share/pd2-launcher/managed",
                        )),
                    }));
                view.state.reduce(SequencedEvent::new(
                    view.state.snapshot().last_event_seq + 1,
                    DomainEvent::FullSnapshot(Box::new(snapshot)),
                ));
                cx.notify();
            });
            cx.update(|window, cx| {
                let _ = window.draw(cx);
            });
        }

        // Diagnostics with every download source row.
        view.update(cx, |view, cx| {
            view.state.navigate(Destination::Diagnostics);
            cx.notify();
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        view.read_with(cx, |view, cx| {
            assert_eq!(view.source_inputs.len(), DownloadArtifact::ALL.len());
            for (artifact, input) in &view.source_inputs {
                assert_eq!(
                    input.read(cx).value().as_ref(),
                    format!("https://mirror.example/{artifact:?}"),
                    "each field shows its effective URL"
                );
            }
        });
    }

    type Window<'a> = &'a mut gpui_kit::VisualTestContext;

    fn open_view(
        cx: &mut TestAppContext,
        startup: StartupInputs,
    ) -> (Entity<LauncherView>, ServiceEndpoint, Window<'_>) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            theme::apply(cx);
        });
        let (endpoint, service) = service_bridge(UiSnapshot::initial());
        let captured: Rc<RefCell<Option<Entity<LauncherView>>>> = Rc::default();
        let capture = captured.clone();
        let (_root, cx) = cx.add_window_view(move |window, cx| {
            let view =
                cx.new(|cx| LauncherView::new(endpoint, Box::new(()), None, startup, window, cx));
            *capture.borrow_mut() = Some(view.clone());
            gpui_kit::component::Root::new(view, window, cx)
        });
        let view = captured.borrow().clone().expect("launcher view");
        (view, service, cx)
    }

    fn show(view: &Entity<LauncherView>, cx: Window<'_>, snapshot: UiSnapshot) {
        view.update(cx, |view, cx| {
            let sequence = view.state.snapshot().last_event_seq + 1;
            view.state.reduce(SequencedEvent::new(
                sequence,
                DomainEvent::FullSnapshot(Box::new(snapshot)),
            ));
            view.follow_service_state(cx);
            cx.notify();
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }

    fn setup_snapshot() -> UiSnapshot {
        UiSnapshot::initial()
            .with_host(HostView::Supported)
            .with_installation(InstallationStatus::NeedsBaseGame)
            .with_installer_media(InstallerMediaView::BlizzardDownload)
            .with_operation(None)
    }

    #[gpui_kit::test]
    fn settings_offer_built_in_or_device_fex_and_a_file_picker(cx: &mut TestAppContext) {
        let (view, service, cx) = open_view(cx, StartupInputs::for_tests(None, None, false));
        while service.intent_receiver.try_recv().is_ok() {}
        view.update(cx, |view, cx| {
            view.state.navigate(Destination::Settings);
            cx.notify();
        });
        let device = super::super::FexSettingView {
            in_use: Some(std::path::PathBuf::from("/usr/local/bin/FEX")),
            ..Default::default()
        };
        show(
            &view,
            cx,
            setup_snapshot().with_fex_setting(Some(device.clone())),
        );
        assert!(shown(cx, "fex-settings").is_some());
        // Device FEX: the FEX in use is shown.
        assert!(shown(cx, "fex-in-use").is_some());
        assert!(shown(cx, "fex-problem").is_none());

        // A file chosen with the picker is saved as the device FEX.
        let chosen = std::path::PathBuf::from("/home/player/fex/FEX");
        view.update(cx, |view, cx| {
            view.chose_fex_file(Some(Some(vec![chosen.clone()])), cx);
        });
        assert!(matches!(
            service.intent_receiver.try_recv(),
            Ok(Intent::SaveFex(Some(path))) if path == chosen
        ));
        // Closing the picker saves nothing.
        view.update(cx, |view, cx| view.chose_fex_file(Some(None), cx));
        assert!(service.intent_receiver.try_recv().is_err());
        // Without a picker the path is typed instead, and the section says so.
        view.update(cx, |view, cx| view.chose_fex_file(None, cx));
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(shown(cx, "fex-problem").is_some());
        view.update(cx, |view, _| {
            assert_eq!(
                view.fex_picker_problem,
                Some(super::super::FEX_PICKER_UNAVAILABLE)
            );
        });

        // A typed path that isn't usable comes back with its problem.
        show(
            &view,
            cx,
            setup_snapshot().with_fex_setting(Some(super::super::FexSettingView {
                problem: Some("There's no executable file there."),
                ..device.clone()
            })),
        );
        assert!(shown(cx, "fex-problem").is_some());

        // With the built-in FEX in use the device's details are not shown;
        // the built-in FEX says where it stands.
        let builtin = super::super::FexSettingView {
            source: super::super::FexSourceView::Builtin,
            builtin_available: true,
            builtin_download_bytes: 528_000_000,
            ..device
        };
        show(
            &view,
            cx,
            setup_snapshot().with_fex_setting(Some(builtin.clone())),
        );
        assert!(shown(cx, "fex-in-use").is_none());
        assert!(shown(cx, "fex-builtin-status").is_some());
        // Step 1 offers the same choice.
        view.update(cx, |view, cx| {
            view.state.navigate(Destination::Operations);
            view.guide = Some(crate::presentation::GuidedCursor::at(
                crate::presentation::SetupStep::Device,
            ));
            cx.notify();
        });
        show(
            &view,
            cx,
            located(setup_snapshot()).with_fex_setting(Some(builtin)),
        );
        assert!(shown(cx, "guided-fex").is_some());
        assert!(shown(cx, "fex-builtin-status").is_some());
    }

    #[gpui_kit::test]
    fn start_over_brings_the_guided_setup_back_to_step_one(cx: &mut TestAppContext) {
        let (view, service, cx) = open_view(cx, StartupInputs::for_tests(None, None, false));
        while service.intent_receiver.try_recv().is_ok() {}
        // Install was pressed: the steps followed the installation, which
        // then stopped at Install Diablo II.
        view.update(cx, |view, _| view.guide = None);
        show(
            &view,
            cx,
            located(setup_snapshot()).with_installation(InstallationStatus::StageInterrupted(
                crate::domain::InterruptedStage::Prefix,
            )),
        );
        view.update(cx, |view, cx| assert!(view.start_over_confirmed(cx)));
        assert!(matches!(
            service.intent_receiver.try_recv(),
            Ok(Intent::StartOver)
        ));
        // The prefix is gone and the downloads are kept.
        show(
            &view,
            cx,
            located(setup_snapshot()).with_downloads(crate::presentation::DownloadsView {
                complete: true,
                installers: true,
                total_bytes: 1,
            }),
        );
        view.update(cx, |view, _| {
            let cursor = view.guide.expect("the guided setup is back");
            assert_eq!(cursor.current(), crate::presentation::SetupStep::Device);
        });
        // Step 1 shows its computer check again.
        assert!(shown(cx, "check-list").is_some());
    }

    #[gpui_kit::test]
    fn a_first_start_asks_for_the_default_installer_source(cx: &mut TestAppContext) {
        let (_view, service, _cx) = open_view(cx, StartupInputs::for_tests(None, None, false));
        // Blizzard's download, unless a choice was restored from the last
        // Download.
        assert!(matches!(
            service.intent_receiver.try_recv(),
            Ok(Intent::UseDefaultInstallerSource)
        ));
    }

    #[gpui_kit::test]
    fn a_startup_folder_is_used_instead_of_the_default(cx: &mut TestAppContext) {
        let folder = std::path::PathBuf::from("/media/installers");
        let (_view, service, _cx) = open_view(
            cx,
            StartupInputs::for_tests(None, Some(folder.clone()), false),
        );
        assert!(matches!(
            service.intent_receiver.try_recv(),
            Ok(Intent::SelectInstallerMedia(path)) if path == folder
        ));
        assert!(service.intent_receiver.try_recv().is_err());
    }

    #[gpui_kit::test]
    fn install_waits_for_two_valid_cd_keys_and_consent(cx: &mut TestAppContext) {
        let (view, _service, cx) = open_view(cx, StartupInputs::for_tests(None, None, false));
        show(&view, cx, setup_snapshot());
        view.update(cx, |view, cx| {
            // Pasted with dashes and in lower case: normalized as typed.
            view.base_key.update(cx, |input, cx| {
                input.restore("abcd-efgh-ijkl-mno".to_owned(), cx)
            });
            view.expansion_key.update(cx, |input, cx| {
                input.restore("CDFG HJKM NPRT VWXY Z246 789B CD".to_owned(), cx)
            });
            view.consent = true;
            assert_eq!(view.base_key.read(cx).len(), 15);
            assert_eq!(
                view.expansion_key.read(cx).copy_value(),
                "CDFGHJKMNPRTVWXYZ246789BCD"
            );
            assert!(
                !view.authorization_complete(cx),
                "15 characters is not a key"
            );
            view.base_key.update(cx, |input, cx| {
                input.restore("BCDFGHJKMNPRTVWX".to_owned(), cx)
            });
            assert!(
                !view.authorization_complete(cx),
                "an old 16-character key is not accepted"
            );
            view.base_key.update(cx, |input, cx| {
                input.restore("BCDFGHJKMNPRTVWXYZ24678922".to_owned(), cx);
                input.toggle_revealed(cx);
            });
            assert!(view.authorization_complete(cx));
            view.consent = false;
            assert!(!view.authorization_complete(cx));
            view.consent = true;
            cx.notify();
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        // Once both games are installed the keys are done with.
        show(
            &view,
            cx,
            setup_snapshot().with_installation(InstallationStatus::NeedsClient),
        );
        view.update(cx, |view, cx| {
            assert_eq!(view.base_key.read(cx).len(), 0);
            assert_eq!(view.expansion_key.read(cx).len(), 0);
            assert!(!view.base_key.read(cx).revealed, "a new key starts masked");
        });
    }

    #[gpui_kit::test]
    fn the_cd_keys_are_forgotten_once_they_are_done_with(cx: &mut TestAppContext) {
        let (view, service, cx) = open_view(cx, StartupInputs::for_tests(None, None, false));
        let fill = |view: &Entity<LauncherView>, cx: Window<'_>| {
            view.update(cx, |view, cx| {
                for input in [view.base_key.clone(), view.expansion_key.clone()] {
                    input.update(cx, |input, cx| {
                        input.restore("BCDFGHJKMNPRTVWXYZ24678922".to_owned(), cx)
                    });
                }
            });
        };
        let held = |view: &Entity<LauncherView>, cx: Window<'_>| {
            view.update(cx, |view, cx| {
                (
                    view.base_key.read(cx).len(),
                    view.expansion_key.read(cx).len(),
                )
            })
        };
        let outcome = |status, failure: Option<&'static str>| {
            let operation = OperationView::mutating(
                OperationPhase::BaseGameInstall,
                status,
                "Operation complete",
            )
            .with_job(ActiveJobView::InstallBaseGame);
            setup_snapshot().with_operation(Some(match failure {
                Some(code) => operation.with_failure(code),
                None => operation,
            }))
        };
        show(&view, cx, setup_snapshot());
        fill(&view, cx);

        // A stop continues later with the same keys.
        show(&view, cx, outcome(OperationStatusView::Cancelled, None));
        assert_eq!(held(&view, cx), (26, 26));
        // A failed Install Diablo II forgets them: a key may have been wrong.
        show(
            &view,
            cx,
            outcome(OperationStatusView::Failed, Some("base-game-key-rejected")),
        );
        assert_eq!(held(&view, cx), (0, 0));
        // Keys entered again under the same result card stay for the retry.
        fill(&view, cx);
        show(
            &view,
            cx,
            outcome(OperationStatusView::Failed, Some("base-game-key-rejected")),
        );
        assert_eq!(held(&view, cx), (26, 26));

        // Start over forgets them.
        let interrupted = setup_snapshot().with_installation(InstallationStatus::StageInterrupted(
            crate::InterruptedStage::BaseGame,
        ));
        show(&view, cx, interrupted);
        assert_eq!(held(&view, cx), (26, 26));
        view.update(cx, |view, cx| assert!(view.start_over_confirmed(cx)));
        assert_eq!(held(&view, cx), (0, 0));

        // An import needs none: none stay behind.
        while service.intent_receiver.try_recv().is_ok() {}
        fill(&view, cx);
        press(&view, cx, super::super::ActionId::ImportExisting);
        assert_eq!(held(&view, cx), (0, 0));
        assert!(matches!(
            service.intent_receiver.try_recv(),
            Ok(Intent::ImportExistingInstall(_))
        ));
    }

    #[gpui_kit::test]
    fn closing_during_an_install_asks_and_then_stops_safely(cx: &mut TestAppContext) {
        let (view, service, cx) = open_view(cx, StartupInputs::for_tests(None, None, false));
        let _ = service.intent_receiver.try_recv();
        let installing = setup_snapshot()
            .with_active_job(Some(ActiveJobView::InstallBaseGame))
            .with_operation(Some(OperationView::mutating(
                OperationPhase::BaseGameInstall,
                OperationStatusView::Running,
                "Installing Diablo II",
            )));
        show(&view, cx, installing);
        view.update(cx, |view, cx| {
            assert!(!view.should_close(cx), "the first close only asks");
            assert!(view.confirm_close);
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        view.update(cx, |view, cx| {
            view.stop_and_close(cx);
            assert!(view.closing && !view.confirm_close);
            // Still stopping: the window stays open and responsive.
            assert!(!view.should_close(cx));
        });
        assert!(matches!(
            service.intent_receiver.try_recv(),
            Ok(Intent::RequestSafeStop)
        ));
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }

    /// Where an element observed in the last frame whose id is `id` is.
    fn shown(cx: Window<'_>, id: &'static str) -> Option<gpui_kit::Bounds<gpui_kit::Pixels>> {
        cx.update(|window, _| {
            gpui_kit::base::test_support::snapshots(window)
                .into_iter()
                .find(|element| element.path().last() == Some(&gpui_kit::ElementId::from(id)))
                .map(|element| element.bounds())
        })
    }

    #[gpui_kit::test]
    fn the_shell_shows_one_task_under_the_step_marks_with_no_footer(cx: &mut TestAppContext) {
        let (view, _service, cx) = open_view(cx, StartupInputs::for_tests(None, None, false));
        cx.simulate_resize(gpui_kit::size(gpui_kit::px(960.0), gpui_kit::px(600.0)));
        show(&view, cx, located(setup_snapshot()));
        let header = shown(cx, "header").expect("the header");
        let marks = shown(cx, "step-marks").expect("the four step marks");
        assert!(marks.top() >= header.top() && marks.bottom() <= header.bottom());
        assert!(shown(cx, "step-rail").is_none(), "no rail");
        assert!(shown(cx, "action-bar").is_none(), "no footer bar");
        for index in 0..4 {
            assert!(shown_indexed(cx, "step-mark", index), "mark {index}");
        }
        // The title bar leads with the launcher's icon.
        let mark = shown(cx, "brand-mark").expect("the brand mark");
        assert_eq!(mark.size.width, gpui_kit::px(24.0));
        assert!(mark.bottom() <= header.bottom());
        assert_eq!(
            super::brand_mark().format(),
            gpui_kit::ImageFormat::Svg,
            "drawn from the app menu's SVG"
        );
        assert_eq!(super::brand_mark().bytes(), crate::app_menu::ICON);
        assert!(shown(cx, "overview-page").is_some());
        // The step's action sits in the bottom-right corner.
        let action = shown(cx, "primary-action").expect("the step's action");
        assert!(action.right() >= gpui_kit::px(900.0), "{action:?}");
        assert!(action.bottom() >= gpui_kit::px(560.0), "{action:?}");
        assert!(shown(cx, "back-action").is_none(), "no Back on step 1");

        // Back sits in the bottom-left corner.
        view.update(cx, |view, _| {
            view.guide = Some(super::super::GuidedCursor::at(
                super::super::SetupStep::Setup,
            ));
        });
        show(&view, cx, located(setup_snapshot()));
        let back = shown(cx, "back-action").expect("Back");
        assert!(back.left() <= gpui_kit::px(40.0), "{back:?}");
        assert!(back.bottom() >= gpui_kit::px(560.0), "{back:?}");

        // Progress replaces the form in the pane, its percent beside the
        // current mark and along the header, and Stop takes the corner.
        show(
            &view,
            cx,
            located(setup_snapshot())
                .with_active_job(Some(ActiveJobView::Download))
                .with_operation(Some(
                    OperationView::mutating(
                        OperationPhase::BaseGameDownload,
                        OperationStatusView::Running,
                        "Downloading Diablo II",
                    )
                    .with_job(ActiveJobView::Download)
                    .with_typed_progress(40, Some(100), "bytes"),
                )),
        );
        assert!(shown(cx, "progress-view").is_some());
        assert!(shown(cx, "step-percent").is_some());
        let rule = shown(cx, "header-progress").expect("the header's rule");
        assert_eq!(
            rule.size.height,
            gpui_kit::px(2.0),
            "it carries the progress"
        );
        assert!(shown(cx, "stop-action").is_some());
        assert!(shown(cx, "back-action").is_none(), "no Back while it runs");
        // The installer choice changes after a confirmed stop.
        let change = shown(cx, "stop-and-change").expect("Stop and change");
        cx.simulate_click(change.center(), gpui_kit::Modifiers::none());
        view.read_with(cx, |view, _| {
            assert_eq!(view.confirm_stop, Some(super::super::SetupStep::Setup))
        });
        view.update(cx, |view, _| view.confirm_stop = None);

        // Ready: Play alone in the middle, the destinations along its foot,
        // no step marks and no header links (a returning user).
        view.update(cx, |view, _| {
            view.guide = Some(super::super::GuidedCursor::new());
        });
        let ready = located(setup_snapshot())
            .with_installation(InstallationStatus::Ready)
            .with_launch_preflight(true);
        show(&view, cx, ready.clone());
        let surface = shown(cx, "play-surface").expect("the Play page");
        let foot = shown(cx, "play-foot").expect("its foot");
        assert!(surface.bottom() <= foot.top());
        assert!(shown(cx, "step-marks").is_none());
        assert!(shown(cx, "sections").is_none());
        assert!(shown(cx, "corner-actions").is_none());
        let play = shown(cx, "primary-action").expect("Play");
        assert!(
            (play.center().x - gpui_kit::px(480.0)).abs() <= gpui_kit::px(2.0),
            "Play is centred: {play:?}"
        );
        // One line of facts under it: renderer, card, update state.
        assert!(shown(cx, "play-facts").expect("the facts").top() >= play.bottom());
        for id in [
            "play-settings",
            "play-repair",
            "play-diagnostics",
            "play-about",
        ] {
            assert!(shown(cx, id).is_some(), "{id}");
        }
        assert!(shown(cx, "game-notice").is_none());
        assert!(shown(cx, "software-rendering").is_none());

        // A game that renders in software is said so on Play.
        show(&view, cx, ready.clone().with_game_software_rendering(true));
        assert!(shown(cx, "software-rendering").is_some());

        // Until the game has been launched once, Play warns that the first
        // launch opens at the wrong size, above the foot.
        show(
            &view,
            cx,
            ready
                .clone()
                .with_game_notice(Some(super::super::GameNoticeView::FirstD2glLaunch)),
        );
        let warning = shown(cx, "game-notice").expect("the first-launch warning is shown");
        let foot = shown(cx, "play-foot").expect("its foot");
        assert!(
            warning.bottom() <= foot.top(),
            "the warning fits above the foot"
        );

        // Settings, Diagnostics and About fill the pane, with Back.
        for (destination, page) in [
            (Destination::Settings, "settings-page"),
            (Destination::Diagnostics, "diagnostics-page"),
            (Destination::About, "about-page"),
        ] {
            view.update(cx, |view, cx| {
                view.state.navigate(destination);
                cx.notify();
            });
            cx.update(|window, cx| {
                let _ = window.draw(cx);
            });
            assert!(shown(cx, page).is_some(), "{page} is shown");
            assert!(shown(cx, "step-marks").is_none(), "no marks on {page}");
            assert!(shown(cx, "sections").is_some(), "links on {page}");
            assert!(shown(cx, "back-action").is_some(), "Back on {page}");
            // About names the version and what it runs with.
            assert_eq!(
                shown(cx, "about-facts").is_some(),
                destination == Destination::About
            );
            if destination == Destination::About {
                let end = shown(cx, "about-end").expect("About's last line");
                let terms = shown(cx, "about-terms-list").expect("How it works");
                assert!(end.top() >= terms.bottom(), "{end:?} {terms:?}");
            }
        }
        // Back returns to the Overview.
        press(&view, cx, super::super::ActionId::ReturnToOverview);
        view.read_with(cx, |view, _| {
            assert_eq!(view.state.destination(), Destination::Operations)
        });
    }

    #[gpui_kit::test]
    fn repair_opens_from_play_with_check_again_and_start_over(cx: &mut TestAppContext) {
        let (view, service, cx) = open_view(cx, StartupInputs::for_tests(None, None, false));
        cx.simulate_resize(gpui_kit::size(gpui_kit::px(960.0), gpui_kit::px(600.0)));
        let ready = located(setup_snapshot())
            .with_installation(InstallationStatus::Ready)
            .with_launch_preflight(true)
            .with_update(crate::UpdateStatus::UpToDate);
        show(&view, cx, ready.clone());
        while service.intent_receiver.try_recv().is_ok() {}
        let repair = shown(cx, "play-repair").expect("Repair along Play's foot");
        cx.simulate_click(repair.center(), gpui_kit::Modifiers::none());
        show(&view, cx, ready.clone());
        assert!(shown(cx, "repair-page").is_some());
        for index in 0..3 {
            assert!(shown_indexed(cx, "repair-row", index), "row {index}");
        }
        assert!(shown(cx, "back-action").is_some());
        assert!(
            shown(cx, "repair-continue").is_none(),
            "nothing to continue"
        );
        // Check again sits in the corner and checks again.
        while service.intent_receiver.try_recv().is_ok() {}
        press(&view, cx, super::super::ActionId::CheckAgain);
        assert!(matches!(
            service.intent_receiver.try_recv(),
            Ok(Intent::CheckAgain)
        ));
        let check = shown(cx, "primary-action").expect("Check again");
        assert!(check.right() >= gpui_kit::px(900.0));
        // Start over asks first; confirmed, the setup begins again in view.
        while service.intent_receiver.try_recv().is_ok() {}
        let start_over = shown(cx, "repair-start-over").expect("Start over");
        cx.simulate_click(start_over.center(), gpui_kit::Modifiers::none());
        show(&view, cx, ready.clone());
        assert!(shown_indexed(cx, "confirm-start-over", 2));
        assert!(service.intent_receiver.try_recv().is_err(), "nothing yet");
        let confirm = cx
            .update(|window, _| {
                gpui_kit::base::test_support::snapshots(window)
                    .into_iter()
                    .find(|element| {
                        element.path().last()
                            == Some(&gpui_kit::ElementId::from((
                                "confirm-start-over-button",
                                2_usize,
                            )))
                    })
                    .map(|element| element.bounds())
            })
            .expect("Remove and start over");
        cx.simulate_click(confirm.center(), gpui_kit::Modifiers::none());
        assert!(matches!(
            service.intent_receiver.try_recv(),
            Ok(Intent::StartOver)
        ));
        view.read_with(cx, |view, _| {
            assert_eq!(view.state.destination(), Destination::Operations)
        });

        // A broken installed game goes on with Continue installation.
        view.update(cx, |view, cx| {
            view.state.navigate(Destination::Repair);
            cx.notify();
        });
        show(
            &view,
            cx,
            located(setup_snapshot()).with_installation(InstallationStatus::NeedsRuntime),
        );
        assert!(shown(cx, "repair-continue").is_some());
    }

    #[gpui_kit::test]
    fn results_prerequisites_and_progress_render(cx: &mut TestAppContext) {
        let (view, _service, cx) = open_view(cx, StartupInputs::for_tests(None, None, false));
        let failure = |code| {
            setup_snapshot().with_operation(Some(
                OperationView::mutating(
                    OperationPhase::Verification,
                    OperationStatusView::Failed,
                    "Operation complete",
                )
                .with_job(ActiveJobView::InstallBaseGame)
                .with_failure(code),
            ))
        };
        for snapshot in [
            // A rejected key: the card and the form with the keys.
            failure("base-game-key-rejected"),
            failure("expansion-key-rejected").with_installation(InstallationStatus::NeedsExpansion),
            failure("stage-interrupted").with_installation(InstallationStatus::StageInterrupted(
                crate::InterruptedStage::Expansion,
            )),
            // Missing prerequisites, with and without a known package manager.
            setup_snapshot()
                .with_host(HostView::Unsupported)
                .with_system(SystemContractView {
                    missing: vec![Prerequisite::VulkanDriver, Prerequisite::XWayland],
                    distro: DistroFamily::Arch,
                    ..setup_snapshot().system().clone()
                }),
            setup_snapshot().with_host(HostView::Unsupported),
            // The launcher's own service failed.
            setup_snapshot().with_operation(Some(
                OperationView::read_only(
                    OperationPhase::HostAudit,
                    OperationStatusView::Failed,
                    "Launcher service could not establish safe evidence",
                )
                .with_failure("paths-unavailable"),
            )),
            // Blizzard's installer without a measure, then its download.
            setup_snapshot().with_operation(Some(OperationView::mutating(
                OperationPhase::BaseGameInstall,
                OperationStatusView::Running,
                "Installing Diablo II",
            ))),
            setup_snapshot().with_operation(Some(
                OperationView::mutating(
                    OperationPhase::BaseGameInstall,
                    OperationStatusView::Running,
                    "Installing Diablo II",
                )
                .with_typed_progress(800_000_000, Some(1_554_530_117), "bytes")
                .with_progress_summary("800.0 MB of 1.6 GB · 5.0 MB/s · about 3 minutes left"),
            )),
            // A folder chosen instead of the download.
            setup_snapshot().with_installer_media(InstallerMediaView::Validated(
                std::path::PathBuf::from("/media/installers"),
            )),
        ] {
            show(&view, cx, snapshot);
        }
    }

    fn shown_indexed(cx: Window<'_>, id: &'static str, index: usize) -> bool {
        cx.update(|window, _| {
            gpui_kit::base::test_support::snapshots(window)
                .into_iter()
                .any(|element| {
                    element.path().last() == Some(&gpui_kit::ElementId::from((id, index)))
                })
        })
    }

    #[gpui_kit::test]
    fn save_report_is_offered_on_a_failure_and_in_diagnostics_with_its_result(
        cx: &mut TestAppContext,
    ) {
        let (view, service, cx) = open_view(cx, StartupInputs::for_tests(None, None, false));
        let failed = setup_snapshot().with_operation(Some(
            OperationView::mutating(
                OperationPhase::Verification,
                OperationStatusView::Failed,
                "Operation complete",
            )
            .with_job(ActiveJobView::InstallBaseGame)
            .with_failure("base-game-install-failed"),
        ));
        show(&view, cx, failed.clone());
        assert!(shown(cx, "result-save-report").is_some());
        assert!(!shown_indexed(cx, "report-status", 1));
        show(
            &view,
            cx,
            failed
                .clone()
                .with_report(Some(crate::presentation::ReportView::Creating)),
        );
        assert!(
            shown_indexed(cx, "report-status", 1),
            "creating is shown on the card"
        );
        let saved = crate::presentation::ReportView::Saved {
            path: std::path::PathBuf::from(
                "/home/deck/.pd2launcher/logs/reports/pd2-launcher-report-20260928T101500Z.tar.xz",
            ),
            opened: Some(false),
        };
        show(&view, cx, failed.clone().with_report(Some(saved.clone())));
        assert!(shown_indexed(cx, "report-open-folder", 1));
        assert!(shown_indexed(cx, "report-copy-path", 1));

        view.update(cx, |view, cx| {
            view.state.navigate(Destination::Diagnostics);
            cx.notify();
        });
        show(
            &view,
            cx,
            failed.with_report(Some(crate::presentation::ReportView::Failed(
                "report-write-failed",
            ))),
        );
        assert!(shown(cx, "save-report").is_some());
        assert!(
            shown_indexed(cx, "report-status", 0),
            "the failure shows in Diagnostics"
        );
        while service.intent_receiver.try_recv().is_ok() {}
        press(&view, cx, super::super::ActionId::ExportDiagnostics);
        assert!(matches!(
            service.intent_receiver.try_recv(),
            Ok(super::super::Intent::ExportDiagnostics)
        ));
    }

    fn located(snapshot: UiSnapshot) -> UiSnapshot {
        snapshot.with_location(Some(super::super::LocationView {
            path: std::path::PathBuf::from("/home/deck/.pd2launcher/game"),
            default: true,
            available: true,
            previous: None,
        }))
    }

    fn press(view: &Entity<LauncherView>, cx: Window<'_>, action: super::super::ActionId) {
        view.update(cx, |view, cx| view.activate_action(action, cx));
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }

    #[gpui_kit::test]
    fn a_first_run_walks_the_steps_and_back_keeps_what_was_entered(cx: &mut TestAppContext) {
        use super::super::{ActionId, DownloadsView, SetupStep};
        let (view, _service, cx) = open_view(cx, StartupInputs::for_tests(None, None, false));
        // Still checking: step 1 with its indicator, Continue disabled.
        show(&view, cx, located(UiSnapshot::initial()));
        assert!(shown(cx, "guided-check").is_some());
        assert!(shown(cx, "guided-checking").is_some());
        assert!(shown(cx, "back-action").is_none(), "no Back on step 1");
        press(&view, cx, ActionId::GuidedContinue);
        assert!(shown(cx, "guided-check").is_some(), "Continue waits");

        // A green audit shows the checks but stays on step 1.
        show(&view, cx, located(setup_snapshot()));
        assert!(shown(cx, "guided-check").is_some());
        assert!(shown(cx, "check-list").is_some());
        assert!(shown(cx, "guided-checking").is_none());

        press(&view, cx, ActionId::GuidedContinue);
        assert!(shown(cx, "guided-location").is_some());
        assert!(shown(cx, "back-action").is_some());
        assert!(shown(cx, "primary-action").is_some());

        press(&view, cx, ActionId::GuidedContinue);
        assert!(shown(cx, "guided-download").is_some());
        assert!(shown(cx, "download-list").is_some());
        press(&view, cx, ActionId::GuidedContinue);
        assert!(
            shown(cx, "guided-download").is_some(),
            "Continue waits for the downloads"
        );

        // Downloads verified: the result, then Continue to step 4.
        let downloaded = located(setup_snapshot()).with_downloads(DownloadsView {
            complete: true,
            installers: true,
            total_bytes: 2_600_000_000,
        });
        show(&view, cx, downloaded.clone());
        assert!(shown(cx, "guided-download").is_some());
        assert!(shown(cx, "step-result").is_some());
        press(&view, cx, ActionId::GuidedContinue);
        assert!(shown(cx, "guided-install-d2").is_some());
        view.update(cx, |view, cx| {
            view.base_key.update(cx, |input, cx| {
                input.restore("BCDFGHJKMNPRTVWXYZ24678922".to_owned(), cx)
            });
            view.expansion_key.update(cx, |input, cx| {
                input.restore("CDFGHJKMNPRTVWXYZ246789BCD".to_owned(), cx)
            });
            view.consent = true;
            view.prefers_folder = true;
        });

        // Back twice, then the rail, then forward again: nothing is lost.
        press(&view, cx, ActionId::GuidedBack);
        assert!(shown(cx, "guided-download").is_some());
        press(&view, cx, ActionId::GuidedBack);
        assert!(shown(cx, "guided-location").is_some());
        view.update(cx, |view, cx| view.guided_go_to(SetupStep::InstallD2, cx));
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(shown(cx, "guided-install-d2").is_some());
        view.read_with(cx, |view, cx| {
            assert_eq!(view.base_key.read(cx).len(), 26);
            assert_eq!(view.expansion_key.read(cx).len(), 26);
            assert!(view.consent && view.prefers_folder);
            assert!(view.authorization_complete(cx));
        });
    }

    #[gpui_kit::test]
    fn each_step_starts_its_own_job_and_shows_its_progress(cx: &mut TestAppContext) {
        use super::super::{ActionId, AuthorizationView, DownloadsView, GuidedCursor, SetupStep};
        let (view, service, cx) = open_view(cx, StartupInputs::for_tests(None, None, false));
        let _ = service.intent_receiver.try_recv();
        let downloaded = located(setup_snapshot()).with_downloads(DownloadsView {
            complete: true,
            installers: true,
            total_bytes: 1,
        });
        view.update(cx, |view, _| {
            view.guide = Some(GuidedCursor::at(SetupStep::Download));
        });
        show(&view, cx, located(setup_snapshot()));
        press(&view, cx, ActionId::Download);
        assert!(matches!(
            service.intent_receiver.try_recv(),
            Ok(Intent::DownloadAll)
        ));
        view.update(cx, |view, cx| {
            view.guide = Some(GuidedCursor::at(SetupStep::InstallD2));
            view.base_key.update(cx, |input, cx| {
                input.restore("BCDFGHJKMNPRTVWXYZ24678922".to_owned(), cx)
            });
            view.expansion_key.update(cx, |input, cx| {
                input.restore("CDFGHJKMNPRTVWXYZ246789BCD".to_owned(), cx)
            });
            view.consent = true;
        });
        show(&view, cx, downloaded.clone());
        press(&view, cx, ActionId::AuthorizeAndInstall);
        assert!(matches!(
            service.intent_receiver.try_recv(),
            Ok(Intent::AuthorizeAndInstall(_))
        ));
        // The running job's step shows its progress; nothing navigates.
        show(
            &view,
            cx,
            downloaded
                .clone()
                .with_authorization(AuthorizationView::Authorized)
                .with_active_job(Some(ActiveJobView::InstallBaseGame))
                .with_operation(Some(
                    OperationView::mutating(
                        OperationPhase::BaseGameInstall,
                        OperationStatusView::Running,
                        "Installing Diablo II",
                    )
                    .with_job(ActiveJobView::InstallBaseGame),
                )),
        );
        assert!(shown(cx, "progress-view").is_some());
        // One figure for the whole install, and its tasks where a log was.
        assert!(shown(cx, "install-figure").is_some());
        assert!(shown(cx, "install-tasks").is_some());
        assert!(shown(cx, "progress-log").is_none());
        // Blizzard's installers run now: the notice stays with the progress.
        assert!(shown(cx, "blizzard-windows-notice").is_some());
        assert!(shown(cx, "back-action").is_none());
        // Step 5 installs Project Diablo 2.
        view.update(cx, |view, _| {
            view.guide = Some(GuidedCursor::at(SetupStep::InstallPd2));
        });
        show(
            &view,
            cx,
            downloaded.with_installation(InstallationStatus::NeedsClient),
        );
        press(&view, cx, ActionId::InstallProjectDiablo);
        assert!(matches!(
            service.intent_receiver.try_recv(),
            Ok(Intent::InstallProjectDiablo)
        ));
    }

    #[gpui_kit::test]
    fn the_steps_start_themselves(cx: &mut TestAppContext) {
        use super::super::{ActionId, AuthorizationView, DownloadsView, GuidedCursor, SetupStep};
        let (view, service, cx) = open_view(cx, StartupInputs::for_tests(None, None, false));
        view.update(cx, |view, _| {
            view.guide = Some(GuidedCursor::at(SetupStep::Setup));
        });
        show(&view, cx, located(setup_snapshot()));
        while service.intent_receiver.try_recv().is_ok() {}
        // Continue on Location & installers starts the download.
        press(&view, cx, ActionId::GuidedContinue);
        assert!(matches!(
            service.intent_receiver.try_recv(),
            Ok(Intent::DownloadAll)
        ));
        let downloading = located(setup_snapshot())
            .with_active_job(Some(ActiveJobView::Download))
            .with_operation(Some(
                OperationView::mutating(
                    OperationPhase::BaseGameDownload,
                    OperationStatusView::Running,
                    "Downloading",
                )
                .with_job(ActiveJobView::Download),
            ));
        show(&view, cx, downloading);
        assert!(shown(cx, "progress-view").is_some());
        // A finished download opens Install Diablo II.
        let downloaded = located(setup_snapshot()).with_downloads(DownloadsView {
            complete: true,
            installers: true,
            total_bytes: 1,
        });
        let succeeded = |snapshot: crate::presentation::UiSnapshot, job| {
            snapshot.with_operation(Some(
                OperationView::mutating(
                    OperationPhase::Verification,
                    OperationStatusView::Succeeded,
                    "Operation complete",
                )
                .with_job(job),
            ))
        };
        show(
            &view,
            cx,
            succeeded(downloaded.clone(), ActiveJobView::Download),
        );
        assert!(shown(cx, "guided-install-d2").is_some());
        assert!(
            service.intent_receiver.try_recv().is_err(),
            "keys come first"
        );
        // Install Diablo II hands on to Install Project Diablo 2, once.
        show(
            &view,
            cx,
            downloaded
                .clone()
                .with_authorization(AuthorizationView::Authorized)
                .with_active_job(Some(ActiveJobView::InstallBaseGame))
                .with_operation(Some(
                    OperationView::mutating(
                        OperationPhase::BaseGameInstall,
                        OperationStatusView::Running,
                        "Installing Diablo II",
                    )
                    .with_job(ActiveJobView::InstallBaseGame),
                )),
        );
        let installed = succeeded(
            downloaded
                .clone()
                .with_installation(InstallationStatus::NeedsClient),
            ActiveJobView::InstallBaseGame,
        );
        show(&view, cx, installed.clone());
        assert!(matches!(
            service.intent_receiver.try_recv(),
            Ok(Intent::InstallProjectDiablo)
        ));
        show(&view, cx, installed);
        assert!(service.intent_receiver.try_recv().is_err(), "only once");
        // The figure keeps the scale it started with: the game environment
        // set up by Install Diablo II still counts, so Install Project
        // Diablo 2 goes on from where Install Diablo II ended.
        let pd2 = downloaded
            .with_installation(InstallationStatus::NeedsClient)
            .with_environment_ready(true)
            .with_active_job(Some(ActiveJobView::InstallProjectDiablo))
            .with_operation(Some(
                OperationView::mutating(
                    OperationPhase::ClientDownload,
                    OperationStatusView::Running,
                    "Installing Project Diablo 2",
                )
                .with_job(ActiveJobView::InstallProjectDiablo)
                .with_typed_progress(0, Some(10), "files"),
            ));
        show(&view, cx, pd2.clone());
        view.read_with(cx, |view, _| {
            assert_eq!(view.flow_percent(&pd2), Some(76));
        });
        assert!(shown(cx, "step-percent").is_some());
    }

    #[gpui_kit::test]
    fn an_install_found_part_way_waits_for_a_press(cx: &mut TestAppContext) {
        let (view, service, cx) = open_view(cx, StartupInputs::for_tests(None, None, false));
        while service.intent_receiver.try_recv().is_ok() {}
        // A restart after Install Diablo II succeeded starts nothing.
        show(
            &view,
            cx,
            located(setup_snapshot())
                .with_installation(InstallationStatus::NeedsClient)
                .with_operation(Some(
                    OperationView::mutating(
                        OperationPhase::Verification,
                        OperationStatusView::Succeeded,
                        "Operation complete",
                    )
                    .with_job(ActiveJobView::InstallBaseGame),
                )),
        );
        assert!(service.intent_receiver.try_recv().is_err());
    }

    #[gpui_kit::test]
    fn an_existing_installation_is_imported_as_chosen(cx: &mut TestAppContext) {
        use super::super::{
            ActionId, DownloadsView, ExistingInstallView, GuidedCursor, ImportModeView, SetupStep,
        };
        let (view, service, cx) = open_view(cx, StartupInputs::for_tests(None, None, false));
        let _ = service.intent_receiver.try_recv();
        view.update(cx, |view, _| {
            view.guide = Some(GuidedCursor::at(SetupStep::InstallD2));
        });
        show(
            &view,
            cx,
            located(setup_snapshot())
                .with_installer_media(InstallerMediaView::Existing(ExistingInstallView {
                    path: std::path::PathBuf::from("/home/deck/Games/Diablo II"),
                    problem: None,
                    copy_bytes: 1_900_000_000,
                    in_place_possible: true,
                    project_diablo: false,
                }))
                .with_downloads(DownloadsView {
                    complete: true,
                    installers: false,
                    total_bytes: 1,
                }),
        );
        assert!(shown(cx, "guided-install-d2").is_some());
        press(&view, cx, ActionId::ImportExisting);
        assert!(matches!(
            service.intent_receiver.try_recv(),
            Ok(Intent::ImportExistingInstall(ImportModeView::Copy))
        ));
        view.update(cx, |view, _| view.import_mode = ImportModeView::InPlace);
        press(&view, cx, ActionId::ImportExisting);
        assert!(matches!(
            service.intent_receiver.try_recv(),
            Ok(Intent::ImportExistingInstall(ImportModeView::InPlace))
        ));
    }

    #[gpui_kit::test]
    fn each_step_renders_and_fits_a_960_by_600_window(cx: &mut TestAppContext) {
        use super::super::{
            DownloadsView, ExistingInstallView, ExistingProblemView, GameFolderView, GuidedCursor,
            SetupStep,
        };
        let (view, _service, cx) = open_view(cx, StartupInputs::for_tests(None, None, false));
        cx.simulate_resize(gpui_kit::size(gpui_kit::px(960.0), gpui_kit::px(600.0)));
        // A full progress log still fits.
        for _ in 0..super::PROGRESS_LOG_LINES {
            crate::activity::push_recent_for_test(
                &"wine installer exited (exit code 1) ".repeat(8),
            );
        }
        let missing = located(setup_snapshot())
            .with_host(HostView::Unsupported)
            .with_system(SystemContractView {
                missing: vec![Prerequisite::VulkanDriver],
                distro: DistroFamily::Debian,
                ..setup_snapshot().system().clone()
            });
        let downloads = |installers| DownloadsView {
            complete: true,
            installers,
            total_bytes: 2_600_000_000,
        };
        let existing = |problem| {
            InstallerMediaView::Existing(ExistingInstallView {
                path: std::path::PathBuf::from("/home/deck/Games/Diablo II"),
                problem,
                copy_bytes: 1_900_000_000,
                in_place_possible: problem.is_none(),
                project_diablo: false,
            })
        };
        let downloaded = located(setup_snapshot()).with_downloads(downloads(true));
        for (step, snapshot, id) in [
            (
                SetupStep::Device,
                located(UiSnapshot::initial()),
                "guided-checking",
            ),
            (SetupStep::Device, located(setup_snapshot()), "check-list"),
            (SetupStep::Device, missing.clone(), "missing-prerequisites"),
            (
                SetupStep::Setup,
                located(setup_snapshot()),
                "guided-location",
            ),
            (
                SetupStep::Setup,
                located(setup_snapshot()).with_installer_media(InstallerMediaView::Validated(
                    std::path::PathBuf::from("/media/installers"),
                )),
                "guided-location",
            ),
            (
                SetupStep::Setup,
                located(setup_snapshot())
                    .with_installer_media(existing(Some(ExistingProblemView::NoExpansion))),
                "guided-location",
            ),
            (
                SetupStep::Download,
                located(setup_snapshot()),
                "download-list",
            ),
            (SetupStep::Download, downloaded.clone(), "step-result"),
            (
                SetupStep::InstallD2,
                downloaded.clone(),
                "guided-install-d2",
            ),
            (
                SetupStep::InstallD2,
                located(setup_snapshot())
                    .with_installer_media(existing(None))
                    .with_downloads(downloads(false)),
                "guided-install-d2",
            ),
            (
                SetupStep::InstallD2,
                downloaded
                    .clone()
                    .with_installation(InstallationStatus::NeedsClient),
                "step-result",
            ),
            (
                SetupStep::InstallPd2,
                downloaded
                    .clone()
                    .with_installation(InstallationStatus::NeedsClient),
                "guided-install-pd2",
            ),
            (
                SetupStep::Play,
                downloaded
                    .clone()
                    .with_installation(InstallationStatus::Ready)
                    .with_launch_preflight(true),
                "play-surface",
            ),
            (
                SetupStep::Play,
                downloaded
                    .clone()
                    .with_installation(InstallationStatus::Indeterminate)
                    .with_game_folder(Some(GameFolderView {
                        path: std::path::PathBuf::from("/run/media/deck/games/Diablo II"),
                        available: false,
                    })),
                "game-folder-unavailable",
            ),
            // Blizzard's downloader and installers run: the notice about
            // their windows stays beside the progress for the whole step.
            (
                SetupStep::Download,
                located(setup_snapshot())
                    .with_active_job(Some(ActiveJobView::Download))
                    .with_operation(Some(
                        OperationView::mutating(
                            OperationPhase::BaseGameDownload,
                            OperationStatusView::Running,
                            "Downloading Diablo II",
                        )
                        .with_job(ActiveJobView::Download),
                    )),
                "blizzard-windows-notice",
            ),
            (
                SetupStep::InstallD2,
                downloaded
                    .clone()
                    .with_active_job(Some(ActiveJobView::InstallBaseGame))
                    .with_operation(Some(
                        OperationView::mutating(
                            OperationPhase::ExpansionInstall,
                            OperationStatusView::Running,
                            "Installing Lord of Destruction",
                        )
                        .with_job(ActiveJobView::InstallBaseGame),
                    )),
                "blizzard-windows-notice",
            ),
            // A computer check that fails again returns to step 1.
            (SetupStep::InstallD2, missing, "missing-prerequisites"),
        ] {
            view.update(cx, |view, _| view.guide = Some(GuidedCursor::at(step)));
            show(&view, cx, snapshot);
            assert!(shown(cx, id).is_some(), "{step:?} shows {id}");
            // A running Download lists its items where the log would be.
            if step == SetupStep::Download && id == "blizzard-windows-notice" {
                assert!(shown(cx, "download-list").is_some());
                assert!(shown(cx, "progress-log").is_none());
            }
            // A running step has no main action to offer, only Stop.
            if id == "blizzard-windows-notice" {
                assert!(shown(cx, "stop-action").is_some(), "{step:?} {id}");
            } else {
                assert!(shown(cx, "primary-action").is_some(), "{step:?} {id}");
            }
            // Play stands above its foot; every other page needs no
            // scrolling at 960×600 and ends above the corner pills.
            if let Some(surface) = shown(cx, "play-surface") {
                let foot = shown(cx, "play-foot").expect("Play's foot");
                assert!(surface.bottom() <= foot.top(), "{step:?} {id} fits");
                continue;
            }
            let page = shown(cx, "overview-page").expect("the pane");
            let end = shown(cx, "wait-note").map_or(page.bottom(), |note| note.bottom());
            let pills = ["primary-action", "stop-action", "back-action"]
                .into_iter()
                .filter_map(|id| shown(cx, id))
                .map(|pill| pill.top())
                .fold(gpui_kit::px(600.0), |top, pill| top.min(pill));
            assert!(
                end + gpui_kit::px(8.0) <= pills,
                "{step:?} {id} fits: {page:?} ends at {end:?}, pills at {pills:?}"
            );
        }
    }

    #[gpui_kit::test]
    fn returning_users_see_where_they_stand(cx: &mut TestAppContext) {
        use super::super::DownloadsView;
        let (view, _service, cx) = open_view(cx, StartupInputs::for_tests(None, None, false));
        let downloaded = located(setup_snapshot()).with_downloads(DownloadsView {
            complete: true,
            installers: true,
            total_bytes: 1,
        });
        for (snapshot, id) in [
            (downloaded.clone(), "guided-install-d2"),
            (
                located(setup_snapshot()).with_stop_result(Some(
                    super::super::StopResultView::new(InstallationStatus::NeedsBaseGame, false),
                )),
                "stop-result",
            ),
            (
                downloaded
                    .clone()
                    .with_installation(InstallationStatus::NeedsClient),
                "guided-install-pd2",
            ),
            (
                downloaded
                    .with_installation(InstallationStatus::Ready)
                    .with_launch_preflight(true),
                "play-surface",
            ),
        ] {
            // A new window: the cursor is placed from the first settled audit.
            view.update(cx, |view, _| {
                view.guide = Some(super::super::GuidedCursor::new());
            });
            show(&view, cx, snapshot);
            assert!(shown(cx, id).is_some(), "{id} is shown");
            assert!(shown(cx, "guided-check").is_none(), "no replay for {id}");
        }
    }

    #[gpui_kit::test]
    fn update_takes_the_main_button_on_play_with_play_without_updating(cx: &mut TestAppContext) {
        let (view, service, cx) = open_view(cx, StartupInputs::for_tests(None, None, false));
        cx.simulate_resize(gpui_kit::size(gpui_kit::px(960.0), gpui_kit::px(600.0)));
        let available = located(setup_snapshot())
            .with_installation(InstallationStatus::Ready)
            .with_launch_preflight(true)
            .with_update(crate::UpdateStatus::Available);
        show(&view, cx, available.clone());
        while service.intent_receiver.try_recv().is_ok() {}
        assert!(shown(cx, "play-without-updating").is_some());
        press(&view, cx, super::super::ActionId::Update);
        assert!(matches!(
            service.intent_receiver.try_recv(),
            Ok(Intent::ApplyUpdate)
        ));
        let without = shown(cx, "play-without-updating").expect("Play without updating");
        cx.simulate_click(without.center(), gpui_kit::Modifiers::none());
        assert!(matches!(
            service.intent_receiver.try_recv(),
            Ok(Intent::LaunchGame(..))
        ));
        // While it runs: its figure on Play, Stop, and no Play without it.
        show(
            &view,
            cx,
            available
                .with_active_job(Some(ActiveJobView::Update))
                .with_operation(Some(
                    OperationView::mutating(
                        OperationPhase::ClientDownload,
                        OperationStatusView::Running,
                        "Updating",
                    )
                    .with_job(ActiveJobView::Update)
                    .with_typed_progress(3, Some(8), "files"),
                )),
        );
        assert!(shown(cx, "play-surface").is_some());
        assert!(shown(cx, "update-figure").is_some());
        assert!(shown(cx, "play-without-updating").is_none());
        assert!(shown(cx, "progress-view").is_none());
    }

    #[gpui_kit::test]
    fn the_official_launcher_is_plays_second_button_for_loot_filters(cx: &mut TestAppContext) {
        let (view, service, cx) = open_view(cx, StartupInputs::for_tests(None, None, false));
        cx.simulate_resize(gpui_kit::size(gpui_kit::px(960.0), gpui_kit::px(600.0)));
        let ready = located(setup_snapshot())
            .with_installation(InstallationStatus::Ready)
            .with_launch_preflight(true)
            .with_update(crate::UpdateStatus::UpToDate);
        show(&view, cx, ready.clone());
        while service.intent_receiver.try_recv().is_ok() {}
        let play = shown(cx, "primary-action").expect("Play");
        let official = shown(cx, "official-launcher").expect("its second button");
        assert!(official.top() >= play.bottom(), "under Play");
        assert!(shown(cx, "official-launcher-line").is_some());
        cx.simulate_click(official.center(), gpui_kit::Modifiers::none());
        assert!(matches!(
            service.intent_receiver.try_recv(),
            Ok(Intent::OpenOfficialLauncher)
        ));
        // An unverified PD2Launcher.exe asks first.
        let unverified = ready.clone().with_official_launcher_unverified(true);
        show(&view, cx, unverified.clone());
        let official = shown(cx, "official-launcher").expect("its second button");
        cx.simulate_click(official.center(), gpui_kit::Modifiers::none());
        show(&view, cx, unverified);
        assert!(service.intent_receiver.try_recv().is_err(), "it asks first");
        view.read_with(cx, |view, _| assert!(view.confirm_official_launcher));
        let run = cx
            .update(|window, _| {
                gpui_kit::base::test_support::snapshots(window)
                    .into_iter()
                    .find(|element| {
                        element.path().last()
                            == Some(&gpui_kit::ElementId::from((
                                "confirm-official-launcher-button",
                                0_usize,
                            )))
                    })
                    .map(|element| element.bounds())
            })
            .expect("Run it");
        cx.simulate_click(run.center(), gpui_kit::Modifiers::none());
        assert!(matches!(
            service.intent_receiver.try_recv(),
            Ok(Intent::OpenOfficialLauncherConfirmed(false, false))
        ));
        // While it is open the same button closes it.
        show(
            &view,
            cx,
            ready
                .with_active_job(Some(ActiveJobView::OfficialLauncher))
                .with_operation(Some(
                    OperationView::mutating(
                        OperationPhase::OfficialLauncher,
                        OperationStatusView::Running,
                        "Official PD2 launcher",
                    )
                    .with_job(ActiveJobView::OfficialLauncher),
                )),
        );
        assert!(shown(cx, "secondary-action").is_none(), "no second Close");
        let close = shown(cx, "official-launcher").expect("Close official launcher");
        cx.simulate_click(close.center(), gpui_kit::Modifiers::none());
        assert!(matches!(
            service.intent_receiver.try_recv(),
            Ok(Intent::RequestSafeStop)
        ));
    }

    /// The setup flow speaks plainly: "Windows support" and "the game
    /// environment", never Wine, a prefix or FEX (those are in About, and
    /// FEX only in its own ARM64 choice).
    #[test]
    fn the_setup_flow_names_no_wine_prefix_or_fex() {
        use super::super::{
            DownloadsView, ExistingProblemView, Prerequisite, SetupStep, download_rows,
        };
        let technical = |text: &str| {
            let lower = text.to_ascii_lowercase();
            ["wine", "prefix", "fex"]
                .into_iter()
                .find(|word| lower.contains(word))
                .map(|word| format!("{word:?} in {text:?}"))
        };
        let mut texts: Vec<String> = Vec::new();
        for phase in [
            OperationPhase::HostAudit,
            OperationPhase::RuntimeAcquisition,
            OperationPhase::PrefixInitialization,
            OperationPhase::BaseGameInstall,
            OperationPhase::ExpansionInstall,
            OperationPhase::BaseGameDownload,
            OperationPhase::ExpansionDownload,
            OperationPhase::ImportGame,
            OperationPhase::ClientDownload,
            OperationPhase::Verification,
            OperationPhase::Promotion,
            OperationPhase::Reset,
        ] {
            texts.push(super::phase_headline(phase).to_owned());
            let operation = OperationView::mutating(phase, OperationStatusView::Running, "x");
            let (headline, detail) = super::working_status(&operation, &setup_snapshot());
            texts.extend([headline.to_owned(), detail.to_string()]);
        }
        for step in SetupStep::ALL {
            let (title, intro) = super::guided_copy(step);
            texts.extend([title.to_owned(), intro.to_owned()]);
        }
        for snapshot in [
            setup_snapshot(),
            setup_snapshot().with_downloads(DownloadsView {
                complete: true,
                installers: true,
                total_bytes: 1,
            }),
        ] {
            texts.extend(
                download_rows(&snapshot)
                    .into_iter()
                    .flat_map(|row| [row.label.to_owned(), row.detail]),
            );
            texts.extend(
                super::check_items(&snapshot)
                    .into_iter()
                    .flat_map(|(label, detail, _)| [label.to_owned(), detail.to_string()]),
            );
            texts.push(super::start_over_warning(&snapshot).to_string());
        }
        for code in [
            "location-parent-invalid",
            "location-case-insensitive",
            "location-free-space",
        ] {
            texts.push(super::location_error_message(code).to_owned());
        }
        for prerequisite in [
            Prerequisite::DisplaySession,
            Prerequisite::WineCannotRun,
            Prerequisite::Libraries,
            Prerequisite::WineLibrary(crate::audit::WineLibrary::Fontconfig),
            Prerequisite::XWayland,
            Prerequisite::VulkanDriver,
        ] {
            texts.push(prerequisite.plain().to_owned());
        }
        texts.push(ExistingProblemView::NoExpansion.message());
        let found = texts
            .iter()
            .filter_map(|text| technical(text))
            .collect::<Vec<_>>();
        assert!(found.is_empty(), "{found:#?}");
        // About names them.
        let about = super::super::about_terms()
            .into_iter()
            .map(|(term, meaning)| format!("{term}: {meaning}"))
            .collect::<String>();
        assert!(about.contains("Wine"));
        assert!(about.contains("prefix"));
    }

    #[gpui_kit::test]
    fn the_programs_own_lines_are_details_and_in_diagnostics(cx: &mut TestAppContext) {
        let (view, _service, cx) = open_view(cx, StartupInputs::for_tests(None, None, false));
        crate::activity::push_recent_for_test("wineboot: starting the prefix");
        let resetting = located(setup_snapshot())
            .with_active_job(Some(ActiveJobView::Reset))
            .with_operation(Some(
                OperationView::mutating(
                    OperationPhase::Reset,
                    OperationStatusView::Running,
                    "Starting over",
                )
                .with_job(ActiveJobView::Reset),
            ));
        view.update(cx, |view, _| view.guide = None);
        show(&view, cx, resetting.clone());
        assert!(shown(cx, "progress-view").is_some());
        assert!(shown(cx, "progress-log").is_none(), "folded away");
        let details = shown(cx, "progress-details").expect("Show details");
        cx.simulate_click(details.center(), gpui_kit::Modifiers::none());
        show(&view, cx, resetting.clone());
        assert!(shown(cx, "progress-log").is_some(), "unfolded");
        view.update(cx, |view, cx| {
            view.details_open = false;
            view.state.navigate(Destination::Diagnostics);
            cx.notify();
        });
        show(&view, cx, resetting);
        assert!(shown(cx, "diagnostics-activity").is_some());
        assert!(shown(cx, "progress-log").is_some(), "always in Diagnostics");
    }

    #[test]
    fn start_overs_confirmation_says_the_characters_are_kept() {
        let managed = setup_snapshot();
        let warning = super::start_over_warning(&managed);
        assert!(warning.contains("Your characters are kept"), "{warning}");
        assert!(!warning.contains("characters saved inside it"));
        // A folder used where it is keeps its own characters.
        let in_place = setup_snapshot().with_game_folder(Some(super::super::GameFolderView {
            path: std::path::PathBuf::from("/home/deck/Games/Diablo II"),
            available: true,
        }));
        let warning = super::start_over_warning(&in_place);
        assert!(warning.contains("is not deleted"), "{warning}");
        assert!(warning.contains("characters in it stay there"), "{warning}");
        assert_eq!(
            super::location_error_message("saves-not-kept"),
            "The old installation was not removed: its characters couldn't be moved to their own folder first. Check that your home folder has free space, then try again."
        );
    }

    #[test]
    fn a_running_update_says_it_is_updating() {
        let ready = setup_snapshot()
            .with_installation(InstallationStatus::Ready)
            .with_launch_preflight(true);
        let operation =
            |phase| OperationView::mutating(phase, OperationStatusView::Running, "Updating");
        // From its start, before its first progress names the job.
        let starting = ready.clone().with_active_job(Some(ActiveJobView::Update));
        assert_eq!(
            super::working_status(&operation(OperationPhase::ClientDownload), &starting).0,
            "Updating Project Diablo 2"
        );
        for (phase, detail) in [
            (
                OperationPhase::ClientDownload,
                "Downloading the files that changed. Keep this window open.",
            ),
            (
                OperationPhase::Verification,
                "Checking the new files. Keep this window open.",
            ),
            (
                OperationPhase::Promotion,
                "Putting the new files in place. Keep this window open.",
            ),
        ] {
            let (headline, line) =
                super::working_status(&operation(phase).with_job(ActiveJobView::Update), &ready);
            assert_eq!(headline, "Updating Project Diablo 2", "{phase:?}");
            assert_eq!(line.as_ref(), detail);
        }
        // Step 5 still installs.
        assert_eq!(
            super::working_status(
                &operation(OperationPhase::Promotion).with_job(ActiveJobView::InstallProjectDiablo),
                &setup_snapshot().with_installation(InstallationStatus::NeedsClient),
            )
            .0,
            "Installing Project Diablo 2"
        );
    }

    #[test]
    fn a_safe_stop_says_it_can_take_up_to_ten_minutes() {
        let stopping = OperationView::mutating(
            OperationPhase::BaseGameInstall,
            OperationStatusView::SafeStopping,
            "Installing Diablo II",
        );
        let (headline, detail) = super::working_status(&stopping, &setup_snapshot());
        assert_eq!(headline, "Stopping safely…");
        assert_eq!(
            detail.as_ref(),
            "The current step finishes at a safe point first. If an installer window is stuck, it is cleared only when its time limit runs out, which can take up to ten minutes."
        );
    }

    #[test]
    fn the_game_environment_names_its_part_while_it_is_set_up() {
        use crate::operation::{EnvironmentPart, EnvironmentProgress};
        let setting_up = OperationView::mutating(
            OperationPhase::PrefixInitialization,
            OperationStatusView::Running,
            "Setting up the game environment",
        );
        let media = setup_snapshot();
        let (headline, detail) = super::working_status(&setting_up, &media);
        assert_eq!(headline, "Setting up the game environment");
        assert_eq!(detail.as_ref(), "Keep this window open until it finishes.");
        let (headline, detail) = super::working_status(
            &setting_up.with_environment(EnvironmentProgress::new(
                EnvironmentPart::WebComponents,
                &EnvironmentPart::ALL,
            )),
            &media,
        );
        assert_eq!(headline, "Setting up the game environment");
        assert_eq!(
            detail.as_ref(),
            "Adding web components (2 of 4). Keep this window open until it finishes."
        );
    }

    #[gpui_kit::test]
    fn play_tells_what_keeping_the_characters_put_aside_with_a_way_to_the_backup(
        cx: &mut TestAppContext,
    ) {
        use super::super::{FolderOpenView, SavesPutAsideView};
        let (view, service, cx) = open_view(cx, StartupInputs::for_tests(None, None, false));
        cx.simulate_resize(gpui_kit::size(gpui_kit::px(960.0), gpui_kit::px(600.0)));
        view.update(cx, |view, _| {
            view.guide = Some(super::super::GuidedCursor::new());
        });
        let ready = located(setup_snapshot())
            .with_installation(InstallationStatus::Ready)
            .with_launch_preflight(true);
        show(&view, cx, ready.clone());
        assert!(shown(cx, "saves-put-aside").is_none());
        let notice = SavesPutAsideView {
            characters: 1,
            files: 1,
            others: 0,
            folder: std::path::PathBuf::from("/home/deck/.pd2launcher/userdata/backups/backup-1"),
            open: None,
        };
        show(
            &view,
            cx,
            ready.clone().with_saves_put_aside(Some(notice.clone())),
        );
        let shown_notice = shown(cx, "saves-put-aside").expect("the notice on Play");
        let foot = shown(cx, "play-foot").expect("its foot");
        assert!(
            shown_notice.bottom() <= foot.top(),
            "it fits above the foot"
        );
        while service.intent_receiver.try_recv().is_ok() {}
        let open = shown(cx, "open-saves-backup").expect("Open backups");
        cx.simulate_click(open.center(), gpui_kit::Modifiers::none());
        assert!(matches!(
            service.intent_receiver.try_recv(),
            Ok(Intent::OpenSavesBackup)
        ));
        let dismiss = shown(cx, "dismiss-saves-put-aside").expect("Dismiss");
        cx.simulate_click(dismiss.center(), gpui_kit::Modifiers::none());
        assert!(matches!(
            service.intent_receiver.try_recv(),
            Ok(Intent::DismissSavesPutAside)
        ));
        // A folder no file manager takes is named instead.
        show(
            &view,
            cx,
            ready.with_saves_put_aside(Some(SavesPutAsideView {
                open: Some(FolderOpenView::Failed),
                ..notice
            })),
        );
        assert!(shown(cx, "saves-put-aside").is_some());
    }

    #[gpui_kit::test]
    fn settings_open_the_game_folder_once_the_game_is_installed(cx: &mut TestAppContext) {
        use super::super::FolderOpenView;
        let (view, service, cx) = open_view(cx, StartupInputs::for_tests(None, None, false));
        view.update(cx, |view, cx| {
            view.state.navigate(Destination::Settings);
            cx.notify();
        });
        show(&view, cx, located(setup_snapshot()));
        assert!(shown(cx, "game-files").is_none(), "nothing installed yet");

        let installed = located(setup_snapshot()).with_installation(InstallationStatus::Ready);
        show(&view, cx, installed.clone());
        assert!(shown(cx, "game-files").is_some());
        while service.intent_receiver.try_recv().is_ok() {}
        let button = shown(cx, "open-game-folder").expect("Open game folder");
        cx.simulate_click(button.center(), gpui_kit::Modifiers::none());
        assert!(matches!(
            service.intent_receiver.try_recv(),
            Ok(Intent::OpenGameFolder)
        ));
        show(
            &view,
            cx,
            installed.with_game_files_open(Some(FolderOpenView::Failed)),
        );
        assert!(shown(cx, "copy-game-folder").is_some());
    }

    #[gpui_kit::test]
    fn the_setup_record_opens_its_page_while_the_check_runs(cx: &mut TestAppContext) {
        use crate::persistence::SetupRecord;
        let (view, _service, cx) = open_view(cx, StartupInputs::for_tests(None, None, false));
        // Installed: Play from the first frame, waiting for the check.
        show(
            &view,
            cx,
            UiSnapshot::initial().with_setup_record(SetupRecord::Installed),
        );
        assert!(shown(cx, "play-surface").is_some());
        assert!(shown(cx, "guided-check").is_none());
        // Started: the install step waits too, without the key form.
        show(
            &view,
            cx,
            UiSnapshot::initial().with_setup_record(SetupRecord::Started),
        );
        assert!(shown(cx, "guided-opening").is_some());
        assert!(shown(cx, "key-privacy").is_none());
        // No record: Check your computer.
        show(&view, cx, UiSnapshot::initial());
        assert!(shown(cx, "guided-check").is_some());
    }

    #[gpui_kit::test]
    fn unattended_mode_bypasses_the_guide(cx: &mut TestAppContext) {
        use super::super::DownloadsView;
        cx.update(|cx| {
            gpui_kit::init(cx);
            theme::apply(cx);
        });
        let (endpoint, service) = service_bridge(UiSnapshot::initial());
        let authorization = super::super::AuthorizationInput::new(
            "BCDFGHJKMNPRTVWXYZ24678922".to_owned(),
            "CDFGHJKMNPRTVWXYZ246789BCD".to_owned(),
            true,
        )
        .expect("placeholder keys");
        let automation = crate::automation::AutomationRequest::download_for_tests(authorization);
        let captured: Rc<RefCell<Option<Entity<LauncherView>>>> = Rc::default();
        let capture = captured.clone();
        let (_root, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| {
                LauncherView::new(
                    endpoint,
                    Box::new(()),
                    Some(automation),
                    StartupInputs::for_tests(None, None, false),
                    window,
                    cx,
                )
            });
            *capture.borrow_mut() = Some(view.clone());
            gpui_kit::component::Root::new(view, window, cx)
        });
        let view = captured.borrow().clone().expect("launcher view");
        assert!(matches!(
            service.intent_receiver.try_recv(),
            Ok(Intent::UseBlizzardDownload)
        ));
        view.read_with(cx, |view, _| assert!(view.guide.is_none()));
        // Straight through: Download, then Install Diablo II with the keys,
        // then Install Project Diablo 2, each once the one before finished.
        show(&view, cx, located(setup_snapshot()));
        assert!(shown(cx, "guided-check").is_none());
        view.update(cx, |view, _| assert!(view.drive_unattended()));
        assert!(matches!(
            service.intent_receiver.try_recv(),
            Ok(Intent::DownloadAll)
        ));
        let downloaded = located(setup_snapshot()).with_downloads(DownloadsView {
            complete: true,
            installers: true,
            total_bytes: 1,
        });
        show(&view, cx, downloaded.clone());
        view.update(cx, |view, _| assert!(view.drive_unattended()));
        assert!(matches!(
            service.intent_receiver.try_recv(),
            Ok(Intent::AuthorizeAndInstall(_))
        ));
        show(
            &view,
            cx,
            downloaded
                .clone()
                .with_installation(InstallationStatus::NeedsClient),
        );
        view.update(cx, |view, _| assert!(view.drive_unattended()));
        assert!(matches!(
            service.intent_receiver.try_recv(),
            Ok(Intent::InstallProjectDiablo)
        ));
        // A step whose job finished without moving on ends the run.
        view.update(cx, |view, _| {
            assert!(view.drive_unattended());
            assert!(view.unattended_install_failed());
        });
    }

    fn chrome(maximized: Option<bool>, controls: gpui_kit::WindowControls) -> super::WindowChrome {
        super::WindowChrome {
            draws_controls: true,
            tiling: gpui_kit::Tiling::default(),
            maximized,
            fullscreen: false,
            resizable: true,
            controls,
        }
    }

    fn render_with(view: &Entity<LauncherView>, cx: Window<'_>, chrome: super::WindowChrome) {
        view.update(cx, |view, cx| {
            view.chrome_override = Some(chrome);
            cx.notify();
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }

    #[gpui_kit::test]
    fn the_title_bar_offers_the_window_buttons_the_compositor_supports(cx: &mut TestAppContext) {
        let (view, _service, cx) = open_view(cx, StartupInputs::for_tests(None, None, false));
        show(&view, cx, setup_snapshot());
        let everything = gpui_kit::WindowControls::default();
        // Server-side decorations: the compositor's own title bar has them.
        render_with(
            &view,
            cx,
            super::WindowChrome {
                draws_controls: false,
                ..chrome(None, everything)
            },
        );
        assert!(shown(cx, "window-controls").is_none());
        assert!(shown(cx, "resize-BottomRight").is_none());

        render_with(&view, cx, chrome(Some(false), everything));
        let header = shown(cx, "header").expect("the header");
        let sections = shown(cx, "sections").expect("the sections");
        let mut previous = sections.right();
        for id in ["window-minimize", "window-maximize", "window-close"] {
            let button = shown(cx, id).unwrap_or_else(|| panic!("{id} is shown"));
            assert!(button.left() >= previous, "{id} follows the sections");
            assert!(
                button.size.width >= gpui_kit::px(28.0),
                "{id} is wide enough"
            );
            assert!(
                button.size.height >= gpui_kit::px(28.0),
                "{id} is tall enough"
            );
            assert!(button.top() >= header.top() && button.bottom() <= header.bottom());
            previous = button.right();
        }
        assert!(shown(cx, "window-restore").is_none());
        // A floating window resizes from every edge and corner.
        for edge in ["Top", "Bottom", "Left", "Right", "TopLeft", "BottomRight"] {
            let id: &'static str = format!("resize-{edge}").leak();
            assert!(shown(cx, id).is_some(), "{id} is shown");
        }

        // Maximized: Restore instead of Maximize, and no resize edges.
        render_with(&view, cx, chrome(Some(true), everything));
        assert!(shown(cx, "window-restore").is_some());
        assert!(shown(cx, "window-maximize").is_none());
        assert!(shown(cx, "resize-Top").is_none());

        // A compositor that cannot minimize (Hyprland) gets no Minimize;
        // one that does not report the state gets a plain Maximize and
        // keeps its own resizing.
        let no_minimize = gpui_kit::WindowControls {
            minimize: false,
            window_menu: false,
            ..everything
        };
        render_with(&view, cx, chrome(None, no_minimize));
        assert!(shown(cx, "window-minimize").is_none());
        assert!(shown(cx, "window-maximize").is_some());
        assert!(shown(cx, "window-close").is_some());
        assert!(shown(cx, "resize-Right").is_none());

        // Close is always offered, even without maximize.
        let close_only = gpui_kit::WindowControls {
            maximize: false,
            ..no_minimize
        };
        render_with(&view, cx, chrome(Some(false), close_only));
        assert!(shown(cx, "window-maximize").is_none());
        assert!(shown(cx, "window-close").is_some());
    }

    #[gpui_kit::test]
    fn the_title_bar_close_asks_first_while_work_runs(cx: &mut TestAppContext) {
        let (view, service, cx) = open_view(cx, StartupInputs::for_tests(None, None, false));
        let _ = service.intent_receiver.try_recv();
        show(
            &view,
            cx,
            setup_snapshot()
                .with_active_job(Some(ActiveJobView::InstallBaseGame))
                .with_operation(Some(OperationView::mutating(
                    OperationPhase::BaseGameInstall,
                    OperationStatusView::Running,
                    "Installing Diablo II",
                ))),
        );
        cx.update(|window, cx| {
            view.update(cx, |view, cx| view.close_window(window, cx));
        });
        view.read_with(cx, |view, _| assert!(view.confirm_close, "it asks first"));
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(shown(cx, "shell").is_some(), "the window stays open");
    }

    #[test]
    fn hyprland_and_niri_draw_no_title_bars() {
        assert!(super::draws_no_title_bars("Hyprland"));
        assert!(super::draws_no_title_bars("niri"));
        assert!(super::draws_no_title_bars("hyprland:wlroots"));
        assert!(!super::draws_no_title_bars("KDE"));
        assert!(!super::draws_no_title_bars("GNOME"));
        assert!(!super::draws_no_title_bars(""));
    }

    #[gpui_kit::test]
    fn the_header_links_never_run_under_the_step_marks(cx: &mut TestAppContext) {
        let (view, _service, cx) = open_view(cx, StartupInputs::for_tests(None, None, false));
        let downloading = located(setup_snapshot())
            .with_active_job(Some(ActiveJobView::Download))
            .with_operation(Some(
                OperationView::mutating(
                    OperationPhase::BaseGameDownload,
                    OperationStatusView::Running,
                    "Downloading Diablo II",
                )
                .with_job(ActiveJobView::Download)
                .with_typed_progress(51, Some(100), "bytes"),
            ));
        // The window's smallest width, the compact header's widest, a
        // width the test's wide letters fill with every name, and a wide
        // window.
        for width in [800.0, 939.0, 1100.0, 1600.0] {
            cx.simulate_resize(gpui_kit::size(gpui_kit::px(width), gpui_kit::px(600.0)));
            show(&view, cx, downloading.clone());
            let header = shown(cx, "header").expect("the header");
            let brand = shown(cx, "brand-mark").expect("the brand mark");
            let marks = shown(cx, "step-marks").expect("the step marks");
            let links = shown(cx, "sections").expect("Settings and Diagnostics");
            assert!(
                brand.right() <= marks.left(),
                "{width}: {brand:?} {marks:?}"
            );
            assert!(
                marks.right() <= links.left(),
                "{width}: {marks:?} {links:?}"
            );
            assert!(links.right() <= header.right(), "{width}: {links:?}");
            if width >= 1600.0 {
                // With room to spare the marks stay in the middle, give or
                // take the header's padding on the right.
                assert!(
                    (marks.center().x - header.center().x).abs() <= gpui_kit::px(12.0),
                    "{marks:?}"
                );
            }
        }
    }

    #[gpui_kit::test]
    fn a_large_window_is_filled_edge_to_edge(cx: &mut TestAppContext) {
        let (view, _service, cx) = open_view(cx, StartupInputs::for_tests(None, None, false));
        let (width, height) = (gpui_kit::px(2400.0), gpui_kit::px(1600.0));
        cx.simulate_resize(gpui_kit::size(width, height));
        view.update(cx, |view, _| {
            view.guide = Some(super::super::GuidedCursor::at(
                super::super::SetupStep::Setup,
            ));
        });
        show(&view, cx, located(setup_snapshot()));
        let shell = shown(cx, "shell").expect("the shell");
        assert_eq!(
            shell.origin,
            gpui_kit::point(gpui_kit::px(0.0), gpui_kit::px(0.0))
        );
        assert_eq!(
            shell.size,
            gpui_kit::size(width, height),
            "no backdrop around it"
        );
        let header = shown(cx, "header").expect("the header");
        assert_eq!(header.size.width, width);
        // Only the content column keeps its readable width, in the middle.
        let pane = shown(cx, "pane").expect("the pane");
        assert!(pane.size.width <= gpui_kit::px(super::PANE_MAX_WIDTH));
        assert!(
            (pane.center().x - width / 2.0).abs() <= gpui_kit::px(1.0),
            "{pane:?}"
        );
        // Back and the action keep to the window's bottom corners.
        let back = shown(cx, "back-action").expect("Back");
        let action = shown(cx, "primary-action").expect("the action");
        assert!(back.left() <= gpui_kit::px(40.0));
        assert!(action.right() >= width - gpui_kit::px(40.0));
        for pill in [back, action] {
            assert!(pill.bottom() >= height - gpui_kit::px(40.0), "{pill:?}");
            assert!(pill.bottom() <= height);
        }
    }
}
