//! The Control channel: the private, versioned connection between the
//! Android app and the Launcher engine (`pd2-launcher --engine`).
//!
//! The Android app creates a connected `AF_UNIX` stream socket pair and
//! starts the engine with its end as standard input, so the engine adopts it
//! without unsafe code. Every message is a frame: a 4-byte big-endian length,
//! then that many bytes of UTF-8 JSON. A frame larger than
//! [`MAX_FRAME_BYTES`] is a protocol error: one received closes the
//! connection, and one the engine would send is never sent.
//!
//! The app speaks first. Its `hello` names the protocol version and the
//! Android host facts; the engine answers `welcome` and only then starts its
//! service, or answers `refused` with the versions it speaks and exits. After
//! that the app sends `command`s and the engine sends a full `snapshot`
//! whenever what the app shows changes (never a delta), and a
//! `command_error` for a command it could not take, or that the Launcher
//! refused (its guard failure, which the desktop window shows as evidence).
//! When the app closes its
//! end, the engine requests a safe stop, waits for the service to finish, and
//! exits.
//!
//! The messages are the protocol's own types, not the Launcher's `Intent`
//! and `UiSnapshot`: the engine maps them both ways, so a change inside the
//! Launcher never changes what an Android app on the same protocol version
//! reads. Any change to a message bumps [`PROTOCOL`], with the Android app's
//! protocol version in step, and regenerates the golden fixtures the Android
//! app's decoder tests read (see the tests).

use std::{
    collections::VecDeque,
    fmt,
    io::{self, Read, Write},
    net::Shutdown,
    os::{fd::AsFd, unix::net::UnixStream},
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{Receiver, SyncSender, TrySendError},
    },
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use crate::{
    InstallationStatus, InterruptedStage, UpdateStatus,
    audit::WineLibrary,
    presentation::{
        self, ActiveJobView, AuthorizationInput, AuthorizationInputError, AuthorizationView,
        DirectDrawRendererView, DownloadSourceView, ExistingProblemView, GameNoticeView, GameView,
        HostView, ImportModeView, InstallSourceView, InstallerMediaView, Intent, OperationPhase,
        OperationStatusView, Prerequisite, ReduceOutcome, RendererPreferenceView, ReportView,
        ResultNextStep, SequencedEvent, ServiceEndpoint, SettingsChange, SettingsPersistenceView,
        SetupStep, SourceBadgeView, UiReducer, UiSnapshot,
    },
    sources::DownloadArtifact,
};

/// The protocol version this engine speaks. 8: the snapshot names the
/// installed copy's language (`game_language`). 7: on the Android host the
/// game folder is outside the disposable prefix and an import is moved into
/// it (`game_folder::ANDROID_GAME_RELATIVE`); an app of an earlier protocol
/// looks for the game in the prefix, so the handshake refuses it.
pub(crate) const PROTOCOL: u32 = 8;

/// The engine's capabilities in `welcome`, which an additive field carries
/// within the same protocol: `gdi_adjmouse`, a DirectDraw start with the
/// `software` renderer (`renderer=gdi`) also writes `adjmouse=true` and puts
/// the earlier lines back afterwards ([`crate::process`]'s `ddraw`), so the
/// Android app offers its Software driver only with it.
pub(crate) const CAPABILITIES: &[&str] = &["gdi_adjmouse"];

/// The largest frame either side may send: 1 MiB of JSON.
pub(crate) const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// How long the engine waits for the app's `hello`.
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);

/// Exit status of an engine that ended on a Control channel failure: no
/// valid `hello`, a refused protocol, a broken frame, or a panic.
const FAILURE_EXIT_STATUS: u8 = 1;

/// What the Android app sends.
#[derive(Debug, Deserialize, Serialize)]
#[cfg_attr(test, derive(PartialEq))]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum AppMessage {
    /// The first message, and only the first.
    Hello {
        protocol: u32,
        app_version: String,
        host_facts: HostFacts,
    },
    /// One player action. `seq` is the app's own counter; a
    /// `command_error` names the command it refuses by it.
    Command { seq: u64, intent: Command },
}

/// What the engine sends.
#[derive(Debug, Deserialize, Serialize)]
#[cfg_attr(test, derive(PartialEq))]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum EngineMessage {
    /// The `hello` was accepted; the service starts now.
    Welcome {
        protocol: u32,
        launcher_version: String,
        /// What this engine can do beyond its protocol, for the app's
        /// screens ([`CAPABILITIES`]); an engine without the field has none.
        #[serde(default)]
        capabilities: Vec<String>,
    },
    /// The `hello` named a protocol this engine does not speak; it exits.
    Refused { supported: Vec<u32> },
    /// The whole state the app shows. `seq` counts the snapshots sent, from
    /// 1; a snapshot that was replaced before it could be sent is skipped
    /// without using a number.
    Snapshot { seq: u64, snapshot: Box<Snapshot> },
    /// The command `seq` was not taken (0: not known, or the engine itself
    /// failed). `reason` is a stable code: `malformed-message`,
    /// `unsupported-command`, `unexpected-hello`, `invalid-product-key`,
    /// a key that the installers would refuse (protocol 6:
    /// `d2-key-wrong-length`, `d2-key-16-characters`,
    /// `d2-key-invalid-character`, `d2-key-not-valid`, `d2-key-is-lod-key`,
    /// and `lod-key-…` with `lod-key-is-d2-key`),
    /// `relative-path`, custom start options that are refused (protocol 8:
    /// `custom-arguments-too-long`, `custom-arguments-invalid-character`),
    /// program paths that are refused (protocol 8: `program-path-empty`,
    /// `program-path-too-long`, `program-path-invalid-character`,
    /// `program-path-absolute`, `program-path-parent`,
    /// `program-path-not-exe`), `engine-busy`, `service-unavailable` or
    /// `engine-failed`; or the Launcher refused it
    /// (a guard failure such as `install-not-permitted`, or
    /// `program-path-missing` for a program path whose file is not in the
    /// game folder), named by the code
    /// the desktop window shows under "Request not started". A Launcher
    /// refusal carries the `seq` of the newest command handed to the
    /// service when the refusal is reported (0 before any), not one the
    /// Launcher names: it is the refused command only when no later command
    /// was handed over before the refusal was reported; otherwise it names
    /// a later command than the one refused. It never holds what the
    /// command carried.
    CommandError { seq: u64, reason: String },
}

/// The Android host, as the app measured it before starting the engine.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct HostFacts {
    /// Android's version name (`Build.VERSION.RELEASE`), such as `13`.
    pub(crate) android_release: String,
    /// Android's API level (`Build.VERSION.SDK_INT`).
    pub(crate) sdk: u32,
    /// The device model (`Build.MODEL`).
    pub(crate) device_model: String,
    /// The GPU's name, such as `Adreno (TM) 740`.
    pub(crate) gpu_name: String,
    /// The GPU's KGSL chip id.
    pub(crate) chip_id: u32,
    /// The kernel release (`uname -r`).
    pub(crate) kernel_release: String,
    /// The memory page size in bytes.
    pub(crate) page_size: u64,
    /// The app's data folder as a canonical path
    /// (`/data/data/com.pd2launcher.app`, not the `/data/user/0` link).
    pub(crate) data_dir: String,
}

impl HostFacts {
    /// One line for the engine's log.
    pub(crate) fn summary(&self) -> String {
        format!(
            "Android {} (SDK {}), {}, {} (chip {:#010x}), kernel {}, {}-byte pages",
            self.android_release,
            self.sdk,
            self.device_model,
            self.gpu_name,
            self.chip_id,
            self.kernel_release,
            self.page_size
        )
    }
}

/// A player action: the Launcher's own actions one to one, except those the
/// Android app does not offer (choosing the install location or removing the
/// one a move left behind, choosing FEX, and the desktop-only settings).
#[derive(Debug, Deserialize, Serialize)]
#[cfg_attr(test, derive(PartialEq))]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Command {
    Navigate {
        destination: Destination,
    },
    /// A folder with Blizzard's installer files (an absolute path).
    SelectInstallerMedia {
        path: String,
    },
    UseBlizzardDownload,
    /// Open the editor of a step whose affordance is Change.
    EditStep {
        step: Step,
    },
    /// "Back to current step".
    CloseEditor,
    /// Stop the running install at a safe point, then open the step's
    /// editor. The app asks for confirmation first.
    StopAndChange {
        step: Step,
    },
    DismissStopResult,
    /// Hide the result card of the last operation that did not succeed.
    DismissResult,
    CheckAgain,
    SaveDownloadSource {
        artifact: Artifact,
        url: String,
    },
    ResetDownloadSource {
        artifact: Artifact,
    },
    /// Install Diablo II with both CD keys. The app sends it only after the
    /// player confirmed Blizzard's license terms in its own dialog, so the
    /// command is the confirmed intent.
    Authorize {
        d2_key: ProductKey,
        lod_key: ProductKey,
    },
    /// Continue the step the installation stands at, without keys.
    ContinueInstallation,
    DownloadAll,
    /// Install a Diablo II the player already has.
    ImportExistingInstall {
        mode: ImportMode,
    },
    InstallProjectDiablo,
    /// A Diablo II folder the player already has, of any version (an
    /// absolute path).
    SelectExistingInstall {
        path: String,
    },
    /// Download from Blizzard, unless a choice was already restored. The
    /// engine sends it itself after `welcome`.
    UseDefaultInstallerSource,
    RequestSafeStop,
    /// Apply the Available update. The player pressed Update on Play, so
    /// the command is the confirmed intent.
    ApplyUpdate,
    /// Play, with the app's graphics driver (`standard` when absent), its
    /// D2GL graphics profile (none when absent: `d2gl.json`'s graphics stay
    /// as they are), and whether the game gets Box64's strict x86 memory
    /// order (the app's Strict CPU emulation, Automatic already resolved; off
    /// when absent), and the app's Detailed logging (off when absent; it
    /// goes with the intent, [`Command::into_intent`]). The app may also
    /// send the X screen's size (`display`), which the engine does not use:
    /// D2GL sizes the full-screen game from the X screen itself.
    LaunchGame {
        #[serde(default)]
        graphics_driver: GraphicsDriver,
        #[serde(default)]
        graphics_profile: Option<GraphicsProfile>,
        #[serde(default)]
        strict_memory_order: bool,
        #[serde(default)]
        detailed_logging: bool,
    },
    /// Open the official PD2 launcher. The player pressed its own button, so
    /// the command is the confirmed intent: the launcher updates itself and
    /// is never pinned, and Android has no separate confirmation for it.
    /// Whether a game that it starts gets Box64's strict x86 memory order is
    /// as for Play, and the app's Detailed logging (each off when absent).
    OpenOfficialLauncher {
        #[serde(default)]
        strict_memory_order: bool,
        #[serde(default)]
        detailed_logging: bool,
    },
    /// A confirmed Start over.
    StartOver,
    /// Resolve an unavailable install location again.
    RetryLocation,
    ChangeSettings {
        change: SettingChange,
    },
    /// Stop a running Download job, then remove what the Launcher's own
    /// downloads made, for an import (protocol 6); refused while an install
    /// stage runs.
    DiscardDownloads,
    ExportDiagnostics,
    RevealReport,
    DismissReport,
    /// Ask for a full snapshot now.
    RequestFullSnapshot,
}

/// A CD key as it arrives on the Control channel: decoded straight into
/// bytes (never a `String`), wiped when dropped and never shown by `Debug`.
/// It is validated only when the command is mapped, so a bad key gets
/// `invalid-product-key`, not `malformed-message`.
pub(crate) struct ProductKey(Vec<u8>);

impl ProductKey {
    fn take(mut self) -> Vec<u8> {
        std::mem::take(&mut self.0)
    }
}

impl Drop for ProductKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for ProductKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

#[cfg(test)]
impl PartialEq for ProductKey {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Serialize for ProductKey {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let key = std::str::from_utf8(&self.0).map_err(serde::ser::Error::custom)?;
        serializer.serialize_str(key)
    }
}

impl<'de> Deserialize<'de> for ProductKey {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Bytes;

        impl serde::de::Visitor<'_> for Bytes {
            type Value = ProductKey;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a product key")
            }

            // A borrowed value lies in the frame, which the engine wipes.
            // Each copy is exactly the key's length, so the authorization's
            // boxed bytes never reallocate and leave no unwiped copy behind.
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<ProductKey, E> {
                Ok(ProductKey(value.as_bytes().to_vec()))
            }

            fn visit_string<E: serde::de::Error>(self, mut value: String) -> Result<ProductKey, E> {
                let key = ProductKey(value.as_bytes().to_vec());
                value.zeroize();
                Ok(key)
            }
        }

        deserializer.deserialize_str(Bytes)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Destination {
    Operations,
    Settings,
    Diagnostics,
}

/// One of the Launcher's six setup steps.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Step {
    Device,
    Setup,
    Download,
    InstallD2,
    InstallPd2,
    Play,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ImportMode {
    /// Copy it into the Launcher's game folder.
    Copy,
    /// Use it where it is.
    InPlace,
}

/// One download whose source Diagnostics shows and may override.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Artifact {
    Wine,
    Gecko,
    WineMono,
    DotnetDesktop,
    D2Installer,
    LodInstaller,
    Pd2Installer,
    Pd2ClientBase,
    Fex,
    FexRootFs,
}

/// The app's graphics driver for a Play: the App userland's Turnip as it is,
/// or with its Compatibility values.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GraphicsDriver {
    #[default]
    Standard,
    Compatibility,
}

/// The app's D2GL graphics profile for a Play, Automatic already resolved:
/// the engine writes its values into `d2gl.json` when it is not the profile
/// last applied there.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GraphicsProfile {
    /// Project Diablo 2's own D2GL settings.
    Default,
    /// D2GL's advanced settings tuned down for older devices.
    Performance,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "setting", rename_all = "snake_case")]
pub(crate) enum SettingChange {
    Renderer {
        renderer: Renderer,
    },
    DirectDrawRenderer {
        renderer: DirectDrawRenderer,
    },
    SkipToBnet {
        enabled: bool,
    },
    SoundInBackground {
        enabled: bool,
    },
    /// Project Diablo 2's update checks; off, the client plays as installed.
    UpdateChecks {
        enabled: bool,
    },
    /// Where Blizzard's installers are downloaded from, from the next
    /// Download or install step on (protocol 6).
    InstallSource {
        source: InstallSource,
    },
    /// The player's own start options, added after the Launcher's own when
    /// Play starts Game.exe (protocol 8): at most 200 characters, no
    /// control character and no double quote; empty for none.
    CustomArguments {
        arguments: String,
    },
    /// The program Play starts, a path in the game folder (protocol 8):
    /// `ProjectD2/Game.exe` by default; `/` or `\\` between the parts.
    GameProgram {
        path: String,
    },
    /// The program the official PD2 launcher button opens, a path in the
    /// game folder (protocol 8): `ProjectD2/PD2Launcher.exe` by default.
    LauncherProgram {
        path: String,
    },
    /// The official PD2 launcher's own start options (protocol 8), with the
    /// rules of [`SettingChange::CustomArguments`].
    LauncherCustomArguments {
        arguments: String,
    },
}

/// Where Blizzard's installers come from: Blizzard's own downloader (the
/// default), or the direct download, which fetches the same files without
/// it and without Wine.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InstallSource {
    Official,
    DirectDownload,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Renderer {
    Glide,
    DirectDraw,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DirectDrawRenderer {
    Pd2,
    OpenGl,
    Direct3d9,
    Software,
}

impl Command {
    /// The Launcher action this command asks for, or the `command_error`
    /// reason it is refused with. A Play and a press of the official PD2
    /// launcher carry the app's Detailed logging in their intent, for the
    /// Wine processes they start (a game that the official launcher starts
    /// inherits the launcher's).
    pub(crate) fn into_intent(self) -> Result<Intent, &'static str> {
        Ok(match self {
            Self::Navigate { destination } => Intent::Navigate(match destination {
                Destination::Operations => presentation::Destination::Operations,
                Destination::Settings => presentation::Destination::Settings,
                Destination::Diagnostics => presentation::Destination::Diagnostics,
            }),
            Self::SelectInstallerMedia { path } => Intent::SelectInstallerMedia(absolute(path)?),
            Self::UseBlizzardDownload => Intent::UseBlizzardDownload,
            Self::EditStep { step } => Intent::EditStep(step.into()),
            Self::CloseEditor => Intent::CloseEditor,
            Self::StopAndChange { step } => Intent::StopAndChange(step.into()),
            Self::DismissStopResult => Intent::DismissStopResult,
            Self::DismissResult => Intent::DismissResult,
            Self::CheckAgain => Intent::CheckAgain,
            Self::SaveDownloadSource { artifact, url } => {
                Intent::SaveDownloadSource(artifact.into(), url)
            }
            Self::ResetDownloadSource { artifact } => Intent::ResetDownloadSource(artifact.into()),
            // The desktop's in-memory authorization, with the terms the
            // app's dialog confirmed.
            Self::Authorize { d2_key, lod_key } => {
                use crate::blizzard::{KeyField, check_key};
                // Each key as the installers check it, before anything is
                // downloaded.
                let d2_check = check_key(KeyField::BaseGame, &d2_key.0).map(drop);
                let lod_check = check_key(KeyField::Expansion, &lod_key.0).map(drop);
                let input = AuthorizationInput::from_bytes(d2_key.take(), lod_key.take(), true)
                    .map_err(|_: AuthorizationInputError| "invalid-product-key")?;
                d2_check.map_err(|problem| key_reason(KeyField::BaseGame, problem))?;
                lod_check.map_err(|problem| key_reason(KeyField::Expansion, problem))?;
                Intent::AuthorizeAndInstall(input)
            }
            Self::ContinueInstallation => Intent::ContinueInstallation,
            Self::DownloadAll => Intent::DownloadAll,
            Self::ImportExistingInstall { mode } => Intent::ImportExistingInstall(match mode {
                ImportMode::Copy => ImportModeView::Copy,
                ImportMode::InPlace => ImportModeView::InPlace,
            }),
            Self::InstallProjectDiablo => Intent::InstallProjectDiablo,
            Self::SelectExistingInstall { path } => Intent::SelectExistingInstall(absolute(path)?),
            Self::UseDefaultInstallerSource => Intent::UseDefaultInstallerSource,
            Self::RequestSafeStop => Intent::RequestSafeStop,
            Self::ApplyUpdate => Intent::ApplyUpdate,
            Self::LaunchGame {
                graphics_driver,
                graphics_profile,
                strict_memory_order,
                detailed_logging,
            } => Intent::LaunchGame(
                match graphics_driver {
                    GraphicsDriver::Standard => presentation::GraphicsDriver::Standard,
                    GraphicsDriver::Compatibility => presentation::GraphicsDriver::Compatibility,
                },
                graphics_profile.map(|profile| match profile {
                    GraphicsProfile::Default => presentation::GraphicsProfile::Default,
                    GraphicsProfile::Performance => presentation::GraphicsProfile::Performance,
                }),
                strict_memory_order,
                detailed_logging,
            ),
            Self::OpenOfficialLauncher {
                strict_memory_order,
                detailed_logging,
            } => Intent::OpenOfficialLauncherConfirmed(strict_memory_order, detailed_logging),
            Self::DiscardDownloads => Intent::DiscardDownloads,
            Self::StartOver => Intent::StartOver,
            Self::RetryLocation => Intent::RetryLocation,
            Self::ChangeSettings { change } => Intent::ChangeSettings(match change {
                SettingChange::Renderer { renderer } => SettingsChange::Renderer(match renderer {
                    Renderer::Glide => RendererPreferenceView::Glide,
                    Renderer::DirectDraw => RendererPreferenceView::DirectDraw,
                }),
                SettingChange::DirectDrawRenderer { renderer } => {
                    SettingsChange::DirectDrawRenderer(match renderer {
                        DirectDrawRenderer::Pd2 => DirectDrawRendererView::Pd2,
                        DirectDrawRenderer::OpenGl => DirectDrawRendererView::OpenGl,
                        DirectDrawRenderer::Direct3d9 => DirectDrawRendererView::Direct3d9,
                        DirectDrawRenderer::Software => DirectDrawRendererView::Software,
                    })
                }
                SettingChange::SkipToBnet { enabled } => SettingsChange::SkipToBnet(enabled),
                SettingChange::SoundInBackground { enabled } => {
                    SettingsChange::SoundInBackground(enabled)
                }
                SettingChange::UpdateChecks { enabled } => SettingsChange::UpdateChecks(enabled),
                SettingChange::InstallSource { source } => {
                    SettingsChange::InstallSource(match source {
                        InstallSource::Official => InstallSourceView::Official,
                        InstallSource::DirectDownload => InstallSourceView::DirectDownload,
                    })
                }
                SettingChange::CustomArguments { arguments } => SettingsChange::CustomArguments(
                    crate::persistence::custom_arguments(&arguments)?,
                ),
                SettingChange::GameProgram { path } => {
                    SettingsChange::GameProgram(crate::persistence::program_path(&path)?)
                }
                SettingChange::LauncherProgram { path } => {
                    SettingsChange::LauncherProgram(crate::persistence::program_path(&path)?)
                }
                SettingChange::LauncherCustomArguments { arguments } => {
                    SettingsChange::LauncherCustomArguments(crate::persistence::custom_arguments(
                        &arguments,
                    )?)
                }
            }),
            Self::ExportDiagnostics => Intent::ExportDiagnostics,
            Self::RevealReport => Intent::RevealReport,
            Self::DismissReport => Intent::DismissReport,
            Self::RequestFullSnapshot => Intent::RequestFullSnapshot,
        })
    }
}

/// The `command_error` reason for a key that the installers would refuse.
fn key_reason(
    field: crate::blizzard::KeyField,
    problem: crate::blizzard::KeyProblem,
) -> &'static str {
    use crate::blizzard::{KeyField, KeyProblem};
    match (field, problem) {
        (KeyField::BaseGame, KeyProblem::WrongLength) => "d2-key-wrong-length",
        (KeyField::BaseGame, KeyProblem::OldKeyLength) => "d2-key-16-characters",
        (KeyField::BaseGame, KeyProblem::InvalidCharacter) => "d2-key-invalid-character",
        (KeyField::BaseGame, KeyProblem::ExpansionKeyInBaseField) => "d2-key-is-lod-key",
        (KeyField::BaseGame, KeyProblem::WrongProduct | KeyProblem::BaseKeyInExpansionField) => {
            "d2-key-not-valid"
        }
        (KeyField::Expansion, KeyProblem::WrongLength) => "lod-key-wrong-length",
        (KeyField::Expansion, KeyProblem::OldKeyLength) => "lod-key-16-characters",
        (KeyField::Expansion, KeyProblem::InvalidCharacter) => "lod-key-invalid-character",
        (KeyField::Expansion, KeyProblem::BaseKeyInExpansionField) => "lod-key-is-d2-key",
        (KeyField::Expansion, KeyProblem::WrongProduct | KeyProblem::ExpansionKeyInBaseField) => {
            "lod-key-not-valid"
        }
    }
}

fn absolute(path: String) -> Result<PathBuf, &'static str> {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        Ok(path)
    } else {
        Err("relative-path")
    }
}

impl From<Step> for SetupStep {
    fn from(step: Step) -> Self {
        match step {
            Step::Device => Self::Device,
            Step::Setup => Self::Setup,
            Step::Download => Self::Download,
            Step::InstallD2 => Self::InstallD2,
            Step::InstallPd2 => Self::InstallPd2,
            Step::Play => Self::Play,
        }
    }
}

impl From<SetupStep> for Step {
    fn from(step: SetupStep) -> Self {
        match step {
            SetupStep::Device => Self::Device,
            SetupStep::Setup => Self::Setup,
            SetupStep::Download => Self::Download,
            SetupStep::InstallD2 => Self::InstallD2,
            SetupStep::InstallPd2 => Self::InstallPd2,
            SetupStep::Play => Self::Play,
        }
    }
}

impl From<Artifact> for DownloadArtifact {
    fn from(artifact: Artifact) -> Self {
        match artifact {
            Artifact::Wine => Self::Wine,
            Artifact::Gecko => Self::Gecko,
            Artifact::WineMono => Self::WineMono,
            Artifact::DotnetDesktop => Self::DotnetDesktop,
            Artifact::D2Installer => Self::D2Installer,
            Artifact::LodInstaller => Self::LodInstaller,
            Artifact::Pd2Installer => Self::Pd2Installer,
            Artifact::Pd2ClientBase => Self::Pd2ClientBase,
            Artifact::Fex => Self::Fex,
            Artifact::FexRootFs => Self::FexRootFs,
        }
    }
}

impl From<DownloadArtifact> for Artifact {
    fn from(artifact: DownloadArtifact) -> Self {
        match artifact {
            DownloadArtifact::Wine => Self::Wine,
            DownloadArtifact::Gecko => Self::Gecko,
            DownloadArtifact::WineMono => Self::WineMono,
            DownloadArtifact::DotnetDesktop => Self::DotnetDesktop,
            DownloadArtifact::D2Installer => Self::D2Installer,
            DownloadArtifact::LodInstaller => Self::LodInstaller,
            DownloadArtifact::Pd2Installer => Self::Pd2Installer,
            DownloadArtifact::Pd2ClientBase => Self::Pd2ClientBase,
            DownloadArtifact::Fex => Self::Fex,
            DownloadArtifact::FexRootFs => Self::FexRootFs,
        }
    }
}

/// Everything the Android app's screens show, as one value. Text meant for
/// the player comes as stable codes the app words itself; the English
/// `title`, `detail` and `message` fields are the Launcher's own wording,
/// for the app to show where it has none.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Snapshot {
    /// Step 1's verdict on this device.
    pub(crate) host: Host,
    /// What the device is missing, in the order the check found it.
    pub(crate) missing: Vec<Finding>,
    pub(crate) installation: Installation,
    /// The stage that stopped part way, when `installation` is
    /// `stage_interrupted`.
    pub(crate) interrupted_stage: Option<Stage>,
    pub(crate) update: Update,
    /// The step the installation stands at: the first whose result is not
    /// there yet.
    pub(crate) step: Step,
    /// The step whose job is running.
    pub(crate) running_step: Option<Step>,
    /// The step whose editor is open.
    pub(crate) editing: Option<Step>,
    /// Both CD keys were accepted for the install that is running.
    pub(crate) authorized: bool,
    /// How Diablo II is to be installed.
    pub(crate) installer: Installer,
    /// Why the last installer choice was refused (a prior valid one is kept).
    pub(crate) installer_rejection: Option<String>,
    /// The running or last operation.
    pub(crate) operation: Option<Operation>,
    /// The job that holds the installation now.
    pub(crate) active_job: Option<Job>,
    /// The last operation that did not succeed, until dismissed or new work
    /// starts.
    pub(crate) result: Option<ResultCard>,
    /// What a player-requested install stop kept, until dismissed.
    pub(crate) stop_result: Option<StopResult>,
    pub(crate) game: Game,
    pub(crate) game_notice: Option<GameNotice>,
    /// Play's last preflight passed.
    pub(crate) launch_preflight_passed: bool,
    /// Recorded work waits for Repair.
    pub(crate) unresolved_journal: bool,
    /// The installed game's OpenGL renderer draws in software (advisory).
    pub(crate) game_software_rendering: bool,
    /// The installed copy's language as D2Lang names it (`eng`, `chi`, ...),
    /// when an audit read it. PD2 needs `eng`; another is a warning.
    pub(crate) game_language: Option<String>,
    /// The last audit found the game environment set up (its Wine and every
    /// Windows component): Install's first task is done.
    pub(crate) environment_ready: bool,
    pub(crate) downloads: Downloads,
    /// Where the Managed installation lives.
    pub(crate) location: Option<Location>,
    /// Why the last location request was refused.
    pub(crate) location_error: Option<String>,
    /// Set only for a Diablo II folder used where it is.
    pub(crate) game_folder: Option<GameFolder>,
    pub(crate) settings: Settings,
    /// Diagnostics' download sources.
    pub(crate) download_sources: Vec<DownloadSource>,
    /// The report being saved, or the last one saved, until dismissed.
    pub(crate) report: Option<Report>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Host {
    Checking,
    Supported,
    Unsupported,
}

/// One thing the device needs before the game can be installed.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Finding {
    DisplaySession,
    Fex,
    FexRootFs,
    PageSize,
    WineCannotRun,
    Ia32Execution,
    NoExec,
    I386Loader,
    Libraries,
    WineLibraryX11,
    WineLibraryXext,
    WineLibraryFreeType,
    WineLibraryFontconfig,
    WineLibraryOpenGl,
    Libunwind,
    #[serde(rename = "xwayland")]
    XWayland,
    VulkanLoader,
    VulkanDriver,
    HardwareGraphics,
    LauncherBuild,
    Architecture,
    HostInformation,
    UncertifiedBuild,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Installation {
    NeedsBaseGame,
    NeedsExpansion,
    NeedsClient,
    /// The game is installed; a Windows component is still missing.
    NeedsRuntime,
    RepairRequired,
    Indeterminate,
    StageInterrupted,
    Ready,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Stage {
    Prefix,
    BaseGame,
    Expansion,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Update {
    Unknown,
    Checking,
    UpToDate,
    Available,
    CheckFailed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Installer {
    NotSelected,
    /// A folder with Blizzard's installer files, checked.
    Folder {
        path: String,
    },
    /// Blizzard's downloaders fetch the installers.
    BlizzardDownload,
    /// The chosen folder was refused; `reason` is a stable code.
    Invalid {
        reason: String,
    },
    /// A Diablo II the player already has; usable when `problem` is null.
    Existing {
        path: String,
        problem: Option<ExistingProblem>,
        /// What a copy writes.
        copy_bytes: u64,
        /// Windows programs can use it where it is.
        in_place_possible: bool,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub(crate) enum ExistingProblem {
    NotFound,
    Unreadable,
    NotDiablo,
    /// Not sent: a folder of any Diablo II version is used as it is. This
    /// code and the next two stay so that the protocol, and an app that
    /// decodes them, are unchanged.
    UnsupportedVersion {
        version: Option<String>,
    },
    /// Not sent.
    ModifiedGame,
    /// Not sent.
    ArchiveMismatch,
    NoExpansion,
    MissingFile {
        file: String,
    },
    Links,
    InsideLauncher,
    UnsupportedPath,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Operation {
    pub(crate) phase: Phase,
    pub(crate) status: OperationStatus,
    pub(crate) title: String,
    pub(crate) detail: String,
    pub(crate) progress: Option<Progress>,
    /// Work that changes the installation is running: Request safe stop
    /// applies to it.
    pub(crate) mutating: bool,
    /// The stable code of a failed, cancelled or indeterminate outcome.
    pub(crate) failure: Option<String>,
    /// The job this operation belongs to.
    pub(crate) job: Option<Job>,
    /// While the game environment is set up (`prefix_initialization`): the
    /// part running, once the first has started.
    pub(crate) environment: Option<EnvironmentSetup>,
}

/// One setup of the game environment: the parts it runs, in order, and the
/// one running.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct EnvironmentSetup {
    pub(crate) part: EnvironmentPart,
    pub(crate) parts: Vec<EnvironmentPart>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EnvironmentPart {
    /// Wine unpacked and Windows set up in the prefix.
    Windows,
    /// Wine Gecko.
    WebComponents,
    /// Wine Mono.
    Dotnet,
    /// The .NET Desktop Runtime the official PD2 launcher needs.
    LauncherSupport,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Phase {
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OperationStatus {
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

/// How far the operation is; each part only when known.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Progress {
    pub(crate) percent: Option<u8>,
    pub(crate) completed: Option<u64>,
    pub(crate) total: Option<u64>,
    /// What `completed` and `total` count, such as `bytes`.
    pub(crate) unit: Option<String>,
    /// One line, such as the speed and the time left.
    pub(crate) summary: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Job {
    Download,
    InstallBaseGame,
    InstallProjectDiablo,
    Update,
    Repair,
    OfficialLauncher,
    Reset,
    Relocation,
    /// Removing what the Launcher's own downloads made (protocol 6).
    DiscardDownloads,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct ResultCard {
    pub(crate) title: String,
    pub(crate) message: String,
    /// What the card offers besides dismissing it.
    pub(crate) next: Option<ResultNext>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ResultNext {
    /// Run the same work again.
    TryAgain,
    OpenDiagnostics,
    StartOver,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct StopResult {
    /// The installation the audit after the stop found.
    pub(crate) installation: Installation,
    pub(crate) interrupted_stage: Option<Stage>,
    /// That audit could not establish one outcome: only Diagnostics and a
    /// confirmed Start over remain.
    pub(crate) needs_recovery: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Game {
    Idle,
    Running,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GameNotice {
    /// D2GL has not written its configuration yet.
    FirstD2glLaunch,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Downloads {
    /// Everything the remaining install needs is downloaded and verified.
    pub(crate) complete: bool,
    /// The downloads include Blizzard's installers.
    pub(crate) installers: bool,
    pub(crate) total_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Location {
    pub(crate) path: String,
    /// The default location rather than a chosen one.
    pub(crate) default: bool,
    /// The location was found and proven; nothing is installed into an
    /// unavailable one.
    pub(crate) available: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct GameFolder {
    pub(crate) path: String,
    pub(crate) available: bool,
}

/// The next launch's settings.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Settings {
    pub(crate) renderer: Renderer,
    pub(crate) direct_draw_renderer: DirectDrawRenderer,
    pub(crate) skip_to_bnet: bool,
    pub(crate) sound_in_background: bool,
    /// Project Diablo 2's update checks run (the default); off, none runs,
    /// the update stays unknown and the client plays as installed.
    pub(crate) update_checks: bool,
    /// Where Blizzard's installers are downloaded from (protocol 6).
    pub(crate) install_source: InstallSource,
    /// The player's own start options for Play; empty for none (protocol 8).
    pub(crate) custom_arguments: String,
    /// The program Play starts, from the game folder (protocol 8).
    pub(crate) game_program: String,
    /// The program the official PD2 launcher button opens, from the game
    /// folder (protocol 8).
    pub(crate) launcher_program: String,
    /// The official PD2 launcher's own start options; empty for none
    /// (protocol 8).
    pub(crate) launcher_custom_arguments: String,
    pub(crate) persistence: SettingsPersistence,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum SettingsPersistence {
    Loading,
    Saved,
    Failed { reason: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct DownloadSource {
    pub(crate) artifact: Artifact,
    pub(crate) effective_url: String,
    pub(crate) badge: SourceBadge,
    /// The stable code of the last refused Save for this source.
    pub(crate) error: Option<String>,
    /// The source the installed component came from, when it differs.
    pub(crate) installed: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SourceBadge {
    BuiltInVerified,
    CustomFromEnvironment,
    Custom,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum Report {
    Creating,
    /// `opened`: whether its folder opened (null while that is tried).
    Saved {
        path: String,
        opened: Option<bool>,
    },
    Failed {
        reason: String,
    },
}

impl Snapshot {
    /// The protocol's view of the Launcher's presentation state.
    pub(crate) fn project(snapshot: &UiSnapshot) -> Self {
        let (installation, interrupted_stage) = installation_state(snapshot.installation());
        let settings = snapshot.settings();
        Self {
            host: match snapshot.host() {
                HostView::Checking => Host::Checking,
                HostView::Supported => Host::Supported,
                HostView::Unsupported => Host::Unsupported,
            },
            missing: snapshot
                .system()
                .missing
                .iter()
                .map(|missing| finding(*missing))
                .collect(),
            installation,
            interrupted_stage,
            update: match snapshot.update() {
                UpdateStatus::Unknown => Update::Unknown,
                UpdateStatus::Checking => Update::Checking,
                UpdateStatus::UpToDate => Update::UpToDate,
                UpdateStatus::Available => Update::Available,
                UpdateStatus::CheckFailed => Update::CheckFailed,
            },
            step: presentation::standing_step(snapshot).into(),
            running_step: presentation::running_step(snapshot).map(Step::from),
            editing: snapshot.editing().map(Step::from),
            authorized: snapshot.authorization() == AuthorizationView::Authorized,
            installer: match snapshot.installer_media() {
                InstallerMediaView::NotSelected => Installer::NotSelected,
                InstallerMediaView::Validated(path) => Installer::Folder {
                    path: path.display().to_string(),
                },
                InstallerMediaView::BlizzardDownload => Installer::BlizzardDownload,
                InstallerMediaView::Invalid(reason) => Installer::Invalid {
                    reason: (*reason).to_owned(),
                },
                InstallerMediaView::Existing(existing) => Installer::Existing {
                    path: existing.path.display().to_string(),
                    problem: existing.problem.map(existing_problem),
                    copy_bytes: existing.copy_bytes,
                    in_place_possible: existing.in_place_possible,
                },
            },
            installer_rejection: snapshot.installer_media_rejection().map(str::to_owned),
            operation: snapshot.operation().map(|operation| {
                let progress = Progress {
                    percent: operation.progress_percent(),
                    completed: operation.progress_completed(),
                    total: operation.progress_total(),
                    unit: operation.progress_unit().map(str::to_owned),
                    summary: operation.progress_summary().map(str::to_owned),
                };
                let known = progress.percent.is_some()
                    || progress.completed.is_some()
                    || progress.total.is_some()
                    || progress.unit.is_some()
                    || progress.summary.is_some();
                Operation {
                    phase: phase(operation.phase()),
                    status: operation_status(operation.status()),
                    title: operation.title().to_owned(),
                    detail: operation.detail().to_owned(),
                    progress: known.then_some(progress),
                    mutating: operation.owns_mutation(),
                    failure: operation.failure().map(str::to_owned),
                    job: operation.job().map(job),
                    environment: operation.environment().map(|setup| EnvironmentSetup {
                        part: environment_part(setup.running()),
                        parts: setup.planned().map(environment_part).collect(),
                    }),
                }
            }),
            active_job: snapshot.active_job().map(job),
            result: presentation::result_card(snapshot).map(|card| ResultCard {
                title: card.title.to_owned(),
                message: card.message.to_owned(),
                next: card.next.map(|next| match next {
                    ResultNextStep::TryAgain => ResultNext::TryAgain,
                    ResultNextStep::OpenDiagnostics => ResultNext::OpenDiagnostics,
                    ResultNextStep::StartOver => ResultNext::StartOver,
                }),
            }),
            stop_result: snapshot.stop_result().map(|result| {
                let (installation, interrupted_stage) = installation_state(result.installation());
                StopResult {
                    installation,
                    interrupted_stage,
                    needs_recovery: result.needs_recovery(),
                }
            }),
            game: match snapshot.game() {
                GameView::Idle => Game::Idle,
                GameView::Running => Game::Running,
            },
            game_notice: snapshot.game_notice().map(|notice| match notice {
                GameNoticeView::FirstD2glLaunch => GameNotice::FirstD2glLaunch,
            }),
            launch_preflight_passed: snapshot.launch_preflight_passed(),
            unresolved_journal: snapshot.unresolved_journal(),
            game_software_rendering: snapshot.game_software_rendering(),
            game_language: snapshot
                .game_language()
                .map(|language| language.code().to_owned()),
            environment_ready: snapshot.environment_ready(),
            downloads: {
                let downloads = snapshot.downloads();
                Downloads {
                    complete: downloads.complete,
                    installers: downloads.installers,
                    total_bytes: downloads.total_bytes,
                }
            },
            location: snapshot.location().map(|location| Location {
                path: location.path.display().to_string(),
                default: location.default,
                available: location.available,
            }),
            location_error: snapshot.location_error().map(str::to_owned),
            game_folder: snapshot.game_folder().map(|folder| GameFolder {
                path: folder.path.display().to_string(),
                available: folder.available,
            }),
            settings: Settings {
                renderer: match settings.renderer() {
                    RendererPreferenceView::Glide => Renderer::Glide,
                    RendererPreferenceView::DirectDraw => Renderer::DirectDraw,
                },
                direct_draw_renderer: match settings.direct_draw_renderer() {
                    DirectDrawRendererView::Pd2 => DirectDrawRenderer::Pd2,
                    DirectDrawRendererView::OpenGl => DirectDrawRenderer::OpenGl,
                    DirectDrawRendererView::Direct3d9 => DirectDrawRenderer::Direct3d9,
                    DirectDrawRendererView::Software => DirectDrawRenderer::Software,
                },
                skip_to_bnet: settings.skip_to_bnet(),
                sound_in_background: settings.sound_in_background(),
                update_checks: settings.update_checks(),
                install_source: match settings.install_source() {
                    InstallSourceView::Official => InstallSource::Official,
                    InstallSourceView::DirectDownload => InstallSource::DirectDownload,
                },
                custom_arguments: settings.custom_arguments().to_owned(),
                game_program: settings.game_program().to_owned(),
                launcher_program: settings.launcher_program().to_owned(),
                launcher_custom_arguments: settings.launcher_custom_arguments().to_owned(),
                persistence: match settings.persistence() {
                    SettingsPersistenceView::Loading => SettingsPersistence::Loading,
                    SettingsPersistenceView::Saved => SettingsPersistence::Saved,
                    SettingsPersistenceView::Failed(reason) => SettingsPersistence::Failed {
                        reason: reason.to_owned(),
                    },
                },
            },
            download_sources: snapshot
                .download_sources()
                .iter()
                .map(download_source)
                .collect(),
            report: snapshot.report().map(|report| match report {
                ReportView::Creating => Report::Creating,
                ReportView::Saved { path, opened } => Report::Saved {
                    path: path.display().to_string(),
                    opened: *opened,
                },
                ReportView::Failed(reason) => Report::Failed {
                    reason: (*reason).to_owned(),
                },
            }),
        }
    }
}

fn installation_state(status: InstallationStatus) -> (Installation, Option<Stage>) {
    match status {
        InstallationStatus::NeedsBaseGame => (Installation::NeedsBaseGame, None),
        InstallationStatus::NeedsExpansion => (Installation::NeedsExpansion, None),
        InstallationStatus::NeedsClient => (Installation::NeedsClient, None),
        InstallationStatus::NeedsRuntime => (Installation::NeedsRuntime, None),
        InstallationStatus::RepairRequired => (Installation::RepairRequired, None),
        InstallationStatus::Indeterminate => (Installation::Indeterminate, None),
        InstallationStatus::StageInterrupted(stage) => (
            Installation::StageInterrupted,
            Some(match stage {
                InterruptedStage::Prefix => Stage::Prefix,
                InterruptedStage::BaseGame => Stage::BaseGame,
                InterruptedStage::Expansion => Stage::Expansion,
            }),
        ),
        InstallationStatus::Ready => (Installation::Ready, None),
    }
}

fn finding(missing: Prerequisite) -> Finding {
    match missing {
        Prerequisite::DisplaySession => Finding::DisplaySession,
        Prerequisite::Fex => Finding::Fex,
        Prerequisite::FexRootFs => Finding::FexRootFs,
        Prerequisite::PageSize => Finding::PageSize,
        Prerequisite::WineCannotRun => Finding::WineCannotRun,
        Prerequisite::Ia32Execution => Finding::Ia32Execution,
        Prerequisite::NoExec => Finding::NoExec,
        Prerequisite::I386Loader => Finding::I386Loader,
        Prerequisite::Libraries => Finding::Libraries,
        Prerequisite::WineLibrary(WineLibrary::X11) => Finding::WineLibraryX11,
        Prerequisite::WineLibrary(WineLibrary::Xext) => Finding::WineLibraryXext,
        Prerequisite::WineLibrary(WineLibrary::FreeType) => Finding::WineLibraryFreeType,
        Prerequisite::WineLibrary(WineLibrary::Fontconfig) => Finding::WineLibraryFontconfig,
        Prerequisite::WineLibrary(WineLibrary::OpenGl) => Finding::WineLibraryOpenGl,
        Prerequisite::Libunwind => Finding::Libunwind,
        Prerequisite::XWayland => Finding::XWayland,
        Prerequisite::VulkanLoader => Finding::VulkanLoader,
        Prerequisite::VulkanDriver => Finding::VulkanDriver,
        Prerequisite::HardwareGraphics => Finding::HardwareGraphics,
        Prerequisite::LauncherBuild => Finding::LauncherBuild,
        Prerequisite::Architecture => Finding::Architecture,
        Prerequisite::HostInformation => Finding::HostInformation,
        Prerequisite::UncertifiedBuild => Finding::UncertifiedBuild,
    }
}

fn existing_problem(problem: ExistingProblemView) -> ExistingProblem {
    match problem {
        ExistingProblemView::NotFound => ExistingProblem::NotFound,
        ExistingProblemView::Unreadable => ExistingProblem::Unreadable,
        ExistingProblemView::NotDiablo => ExistingProblem::NotDiablo,
        ExistingProblemView::NoExpansion => ExistingProblem::NoExpansion,
        ExistingProblemView::MissingFile(file) => ExistingProblem::MissingFile {
            file: file.to_owned(),
        },
        ExistingProblemView::Links => ExistingProblem::Links,
        ExistingProblemView::InsideLauncher => ExistingProblem::InsideLauncher,
        ExistingProblemView::UnsupportedPath => ExistingProblem::UnsupportedPath,
    }
}

fn phase(phase: OperationPhase) -> Phase {
    match phase {
        OperationPhase::HostAudit => Phase::HostAudit,
        OperationPhase::RuntimeAcquisition => Phase::RuntimeAcquisition,
        OperationPhase::PrefixInitialization => Phase::PrefixInitialization,
        OperationPhase::BaseGameInstall => Phase::BaseGameInstall,
        OperationPhase::ExpansionInstall => Phase::ExpansionInstall,
        OperationPhase::BaseGameDownload => Phase::BaseGameDownload,
        OperationPhase::ExpansionDownload => Phase::ExpansionDownload,
        OperationPhase::ImportGame => Phase::ImportGame,
        OperationPhase::ClientDownload => Phase::ClientDownload,
        OperationPhase::Verification => Phase::Verification,
        OperationPhase::Promotion => Phase::Promotion,
        OperationPhase::Repair => Phase::Repair,
        OperationPhase::GameExecution => Phase::GameExecution,
        OperationPhase::OfficialLauncher => Phase::OfficialLauncher,
        OperationPhase::Reset => Phase::Reset,
        OperationPhase::Relocation => Phase::Relocation,
    }
}

fn operation_status(status: OperationStatusView) -> OperationStatus {
    match status {
        OperationStatusView::Queued => OperationStatus::Queued,
        OperationStatusView::Planning => OperationStatus::Planning,
        OperationStatusView::Running => OperationStatus::Running,
        OperationStatusView::SafeStopping => OperationStatus::SafeStopping,
        OperationStatusView::Verifying => OperationStatus::Verifying,
        OperationStatusView::Committing => OperationStatus::Committing,
        OperationStatusView::Succeeded => OperationStatus::Succeeded,
        OperationStatusView::Failed => OperationStatus::Failed,
        OperationStatusView::Indeterminate => OperationStatus::Indeterminate,
        OperationStatusView::Cancelled => OperationStatus::Cancelled,
    }
}

fn job(job: ActiveJobView) -> Job {
    match job {
        ActiveJobView::Download => Job::Download,
        ActiveJobView::InstallBaseGame => Job::InstallBaseGame,
        ActiveJobView::InstallProjectDiablo => Job::InstallProjectDiablo,
        ActiveJobView::Update => Job::Update,
        ActiveJobView::Repair => Job::Repair,
        ActiveJobView::OfficialLauncher => Job::OfficialLauncher,
        ActiveJobView::Reset => Job::Reset,
        ActiveJobView::Relocation => Job::Relocation,
        ActiveJobView::DiscardDownloads => Job::DiscardDownloads,
    }
}

fn environment_part(part: crate::operation::EnvironmentPart) -> EnvironmentPart {
    use crate::operation::EnvironmentPart as Work;
    match part {
        Work::Windows => EnvironmentPart::Windows,
        Work::WebComponents => EnvironmentPart::WebComponents,
        Work::Dotnet => EnvironmentPart::Dotnet,
        Work::LauncherSupport => EnvironmentPart::LauncherSupport,
    }
}

fn download_source(source: &DownloadSourceView) -> DownloadSource {
    DownloadSource {
        artifact: source.artifact.into(),
        effective_url: source.effective_url.clone(),
        badge: match source.badge {
            SourceBadgeView::BuiltInVerified => SourceBadge::BuiltInVerified,
            SourceBadgeView::CustomFromEnvironment => SourceBadge::CustomFromEnvironment,
            SourceBadgeView::Custom => SourceBadge::Custom,
        },
        error: source.error.map(str::to_owned),
        installed: source.installed.clone(),
    }
}

/// Why a frame could not be read or written.
#[derive(Debug)]
pub(crate) enum FrameError {
    Io(io::Error),
    /// A frame of this many bytes is over [`MAX_FRAME_BYTES`].
    TooLarge(usize),
    Encode(serde_json::Error),
}

impl fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::TooLarge(length) => write!(
                formatter,
                "a {length}-byte frame is over the {MAX_FRAME_BYTES}-byte limit"
            ),
            Self::Encode(error) => write!(formatter, "the message could not be encoded: {error}"),
        }
    }
}

impl From<io::Error> for FrameError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Read one frame's JSON. `None` is the end of the connection between
/// frames; a frame over the limit is refused before its body is read.
pub(crate) fn read_frame(reader: &mut impl Read) -> Result<Option<Vec<u8>>, FrameError> {
    let mut header = [0; 4];
    let mut filled = 0;
    while filled < header.len() {
        match reader.read(&mut header[filled..]) {
            Ok(0) if filled == 0 => return Ok(None),
            Ok(0) => return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into()),
            Ok(read) => filled += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
    let length = u32::from_be_bytes(header) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge(length));
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    Ok(Some(body))
}

/// Write `message` as one frame; one over the limit is never written.
pub(crate) fn write_frame(
    writer: &mut impl Write,
    message: &EngineMessage,
) -> Result<(), FrameError> {
    let body = serde_json::to_vec(message).map_err(FrameError::Encode)?;
    if body.len() > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge(body.len()));
    }
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(&body);
    writer.write_all(&frame)?;
    writer.flush()?;
    Ok(())
}

/// Reads that fail once `deadline` has passed.
struct DeadlineReader<'a> {
    stream: &'a UnixStream,
    deadline: Instant,
}

impl Read for DeadlineReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "no hello within 10 seconds",
            ));
        }
        self.stream.set_read_timeout(Some(remaining))?;
        let mut stream = self.stream;
        stream.read(buffer)
    }
}

/// Outgoing messages for the writer thread: command errors in order, and
/// only the newest snapshot not yet sent. A slow reader thus never holds up
/// the event pump, which keeps the service's bounded event channel drained.
#[derive(Default)]
struct Outbox {
    state: Mutex<OutboxState>,
    ready: Condvar,
}

#[derive(Default)]
struct OutboxState {
    errors: VecDeque<(u64, String)>,
    snapshot: Option<Box<Snapshot>>,
    closed: bool,
}

enum Outgoing {
    Error(u64, String),
    Snapshot(Box<Snapshot>),
}

impl Outbox {
    fn post_snapshot(&self, snapshot: Snapshot) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .snapshot = Some(Box::new(snapshot));
        self.ready.notify_one();
    }

    fn post_error(&self, seq: u64, reason: impl Into<String>) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .errors
            .push_back((seq, reason.into()));
        self.ready.notify_one();
    }

    /// Let the writer finish once everything posted is sent.
    fn close(&self) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .closed = true;
        self.ready.notify_one();
    }

    /// The next message to send; `None` once closed and empty.
    fn next(&self) -> Option<Outgoing> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some((seq, reason)) = state.errors.pop_front() {
                return Some(Outgoing::Error(seq, reason));
            }
            if let Some(snapshot) = state.snapshot.take() {
                return Some(Outgoing::Snapshot(snapshot));
            }
            if state.closed {
                return None;
            }
            state = self
                .ready
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

static PANICKED: AtomicBool = AtomicBool::new(false);

/// Run the Launcher engine on the Control channel that is standard input.
/// `start` builds and starts the service once a valid `hello` arrived; what
/// it returns keeps the service running until it is dropped. Returns the
/// process exit status.
pub(crate) fn run_engine(start: impl FnOnce(&HostFacts, ServiceEndpoint) -> Box<dyn Send>) -> u8 {
    let stream = match adopt_standard_input() {
        Ok(stream) => stream,
        Err(error) => {
            eprintln!(
                "pd2-launcher: --engine needs the Control channel on standard input: {error}"
            );
            return FAILURE_EXIT_STATUS;
        }
    };
    let (writer, channel) = match (stream.try_clone(), stream.try_clone()) {
        (Ok(writer), Ok(channel)) => (Arc::new(Mutex::new(writer)), channel),
        (Err(error), _) | (_, Err(error)) => {
            eprintln!("pd2-launcher: the Control channel could not be opened: {error}");
            return FAILURE_EXIT_STATUS;
        }
    };
    install_panic_hook(writer.clone(), channel);
    let Some(host_facts) = handshake(&stream, &writer) else {
        let _ = stream.shutdown(Shutdown::Both);
        return FAILURE_EXIT_STATUS;
    };

    let (presentation, service) = presentation::service_bridge(UiSnapshot::initial());
    let (initial, intents, events) = presentation.into_parts();
    let service_runtime = start(&host_facts, service);
    let outbox = Arc::new(Outbox::default());
    let taken = Arc::new(AtomicU64::new(0));
    let write_thread = {
        let outbox = outbox.clone();
        thread::Builder::new()
            .name("pd2-control-writer".to_owned())
            .spawn(move || write_messages(&writer, &outbox))
    };
    let pump_thread = {
        let outbox = outbox.clone();
        let intents = intents.clone();
        let taken = taken.clone();
        thread::Builder::new()
            .name("pd2-control-events".to_owned())
            .spawn(move || pump_events(events, initial, &intents, &outbox, &taken))
    };
    let status = match (&write_thread, &pump_thread) {
        (Ok(_), Ok(_)) => {
            // Like the desktop window: download from Blizzard unless a
            // choice the last Download was made for is restored.
            let _ = intents.try_send(Intent::UseDefaultInstallerSource);
            serve_commands(&stream, &intents, &outbox, &taken)
        }
        _ => {
            eprintln!("pd2-launcher: the Control channel's threads could not start");
            FAILURE_EXIT_STATUS
        }
    };

    // The app closed its end, or the channel failed: stop any running work
    // at a safe point, and wait for the service to finish before exiting so
    // nothing it started is left detached.
    let _ = intents.try_send(Intent::RequestSafeStop);
    drop(intents);
    drop(service_runtime);
    if let Ok(pump_thread) = pump_thread {
        let _ = pump_thread.join();
    }
    outbox.close();
    if let Ok(write_thread) = write_thread {
        let _ = write_thread.join();
    }
    if PANICKED.load(Ordering::SeqCst) {
        FAILURE_EXIT_STATUS
    } else {
        status
    }
}

/// Standard input as the Control channel's socket, without unsafe code: a
/// duplicate of its descriptor becomes the stream.
fn adopt_standard_input() -> io::Result<UnixStream> {
    let descriptor = io::stdin().as_fd().try_clone_to_owned()?;
    let status = rustix::fs::fstat(&descriptor)?;
    if rustix::fs::FileType::from_raw_mode(status.st_mode) != rustix::fs::FileType::Socket {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "standard input is not a socket",
        ));
    }
    Ok(UnixStream::from(descriptor))
}

/// A panic has no window to show it in: the default hook writes it to
/// standard error (the app's log of the engine), then the app is told and
/// the channel closed, so the engine stops safely and exits with a failure.
fn install_panic_hook(writer: Arc<Mutex<UnixStream>>, channel: UnixStream) {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default(info);
        PANICKED.store(true, Ordering::SeqCst);
        // A panic while a frame is being written leaves the lock held: the
        // message is skipped rather than interleaved or waited for.
        if let Ok(mut stream) = writer.try_lock() {
            let _ = write_frame(
                &mut *stream,
                &EngineMessage::CommandError {
                    seq: 0,
                    reason: "engine-failed".to_owned(),
                },
            );
        }
        let _ = channel.shutdown(Shutdown::Both);
    }));
}

/// Wait for the app's `hello` and answer it. `None` when the engine must
/// exit: no valid `hello` in time, or a protocol it does not speak.
fn handshake(stream: &UnixStream, writer: &Mutex<UnixStream>) -> Option<HostFacts> {
    let mut reader = DeadlineReader {
        stream,
        deadline: Instant::now() + HELLO_TIMEOUT,
    };
    let frame = match read_frame(&mut reader) {
        Ok(Some(frame)) => frame,
        Ok(None) => {
            eprintln!("pd2-launcher: the Control channel closed before hello");
            return None;
        }
        Err(error) => {
            eprintln!("pd2-launcher: no valid hello on the Control channel: {error}");
            return None;
        }
    };
    if let Err(error) = stream.set_read_timeout(None) {
        eprintln!("pd2-launcher: the Control channel could not be read: {error}");
        return None;
    }
    // The version alone first: a later protocol's hello may differ in
    // everything else.
    #[derive(Deserialize)]
    struct Greeting {
        #[serde(rename = "type")]
        kind: String,
        protocol: u32,
    }
    let send = |message: &EngineMessage| {
        let mut writer = writer.lock().unwrap_or_else(PoisonError::into_inner);
        write_frame(&mut *writer, message)
    };
    match serde_json::from_slice::<Greeting>(&frame) {
        Ok(greeting) if greeting.kind == "hello" && greeting.protocol != PROTOCOL => {
            eprintln!(
                "pd2-launcher: the Android app speaks protocol {}, this engine {PROTOCOL}",
                greeting.protocol
            );
            let _ = send(&EngineMessage::Refused {
                supported: vec![PROTOCOL],
            });
            return None;
        }
        Ok(greeting) if greeting.kind == "hello" => {}
        _ => {
            eprintln!("pd2-launcher: the Control channel's first message was not hello");
            return None;
        }
    }
    let Ok(AppMessage::Hello { host_facts, .. }) = serde_json::from_slice(&frame) else {
        eprintln!("pd2-launcher: the Android app's hello was malformed");
        return None;
    };
    match send(&EngineMessage::Welcome {
        protocol: PROTOCOL,
        launcher_version: env!("CARGO_PKG_VERSION").to_owned(),
        capabilities: CAPABILITIES.iter().map(|&name| name.to_owned()).collect(),
    }) {
        Ok(()) => Some(host_facts),
        Err(error) => {
            eprintln!("pd2-launcher: welcome could not be sent: {error}");
            None
        }
    }
}

/// Turn the service's events into snapshots for the writer until the
/// service stops, asking for a full snapshot when an event was missed. A
/// refusal becomes a `command_error` for `taken`, the newest command handed
/// to the service.
fn pump_events(
    events: Receiver<SequencedEvent>,
    initial: UiSnapshot,
    intents: &SyncSender<Intent>,
    outbox: &Outbox,
    taken: &AtomicU64,
) {
    let mut reducer = UiReducer::new(initial);
    let mut sent = Snapshot::project(reducer.snapshot());
    outbox.post_snapshot(sent.clone());
    let mut snapshot_requested = false;
    while let Ok(event) = events.recv() {
        if let Some(reason) = event.refusal() {
            outbox.post_error(taken.load(Ordering::SeqCst), reason);
        }
        let full_snapshot = event.is_full_snapshot();
        match reducer.reduce(event) {
            ReduceOutcome::Applied => {
                if full_snapshot {
                    snapshot_requested = false;
                }
                let snapshot = Snapshot::project(reducer.snapshot());
                if snapshot != sent {
                    sent = snapshot.clone();
                    outbox.post_snapshot(snapshot);
                }
            }
            ReduceOutcome::IgnoredStale => {}
            ReduceOutcome::SnapshotRequired if !snapshot_requested => {
                snapshot_requested = intents.try_send(Intent::RequestFullSnapshot).is_ok();
            }
            ReduceOutcome::SnapshotRequired => {}
        }
    }
}

/// Send what the outbox holds until it is closed and empty, or the app is
/// gone.
fn write_messages(writer: &Mutex<UnixStream>, outbox: &Outbox) {
    let mut seq = 0;
    while let Some(outgoing) = outbox.next() {
        let message = match outgoing {
            Outgoing::Error(seq, reason) => EngineMessage::CommandError {
                seq,
                reason: reason.to_owned(),
            },
            Outgoing::Snapshot(snapshot) => EngineMessage::Snapshot {
                seq: seq + 1,
                snapshot,
            },
        };
        let mut stream = writer.lock().unwrap_or_else(PoisonError::into_inner);
        match write_frame(&mut *stream, &message) {
            Ok(()) => {
                if let EngineMessage::Snapshot { seq: sent, .. } = message {
                    seq = sent;
                }
            }
            // Never sent; the next change sends a whole snapshot again.
            Err(FrameError::TooLarge(length)) => {
                eprintln!("pd2-launcher: a {length}-byte message was not sent (over the limit)");
            }
            Err(error) => {
                eprintln!("pd2-launcher: the Control channel could not be written: {error}");
                return;
            }
        }
    }
}

/// Read commands until the app closes its end; `taken` holds the `seq` of
/// the newest command handed to the service. Returns the exit status: 0 for
/// a closed channel, a failure for a broken one (which is closed).
fn serve_commands(
    stream: &UnixStream,
    intents: &SyncSender<Intent>,
    outbox: &Outbox,
    taken: &AtomicU64,
) -> u8 {
    let mut reader = stream;
    loop {
        let mut frame = match read_frame(&mut reader) {
            Ok(Some(frame)) => frame,
            Ok(None) => return 0,
            Err(error) => {
                eprintln!("pd2-launcher: the Control channel is closed: {error}");
                let _ = stream.shutdown(Shutdown::Both);
                return FAILURE_EXIT_STATUS;
            }
        };
        let command = decode_command(&frame);
        // The frame may hold CD keys.
        frame.zeroize();
        let refused = match command {
            Ok((seq, command)) => {
                let authorize = matches!(command, Command::Authorize { .. });
                let intent = command.into_intent();
                if authorize && intent.is_err() {
                    // New keys the check refused: keys an earlier authorize
                    // left waiting must not install either. A blocking send:
                    // a full queue must not lose it (the service empties
                    // the queue every tick).
                    let _ = intents.send(Intent::ForgetWaitingKeys);
                }
                match intent {
                    Ok(intent) => {
                        // Set first: the service may refuse it before try_send returns.
                        taken.store(seq, Ordering::SeqCst);
                        match intents.try_send(intent) {
                            Ok(()) => None,
                            Err(TrySendError::Full(_)) => Some((seq, "engine-busy")),
                            Err(TrySendError::Disconnected(_)) => {
                                Some((seq, "service-unavailable"))
                            }
                        }
                    }
                    Err(reason) => Some((seq, reason)),
                }
            }
            Err(refused) => Some(refused),
        };
        if let Some((seq, reason)) = refused {
            outbox.post_error(seq, reason);
        }
    }
}

/// A command and its `seq`, or the `command_error` it gets.
fn decode_command(frame: &[u8]) -> Result<(u64, Command), (u64, &'static str)> {
    #[derive(Deserialize)]
    struct Numbered {
        seq: u64,
    }
    match serde_json::from_slice::<AppMessage>(frame) {
        Ok(AppMessage::Command { seq, intent }) => Ok((seq, intent)),
        Ok(AppMessage::Hello { .. }) => Err((0, "unexpected-hello")),
        Err(_) => Err(match serde_json::from_slice::<Numbered>(frame) {
            Ok(numbered) => (numbered.seq, "unsupported-command"),
            Err(_) => (0, "malformed-message"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::operation::EnvironmentProgress;
    use crate::presentation::{
        DistroFamily, DownloadsView, ExistingInstallView, GameFolderView, LaunchSettingsView,
        LocationView, OperationView, StopResultView, SystemContractView,
    };

    /// Set to regenerate the golden fixtures instead of checking them.
    const REGENERATE: &str = "PD2_REGENERATE_CONTROL_CHANNEL_FIXTURES";

    /// How many commands the protocol has. A new command needs a sample:
    /// `expected_label` does not compile without it, and this count keeps
    /// the samples complete.
    const COMMANDS: usize = 30;

    /// Obviously fake keys: tests never hold real ones. These are the test
    /// builds' fake keys, which pass the check only there.
    const FAKE_D2_KEY: &str = crate::blizzard::TEST_BASE_KEY;
    const FAKE_LOD_KEY: &str = crate::blizzard::TEST_EXPANSION_KEY;

    fn key(value: &str) -> ProductKey {
        ProductKey(value.as_bytes().to_vec())
    }

    /// One command of each kind, every setting change and destination.
    fn commands() -> Vec<Command> {
        vec![
            Command::Navigate {
                destination: Destination::Operations,
            },
            Command::Navigate {
                destination: Destination::Settings,
            },
            Command::Navigate {
                destination: Destination::Diagnostics,
            },
            Command::SelectInstallerMedia {
                path: "/storage/emulated/0/Download/installers".to_owned(),
            },
            Command::UseBlizzardDownload,
            Command::EditStep { step: Step::Setup },
            Command::CloseEditor,
            Command::StopAndChange {
                step: Step::InstallD2,
            },
            Command::DismissStopResult,
            Command::DismissResult,
            Command::CheckAgain,
            Command::SaveDownloadSource {
                artifact: Artifact::Wine,
                url: "https://mirror.example/wine.tar.xz".to_owned(),
            },
            Command::ResetDownloadSource {
                artifact: Artifact::Wine,
            },
            Command::Authorize {
                d2_key: key(FAKE_D2_KEY),
                lod_key: key(FAKE_LOD_KEY),
            },
            Command::ContinueInstallation,
            Command::DownloadAll,
            Command::ImportExistingInstall {
                mode: ImportMode::Copy,
            },
            Command::InstallProjectDiablo,
            Command::SelectExistingInstall {
                path: "/storage/emulated/0/Diablo II".to_owned(),
            },
            Command::UseDefaultInstallerSource,
            Command::RequestSafeStop,
            Command::ApplyUpdate,
            Command::LaunchGame {
                graphics_driver: GraphicsDriver::Compatibility,
                graphics_profile: Some(GraphicsProfile::Performance),
                strict_memory_order: true,
                detailed_logging: true,
            },
            Command::OpenOfficialLauncher {
                strict_memory_order: true,
                detailed_logging: true,
            },
            Command::StartOver,
            Command::RetryLocation,
            Command::ChangeSettings {
                change: SettingChange::Renderer {
                    renderer: Renderer::DirectDraw,
                },
            },
            Command::ChangeSettings {
                change: SettingChange::DirectDrawRenderer {
                    renderer: DirectDrawRenderer::OpenGl,
                },
            },
            Command::ChangeSettings {
                change: SettingChange::SkipToBnet { enabled: false },
            },
            Command::ChangeSettings {
                change: SettingChange::SoundInBackground { enabled: true },
            },
            Command::ChangeSettings {
                change: SettingChange::UpdateChecks { enabled: false },
            },
            Command::ChangeSettings {
                change: SettingChange::InstallSource {
                    source: InstallSource::DirectDownload,
                },
            },
            Command::ChangeSettings {
                change: SettingChange::CustomArguments {
                    arguments: "-direct".to_owned(),
                },
            },
            Command::ChangeSettings {
                change: SettingChange::GameProgram {
                    path: "ZYLPD2/Game.exe".to_owned(),
                },
            },
            Command::ChangeSettings {
                change: SettingChange::LauncherProgram {
                    path: "ProjectD2/PD2Launcher.exe".to_owned(),
                },
            },
            Command::ChangeSettings {
                change: SettingChange::LauncherCustomArguments {
                    arguments: "-lang zh".to_owned(),
                },
            },
            Command::ExportDiagnostics,
            Command::RevealReport,
            Command::DismissReport,
            Command::RequestFullSnapshot,
            Command::DiscardDownloads,
        ]
    }

    /// The Launcher action each command must become.
    fn expected_label(command: &Command) -> &'static str {
        match command {
            Command::Navigate { destination } => match destination {
                Destination::Operations => "Open Operations Deck",
                Destination::Settings => "Open Settings",
                Destination::Diagnostics => "Open Diagnostics",
            },
            Command::SelectInstallerMedia { .. } => "Select D2 installer folder",
            Command::UseBlizzardDownload => "Download D2 installers from Blizzard",
            Command::EditStep { .. } => "Change a setup step",
            Command::CloseEditor => "Back to current step",
            Command::StopAndChange { .. } => "Stop the installation to change a step",
            Command::DismissStopResult => "Dismiss the stop result",
            Command::DismissResult => "Dismiss the result",
            Command::CheckAgain => "Check this computer again",
            Command::SaveDownloadSource { .. } => "Save custom download source",
            Command::ResetDownloadSource { .. } => "Reset download source to built-in",
            Command::Authorize { .. } => "Authorize and start Managed installation",
            Command::ContinueInstallation => "Continue Managed installation",
            Command::DownloadAll => "Download everything",
            Command::DiscardDownloads => "Remove the downloads for an import",
            Command::ImportExistingInstall { .. } => "Import the existing Diablo II",
            Command::InstallProjectDiablo => "Install Project Diablo 2",
            Command::SelectExistingInstall { .. } => "Choose an existing Diablo II folder",
            Command::UseDefaultInstallerSource => "Use the default installer source",
            Command::RequestSafeStop => "Request safe stop",
            Command::ApplyUpdate => "Apply Available update",
            Command::LaunchGame { .. } => "Launch Game client",
            Command::OpenOfficialLauncher { .. } => "Open the unverified official PD2 launcher",
            Command::StartOver => "Start over",
            Command::RetryLocation => "Check the install location again",
            Command::ChangeSettings { change } => match change {
                SettingChange::Renderer { .. } => "Change next-launch renderer",
                SettingChange::DirectDrawRenderer { .. } => "Change how DirectDraw draws",
                SettingChange::SkipToBnet { .. } => "Change next-launch online shortcut",
                SettingChange::SoundInBackground { .. } => "Change next-launch background sound",
                SettingChange::UpdateChecks { .. } => "Change Project Diablo 2's update checks",
                SettingChange::InstallSource { .. } => {
                    "Change where Blizzard's installers come from"
                }
                SettingChange::CustomArguments { .. } => "Change next-launch custom start options",
                SettingChange::GameProgram { .. } => "Change the program Play starts",
                SettingChange::LauncherProgram { .. } => {
                    "Change the program the official PD2 launcher button opens"
                }
                SettingChange::LauncherCustomArguments { .. } => {
                    "Change the official PD2 launcher's custom start options"
                }
            },
            Command::ExportDiagnostics => "Save a redacted report",
            Command::RevealReport => "Open the report's folder",
            Command::DismissReport => "Dismiss the saved report",
            Command::RequestFullSnapshot => "Request full UI snapshot",
        }
    }

    fn tag(value: &impl Serialize) -> String {
        serde_json::to_value(value).unwrap()["type"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    #[test]
    fn every_command_maps_to_exactly_one_intent_and_none_to_a_disabled_one() {
        let commands = commands();
        let mut tags = commands.iter().map(tag).collect::<Vec<_>>();
        tags.dedup();
        assert_eq!(tags.len(), COMMANDS, "one sample per command: {tags:?}");

        let mut labels = Vec::new();
        for command in commands {
            let expected = expected_label(&command);
            let intent = command
                .into_intent()
                .unwrap_or_else(|reason| panic!("{expected} refused: {reason}"));
            assert_eq!(intent.history_label(), expected);
            assert!(!labels.contains(&expected), "{expected} twice");
            labels.push(expected);
        }
        for disabled in [
            "Open official PD2 launcher",
            "Choose the install location",
            "Use a fresh install location",
            "Remove the old installation",
            "Set where FEX is",
            "Choose which FEX to use",
            "Change official launcher software rendering",
            "Change the app menu entry",
        ] {
            assert!(!labels.contains(&disabled), "{disabled} has a command");
        }
    }

    #[test]
    fn commands_the_launcher_cannot_take_are_refused_with_a_reason() {
        let refused = |command: Command| command.into_intent().err();
        assert_eq!(
            refused(Command::Authorize {
                d2_key: key("not a key"),
                lod_key: key(FAKE_LOD_KEY),
            }),
            Some("invalid-product-key")
        );
        assert_eq!(
            refused(Command::Authorize {
                d2_key: key(FAKE_D2_KEY),
                lod_key: key(""),
            }),
            Some("invalid-product-key")
        );
        // Of the right characters, but not the installers' form.
        for (d2, lod, reason) in [
            ("BCDF-GHJK-MNPR-TVWX", FAKE_LOD_KEY, "d2-key-16-characters"),
            (
                "ABCDGHJKMNPRTVWXYZ24678922",
                FAKE_LOD_KEY,
                "d2-key-invalid-character",
            ),
            (FAKE_D2_KEY, "CDFGHJK", "lod-key-wrong-length"),
            // Of the form, but no key of the product.
            (
                "2222-2222-2222-2222-2222-2222-22",
                FAKE_LOD_KEY,
                "d2-key-not-valid",
            ),
            (FAKE_D2_KEY, FAKE_D2_KEY, "lod-key-not-valid"),
        ] {
            assert_eq!(
                refused(Command::Authorize {
                    d2_key: key(d2),
                    lod_key: key(lod),
                }),
                Some(reason)
            );
        }
        assert_eq!(
            refused(Command::SelectInstallerMedia {
                path: "Download".to_owned()
            }),
            Some("relative-path")
        );
        let custom = |arguments: &str| {
            Command::ChangeSettings {
                change: SettingChange::CustomArguments {
                    arguments: arguments.to_owned(),
                },
            }
            .into_intent()
        };
        assert_eq!(
            custom(&"a".repeat(201)).err(),
            Some("custom-arguments-too-long")
        );
        assert_eq!(
            custom("-direct \"C:\\x\"").err(),
            Some("custom-arguments-invalid-character")
        );
        assert_eq!(
            custom("-direct\t-txt\u{7}").err(),
            Some("custom-arguments-invalid-character")
        );
        assert!(matches!(
            custom("  -direct ").ok(),
            Some(Intent::ChangeSettings(SettingsChange::CustomArguments(arguments)))
                if arguments == "-direct"
        ));
        let program = |path: &str| {
            Command::ChangeSettings {
                change: SettingChange::GameProgram {
                    path: path.to_owned(),
                },
            }
            .into_intent()
        };
        assert_eq!(program("../Game.exe").err(), Some("program-path-parent"));
        assert_eq!(program("C:\\Game.exe").err(), Some("program-path-absolute"));
        assert_eq!(program("ZYLPD2/Game").err(), Some("program-path-not-exe"));
        assert!(matches!(
            program("ZYLPD2\\Game.exe").ok(),
            Some(Intent::ChangeSettings(SettingsChange::GameProgram(path)))
                if path == "ZYLPD2/Game.exe"
        ));
        assert!(matches!(
            Command::ChangeSettings {
                change: SettingChange::LauncherProgram {
                    path: "ZYLPD2/Launcher.exe".to_owned(),
                },
            }
            .into_intent()
            .ok(),
            Some(Intent::ChangeSettings(SettingsChange::LauncherProgram(path)))
                if path == "ZYLPD2/Launcher.exe"
        ));
        assert_eq!(
            Command::ChangeSettings {
                change: SettingChange::LauncherCustomArguments {
                    arguments: "-lang \"zh".to_owned(),
                },
            }
            .into_intent()
            .err(),
            Some("custom-arguments-invalid-character")
        );
        assert_eq!(
            refused(Command::LaunchGame {
                graphics_driver: GraphicsDriver::Standard,
                graphics_profile: None,
                strict_memory_order: false,
                detailed_logging: false,
            }),
            None
        );
    }

    #[test]
    fn play_without_a_graphics_driver_or_profile_uses_the_standard_driver_and_no_profile() {
        let intent = |json: &str| {
            serde_json::from_str::<Command>(json)
                .expect("decodes")
                .into_intent()
                .expect("accepted")
        };
        assert!(matches!(
            intent(r#"{"type":"launch_game"}"#),
            Intent::LaunchGame(presentation::GraphicsDriver::Standard, None, false, false)
        ));
        assert!(matches!(
            intent(r#"{"type":"launch_game","display":null,"graphics_driver":"compatibility"}"#),
            Intent::LaunchGame(
                presentation::GraphicsDriver::Compatibility,
                None,
                false,
                false
            )
        ));
        assert!(matches!(
            intent(r#"{"type":"launch_game","display":null,"graphics_profile":null}"#),
            Intent::LaunchGame(presentation::GraphicsDriver::Standard, None, false, false)
        ));
        // The X screen's size, which the app sends, changes nothing, whatever
        // it is.
        for display in [
            r#"{"width":1280,"height":960}"#,
            r#"{"width":0,"height":0}"#,
        ] {
            assert!(matches!(
                intent(&format!(
                    r#"{{"type":"launch_game","display":{display},"graphics_driver":"standard","graphics_profile":"performance"}}"#
                )),
                Intent::LaunchGame(
                    presentation::GraphicsDriver::Standard,
                    Some(presentation::GraphicsProfile::Performance),
                    false,
                    false
                )
            ));
        }
        for (json, profile) in [
            ("default", presentation::GraphicsProfile::Default),
            ("performance", presentation::GraphicsProfile::Performance),
        ] {
            assert!(matches!(
                intent(&format!(
                    r#"{{"type":"launch_game","display":null,"graphics_profile":"{json}"}}"#
                )),
                Intent::LaunchGame(presentation::GraphicsDriver::Standard, Some(chosen), false, false)
                    if chosen == profile
            ));
        }
        assert!(
            serde_json::from_str::<Command>(
                r#"{"type":"launch_game","display":null,"graphics_profile":"ultra"}"#
            )
            .is_err()
        );
    }

    #[test]
    fn play_and_the_official_launcher_carry_detailed_logging_in_their_intent_off_when_absent() {
        let intent = |json: &str| {
            serde_json::from_str::<Command>(json)
                .expect("decodes")
                .into_intent()
                .expect("accepted")
        };
        assert!(matches!(
            intent(r#"{"type":"launch_game","display":null}"#),
            Intent::LaunchGame(_, _, false, false)
        ));
        assert!(matches!(
            intent(r#"{"type":"launch_game","display":null,"detailed_logging":true}"#),
            Intent::LaunchGame(_, _, false, true)
        ));
        assert!(matches!(
            intent(r#"{"type":"open_official_launcher"}"#),
            Intent::OpenOfficialLauncherConfirmed(false, false)
        ));
        assert!(matches!(
            intent(
                r#"{"type":"open_official_launcher","strict_memory_order":true,"detailed_logging":true}"#
            ),
            Intent::OpenOfficialLauncherConfirmed(true, true)
        ));
        assert!(
            serde_json::from_str::<Command>(
                r#"{"type":"open_official_launcher","detailed_logging":"yes"}"#
            )
            .is_err()
        );
    }

    #[test]
    fn play_asks_for_strict_memory_order_only_when_it_says_so() {
        let intent = |json: &str| {
            serde_json::from_str::<Command>(json)
                .expect("decodes")
                .into_intent()
                .expect("accepted")
        };
        assert!(matches!(
            intent(r#"{"type":"launch_game","display":null,"strict_memory_order":true}"#),
            Intent::LaunchGame(presentation::GraphicsDriver::Standard, None, true, false)
        ));
        assert!(matches!(
            intent(r#"{"type":"launch_game","display":null,"strict_memory_order":false}"#),
            Intent::LaunchGame(presentation::GraphicsDriver::Standard, None, false, false)
        ));
        assert!(
            serde_json::from_str::<Command>(
                r#"{"type":"launch_game","display":null,"strict_memory_order":"on"}"#
            )
            .is_err()
        );
    }

    #[test]
    fn the_official_launcher_asks_for_strict_memory_order_for_its_game_only_when_it_says_so() {
        let intent = |json: &str| {
            serde_json::from_str::<Command>(json)
                .expect("decodes")
                .into_intent()
                .expect("accepted")
        };
        assert!(matches!(
            intent(r#"{"type":"open_official_launcher"}"#),
            Intent::OpenOfficialLauncherConfirmed(false, false)
        ));
        assert!(matches!(
            intent(r#"{"type":"open_official_launcher","strict_memory_order":true}"#),
            Intent::OpenOfficialLauncherConfirmed(true, false)
        ));
        assert!(matches!(
            intent(r#"{"type":"open_official_launcher","strict_memory_order":false}"#),
            Intent::OpenOfficialLauncherConfirmed(false, false)
        ));
        assert!(
            serde_json::from_str::<Command>(
                r#"{"type":"open_official_launcher","strict_memory_order":"on"}"#
            )
            .is_err()
        );
    }

    fn shows_no_key(shown: &str) {
        assert!(
            !shown.contains("AAAA") && !shown.contains("HHHH"),
            "a key is shown: {shown}"
        );
    }

    /// The install source on the wire (protocol 6): snake_case in the
    /// command and in the snapshot (the settings file has `direct-download`).
    #[test]
    fn the_install_source_has_its_wire_form() {
        let frame = br#"{"type":"command","seq":4,"intent":{"type":"change_settings","change":{"setting":"install_source","source":"direct_download"}}}"#;
        let Ok((4, command)) = decode_command(frame) else {
            panic!("the change decodes");
        };
        assert_eq!(
            command,
            Command::ChangeSettings {
                change: SettingChange::InstallSource {
                    source: InstallSource::DirectDownload,
                },
            }
        );
        assert_eq!(
            serde_json::to_value(&command).unwrap(),
            serde_json::json!({
                "type": "change_settings",
                "change": {"setting": "install_source", "source": "direct_download"}
            })
        );
        assert!(
            decode_command(
                br#"{"type":"command","seq":5,"intent":{"type":"change_settings","change":{"setting":"install_source","source":"direct-download"}}}"#
            )
            .is_err(),
            "the settings file's spelling is not the wire's"
        );
        let snapshot = full_ui_snapshot().with_settings(
            crate::presentation::LaunchSettingsView::new(
                RendererPreferenceView::Glide,
                false,
                false,
            )
            .with_install_source(InstallSourceView::DirectDownload),
        );
        let message = serde_json::to_value(snapshot_message(&snapshot)).unwrap();
        assert_eq!(
            message["snapshot"]["settings"]["install_source"],
            serde_json::json!("direct_download")
        );
        let official = serde_json::to_value(snapshot_message(&full_ui_snapshot())).unwrap();
        assert_eq!(
            official["snapshot"]["settings"]["install_source"],
            serde_json::json!("official")
        );
    }

    /// `discard_downloads` on the wire (protocol 6), its refusal, and the
    /// job a snapshot names while it runs.
    #[test]
    fn discard_downloads_has_its_wire_form() {
        let Ok((12, command)) =
            decode_command(br#"{"type":"command","seq":12,"intent":{"type":"discard_downloads"}}"#)
        else {
            panic!("decoded");
        };
        assert_eq!(command, Command::DiscardDownloads);
        assert!(matches!(
            command.into_intent(),
            Ok(Intent::DiscardDownloads)
        ));
        assert_eq!(
            serde_json::to_string(&EngineMessage::CommandError {
                seq: 12,
                reason: "discard-downloads-not-permitted".to_owned(),
            })
            .unwrap(),
            r#"{"type":"command_error","seq":12,"reason":"discard-downloads-not-permitted"}"#
        );
        assert_eq!(
            serde_json::to_value(job(ActiveJobView::DiscardDownloads)).unwrap(),
            serde_json::json!("discard_downloads")
        );
        // The early start is gone.
        assert!(
            decode_command(
                br#"{"type":"command","seq":13,"intent":{"type":"start_direct_download"}}"#
            )
            .is_err()
        );
    }

    fn authorize_frame(d2_key: &str, lod_key: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "type": "command",
            "seq": 9,
            "intent": {"type": "authorize", "d2_key": d2_key, "lod_key": lod_key}
        }))
        .unwrap()
    }

    #[test]
    fn a_key_of_the_other_product_has_its_own_reason() {
        use crate::blizzard::{KeyField, KeyProblem};
        assert_eq!(
            key_reason(KeyField::BaseGame, KeyProblem::ExpansionKeyInBaseField),
            "d2-key-is-lod-key"
        );
        assert_eq!(
            key_reason(KeyField::Expansion, KeyProblem::BaseKeyInExpansionField),
            "lod-key-is-d2-key"
        );
        assert_eq!(
            key_reason(KeyField::Expansion, KeyProblem::WrongProduct),
            "lod-key-not-valid"
        );
    }

    #[test]
    fn product_keys_never_show_in_debug_output() {
        let command = Command::Authorize {
            d2_key: key(FAKE_D2_KEY),
            lod_key: key(FAKE_LOD_KEY),
        };
        let shown = format!("{command:?}");
        shows_no_key(&shown);
        assert!(shown.contains("[REDACTED]"));

        // Decoded from a frame, then mapped to the Launcher's authorization.
        let decoded = decode_command(&authorize_frame(FAKE_D2_KEY, FAKE_LOD_KEY));
        shows_no_key(&format!("{decoded:?}"));
        let Ok((9, command)) = decoded else {
            panic!("authorize decoded: {decoded:?}");
        };
        let Ok(Intent::AuthorizeAndInstall(input)) = command.into_intent() else {
            panic!("authorize maps to the Launcher's authorization");
        };
        shows_no_key(&format!("{input:?}"));
        // The keys arrive as sent, terms confirmed by the app's dialog.
        assert_eq!(
            input.into_parts(),
            (FAKE_D2_KEY.to_owned(), FAKE_LOD_KEY.to_owned(), true)
        );
    }

    #[test]
    fn product_keys_are_never_echoed_in_a_snapshot_or_command_error() {
        // A bad key: the error is a fixed code, never the value.
        let (seq, command) =
            decode_command(&authorize_frame("AAAA BBBB", FAKE_LOD_KEY)).expect("decoded");
        let Err(reason) = command.into_intent() else {
            panic!("a bad key is refused");
        };
        let error = serde_json::to_string(&EngineMessage::CommandError {
            seq,
            reason: reason.to_owned(),
        })
        .unwrap();
        assert_eq!(
            error,
            r#"{"type":"command_error","seq":9,"reason":"invalid-product-key"}"#
        );

        // A good key: the snapshot says only that an authorization is held.
        let Ok((_, command)) = decode_command(&authorize_frame(FAKE_D2_KEY, FAKE_LOD_KEY)) else {
            panic!("authorize decoded");
        };
        assert!(command.into_intent().is_ok());
        let snapshot = full_ui_snapshot().with_authorization(AuthorizationView::Authorized);
        let message = serde_json::to_string(&snapshot_message(&snapshot)).unwrap();
        shows_no_key(&message);
        assert!(message.contains(r#""authorized":true"#));
    }

    #[test]
    fn a_frame_from_the_app_is_decoded_into_its_command_or_refusal() {
        let frame = |value: serde_json::Value| serde_json::to_vec(&value).unwrap();
        assert!(matches!(
            decode_command(&frame(serde_json::json!({
                "type": "command", "seq": 7, "intent": {"type": "check_again"}
            }))),
            Ok((7, Command::CheckAgain))
        ));
        assert!(matches!(
            decode_command(&frame(serde_json::json!({
                "type": "command", "seq": 8, "intent": {"type": "open_official_launcher"}
            }))),
            Ok((
                8,
                Command::OpenOfficialLauncher {
                    strict_memory_order: false,
                    detailed_logging: false
                }
            ))
        ));
        assert!(matches!(
            decode_command(&frame(serde_json::json!({
                "type": "command", "seq": 9, "intent": {"type": "choose_install_location"}
            }))),
            Err((9, "unsupported-command"))
        ));
        assert!(matches!(
            decode_command(b"not json"),
            Err((0, "malformed-message"))
        ));
        let hello = serde_json::to_vec(&hello()).unwrap();
        assert!(matches!(
            decode_command(&hello),
            Err((0, "unexpected-hello"))
        ));
    }

    fn host_facts() -> HostFacts {
        HostFacts {
            android_release: "13".to_owned(),
            sdk: 33,
            device_model: "Retroid Pocket Nova".to_owned(),
            gpu_name: "Adreno (TM) 740".to_owned(),
            chip_id: 0x4305_0a01,
            kernel_release: "5.15.123-android13-8".to_owned(),
            page_size: 4096,
            data_dir: "/data/data/com.pd2launcher.app".to_owned(),
        }
    }

    fn hello() -> AppMessage {
        AppMessage::Hello {
            protocol: PROTOCOL,
            app_version: "1.0.0".to_owned(),
            host_facts: host_facts(),
        }
    }

    /// A snapshot with every optional part present.
    fn full_ui_snapshot() -> UiSnapshot {
        let operation = OperationView::mutating(
            OperationPhase::BaseGameDownload,
            OperationStatusView::Running,
            "Downloading Diablo II",
        )
        .with_detail("Blizzard's downloader is fetching the game files.")
        .with_typed_progress(1_250_000_000, Some(2_500_000_000), "bytes")
        .with_progress_summary("1.2 GB of 2.5 GB · 12.0 MB/s · 2 minutes left")
        .with_job(ActiveJobView::Download)
        .with_environment(EnvironmentProgress::new(
            crate::operation::EnvironmentPart::Dotnet,
            &crate::operation::EnvironmentPart::ALL,
        ));
        UiSnapshot::initial()
            .with_host(HostView::Supported)
            .with_installation(InstallationStatus::NeedsBaseGame)
            .with_environment_ready(true)
            .with_update(UpdateStatus::Available)
            .with_authorization(AuthorizationView::Authorized)
            .with_installer_media(InstallerMediaView::Existing(ExistingInstallView {
                path: PathBuf::from("/storage/emulated/0/Diablo II"),
                problem: Some(ExistingProblemView::NoExpansion),
                copy_bytes: 2_000_000_000,
                in_place_possible: false,
                project_diablo: false,
            }))
            .with_installer_media_rejection(Some("installer-media-invalid"))
            .with_operation(Some(operation))
            .with_active_job(Some(ActiveJobView::Download))
            .with_editing(Some(SetupStep::Setup))
            .with_stop_result(Some(StopResultView::new(
                InstallationStatus::StageInterrupted(InterruptedStage::BaseGame),
                false,
            )))
            .with_game_notice(Some(GameNoticeView::FirstD2glLaunch))
            .with_journal(true)
            .with_game_software_rendering(true)
            .with_game_language(Some(crate::game_language::GameLanguage::CHINESE))
            .with_downloads(DownloadsView {
                complete: false,
                installers: true,
                total_bytes: 2_500_000_000,
            })
            .with_location(Some(LocationView {
                path: PathBuf::from("/data/data/com.pd2launcher.app/files/launcher/game"),
                default: true,
                available: true,
                previous: None,
            }))
            .with_location_error(Some("location-unavailable"))
            .with_game_folder(Some(GameFolderView {
                path: PathBuf::from("/storage/emulated/0/Diablo II"),
                available: false,
            }))
            .with_settings(
                LaunchSettingsView::new(RendererPreferenceView::DirectDraw, false, true)
                    .with_direct_draw_renderer(DirectDrawRendererView::OpenGl)
                    .with_update_checks(false)
                    .failed("settings-unwritable"),
            )
            .with_download_sources(vec![DownloadSourceView {
                artifact: DownloadArtifact::Wine,
                effective_url: "https://mirror.example/wine.tar.xz".to_owned(),
                badge: SourceBadgeView::Custom,
                error: Some("source-invalid"),
                installed: Some("https://old-mirror.example/wine.tar.xz".to_owned()),
            }])
            .with_report(Some(ReportView::Saved {
                path: PathBuf::from(
                    "/data/data/com.pd2launcher.app/files/launcher/logs/report.txt",
                ),
                opened: Some(false),
            }))
            .with_system(SystemContractView {
                host: "Android".to_owned(),
                runtime: String::new(),
                prefix: String::new(),
                artifact_health: String::new(),
                missing: vec![Prerequisite::DisplaySession, Prerequisite::Fex],
                distro: DistroFamily::Unknown,
            })
    }

    fn snapshot_message(snapshot: &UiSnapshot) -> EngineMessage {
        EngineMessage::Snapshot {
            seq: 1,
            snapshot: Box::new(Snapshot::project(snapshot)),
        }
    }

    /// The full snapshot with the `unsupported_version` problem, which the
    /// engine no longer sends but the protocol keeps: its
    /// fixture stays as the Android app's decoder reads it.
    fn full_snapshot_message() -> EngineMessage {
        let mut snapshot = Snapshot::project(&full_ui_snapshot());
        let Installer::Existing { problem, .. } = &mut snapshot.installer else {
            unreachable!("the full snapshot has an existing installation");
        };
        *problem = Some(ExistingProblem::UnsupportedVersion {
            version: Some("1.13".to_owned()),
        });
        EngineMessage::Snapshot {
            seq: 1,
            snapshot: Box::new(snapshot),
        }
    }

    fn engine_messages() -> Vec<(&'static str, EngineMessage)> {
        vec![
            (
                "welcome",
                EngineMessage::Welcome {
                    protocol: PROTOCOL,
                    launcher_version: "1.0.0".to_owned(),
                    capabilities: CAPABILITIES.iter().map(|&name| name.to_owned()).collect(),
                },
            ),
            (
                "refused",
                EngineMessage::Refused {
                    supported: vec![PROTOCOL],
                },
            ),
            ("snapshot-initial", snapshot_message(&UiSnapshot::initial())),
            ("snapshot-full", full_snapshot_message()),
            (
                "command-error",
                EngineMessage::CommandError {
                    seq: 7,
                    reason: "engine-busy".to_owned(),
                },
            ),
            (
                "command-refused",
                EngineMessage::CommandError {
                    seq: 9,
                    reason: "install-not-permitted".to_owned(),
                },
            ),
        ]
    }

    fn app_messages() -> Vec<AppMessage> {
        std::iter::once(hello())
            .chain(
                commands()
                    .into_iter()
                    .enumerate()
                    .map(|(index, intent)| AppMessage::Command {
                        seq: index as u64 + 1,
                        intent,
                    }),
            )
            .collect()
    }

    #[test]
    fn every_message_survives_a_serde_round_trip() {
        for message in app_messages() {
            let json = serde_json::to_vec(&message).unwrap();
            let decoded: AppMessage = serde_json::from_slice(&json).unwrap();
            assert_eq!(decoded, message);
            assert_eq!(serde_json::to_vec(&decoded).unwrap(), json);
        }
        for (name, message) in engine_messages() {
            let json = serde_json::to_vec(&message).unwrap();
            let decoded: EngineMessage = serde_json::from_slice(&json).unwrap();
            assert_eq!(decoded, message, "{name}");
            assert_eq!(serde_json::to_vec(&decoded).unwrap(), json, "{name}");
        }
    }

    #[test]
    fn the_largest_snapshot_the_reducer_can_hold_fits_in_one_frame() {
        // Paths are bounded by PATH_MAX and URLs by the source limit; the
        // Launcher's own texts are short, and get 8 KiB each here.
        let path = PathBuf::from(format!("/{}", "p".repeat(4095)));
        let text = "t".repeat(8 * 1024);
        let code: &'static str = Box::leak(text.clone().into_boxed_str());
        let url = format!(
            "https://m.example/{}",
            "u".repeat(crate::sources::MAX_URL_BYTES)
        );
        let operation = OperationView::mutating(
            OperationPhase::BaseGameDownload,
            OperationStatusView::Failed,
            text.clone(),
        )
        .with_detail(text.clone())
        .with_typed_progress(u64::MAX, Some(u64::MAX), text.clone())
        .with_progress_summary(text.clone())
        .with_failure(code)
        .with_job(ActiveJobView::InstallProjectDiablo)
        .with_environment(EnvironmentProgress::new(
            crate::operation::EnvironmentPart::LauncherSupport,
            &crate::operation::EnvironmentPart::ALL,
        ));
        let missing = vec![
            Prerequisite::DisplaySession,
            Prerequisite::Fex,
            Prerequisite::FexRootFs,
            Prerequisite::PageSize,
            Prerequisite::WineCannotRun,
            Prerequisite::Ia32Execution,
            Prerequisite::NoExec,
            Prerequisite::I386Loader,
            Prerequisite::Libraries,
            Prerequisite::WineLibrary(WineLibrary::X11),
            Prerequisite::WineLibrary(WineLibrary::Xext),
            Prerequisite::WineLibrary(WineLibrary::FreeType),
            Prerequisite::WineLibrary(WineLibrary::Fontconfig),
            Prerequisite::WineLibrary(WineLibrary::OpenGl),
            Prerequisite::Libunwind,
            Prerequisite::XWayland,
            Prerequisite::VulkanLoader,
            Prerequisite::VulkanDriver,
            Prerequisite::HardwareGraphics,
            Prerequisite::LauncherBuild,
            Prerequisite::Architecture,
            Prerequisite::HostInformation,
            Prerequisite::UncertifiedBuild,
        ];
        let snapshot = full_ui_snapshot()
            .with_installer_media(InstallerMediaView::Existing(ExistingInstallView {
                path: path.clone(),
                problem: Some(ExistingProblemView::MissingFile(code)),
                copy_bytes: u64::MAX,
                in_place_possible: true,
                project_diablo: false,
            }))
            .with_installer_media_rejection(Some(code))
            .with_operation(Some(operation))
            .with_location(Some(LocationView {
                path: path.clone(),
                default: false,
                available: false,
                previous: Some(path.clone()),
            }))
            .with_location_error(Some(code))
            .with_game_folder(Some(GameFolderView {
                path: path.clone(),
                available: true,
            }))
            .with_settings(LaunchSettingsView::default().failed(code))
            .with_download_sources(
                DownloadArtifact::ALL
                    .into_iter()
                    .map(|artifact| DownloadSourceView {
                        artifact,
                        effective_url: url.clone(),
                        badge: SourceBadgeView::CustomFromEnvironment,
                        error: Some(code),
                        installed: Some(url.clone()),
                    })
                    .collect(),
            )
            .with_report(Some(ReportView::Saved {
                path,
                opened: Some(true),
            }))
            .with_system(SystemContractView {
                host: text.clone(),
                runtime: text.clone(),
                prefix: text.clone(),
                artifact_health: text,
                missing,
                distro: DistroFamily::Unknown,
            });
        let mut frame = Vec::new();
        write_frame(&mut frame, &snapshot_message(&snapshot)).unwrap();
        assert!(frame.len() < MAX_FRAME_BYTES / 4, "{} bytes", frame.len());
    }

    #[test]
    fn frames_over_the_limit_are_neither_read_nor_written() {
        let header = |length: usize| (length as u32).to_be_bytes().to_vec();
        let mut oversized = header(MAX_FRAME_BYTES + 1);
        oversized.extend_from_slice(b"{}");
        assert!(matches!(
            read_frame(&mut oversized.as_slice()),
            Err(FrameError::TooLarge(length)) if length == MAX_FRAME_BYTES + 1
        ));

        let mut largest = header(MAX_FRAME_BYTES);
        largest.resize(4 + MAX_FRAME_BYTES, b' ');
        assert_eq!(
            read_frame(&mut largest.as_slice())
                .unwrap()
                .map(|body| body.len()),
            Some(MAX_FRAME_BYTES)
        );

        let mut written = Vec::new();
        let too_long = EngineMessage::CommandError {
            seq: 1,
            reason: "r".repeat(MAX_FRAME_BYTES),
        };
        assert!(matches!(
            write_frame(&mut written, &too_long),
            Err(FrameError::TooLarge(_))
        ));
        assert!(written.is_empty());
    }

    #[test]
    fn frames_end_cleanly_only_between_frames() {
        let message = EngineMessage::Refused {
            supported: vec![PROTOCOL],
        };
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &message).unwrap();
        assert_eq!(
            u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize,
            bytes.len() - 4
        );
        let mut reader = bytes.as_slice();
        let body = read_frame(&mut reader).unwrap().unwrap();
        assert_eq!(
            serde_json::from_slice::<EngineMessage>(&body).unwrap(),
            message
        );
        assert!(read_frame(&mut reader).unwrap().is_none());
        assert!(matches!(
            read_frame(&mut &bytes[..2]),
            Err(FrameError::Io(error)) if error.kind() == io::ErrorKind::UnexpectedEof
        ));
        assert!(matches!(
            read_frame(&mut &bytes[..bytes.len() - 1]),
            Err(FrameError::Io(error)) if error.kind() == io::ErrorKind::UnexpectedEof
        ));
    }

    #[test]
    fn a_launcher_refusal_is_a_command_error_for_the_newest_command_taken() {
        use crate::presentation::{
            DomainEvent, EvidenceTone, EvidenceView, REQUEST_NOT_STARTED, SequencedEvent,
        };
        let (events, receiver) = std::sync::mpsc::sync_channel(8);
        let (intents, _intent_receiver) = std::sync::mpsc::sync_channel(8);
        let refusal = |code: &str| {
            vec![EvidenceView::new(
                REQUEST_NOT_STARTED,
                code,
                EvidenceTone::Advisory,
            )]
        };
        events
            .send(SequencedEvent::new(
                1,
                DomainEvent::EvidenceChanged(refusal("install-not-permitted")),
            ))
            .unwrap();
        // Evidence that is not a lone refusal is not one.
        let mut audit = refusal("start-over-not-permitted");
        audit.push(EvidenceView::new("Host", "ok", EvidenceTone::Verified));
        events
            .send(SequencedEvent::new(2, DomainEvent::EvidenceChanged(audit)))
            .unwrap();
        drop(events);
        let outbox = Outbox::default();
        pump_events(
            receiver,
            UiSnapshot::initial(),
            &intents,
            &outbox,
            &AtomicU64::new(7),
        );
        outbox.close();
        let mut errors = Vec::new();
        while let Some(outgoing) = outbox.next() {
            if let Outgoing::Error(seq, reason) = outgoing {
                errors.push((seq, reason));
            }
        }
        assert_eq!(errors, vec![(7, "install-not-permitted".to_owned())]);
    }

    #[test]
    fn a_command_the_channel_refuses_changes_nothing_for_the_next_one() {
        let write = |stream: &mut UnixStream, seq: u64, intent: serde_json::Value| {
            let body = serde_json::to_vec(&serde_json::json!({
                "type": "command", "seq": seq, "intent": intent
            }))
            .unwrap();
            stream
                .write_all(&u32::try_from(body.len()).unwrap().to_be_bytes())
                .unwrap();
            stream.write_all(&body).unwrap();
        };
        let (mut app, engine) = UnixStream::pair().unwrap();
        // The service's queue is full: the first Play is refused.
        let (intents, received) = std::sync::mpsc::sync_channel(1);
        intents.send(Intent::CheckAgain).unwrap();
        let outbox = Outbox::default();
        let taken = AtomicU64::new(0);
        let server = std::thread::scope(|scope| {
            let serving = scope.spawn(|| serve_commands(&engine, &intents, &outbox, &taken));
            write(
                &mut app,
                1,
                serde_json::json!({"type": "launch_game", "display": null, "detailed_logging": true}),
            );
            // Once that Play is refused, the queue has room again.
            let deadline = Instant::now() + Duration::from_secs(5);
            while outbox.state.lock().unwrap().errors.is_empty() {
                assert!(Instant::now() < deadline, "no refusal");
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(matches!(received.recv().unwrap(), Intent::CheckAgain));
            write(
                &mut app,
                2,
                serde_json::json!({"type": "launch_game", "display": null}),
            );
            let next = received.recv_timeout(Duration::from_secs(5)).unwrap();
            drop(app);
            (serving.join().unwrap(), next)
        });
        assert_eq!(server.0, 0);
        // The refused Play's Detailed logging reaches nothing: the next
        // Play's intent has its own value.
        assert!(matches!(server.1, Intent::LaunchGame(_, _, false, false)));
        outbox.close();
        assert!(matches!(
            outbox.next(),
            Some(Outgoing::Error(1, reason)) if reason == "engine-busy"
        ));
    }

    #[test]
    fn a_refused_authorize_tells_the_service_to_forget_waiting_keys() {
        let (mut app, engine) = UnixStream::pair().unwrap();
        let (intents, received) = std::sync::mpsc::sync_channel(4);
        let outbox = Outbox::default();
        let taken = AtomicU64::new(0);
        let status = std::thread::scope(|scope| {
            let serving = scope.spawn(|| serve_commands(&engine, &intents, &outbox, &taken));
            let body = serde_json::to_vec(&serde_json::json!({
                "type": "command", "seq": 5,
                "intent": {"type": "authorize", "d2_key": "2222-2222-2222-2222-2222-2222-22", "lod_key": FAKE_LOD_KEY}
            }))
            .unwrap();
            app.write_all(&u32::try_from(body.len()).unwrap().to_be_bytes())
                .unwrap();
            app.write_all(&body).unwrap();
            let forgotten = received.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(matches!(forgotten, Intent::ForgetWaitingKeys));
            drop(app);
            serving.join().unwrap()
        });
        assert_eq!(status, 0);
        outbox.close();
        assert!(matches!(
            outbox.next(),
            Some(Outgoing::Error(5, reason)) if reason == "d2-key-not-valid"
        ));
    }

    #[test]
    fn a_full_queue_does_not_lose_the_forget_of_waiting_keys() {
        let (mut app, engine) = UnixStream::pair().unwrap();
        // The service's queue is full.
        let (intents, received) = std::sync::mpsc::sync_channel(1);
        intents.send(Intent::CheckAgain).unwrap();
        let outbox = Outbox::default();
        let taken = AtomicU64::new(0);
        let status = std::thread::scope(|scope| {
            let serving = scope.spawn(|| serve_commands(&engine, &intents, &outbox, &taken));
            let body = serde_json::to_vec(&serde_json::json!({
                "type": "command", "seq": 6,
                "intent": {"type": "authorize", "d2_key": "BCDF-GHJK-MNPR-TVWX", "lod_key": FAKE_LOD_KEY}
            }))
            .unwrap();
            app.write_all(&u32::try_from(body.len()).unwrap().to_be_bytes())
                .unwrap();
            app.write_all(&body).unwrap();
            // Once the queue has room, the forget arrives.
            assert!(matches!(received.recv().unwrap(), Intent::CheckAgain));
            let forgotten = received.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(matches!(forgotten, Intent::ForgetWaitingKeys));
            drop(app);
            serving.join().unwrap()
        });
        assert_eq!(status, 0);
        outbox.close();
        assert!(matches!(
            outbox.next(),
            Some(Outgoing::Error(6, reason)) if reason == "d2-key-16-characters"
        ));
    }

    #[test]
    fn the_outbox_keeps_only_the_newest_unsent_snapshot() {
        let outbox = Outbox::default();
        let first = Snapshot::project(&UiSnapshot::initial());
        let newest = Snapshot::project(&full_ui_snapshot());
        outbox.post_snapshot(first);
        outbox.post_error(3, "engine-busy");
        outbox.post_snapshot(newest.clone());
        outbox.close();
        assert!(matches!(
            outbox.next(),
            Some(Outgoing::Error(3, reason)) if reason == "engine-busy"
        ));
        assert!(matches!(outbox.next(), Some(Outgoing::Snapshot(snapshot)) if *snapshot == newest));
        assert!(outbox.next().is_none());
    }

    #[test]
    fn the_engine_takes_nothing_from_its_environment() {
        assert!(
            crate::startup_credentials::download_source_environment(
                crate::startup_credentials::LaunchMode::Engine
            )
            .is_empty()
        );
    }

    /// The fixtures the Android app's decoder tests read: one file per
    /// message, and `commands.json` with every command. A tree without the
    /// app (the desktop's) has no fixtures to compare.
    #[test]
    fn golden_fixtures_match_the_protocol_types() {
        let app = Path::new(env!("CARGO_MANIFEST_DIR")).join("android/app");
        if !app.is_dir() {
            return;
        }
        let directory = app.join("src/test/resources/control-channel");
        let mut fixtures = vec![("hello".to_owned(), pretty(&hello()))];
        fixtures.extend(
            engine_messages()
                .into_iter()
                .map(|(name, message)| (name.to_owned(), pretty(&message))),
        );
        fixtures.push(("commands".to_owned(), pretty(&app_messages().split_off(1))));

        let regenerate = std::env::var_os(REGENERATE).is_some();
        if regenerate {
            std::fs::create_dir_all(&directory).unwrap();
        }
        for (name, contents) in fixtures {
            let path = directory.join(format!("{name}.json"));
            if regenerate {
                std::fs::write(&path, &contents).unwrap();
            } else {
                let found = std::fs::read_to_string(&path).unwrap_or_default();
                assert!(
                    found == contents,
                    "{} differs from the protocol types: run the tests with {REGENERATE}=1, and bump PROTOCOL if a message changed",
                    path.display()
                );
            }
        }
    }

    /// Pretty JSON with a final newline.
    fn pretty(value: &impl Serialize) -> String {
        serde_json::to_string_pretty(value).unwrap() + "\n"
    }
}
