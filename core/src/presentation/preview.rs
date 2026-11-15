//! Canned screens for layout review, compiled into debug builds only.
//!
//! `PD2_UI_PREVIEW=<name>` starts the interactive window on a fixed
//! [`UiSnapshot`] with no launcher service behind it, so states that need a
//! real installation (progress, a failed step, Ready) can be looked at and
//! captured. Release builds contain none of this.

use std::path::PathBuf;

use super::{
    ActiveJobView, AuthorizationView, Destination, DistroFamily, DownloadsView, EvidenceTone,
    EvidenceView, ExistingInstallView, ExistingProblemView, GameFolderView, GameNoticeView,
    GameView, HostView, ImportModeView, InstallerMediaView, LocationView, OperationPhase,
    OperationStatusView, OperationView, Prerequisite, SavesPutAsideView, SetupStep, StopResultView,
    SystemContractView, UiSnapshot,
};
use crate::{
    InstallationStatus, InterruptedStage, UpdateStatus,
    operation::{EnvironmentPart, EnvironmentProgress},
    persistence::SetupRecord,
};

const VARIABLE: &str = "PD2_UI_PREVIEW";

/// The view-local parts of a preview that no snapshot carries.
pub(super) struct ViewPreview {
    pub(super) base_key: &'static str,
    pub(super) expansion_key: &'static str,
    pub(super) consent: bool,
    /// The step the pane shows; `None` shows the screen without the step
    /// cursor, as unattended mode sees it.
    pub(super) guided: Option<SetupStep>,
    pub(super) import_mode: ImportModeView,
    pub(super) destination: Destination,
    pub(super) confirm_close: bool,
    pub(super) confirm_start_over: bool,
    /// The check after a finished update runs.
    pub(super) rechecking: bool,
}

fn name() -> Option<String> {
    std::env::var(VARIABLE).ok().filter(|name| !name.is_empty())
}

fn location() -> LocationView {
    LocationView {
        path: PathBuf::from("/home/deck/.pd2launcher/game"),
        default: true,
        available: true,
        previous: None,
    }
}

fn setup() -> UiSnapshot {
    UiSnapshot::initial()
        .with_host(HostView::Supported)
        .with_installation(InstallationStatus::NeedsBaseGame)
        .with_installer_media(InstallerMediaView::BlizzardDownload)
        .with_operation(None)
        .with_location(Some(location()))
        .with_system(SystemContractView {
            host: "Wayland · Vulkan 1.3 · XWayland".to_owned(),
            artifact_health: "Nothing downloaded yet".to_owned(),
            ..SystemContractView::pinned()
        })
        .with_evidence(vec![
            EvidenceView::new(
                "Display",
                "Wayland session with XWayland",
                EvidenceTone::Verified,
            ),
            EvidenceView::new(
                "Graphics",
                "Vulkan 1.3 hardware driver",
                EvidenceTone::Verified,
            ),
            EvidenceView::new(
                "Free space",
                "412 GiB free in the install location",
                EvidenceTone::Verified,
            ),
        ])
}

/// Everything downloaded and verified for Blizzard's installers.
fn downloaded() -> UiSnapshot {
    setup().with_downloads(DownloadsView {
        complete: true,
        installers: true,
        total_bytes: 2_654_000_000,
    })
}

fn existing(problem: Option<ExistingProblemView>) -> InstallerMediaView {
    InstallerMediaView::Existing(ExistingInstallView {
        path: PathBuf::from("/home/deck/Games/Diablo II"),
        problem,
        copy_bytes: 1_893_000_000,
        in_place_possible: problem.is_none(),
        project_diablo: false,
    })
}

fn missing() -> UiSnapshot {
    setup()
        .with_host(HostView::Unsupported)
        .with_system(SystemContractView {
            missing: vec![Prerequisite::VulkanDriver, Prerequisite::XWayland],
            distro: DistroFamily::Arch,
            ..setup().system
        })
}

fn working(job: ActiveJobView, phase: OperationPhase, label: &'static str) -> OperationView {
    OperationView::mutating(phase, OperationStatusView::Running, label).with_job(job)
}

fn ready() -> UiSnapshot {
    downloaded()
        .with_installation(InstallationStatus::Ready)
        .with_launch_preflight(true)
        .with_update(UpdateStatus::UpToDate)
}

/// The snapshot named by `PD2_UI_PREVIEW`, if it names one.
pub(crate) fn snapshot() -> Option<UiSnapshot> {
    let snapshot = match name()?.as_str() {
        "setup" | "settings" | "diagnostics" | "guided-check-ok" | "guided-location"
        | "guided-download" => setup(),
        "checking" => UiSnapshot::initial(),
        // A start by the setup record, while the first check runs.
        "opening-play" => UiSnapshot::initial().with_setup_record(SetupRecord::Installed),
        "opening-install" => UiSnapshot::initial().with_setup_record(SetupRecord::Started),
        "guided-check" => UiSnapshot::initial().with_location(Some(location())),
        "prerequisites" | "guided-check-missing" => missing(),
        "folder" => setup().with_installer_media(InstallerMediaView::Validated(PathBuf::from(
            "/run/media/deck/usb/Diablo II",
        ))),
        "setup-existing" => setup().with_installer_media(existing(None)),
        // A folder that holds Project Diablo 2 too.
        "setup-existing-pd2" | "install-d2-existing-pd2" => {
            let mut media = existing(None);
            if let InstallerMediaView::Existing(found) = &mut media {
                found.project_diablo = true;
                found.copy_bytes += 1_100_000_000;
            }
            let snapshot = setup().with_installer_media(media);
            if name()? == "install-d2-existing-pd2" {
                snapshot.with_downloads(DownloadsView {
                    complete: true,
                    installers: false,
                    total_bytes: 512_000_000,
                })
            } else {
                snapshot
            }
        }
        // A folder that can't be used: no Lord of Destruction in it.
        "setup-existing-refused" => {
            setup().with_installer_media(existing(Some(ExistingProblemView::NoExpansion)))
        }
        "download" => setup()
            .with_active_job(Some(ActiveJobView::Download))
            .with_operation(Some(
                working(
                    ActiveJobView::Download,
                    OperationPhase::BaseGameDownload,
                    "Downloading Diablo II from Blizzard",
                )
                .with_typed_progress(800_000_000, Some(1_554_530_117), "bytes")
                .with_progress_summary("800.0 MB of 1.6 GB · 5.0 MB/s · about 3 minutes left"),
            )),
        "download-done" => downloaded().with_operation(Some(
            OperationView::mutating(
                OperationPhase::Verification,
                OperationStatusView::Succeeded,
                "Operation complete",
            )
            .with_job(ActiveJobView::Download),
        )),
        "install-d2" | "setup-invalid" => downloaded(),
        "install-d2-existing" => {
            setup()
                .with_installer_media(existing(None))
                .with_downloads(DownloadsView {
                    complete: true,
                    installers: false,
                    total_bytes: 512_000_000,
                })
        }
        "download-start" => setup()
            .with_active_job(Some(ActiveJobView::Download))
            .with_operation(Some(
                working(
                    ActiveJobView::Download,
                    OperationPhase::RuntimeAcquisition,
                    "Downloading the Windows compatibility layer (Wine)",
                )
                .with_typed_progress(120_000_000, Some(400_000_000), "bytes")
                .with_progress_summary(
                    "120.0 MB of 400.0 MB · 20.0 MB/s · less than a minute left",
                ),
            )),
        "environment" => setup()
            .with_active_job(Some(ActiveJobView::Download))
            .with_operation(Some(
                working(
                    ActiveJobView::Download,
                    OperationPhase::PrefixInitialization,
                    "Setting up the game environment",
                )
                .with_environment(EnvironmentProgress::new(
                    EnvironmentPart::WebComponents,
                    &EnvironmentPart::ALL,
                )),
            )),
        "stopping" => downloaded()
            .with_authorization(AuthorizationView::Authorized)
            .with_active_job(Some(ActiveJobView::InstallBaseGame))
            .with_operation(Some(
                OperationView::mutating(
                    OperationPhase::BaseGameInstall,
                    OperationStatusView::SafeStopping,
                    "Installing Diablo II",
                )
                .with_job(ActiveJobView::InstallBaseGame),
            )),
        "import-done" => setup()
            .with_installer_media(existing(None))
            .with_installation(InstallationStatus::NeedsClient)
            .with_downloads(DownloadsView {
                complete: true,
                installers: false,
                total_bytes: 512_000_000,
            }),
        "progress" => downloaded()
            .with_authorization(AuthorizationView::Authorized)
            .with_active_job(Some(ActiveJobView::InstallBaseGame))
            .with_operation(Some(working(
                ActiveJobView::InstallBaseGame,
                OperationPhase::BaseGameInstall,
                "Installing Diablo II",
            ))),
        "import" => setup()
            .with_installer_media(existing(None))
            .with_active_job(Some(ActiveJobView::InstallBaseGame))
            .with_operation(Some(
                working(
                    ActiveJobView::InstallBaseGame,
                    OperationPhase::ImportGame,
                    "Setting up your Diablo II",
                )
                .with_typed_progress(700_000_000, Some(1_893_000_000), "bytes")
                .with_progress_summary("700.0 MB of 1.9 GB · 350.0 MB/s · less than a minute left"),
            )),
        "install-pd2" | "continue" => downloaded()
            .with_installation(InstallationStatus::NeedsClient)
            .with_environment_ready(true),
        // Install Project Diablo 2 handed on from Install Diablo II.
        "install-tasks" => downloaded()
            .with_installation(InstallationStatus::NeedsClient)
            .with_environment_ready(true)
            .with_active_job(Some(ActiveJobView::InstallProjectDiablo))
            .with_operation(Some(
                working(
                    ActiveJobView::InstallProjectDiablo,
                    OperationPhase::ClientDownload,
                    "Installing Project Diablo 2",
                )
                .with_typed_progress(31, Some(47), "files")
                .with_progress_summary("31 of 47 files · 18.0 MB/s"),
            )),
        // Install Diablo II stopped at Lord of Destruction.
        "install-paused" => downloaded()
            .with_installation(InstallationStatus::NeedsExpansion)
            .with_environment_ready(true)
            .with_stop_result(Some(StopResultView::new(
                InstallationStatus::NeedsExpansion,
                false,
            )))
            .with_operation(Some(
                OperationView::mutating(
                    OperationPhase::Verification,
                    OperationStatusView::Cancelled,
                    "Operation complete",
                )
                .with_job(ActiveJobView::InstallBaseGame),
            )),
        "close" => downloaded()
            .with_installation(InstallationStatus::NeedsClient)
            .with_active_job(Some(ActiveJobView::InstallProjectDiablo))
            .with_operation(Some(working(
                ActiveJobView::InstallProjectDiablo,
                OperationPhase::ClientDownload,
                "Installing Project Diablo 2",
            ))),
        "result" => downloaded().with_operation(Some(
            OperationView::mutating(
                OperationPhase::BaseGameInstall,
                OperationStatusView::Failed,
                "Operation complete",
            )
            .with_job(ActiveJobView::InstallBaseGame)
            .with_failure("base-game-key-rejected"),
        )),
        "interrupted" => downloaded()
            .with_installation(InstallationStatus::StageInterrupted(
                InterruptedStage::Expansion,
            ))
            .with_operation(Some(
                OperationView::mutating(
                    OperationPhase::ExpansionInstall,
                    OperationStatusView::Failed,
                    "Operation complete",
                )
                .with_job(ActiveJobView::InstallBaseGame)
                .with_failure("stage-interrupted"),
            )),
        "start-over" => downloaded().with_installation(InstallationStatus::StageInterrupted(
            InterruptedStage::Expansion,
        )),
        "stopped" => setup().with_stop_result(Some(StopResultView::new(
            InstallationStatus::NeedsBaseGame,
            false,
        ))),
        "unavailable" => setup().with_location(Some(LocationView {
            path: PathBuf::from("/run/media/deck/card/pd2-launcher"),
            default: false,
            available: false,
            previous: None,
        })),
        "game-folder-unavailable" => ready()
            .with_installation(InstallationStatus::Indeterminate)
            .with_host(HostView::Unsupported)
            .with_launch_preflight(false)
            .with_game_folder(Some(GameFolderView {
                path: PathBuf::from("/run/media/deck/games/Diablo II"),
                available: false,
            }))
            .with_operation(Some(
                OperationView::read_only(
                    OperationPhase::HostAudit,
                    OperationStatusView::Failed,
                    "Launcher service could not establish safe evidence",
                )
                .with_failure("game-folder-unavailable"),
            )),
        "ready" => ready().with_game_notice(Some(GameNoticeView::FirstD2glLaunch)),
        // A keep put another copy of a character in backups.
        "ready-saves-put-aside" => ready().with_saves_put_aside(Some(SavesPutAsideView {
            characters: 1,
            files: 1,
            others: 1,
            folder: PathBuf::from("/home/deck/.pd2launcher/userdata/backups/backup-1800000000"),
            open: None,
        })),
        "ready-update" => ready()
            .with_update(UpdateStatus::Available)
            .with_game_notice(Some(GameNoticeView::FirstD2glLaunch)),
        "running" => ready().with_game(GameView::Running),
        "updating" => ready()
            .with_update(UpdateStatus::Available)
            .with_active_job(Some(ActiveJobView::Update))
            .with_operation(Some(
                working(
                    ActiveJobView::Update,
                    OperationPhase::ClientDownload,
                    "Downloading Project Diablo 2 files",
                )
                .with_typed_progress(3, Some(8), "files")
                .with_progress_summary("12.0 MB of 30.0 MB · 4.0 MB/s · 3 of 8 files"),
            )),
        "settings-installed" | "about" | "repair" | "repair-start-over" => ready(),
        "update-stopped" | "update-failed" => {
            let (status, failure) = if name()? == "update-failed" {
                (OperationStatusView::Failed, Some("client-download-failed"))
            } else {
                (OperationStatusView::Cancelled, None)
            };
            let outcome =
                OperationView::mutating(OperationPhase::Verification, status, "Operation complete")
                    .with_job(ActiveJobView::Update);
            ready()
                .with_update(UpdateStatus::Available)
                .with_operation(Some(match failure {
                    Some(code) => outcome.with_failure(code),
                    None => outcome,
                }))
        }
        "update-rechecking" => ready()
            .with_update(UpdateStatus::Checking)
            .with_operation(Some(
                OperationView::mutating(
                    OperationPhase::Verification,
                    OperationStatusView::Succeeded,
                    "Operation complete",
                )
                .with_job(ActiveJobView::Update),
            )),
        _ => return None,
    };
    Some(snapshot)
}

/// The view-local state for the preview named by `PD2_UI_PREVIEW`.
pub(super) fn view() -> Option<ViewPreview> {
    let name = name()?;
    snapshot()?;
    let mut preview = ViewPreview {
        base_key: "",
        expansion_key: "",
        consent: false,
        guided: Some(SetupStep::Device),
        import_mode: ImportModeView::Copy,
        destination: Destination::Operations,
        confirm_close: false,
        confirm_start_over: false,
        rechecking: false,
    };
    preview.guided = Some(match name.as_str() {
        "checking"
        | "guided-check"
        | "guided-check-ok"
        | "guided-check-missing"
        | "prerequisites" => SetupStep::Device,
        "setup"
        | "guided-location"
        | "folder"
        | "setup-existing"
        | "setup-existing-refused"
        | "setup-existing-pd2"
        | "stopped"
        | "unavailable"
        | "settings"
        | "diagnostics" => SetupStep::Setup,
        "guided-download" | "download" | "download-done" | "download-start" | "environment" => {
            SetupStep::Download
        }
        "install-d2"
        | "setup-invalid"
        | "install-d2-existing"
        | "install-d2-existing-pd2"
        | "progress"
        | "stopping"
        | "import"
        | "import-done"
        | "result"
        | "interrupted"
        | "start-over"
        | "install-paused" => SetupStep::InstallD2,
        "install-pd2" | "continue" | "close" | "install-tasks" => SetupStep::InstallPd2,
        _ => SetupStep::Play,
    });
    match name.as_str() {
        // Placeholder characters, not real keys: one too short to be a key.
        "setup-invalid" => {
            preview.base_key = "ABCD-EFGH-IJKL-MNO";
            preview.expansion_key = "QRST UVWX YZ01 2345";
            preview.consent = true;
        }
        "settings" | "settings-installed" => preview.destination = Destination::Settings,
        "about" => preview.destination = Destination::About,
        "repair" => preview.destination = Destination::Repair,
        "repair-start-over" => {
            preview.destination = Destination::Repair;
            preview.confirm_start_over = true;
        }
        "diagnostics" => preview.destination = Destination::Diagnostics,
        "close" => preview.confirm_close = true,
        "update-rechecking" => preview.rechecking = true,
        "start-over" => preview.confirm_start_over = true,
        _ => {}
    }
    Some(preview)
}
