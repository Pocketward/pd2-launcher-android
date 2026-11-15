//! Read-only host and managed-installation inspection.
//!
//! Audits derive current state from bounded, typed evidence. They never create,
//! repair, remove, or otherwise mutate the managed installation.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    ffi::OsString,
    fmt, fs,
    fs::File,
    io::{self, Read},
    mem::MaybeUninit,
    os::fd::{AsRawFd, OwnedFd},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

use command_fds::{CommandFdExt, FdMapping};
use md5::Md5;
use rustix::fs::{OFlags, RawDir};
use sha2::{Digest, Sha256};

use crate::{
    InstallationStatus, InterruptedStage, LaunchPreflight, RelativeGamePath, safe_fs::Directory,
    stage_marker::MarkerState,
};

const MAX_RECEIPT_BYTES: u64 = 1024;
const MAX_CUSTOM_RECEIPT_BYTES: u64 = 8 * 1024;
/// Larger than any `xinput1_4.dll` Wine puts in a prefix.
const MAX_XINPUT_DLL_BYTES: u64 = 4 * 1024 * 1024;
const CUSTOM_RUNTIME_BASE: &str = "runner/custom-wine";
const APP_RUNTIME_BASE: &str = "runner/wine-11.0-arm64";
const BUILTIN_GECKO_RECEIPT: &str = "prefix/.pd2launcher-gecko.sha256";
const CUSTOM_GECKO_RECEIPT: &str = "prefix/.pd2launcher-gecko-custom.json";
const BUILTIN_MONO_RECEIPT: &str = "prefix/.pd2launcher-mono.sha256";
const CUSTOM_MONO_RECEIPT: &str = "prefix/.pd2launcher-mono-custom.json";
const BUILTIN_DOTNET_DESKTOP_RECEIPT: &str = "prefix/.pd2launcher-dotnet-desktop.sha256";
const CUSTOM_DOTNET_DESKTOP_RECEIPT: &str = "prefix/.pd2launcher-dotnet-desktop-custom.json";
/// SHA-256 of the pinned Wine Mono 11.3.0 x86 MSI; its prefix receipt.
pub const WINE_MONO_MSI_SHA256: &str =
    "df2dfc1665c2511882e7cabd56eafd0c0a3d94e5a7e86f969277f6c189d418d3";
/// SHA-256 of the pinned .NET 6.0.36 Desktop Runtime x86 installer; its
/// prefix receipt.
pub const DOTNET_DESKTOP_INSTALLER_SHA256: &str =
    "4e77bd970df0a06528ee88d33e4a8c9fb85beedbdd7219b017083acf0c3aa39e";
const MAX_PROTECTED_FILES: usize = 16_384;
const MAX_PROTECTED_DEPTH: usize = 32;
const CHECK_COUNT: usize = 7;
const PREFLIGHT_PRELOADER_FD: i32 = 200;
const PREFLIGHT_WINE_PROOF_FD: i32 = 201;
const PREFLIGHT_PREFIX_FD: i32 = 202;
const PREFLIGHT_RUNTIME_BIN_FD: i32 = 203;
const MANAGED_GAME_BASE: &str = "prefix/drive_c/Program Files (x86)/Diablo II/";
/// The Managed game folder of the Android host, outside its prefix
/// (`game_folder::ANDROID_GAME_RELATIVE`).
const ANDROID_MANAGED_GAME_BASE: &str = "Diablo II/";

/// [`MANAGED_GAME_BASE`] or, on the Android host (`android`),
/// [`ANDROID_MANAGED_GAME_BASE`].
const fn managed_game_base(android: bool) -> &'static str {
    if android {
        ANDROID_MANAGED_GAME_BASE
    } else {
        MANAGED_GAME_BASE
    }
}

/// The game folder as the audit reads it: the Managed one below the root,
/// or a folder used in place. Every lookup stays beneath its
/// directory without following a link.
pub(crate) struct GameTree<'a> {
    directory: &'a Directory,
    /// The folder's path below `directory`, with a trailing slash; empty
    /// for a folder used in place.
    base: &'static str,
    /// On-disk top-level names, because Windows programs match names without
    /// regard to case: Blizzard's installers write `patch_d2.mpq`, for one.
    names: BTreeSet<String>,
}

/// The folder's top-level names; empty when it cannot be listed (every
/// lookup then uses the name as given).
fn top_level_names(folder: Option<Directory>) -> BTreeSet<String> {
    folder
        .and_then(|folder| folder.names().ok())
        .map(|names| {
            names
                .into_iter()
                .filter_map(|name| name.into_string().ok())
                .collect()
        })
        .unwrap_or_default()
}

impl<'a> GameTree<'a> {
    pub(crate) fn managed(root: &'a Directory) -> Self {
        Self::managed_on(root, crate::android_host())
    }

    /// [`Self::managed`] on the Android host (`android`) or the desktop.
    pub(crate) fn managed_on(root: &'a Directory, android: bool) -> Self {
        let base = managed_game_base(android);
        let folder = root
            .open_directory(Path::new(base.trim_end_matches('/')))
            .ok();
        Self {
            directory: root,
            base,
            names: top_level_names(folder),
        }
    }

    pub(crate) fn external(folder: &'a Directory) -> Self {
        Self {
            directory: folder,
            base: "",
            names: top_level_names(folder.try_clone().ok()),
        }
    }

    /// Whether the folder holds an entry other than its link to the
    /// player's characters (`Save`; a real `Save` folder counts), whatever
    /// the entry is: it need not be a file that a check names. Only that the
    /// entry is there; nothing is read.
    pub(crate) fn holds_an_entry(&self) -> bool {
        self.names.iter().any(|name| {
            name != "Save"
                || !rustix::fs::statat(
                    self.directory.as_file(),
                    Path::new(&self.path(name)),
                    rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
                )
                .is_ok_and(|stat| {
                    rustix::fs::FileType::from_raw_mode(stat.st_mode)
                        == rustix::fs::FileType::Symlink
                })
        })
    }

    /// The path of `file` (such as `Game.exe` or `ProjectD2/d2gl.json`)
    /// below this tree's directory.
    fn path(&self, file: &str) -> String {
        let (first, rest) = file
            .split_once('/')
            .map_or((file, None), |(first, rest)| (first, Some(rest)));
        // A name that exists exactly as given wins over another spelling;
        // among other spellings the first in sorted order wins, so the
        // choice never depends on the order the folder lists its names.
        let first = if self.names.contains(first) {
            first
        } else {
            self.names
                .iter()
                .find(|name| name.eq_ignore_ascii_case(first))
                .map_or(first, String::as_str)
        };
        match rest {
            Some(rest) => format!("{}{first}/{rest}", self.base),
            None => format!("{}{first}", self.base),
        }
    }
}

/// Run `inspect` over this root's game folder. `Err` when the recorded
/// game folder is unavailable or its record is not the Launcher's.
pub(crate) fn with_game_tree<T>(
    root: &Directory,
    inspect: impl FnOnce(&GameTree<'_>) -> T,
) -> Result<T, ()> {
    match crate::game_folder::GameFolder::read(root).map_err(|_| ())? {
        crate::game_folder::GameFolder::Managed => Ok(inspect(&GameTree::managed(root))),
        crate::game_folder::GameFolder::External(external) => {
            let folder = external.open().map_err(|_| ())?;
            Ok(inspect(&GameTree::external(&folder)))
        }
    }
}
const PROTECTED_FILES: [&str; 10] = [
    "D2.LNG",
    "BnetLog.txt",
    "ProjectDiablo.cfg",
    "ddraw.ini",
    "default.filter",
    "loot.filter",
    "UI.ini",
    "d2gl.yaml",
    "d2gl.json",
    "PD2Shared.dll",
];

/// Files the official PD2 launcher (`PD2Launcher.exe`) owns in ProjectD2: it
/// replaces itself and its updater, adds a Steam shim and a temporary copy of
/// itself, and writes its own logs and settings. They are never tampering
/// evidence and never Launcher update targets.
const OFFICIAL_LAUNCHER_FILES: [&str; 6] = [
    "PD2Launcher.exe",
    "UpdateUtility.exe",
    "SteamPD2.exe",
    "TempLauncher.exe",
    "launcher-update-debug.log",
    "update.log",
];
/// The official launcher's settings: only these two files in `AppData`.
/// Anything else there is unknown and preserved like any other user file.
const OFFICIAL_LAUNCHER_SETTINGS: [&str; 2] = [
    "AppData/launcherSettings.json",
    "AppData/launcherSettings.json.backup",
];
/// The official launcher the fallback job executes.
const OFFICIAL_LAUNCHER_PROGRAM: &str = "PD2Launcher.exe";
/// Trust-on-first-use record of the official launcher binary, at the
/// managed root: the MD5 of `PD2Launcher.exe` after the last session the
/// Launcher ran.
pub(crate) const OFFICIAL_LAUNCHER_RECEIPT: &str = ".pd2launcher-official-launcher.json";

/// Whether a ProjectD2-relative path belongs to the official PD2 launcher.
pub(crate) fn is_official_launcher_file(project_relative: &str) -> bool {
    OFFICIAL_LAUNCHER_FILES.contains(&project_relative)
        || OFFICIAL_LAUNCHER_SETTINGS.contains(&project_relative)
}

/// Whether the official launcher binary may run without asking. It updates
/// itself, so its bytes are accepted when they match the accepted manifest
/// or the copy recorded after the last session the Launcher ran.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OfficialLauncherTrust {
    Absent,
    Verified,
    /// Not in the manifest and never run by this Launcher.
    Unrecorded,
    /// Differs from the manifest and from the recorded copy.
    Changed,
    /// Could not be inspected safely.
    Indeterminate,
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct OfficialLauncherReceipt {
    schema: u32,
    launcher_md5: String,
}

/// MD5 (hex) of the official launcher binary, when present.
pub(crate) fn official_launcher_md5(root: &Directory) -> Result<Option<String>, ()> {
    with_game_tree(root, official_launcher_md5_in)?
}

fn official_launcher_md5_in(game: &GameTree<'_>) -> Result<Option<String>, ()> {
    let relative = relative_path(&game.path(&format!("ProjectD2/{OFFICIAL_LAUNCHER_PROGRAM}")));
    match safe_digest::<Md5, 16>(game.directory, &relative, None) {
        Ok(Some(digest)) => Ok(Some(
            digest.iter().map(|byte| format!("{byte:02x}")).collect(),
        )),
        Ok(None) => Ok(None),
        Err(_) => Err(()),
    }
}

/// Record the official launcher binary as trusted after a session.
pub(crate) fn record_official_launcher(root: &Directory) -> io::Result<()> {
    let Some(launcher_md5) = official_launcher_md5(root)
        .map_err(|()| io::Error::new(io::ErrorKind::InvalidData, "launcher unreadable"))?
    else {
        return Ok(());
    };
    let bytes = serde_json::to_vec(&OfficialLauncherReceipt {
        schema: 1,
        launcher_md5,
    })
    .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "receipt encode failed"))?;
    root.atomic_write(std::ffi::OsStr::new(OFFICIAL_LAUNCHER_RECEIPT), &bytes)
}

fn recorded_official_launcher(root: &Directory) -> Option<String> {
    let file = root.open_file(Path::new(OFFICIAL_LAUNCHER_RECEIPT)).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_RECEIPT_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    serde_json::from_slice::<OfficialLauncherReceipt>(&bytes)
        .ok()
        .filter(|receipt| receipt.schema == 1 && bytes.len() as u64 <= MAX_RECEIPT_BYTES)
        .map(|receipt| receipt.launcher_md5)
}

fn check_official_launcher(
    root: &Directory,
    game: &GameTree<'_>,
    manifest: Option<&AcceptedClientManifest>,
) -> OfficialLauncherTrust {
    let observed = match official_launcher_md5_in(game) {
        Ok(Some(observed)) => observed,
        Ok(None) => return OfficialLauncherTrust::Absent,
        Err(()) => return OfficialLauncherTrust::Indeterminate,
    };
    let listed = manifest
        .and_then(|manifest| {
            RelativeGamePath::parse(OFFICIAL_LAUNCHER_PROGRAM)
                .ok()
                .and_then(|path| manifest.digest_for(&path))
        })
        .map(|digest| {
            digest
                .bytes()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        });
    let recorded = recorded_official_launcher(root);
    if listed.as_deref() == Some(observed.as_str())
        || recorded.as_deref() == Some(observed.as_str())
    {
        OfficialLauncherTrust::Verified
    } else if listed.is_none() && recorded.is_none() {
        OfficialLauncherTrust::Unrecorded
    } else {
        OfficialLauncherTrust::Changed
    }
}

/// One independently reportable audit checkpoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuditCheck {
    Host,
    Runtime,
    Prefix,
    BaseGame,
    Expansion,
    Client,
    Transaction,
}

impl AuditCheck {
    const fn index(self) -> usize {
        match self {
            Self::Host => 0,
            Self::Runtime => 1,
            Self::Prefix => 2,
            Self::BaseGame => 3,
            Self::Expansion => 4,
            Self::Client => 5,
            Self::Transaction => 6,
        }
    }
}

const FILESYSTEM_CHECKS: [AuditCheck; 5] = [
    AuditCheck::Runtime,
    AuditCheck::Prefix,
    AuditCheck::BaseGame,
    AuditCheck::Expansion,
    AuditCheck::Client,
];

/// Quality of the evidence observed for one checkpoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckStatus {
    Satisfied,
    Missing,
    Invalid,
    Indeterminate,
}

/// Stable, non-sensitive reason attached to a failed check.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FindingReason {
    Missing,
    UnexpectedType,
    UnsafePath,
    Unreadable,
    NotExecutable,
    SizeMismatch,
    DigestMismatch,
    ProcessActive,
    AuditLeaseUnavailable,
    RuntimePreflightFailed,
    RuntimePreflightUnavailable,
    HostUnavailable,
    ArchitectureUnsupported,
    DisplaySessionUnavailable,
    FexUnavailable,
    PageSizeUnsupported,
    FexRootFsUnavailable,
    GuestExecutionUnavailable,
    Ia32ExecutionUnavailable,
    GuestExecutionDenied,
    I386LoaderUnavailable,
    MultilibUnavailable,
    LibunwindUnavailable,
    /// A 64-bit library the WoW64 runtime loads was not located.
    WineLibraryUnavailable(WineLibrary),
    X11DisplayUnavailable,
    HostRowUncertified,
    VulkanLoaderUnavailable,
    VulkanDriverUnavailable,
    HardwareGraphicsUnavailable,
    LauncherArtifactUnverified,
    /// A component installed from a custom, unverified source while its
    /// configured download source is the built-in one.
    CustomSourceNotConfigured,
    ManifestUnavailable,
    RecoverableTransaction,
    UnsafeTransaction,
    /// The official launcher binary matches neither the accepted manifest
    /// nor the copy recorded after its last session.
    OfficialLauncherChanged,
    /// The game folder used in place is unavailable, or its record is not
    /// the Launcher's.
    GameFolderUnavailable,
}

/// A redacted diagnostic. Paths, when present, are managed-root relative.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuditFinding {
    check: AuditCheck,
    reason: FindingReason,
    path: Option<RelativeGamePath>,
}

impl AuditFinding {
    pub fn check(&self) -> AuditCheck {
        self.check
    }

    pub fn reason(&self) -> FindingReason {
        self.reason
    }

    pub fn path(&self) -> Option<&RelativeGamePath> {
        self.path.as_ref()
    }
}

/// Display path reported by the host adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisplaySession {
    Wayland,
    X11,
    Other,
    Unknown,
}

/// Vulkan device and driver selected by GPUI for the running window.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphicsExecutionEvidence {
    is_software_emulated: bool,
    device_name: String,
    driver_name: String,
    driver_info: String,
}

impl GraphicsExecutionEvidence {
    pub fn new(
        is_software_emulated: bool,
        device_name: impl Into<String>,
        driver_name: impl Into<String>,
        driver_info: impl Into<String>,
    ) -> Self {
        Self {
            is_software_emulated,
            device_name: device_name.into(),
            driver_name: driver_name.into(),
            driver_info: driver_info.into(),
        }
    }

    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    pub fn driver_name(&self) -> &str {
        &self.driver_name
    }

    pub fn driver_info(&self) -> &str {
        &self.driver_info
    }

    pub fn is_software_emulated(&self) -> bool {
        self.is_software_emulated
    }
}

/// CPU row selected by the Launcher binary's compile target.
///
/// The compile target, not a runtime string supplied by the environment,
/// decides which guest executor composition selects. `Other` is retained so
/// policy can report an explicit unsupported-architecture finding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostPlatform {
    Aarch64,
    X86_64,
    Other(String),
}

impl HostPlatform {
    /// Platform of the running Launcher binary.
    pub fn current() -> Self {
        Self::from_architecture(std::env::consts::ARCH)
    }

    pub fn from_architecture(architecture: &str) -> Self {
        match architecture {
            "aarch64" | "arm64" => Self::Aarch64,
            "x86_64" => Self::X86_64,
            other => Self::Other(other.to_owned()),
        }
    }
}

/// Host capability evidence for executing the pinned x86 Wine guest.
///
/// Each variant belongs to one host row: AArch64 translates the guest through
/// FEX, while x86-64 executes the same pinned 32-bit/64-bit Wine directly and
/// therefore depends on host multilib support.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GuestExecutionEvidence {
    Translated {
        interpreter_present: bool,
        page_size_4k: bool,
        /// FEX ran an x86-64 program from its RootFS. A FEX installed
        /// without one (before `FEXRootFSFetcher`) runs no guest at all.
        rootfs_ready: bool,
        /// FEX or its RootFS comes with the Download step: the player chose
        /// the built-in FEX and it is not installed yet.
        pending: bool,
    },
    Native {
        /// Kernel `ia32_emulation=` state. Only an explicit disable is a
        /// static finding; `Unknown` defers to the runtime preflight, which
        /// remains authoritative.
        ia32_emulation: Ia32Emulation,
        i386_loader_present: bool,
        /// Required 32-bit sonames (or alternatives) that were not located.
        missing_i386_libraries: Vec<String>,
        /// 64-bit host libraries the WoW64 runtime and the game load that
        /// were not located.
        missing_wine_libraries: Vec<WineLibrary>,
        libunwind_present: bool,
        x11_display_available: bool,
    },
    /// The Android host: the app's ARM64 Wine runs every guest directly,
    /// and its x86 code runs in Box64's `wowbox64.dll`. The runtime
    /// preflight is the evidence that it runs.
    AppProvided {
        /// The kernel's memory pages are 4 KB, which Wine and Box64 need.
        page_size_4k: bool,
    },
    Unavailable,
}

/// A 64-bit host library the WoW64 Wine runtime loads for the installers
/// and the game. Wine loads each one only when it needs it, so Wine itself
/// starts without them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WineLibrary {
    /// Loaded by `winex11.drv`: without it no window can be created.
    X11,
    /// Linked by `winex11.drv` beside libX11, with the same effect.
    Xext,
    /// Wine cannot use TrueType fonts without it, and the Diablo II
    /// installer stalls.
    FreeType,
    /// Without it the prefix is never created: `wineboot` does not finish.
    Fontconfig,
    /// OpenGL through EGL or GLX: without both, no OpenGL context and the
    /// game cannot draw.
    OpenGl,
}

impl WineLibrary {
    pub const ALL: [Self; 5] = [
        Self::X11,
        Self::Xext,
        Self::FreeType,
        Self::Fontconfig,
        Self::OpenGl,
    ];

    /// Any one of these 64-bit sonames provides the library.
    pub const fn sonames(self) -> &'static [&'static str] {
        match self {
            Self::X11 => &["libX11.so.6"],
            Self::Xext => &["libXext.so.6"],
            Self::FreeType => &["libfreetype.so.6"],
            Self::Fontconfig => &["libfontconfig.so.1"],
            // Wine 11 draws through EGL and falls back to GLX (libGL).
            Self::OpenGl => &["libEGL.so.1", "libGL.so.1"],
        }
    }
}

/// IA-32 emulation state read from the kernel command line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Ia32Emulation {
    /// Explicitly enabled, or not overridden (the kernel's built-in default).
    NotDisabled,
    /// `ia32_emulation=` parses (as the kernel's `kstrtobool`) to false.
    Disabled,
    /// The kernel command line could not be read.
    Unknown,
}

/// Read-only facts supplied by a host adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostEvidence {
    pub os_id: String,
    /// Optional in os-release(5); rolling distributions may omit it.
    pub os_version: String,
    pub os_variant: Option<String>,
    /// Compile-target architecture of the running Launcher binary.
    pub architecture: String,
    /// Kernel-reported machine, recorded for diagnostics only.
    pub kernel_machine: String,
    pub kernel_release: String,
    pub display_session: DisplaySession,
    pub guest_execution: GuestExecutionEvidence,
    pub fex_package: Option<String>,
    pub vulkan_loader: bool,
    pub vulkan_driver: bool,
    pub graphics_execution: Option<GraphicsExecutionEvidence>,
    pub runtime_execution_preflight: RuntimeExecutionEvidence,
}

/// Outcome of attempting the bounded guest Wine version check.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeExecutionEvidence {
    Passed,
    Failed,
    Unavailable,
    /// The host refused to execute the verified guest for a classified,
    /// host-side reason; the Managed runtime itself is not implicated.
    Blocked(GuestExecutionBlocker),
}

/// Classified host-side reason a direct guest execution could not start.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GuestExecutionBlocker {
    /// `ENOEXEC` for the i386 preloader: the kernel has IA-32 disabled.
    Ia32ExecutionDisabled,
    /// `EACCES`, for example a `noexec` mount below the Managed root.
    ExecutionDenied,
    /// The dynamic loader or a required shared library is absent.
    SharedLibrariesMissing,
}

/// Identity of the currently running Launcher artifact.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LauncherArtifactEvidence {
    /// The standalone executable: the launcher as one file, run as is.
    NativeAarch64StandaloneBinary,
    NativeX86_64StandaloneBinary,
    #[default]
    Unverified,
}

/// Release-support classification for the current host evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostSupport {
    Supported,
    Unsupported,
    Indeterminate,
}

/// Boundary used by the auditor to locate host facts without embedding host
/// process execution in policy.
pub trait HostProbe {
    fn probe(&self, runtime: &RuntimePreflightRequest) -> io::Result<HostEvidence>;
}

/// Exact managed paths used for the read-only guest Wine execution preflight.
#[derive(Debug)]
pub struct RuntimePreflightRequest {
    wine: PathBuf,
    wine_preloader: PathBuf,
    prefix: PathBuf,
    loader_takes_wine_path: bool,
    expected_version: Option<&'static str>,
    execution_allowed: bool,
    handles: Option<RuntimePreflightHandles>,
}

impl RuntimePreflightRequest {
    pub fn new(wine: PathBuf, wine_preloader: PathBuf, prefix: PathBuf) -> Self {
        Self {
            wine,
            wine_preloader,
            prefix,
            loader_takes_wine_path: true,
            expected_version: Some(crate::runtime::current().version),
            execution_allowed: false,
            handles: None,
        }
    }

    /// Require `version` from `wine --version` instead of the row's pinned
    /// runtime's: the installation runs the row's earlier pinned runtime.
    pub fn with_wine_version(mut self, version: &'static str) -> Self {
        self.expected_version = Some(version);
        self
    }

    /// Accept any Wine version string: the runtime came from a custom,
    /// unverified source and has no pinned `wine --version` output.
    pub fn with_any_wine_version(mut self) -> Self {
        self.expected_version = None;
        self
    }

    /// Exact `wine --version` output required, or `None` for any version.
    pub fn expected_version(&self) -> Option<&str> {
        self.expected_version
    }

    /// Whether the executed loader (`wine_preloader`) takes the `wine` path
    /// as its first argument. False when the runtime's loader is `wine`.
    pub fn with_loader_takes_wine_path(mut self, takes_wine_path: bool) -> Self {
        self.loader_takes_wine_path = takes_wine_path;
        self
    }

    pub fn loader_takes_wine_path(&self) -> bool {
        self.loader_takes_wine_path
    }

    pub fn wine(&self) -> &Path {
        &self.wine
    }

    pub fn wine_preloader(&self) -> &Path {
        &self.wine_preloader
    }

    pub fn prefix(&self) -> &Path {
        &self.prefix
    }

    pub fn execution_allowed(&self) -> bool {
        self.execution_allowed
    }

    fn bind_execution(mut self, handles: RuntimePreflightHandles) -> Self {
        self.wine = descriptor_path(&handles.wine);
        self.wine_preloader = descriptor_path(&handles.wine_preloader);
        self.prefix = descriptor_path(&handles.prefix);
        self.execution_allowed = true;
        self.handles = Some(handles);
        self
    }

    /// The verified preloader descriptor the guest executor starts, for
    /// pre-exec image checks.
    pub(crate) fn guest_image(&self) -> Option<&File> {
        self.handles.as_ref().map(|handles| &handles.wine_preloader)
    }

    /// Child-visible descriptor paths for the bound preflight. The guest
    /// executor shapes its argv from these before `configure_command` maps the
    /// verified descriptors into the child.
    pub(crate) fn command_paths(&self) -> Option<RuntimePreflightCommandPaths> {
        self.handles.as_ref()?;
        Some(RuntimePreflightCommandPaths {
            wine: child_descriptor_path(PREFLIGHT_RUNTIME_BIN_FD).join("wine"),
            wine_preloader: child_descriptor_path(PREFLIGHT_PRELOADER_FD),
            prefix: child_descriptor_path(PREFLIGHT_PREFIX_FD),
        })
    }

    /// Map the verified descriptors into `command`. Returns `false` when this
    /// request was never bound to verified runtime handles.
    pub(crate) fn configure_command(&self, command: &mut Command) -> io::Result<bool> {
        let Some(handles) = &self.handles else {
            return Ok(false);
        };
        let mappings = vec![
            descriptor_mapping(&handles.wine_preloader, PREFLIGHT_PRELOADER_FD)?,
            descriptor_mapping(&handles.wine, PREFLIGHT_WINE_PROOF_FD)?,
            descriptor_mapping(&handles.prefix, PREFLIGHT_PREFIX_FD)?,
            descriptor_mapping(&handles.runtime_bin, PREFLIGHT_RUNTIME_BIN_FD)?,
        ];
        command.fd_mappings(mappings).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid runtime descriptor mapping: {error}"),
            )
        })?;
        Ok(true)
    }
}

#[derive(Debug)]
struct RuntimePreflightHandles {
    wine: File,
    wine_preloader: File,
    prefix: File,
    runtime_bin: File,
}

fn descriptor_path(file: &File) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()))
}

fn child_descriptor_path(descriptor: i32) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{descriptor}"))
}

fn descriptor_mapping(file: &File, child_fd: i32) -> io::Result<FdMapping> {
    let parent_fd: OwnedFd = file.try_clone()?.into();
    Ok(FdMapping {
        parent_fd,
        child_fd,
    })
}

pub(crate) struct RuntimePreflightCommandPaths {
    pub(crate) wine: PathBuf,
    pub(crate) wine_preloader: PathBuf,
    pub(crate) prefix: PathBuf,
}

/// Boundary owned by the transaction manager for classifying any active
/// journal from finals and backup evidence.
pub trait TransactionProbe {
    fn probe(&self) -> io::Result<TransactionEvidence>;
}

/// Read-only exclusion boundary shared with every installation mutator.
pub trait AuditLease {
    fn try_acquire(&self) -> io::Result<Box<dyn AuditLeaseGuard + '_>>;
}

/// Keeps the shared audit lease held until the filesystem scan completes.
pub trait AuditLeaseGuard {}

/// OS-backed shared lease for a Launcher-created per-installation lock file.
pub struct FileAuditLease {
    lock: File,
}

impl FileAuditLease {
    /// Locates an existing lock file without creating or changing it.
    pub fn open(lock_path: impl Into<PathBuf>) -> io::Result<Self> {
        let lock_path = lock_path.into();
        let parent = lock_path.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "audit lock parent unavailable")
        })?;
        let name = lock_path.file_name().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "audit lock name unavailable")
        })?;
        let root = Directory::open(parent)?;
        let file = root.open_file(Path::new(name))?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "audit lock is not a regular file",
            ));
        }
        Ok(Self { lock: file })
    }
}

impl AuditLease for FileAuditLease {
    fn try_acquire(&self) -> io::Result<Box<dyn AuditLeaseGuard + '_>> {
        let file = self.lock.try_clone()?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "audit lock is not a regular file",
            ));
        }

        File::try_lock_shared(&file)?;
        Ok(Box::new(FileAuditLeaseGuard(file)))
    }
}

struct FileAuditLeaseGuard(File);

impl AuditLeaseGuard for FileAuditLeaseGuard {}

impl Drop for FileAuditLeaseGuard {
    fn drop(&mut self) {
        let _ = File::unlock(&self.0);
    }
}

/// Conservative journal locator used until the transaction module owns its
/// final schema and can prove a unique Repairable outcome.
pub struct FilesystemTransactionProbe {
    parent: io::Result<Directory>,
    name: OsString,
}

impl FilesystemTransactionProbe {
    pub fn new(transactions_directory: impl Into<PathBuf>) -> Self {
        let path = transactions_directory.into();
        let parent = path.parent().map_or_else(
            || {
                Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "transaction directory parent unavailable",
                ))
            },
            Directory::open,
        );
        let name = path.file_name().unwrap_or_default().to_owned();
        Self { parent, name }
    }
}

impl TransactionProbe for FilesystemTransactionProbe {
    fn probe(&self) -> io::Result<TransactionEvidence> {
        let parent = match &self.parent {
            Ok(parent) => parent,
            Err(error) => return Err(io::Error::new(error.kind(), error.to_string())),
        };
        let directory = match parent.open_directory(Path::new(&self.name)) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(TransactionEvidence::Clear);
            }
            Err(error) => return Err(error),
        };
        if !directory.names()?.is_empty() {
            return Ok(TransactionEvidence::Unsafe);
        }

        Ok(TransactionEvidence::Clear)
    }
}

/// Independently supplied evidence sources required for one audit.
#[derive(Clone, Copy)]
pub struct AuditSources<'a> {
    host: &'a dyn HostProbe,
    transaction: &'a dyn TransactionProbe,
    lease: &'a dyn AuditLease,
}

impl<'a> AuditSources<'a> {
    pub fn new(
        host: &'a dyn HostProbe,
        transaction: &'a dyn TransactionProbe,
        lease: &'a dyn AuditLease,
    ) -> Self {
        Self {
            host,
            transaction,
            lease,
        }
    }
}

/// SHA-256 digest stored as validated bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Sha256Digest([u8; 32]);

impl Sha256Digest {
    pub fn parse_hex(value: &str) -> Result<Self, InvalidDigest> {
        parse_hex::<32>(value).map(Self)
    }
}

/// MD5 digest used by the accepted upstream PD2 manifest contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Md5Digest([u8; 16]);

impl Md5Digest {
    pub fn parse_hex(value: &str) -> Result<Self, InvalidDigest> {
        parse_hex::<16>(value).map(Self)
    }

    pub(crate) fn bytes(self) -> [u8; 16] {
        self.0
    }
}

/// Returned when a digest is not exact lowercase or uppercase hexadecimal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidDigest;

impl fmt::Display for InvalidDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("digest must contain the exact number of hexadecimal bytes")
    }
}

impl Error for InvalidDigest {}

/// Exact file evidence: size and SHA-256.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileSha256Expectation {
    size: u64,
    digest: Sha256Digest,
}

/// Frozen runtime entrypoints and installed Gecko fixture.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeFileExpectations {
    wine: FileSha256Expectation,
    wine_preloader: FileSha256Expectation,
    loader_name: String,
    loader_takes_wine_path: bool,
    gecko_xul: FileSha256Expectation,
    archive_receipt: String,
    gecko_installer_receipt: String,
}

impl RuntimeFileExpectations {
    pub fn new(
        wine: FileSha256Expectation,
        wine_preloader: FileSha256Expectation,
        gecko_xul: FileSha256Expectation,
        archive_receipt: impl Into<String>,
        gecko_installer_receipt: impl Into<String>,
    ) -> Self {
        Self {
            wine,
            wine_preloader,
            loader_name: "wine-preloader".to_owned(),
            loader_takes_wine_path: true,
            gecko_xul,
            archive_receipt: archive_receipt.into(),
            gecko_installer_receipt: gecko_installer_receipt.into(),
        }
    }

    /// Execute `bin/wine` itself instead of `bin/wine-preloader`. The
    /// `wine_preloader` expectation then describes `bin/wine`.
    pub fn with_wine_as_loader(mut self) -> Self {
        self.loader_name = "wine".to_owned();
        self.loader_takes_wine_path = false;
        self
    }
}

impl FileSha256Expectation {
    pub fn new(size: u64, digest: Sha256Digest) -> Self {
        Self { size, digest }
    }
}

/// One file from an accepted PD2 client manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClientFileExpectation {
    path: RelativeGamePath,
    digest: Md5Digest,
}

impl ClientFileExpectation {
    pub fn new(path: RelativeGamePath, digest: Md5Digest) -> Self {
        Self { path, digest }
    }
}

/// Locally accepted client identity used to decide Ready independently of the
/// latest network update check.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptedClientManifest {
    files: BTreeMap<RelativeGamePath, Md5Digest>,
}

impl AcceptedClientManifest {
    pub fn new(
        files: impl IntoIterator<Item = ClientFileExpectation>,
    ) -> Result<Self, InvalidClientManifest> {
        let mut normalized = BTreeMap::new();
        for file in files {
            if normalized.insert(file.path, file.digest).is_some() {
                return Err(InvalidClientManifest);
            }
        }

        if !normalized.contains_key(&RelativeGamePath::parse("Game.exe").expect("static path"))
            || !normalized
                .contains_key(&RelativeGamePath::parse("ProjectDiablo.dll").expect("static path"))
        {
            return Err(InvalidClientManifest);
        }

        Ok(Self { files: normalized })
    }

    pub(crate) fn digest_for(&self, path: &RelativeGamePath) -> Option<Md5Digest> {
        self.files.get(path).copied()
    }
}

/// Returned when the accepted manifest is duplicate or lacks launch fixtures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidClientManifest;

impl fmt::Display for InvalidClientManifest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .write_str("client manifest must be unique and include Game.exe and ProjectDiablo.dll")
    }
}

impl Error for InvalidClientManifest {}

/// Recovery classification supplied by the transaction manager.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransactionEvidence {
    Clear,
    Repairable,
    Unsafe,
}

/// Current process ownership supplied by process supervision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActivityEvidence {
    Idle,
    GameRunning,
    Mutating,
    InstallerRunning,
}

/// Frozen artifact policy used by the auditor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuditPolicy {
    runtime_identity: String,
    runtime_files: RuntimeFileExpectations,
}

impl AuditPolicy {
    /// The Diablo II version is not part of the policy: it is never
    /// checked.
    pub fn new(
        runtime_identity: impl Into<String>,
        runtime_files: RuntimeFileExpectations,
    ) -> Result<Self, InvalidRuntimeIdentity> {
        let runtime_identity = runtime_identity.into();
        if runtime_identity.contains('/') || RelativeGamePath::parse(&runtime_identity).is_err() {
            return Err(InvalidRuntimeIdentity);
        }
        Ok(Self {
            runtime_identity,
            runtime_files,
        })
    }

    fn pinned() -> Self {
        let runtime = crate::runtime::current();
        Self::new(runtime.identity, Self::runtime_file_expectations(runtime))
            .expect("static runtime identity")
    }

    fn runtime_file_expectations(
        runtime: &crate::runtime::ManagedRuntime,
    ) -> RuntimeFileExpectations {
        let runtime_file = |file: crate::runtime::RuntimeFile| {
            FileSha256Expectation::new(
                file.size,
                Sha256Digest::parse_hex(file.sha256).expect("static digest"),
            )
        };
        let runtime_files = RuntimeFileExpectations::new(
            runtime_file(runtime.wine),
            runtime_file(runtime.loader),
            FileSha256Expectation::new(
                71_766_016,
                Sha256Digest::parse_hex(
                    "db2542df83e44540f44ef219e45fe463cd2621911cff5e1bd6ac453cabd600b0",
                )
                .expect("static digest"),
            ),
            runtime.archive_sha512,
            "26cecc47706b091908f7f814bddb074c61beb8063318e9efc5a7f789857793d6",
        );
        if runtime.loader_takes_wine_path {
            runtime_files
        } else {
            runtime_files.with_wine_as_loader()
        }
    }
}

/// Returned when a runtime identity is not one portable directory name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidRuntimeIdentity;

impl fmt::Display for InvalidRuntimeIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("runtime identity must be one portable directory name")
    }
}

impl Error for InvalidRuntimeIdentity {}

/// Inputs needed to inspect one launcher-managed installation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuditRequest {
    managed_root: PathBuf,
    accepted_client: Option<AcceptedClientManifest>,
    activity: ActivityEvidence,
    launcher_artifact: LauncherArtifactEvidence,
    custom_sources: Vec<CustomComponent>,
    xinput_bridge: bool,
    client_as_installed: bool,
}

impl AuditRequest {
    pub fn new(managed_root: PathBuf, activity: ActivityEvidence) -> Self {
        Self {
            managed_root,
            accepted_client: None,
            activity,
            launcher_artifact: LauncherArtifactEvidence::Unverified,
            custom_sources: Vec::new(),
            xinput_bridge: false,
            client_as_installed: false,
        }
    }

    /// The download source of `component` is currently custom.
    /// Only then may its custom-unverified receipt satisfy the audit; with
    /// the built-in source a custom component is reported for replacement.
    pub fn with_custom_source(mut self, component: CustomComponent) -> Self {
        if !self.custom_sources.contains(&component) {
            self.custom_sources.push(component);
        }
        self
    }

    fn custom_source(&self, component: CustomComponent) -> bool {
        self.custom_sources.contains(&component)
    }

    pub fn with_client_manifest(mut self, manifest: AcceptedClientManifest) -> Self {
        self.accepted_client = Some(manifest);
        self
    }

    pub fn with_launcher_artifact(mut self, artifact: LauncherArtifactEvidence) -> Self {
        self.launcher_artifact = artifact;
        self
    }

    pub fn with_activity(mut self, activity: ActivityEvidence) -> Self {
        self.activity = activity;
        self
    }

    /// The prefix holds the Android host's XInput bridge as the game's
    /// `xinput1_4.dll`.
    pub fn with_xinput_bridge(mut self) -> Self {
        self.xinput_bridge = true;
        self
    }

    /// The player turned Project Diablo 2's update checks off: its client
    /// is accepted as installed. Every file the accepted manifest names must
    /// still be there, but what it holds is not compared, so a mod, or the
    /// files another launcher changed, play as they are. `Game.exe` keeps
    /// its digest: the launch runs only the accepted one.
    pub fn with_client_as_installed(mut self) -> Self {
        self.client_as_installed = true;
        self
    }
}

/// Read-only release policy for classifying managed installation evidence.
#[derive(Clone, Debug)]
pub struct Auditor {
    policy: AuditPolicy,
}

impl Auditor {
    pub fn production() -> Self {
        Self::with_policy(AuditPolicy::pinned())
    }

    pub fn with_policy(policy: AuditPolicy) -> Self {
        Self { policy }
    }

    pub fn audit(&self, request: &AuditRequest, sources: AuditSources<'_>) -> InstallationReport {
        self.audit_inner(request, sources, None)
    }

    pub(crate) fn audit_with_root(
        &self,
        request: &AuditRequest,
        sources: AuditSources<'_>,
        retained_root: &Directory,
    ) -> InstallationReport {
        self.audit_inner(request, sources, Some(retained_root))
    }

    fn audit_inner(
        &self,
        request: &AuditRequest,
        sources: AuditSources<'_>,
        retained_root: Option<&Directory>,
    ) -> InstallationReport {
        let mut results = Vec::with_capacity(CHECK_COUNT);
        let mut prefix_components = PrefixComponents::indeterminate();

        let lease_guard = if request.activity == ActivityEvidence::Mutating {
            None
        } else {
            sources.lease.try_acquire().ok()
        };
        let (root_state, managed_root) = if lease_guard.is_some() {
            match retained_root
                .map(Directory::try_clone)
                .unwrap_or_else(|| Directory::open(&request.managed_root))
            {
                Ok(root) => (CheckStatus::Satisfied, Some(root)),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    (CheckStatus::Missing, None)
                }
                Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                    (CheckStatus::Invalid, None)
                }
                Err(_) => (CheckStatus::Indeterminate, None),
            }
        } else {
            (CheckStatus::Indeterminate, None)
        };
        // A custom runtime is audited only while the Wine source is custom;
        // with the built-in source only a pinned runtime is.
        let selected_runtime = managed_root.as_ref().map(|root| {
            crate::runtime::InstalledRuntime::select(
                root,
                request.custom_source(CustomComponent::WineRuntime),
            )
        });
        let custom_runtime =
            selected_runtime.is_some_and(crate::runtime::InstalledRuntime::is_custom);
        // The Android app's runtime: provided by the app, audited by its
        // structure and its exact `wine --version`.
        let app_runtime = selected_runtime == Some(crate::runtime::InstalledRuntime::App);
        let policy = &self.policy;
        let (runtime_base, loader_name, loader_takes_wine_path) = if custom_runtime {
            (CUSTOM_RUNTIME_BASE.to_owned(), "wine", false)
        } else if app_runtime {
            (APP_RUNTIME_BASE.to_owned(), "wine", false)
        } else {
            (
                format!("runner/{}", policy.runtime_identity),
                policy.runtime_files.loader_name.as_str(),
                policy.runtime_files.loader_takes_wine_path,
            )
        };
        let runtime_root = request.managed_root.join(&runtime_base).join("bin");
        let mut runtime_preflight = RuntimePreflightRequest::new(
            runtime_root.join("wine"),
            runtime_root.join(loader_name),
            request.managed_root.join("prefix"),
        )
        .with_loader_takes_wine_path(loader_takes_wine_path);
        if custom_runtime {
            runtime_preflight = runtime_preflight.with_any_wine_version();
        } else if app_runtime {
            runtime_preflight =
                runtime_preflight.with_wine_version(crate::runtime::APP_RUNTIME_VERSION);
        }

        // The game folder: `Some(None)` for the Managed one, `Some(Some(_))`
        // for a folder used in place and proven to be the recorded one.
        let game_folder: Option<Option<Directory>> = managed_root.as_ref().and_then(|root| {
            match crate::game_folder::GameFolder::read(root) {
                Ok(crate::game_folder::GameFolder::Managed) => Some(None),
                Ok(crate::game_folder::GameFolder::External(external)) => {
                    external.open().ok().map(Some)
                }
                Err(_) => None,
            }
        });
        if request.activity == ActivityEvidence::Mutating {
            block_filesystem_checks(&mut results, FindingReason::ProcessActive);
        } else if lease_guard.is_none() {
            block_filesystem_checks(&mut results, FindingReason::AuditLeaseUnavailable);
        } else {
            if root_state != CheckStatus::Satisfied {
                for check in [
                    AuditCheck::Runtime,
                    AuditCheck::Prefix,
                    AuditCheck::BaseGame,
                    AuditCheck::Expansion,
                    AuditCheck::Client,
                ] {
                    results.push(CheckResult::root_failure(check, root_state));
                }
            } else {
                let root = managed_root
                    .as_ref()
                    .expect("satisfied root retains its descriptor");
                results.push(if custom_runtime {
                    check_custom_runtime(root)
                } else if app_runtime {
                    check_app_runtime(root)
                } else {
                    unconfigured_custom_runtime(root, policy)
                        .unwrap_or_else(|| check_runtime(root, policy))
                });
                let (prefix, components) =
                    check_prefix(root, policy, request, crate::android_host());
                results.push(prefix);
                prefix_components = components;
                match game_folder.as_ref() {
                    Some(folder) => {
                        let game = match folder {
                            Some(folder) => GameTree::external(folder),
                            None => GameTree::managed(root),
                        };
                        // An imported game on the Android host is proven by
                        // the files its import checked (the owner's rule of
                        // 2026-10-26); one that Blizzard's installers put
                        // there, and the desktop's, by all of them.
                        let imported = crate::android_host()
                            && crate::game_folder::records_directory(root, true)
                                .and_then(|records| {
                                    records.open_file(Path::new(
                                        crate::game_folder::IMPORT_RECORD_NAME,
                                    ))
                                })
                                .is_ok();
                        results.push(check_base_game(&game, request.activity, imported));
                        results.push(check_expansion(&game, request.activity, imported));
                        results.push(check_client(
                            &game,
                            request.accepted_client.as_ref(),
                            request.client_as_installed,
                        ));
                    }
                    // The recorded game folder is gone, replaced, or its
                    // record is not the Launcher's: nothing about the game
                    // can be established.
                    None => {
                        for check in [
                            AuditCheck::BaseGame,
                            AuditCheck::Expansion,
                            AuditCheck::Client,
                        ] {
                            results.push(single_finding(
                                check,
                                CheckStatus::Indeterminate,
                                FindingReason::GameFolderUnavailable,
                                None,
                            ));
                        }
                    }
                }
            }
        }
        // `wine --version` needs the verified runtime and the prefix Wine
        // created, not the Launcher-installed prefix components, so a prefix
        // that only lacks one of them keeps an established runtime.
        let execution_allowed = prefix_components.base == CheckStatus::Satisfied
            && results.iter().any(|result| {
                result.check == AuditCheck::Runtime && result.status == CheckStatus::Satisfied
            });
        if execution_allowed {
            let root = managed_root
                .as_ref()
                .expect("execution requires a retained managed root");
            let bound = if custom_runtime {
                bind_unpinned_runtime_preflight(root, CUSTOM_RUNTIME_BASE)
            } else if app_runtime {
                bind_unpinned_runtime_preflight(root, APP_RUNTIME_BASE)
            } else {
                bind_runtime_preflight(root, policy)
            };
            match bound {
                Ok(handles) => runtime_preflight = runtime_preflight.bind_execution(handles),
                Err(reason) => {
                    if let Some(runtime_result) = results
                        .iter_mut()
                        .find(|result| result.check == AuditCheck::Runtime)
                    {
                        runtime_result.status = CheckStatus::Indeterminate;
                        runtime_result.findings.push(AuditFinding {
                            check: AuditCheck::Runtime,
                            reason,
                            path: None,
                        });
                    }
                }
            }
        }
        let (host_support, host_result, runtime_execution_preflight) =
            check_host(sources.host, &runtime_preflight, request.launcher_artifact);
        if let Some(runtime_result) = results.iter_mut().find(|result| {
            result.check == AuditCheck::Runtime && result.status == CheckStatus::Satisfied
        }) {
            match runtime_execution_preflight {
                Some(RuntimeExecutionEvidence::Passed) => {}
                Some(RuntimeExecutionEvidence::Failed) => {
                    runtime_result.status = CheckStatus::Invalid;
                    runtime_result.findings.push(AuditFinding {
                        check: AuditCheck::Runtime,
                        reason: FindingReason::RuntimePreflightFailed,
                        path: None,
                    });
                }
                Some(
                    RuntimeExecutionEvidence::Unavailable | RuntimeExecutionEvidence::Blocked(_),
                )
                | None => {
                    runtime_result.status = CheckStatus::Indeterminate;
                    runtime_result.findings.push(AuditFinding {
                        check: AuditCheck::Runtime,
                        reason: FindingReason::RuntimePreflightUnavailable,
                        path: None,
                    });
                }
            }
        }
        results.push(host_result);
        if request.activity == ActivityEvidence::Mutating {
            results.push(single_finding(
                AuditCheck::Transaction,
                CheckStatus::Indeterminate,
                FindingReason::ProcessActive,
                None,
            ));
        } else if lease_guard.is_none() {
            results.push(single_finding(
                AuditCheck::Transaction,
                CheckStatus::Indeterminate,
                FindingReason::AuditLeaseUnavailable,
                None,
            ));
        } else {
            results.push(check_transaction(sources.transaction));
        }

        let mut checks = [CheckStatus::Indeterminate; CHECK_COUNT];
        let mut findings = Vec::new();
        let mut custom_sources = Vec::new();
        for result in results {
            checks[result.check.index()] = result.status;
            findings.extend(result.findings);
            if result.status == CheckStatus::Satisfied {
                custom_sources.extend(result.custom_sources);
            }
        }

        let marker = match managed_root.as_ref() {
            Some(root)
                if lease_guard.is_some() && request.activity != ActivityEvidence::Mutating =>
            {
                crate::stage_marker::read(root)
            }
            _ => MarkerState::Absent,
        };
        let live_game = match (managed_root.as_ref(), game_folder.as_ref()) {
            (Some(root), Some(folder))
                if lease_guard.is_some() && request.activity != ActivityEvidence::Mutating =>
            {
                Some(match folder {
                    Some(folder) => GameTree::external(folder),
                    None => GameTree::managed(root),
                })
            }
            _ => None,
        };
        // Beside a started base-game marker on the Android host (Start over
        // writes one first), any entry of the game folder counts as written.
        let game_entry = crate::android_host()
            && marker == MarkerState::Present(InterruptedStage::BaseGame)
            && live_game.as_ref().is_some_and(GameTree::holds_an_entry);
        let installation_status =
            classify_installation(&checks, prefix_components.base, marker, game_entry);
        // The Android app has no window to show why: its log says it, once per changed result.
        if crate::android_host() {
            log_android_audit(installation_status, &findings);
        }
        let launch_preflight = if installation_status == InstallationStatus::Ready
            && checks[AuditCheck::Transaction.index()] == CheckStatus::Satisfied
            && request.activity == ActivityEvidence::Idle
        {
            LaunchPreflight::Passed
        } else {
            LaunchPreflight::Failed
        };

        let official_launcher = match (managed_root.as_ref(), live_game.as_ref()) {
            (Some(root), Some(game)) => {
                check_official_launcher(root, game, request.accepted_client.as_ref())
            }
            _ => OfficialLauncherTrust::Indeterminate,
        };
        if official_launcher == OfficialLauncherTrust::Changed {
            findings.push(AuditFinding {
                check: AuditCheck::Client,
                reason: FindingReason::OfficialLauncherChanged,
                path: live_game.as_ref().map(|game| {
                    relative_path(&game.path(&format!("ProjectD2/{OFFICIAL_LAUNCHER_PROGRAM}")))
                }),
            });
        }

        let (protected_inventory_status, protected_files) =
            protected_file_inventory(live_game.as_ref(), request.accepted_client.as_ref());

        InstallationReport {
            installation_status,
            host_support,
            launch_preflight,
            checks,
            findings,
            protected_inventory_status,
            protected_files,
            custom_sources,
            custom_runtime,
            app_runtime,
            prefix_components,
            official_launcher,
        }
    }
}

/// Whether the protected inventory completely covered the ProjectD2 tree.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProtectedInventoryStatus {
    Complete,
    Indeterminate,
}

/// Why one ProjectD2 path must be preserved by update planning.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProtectedFileKind {
    Known,
    Unknown,
    /// Written by the official PD2 launcher; preserved, never tampering.
    OfficialLauncher,
}

/// Presence state for one path in the accepted protected-file set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProtectedFileState {
    Present,
    Missing,
    Indeterminate,
}

/// One user-owned or copied-once file that update planning must preserve.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProtectedFileInventoryEntry {
    path: RelativeGamePath,
    kind: ProtectedFileKind,
    state: ProtectedFileState,
}

impl ProtectedFileInventoryEntry {
    pub fn path(&self) -> &RelativeGamePath {
        &self.path
    }

    pub fn state(&self) -> ProtectedFileState {
        self.state
    }

    pub fn kind(&self) -> ProtectedFileKind {
        self.kind
    }
}

/// Stable result consumed by installation and presentation workflows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallationReport {
    installation_status: InstallationStatus,
    host_support: HostSupport,
    launch_preflight: LaunchPreflight,
    checks: [CheckStatus; CHECK_COUNT],
    findings: Vec<AuditFinding>,
    protected_inventory_status: ProtectedInventoryStatus,
    protected_files: Vec<ProtectedFileInventoryEntry>,
    custom_sources: Vec<CustomSourceEvidence>,
    custom_runtime: bool,
    /// The audited runtime is the Android app's.
    app_runtime: bool,
    prefix_components: PrefixComponents,
    official_launcher: OfficialLauncherTrust,
}

/// Evidence for each part of the managed prefix: Wine's own prefix and the
/// Launcher-installed Gecko, Wine Mono, and .NET Desktop Runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PrefixComponents {
    pub base: CheckStatus,
    pub gecko: CheckStatus,
    pub mono: CheckStatus,
    pub dotnet: CheckStatus,
}

impl PrefixComponents {
    const fn indeterminate() -> Self {
        Self {
            base: CheckStatus::Indeterminate,
            gecko: CheckStatus::Indeterminate,
            mono: CheckStatus::Indeterminate,
            dotnet: CheckStatus::Indeterminate,
        }
    }
}

/// A component accepted from a user-supplied custom source. Its
/// content was not verified against a pin; the audit checked only presence
/// and executable structure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CustomComponent {
    WineRuntime,
    Gecko,
    WineMono,
    DotnetDesktop,
}

/// Redacted provenance of one custom-source component: host only.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CustomSourceEvidence {
    component: CustomComponent,
    host: String,
}

impl CustomSourceEvidence {
    pub fn component(&self) -> CustomComponent {
        self.component
    }

    pub fn host(&self) -> &str {
        &self.host
    }
}

impl InstallationReport {
    /// Components currently accepted through custom-unverified receipts.
    pub fn custom_sources(&self) -> &[CustomSourceEvidence] {
        &self.custom_sources
    }

    /// Whether the audited (and preflighted) runtime is the custom-source
    /// runtime rather than the pinned one. A launch executes this runtime.
    pub fn uses_custom_runtime(&self) -> bool {
        self.custom_runtime
    }

    /// The runtime the audit accepted and preflighted, which a launch
    /// executes: the custom-source runtime, the Android app's runtime, or the
    /// row's pinned runtime.
    pub(crate) fn installed_runtime(&self) -> crate::runtime::InstalledRuntime {
        if self.custom_runtime {
            crate::runtime::InstalledRuntime::Custom
        } else if self.app_runtime {
            crate::runtime::InstalledRuntime::App
        } else {
            crate::runtime::InstalledRuntime::BuiltIn(crate::runtime::current())
        }
    }

    /// Per-component prefix evidence behind the Prefix check.
    /// Whether the official launcher binary may run without asking.
    pub fn official_launcher(&self) -> OfficialLauncherTrust {
        self.official_launcher
    }

    pub fn prefix_components(&self) -> PrefixComponents {
        self.prefix_components
    }

    pub fn installation_status(&self) -> InstallationStatus {
        self.installation_status
    }

    pub fn host_support(&self) -> HostSupport {
        self.host_support
    }

    pub fn launch_preflight(&self) -> LaunchPreflight {
        self.launch_preflight
    }

    pub fn check(&self, check: AuditCheck) -> CheckStatus {
        self.checks[check.index()]
    }

    pub fn findings(&self) -> &[AuditFinding] {
        &self.findings
    }

    pub fn protected_inventory_status(&self) -> ProtectedInventoryStatus {
        self.protected_inventory_status
    }

    pub fn protected_files(&self) -> &[ProtectedFileInventoryEntry] {
        &self.protected_files
    }
}

#[derive(Clone, Debug)]
struct CheckResult {
    check: AuditCheck,
    status: CheckStatus,
    findings: Vec<AuditFinding>,
    custom_sources: Vec<CustomSourceEvidence>,
}

impl CheckResult {
    fn root_failure(check: AuditCheck, status: CheckStatus) -> Self {
        let reason = match status {
            CheckStatus::Missing => FindingReason::Missing,
            CheckStatus::Invalid => FindingReason::UnexpectedType,
            CheckStatus::Indeterminate => FindingReason::UnsafePath,
            CheckStatus::Satisfied => unreachable!("satisfied root is audited normally"),
        };
        Self {
            check,
            status,
            findings: vec![AuditFinding {
                check,
                reason,
                path: None,
            }],
            custom_sources: Vec::new(),
        }
    }
}

fn block_filesystem_checks(results: &mut Vec<CheckResult>, reason: FindingReason) {
    results.extend(
        FILESYSTEM_CHECKS
            .map(|check| single_finding(check, CheckStatus::Indeterminate, reason, None)),
    );
}

fn check_host(
    probe: &dyn HostProbe,
    runtime: &RuntimePreflightRequest,
    launcher_artifact: LauncherArtifactEvidence,
) -> (HostSupport, CheckResult, Option<RuntimeExecutionEvidence>) {
    match probe.probe(runtime) {
        Ok(evidence) => {
            let runtime_execution_preflight = evidence.runtime_execution_preflight;
            let mut findings = host_prerequisite_findings(&evidence, launcher_artifact);
            // A classified refusal to execute the verified guest is a host
            // capability failure, not evidence against the Managed runtime.
            if let RuntimeExecutionEvidence::Blocked(blocker) = runtime_execution_preflight {
                let reason = match blocker {
                    GuestExecutionBlocker::Ia32ExecutionDisabled => {
                        FindingReason::Ia32ExecutionUnavailable
                    }
                    GuestExecutionBlocker::ExecutionDenied => FindingReason::GuestExecutionDenied,
                    GuestExecutionBlocker::SharedLibrariesMissing => {
                        FindingReason::MultilibUnavailable
                    }
                };
                if !findings.iter().any(|finding| finding.reason == reason) {
                    findings.push(host_finding(reason));
                }
            }
            if findings.is_empty() {
                (
                    HostSupport::Supported,
                    CheckResult {
                        check: AuditCheck::Host,
                        status: CheckStatus::Satisfied,
                        findings,
                        custom_sources: Vec::new(),
                    },
                    Some(runtime_execution_preflight),
                )
            } else {
                (
                    HostSupport::Unsupported,
                    CheckResult {
                        check: AuditCheck::Host,
                        status: CheckStatus::Invalid,
                        findings,
                        custom_sources: Vec::new(),
                    },
                    Some(runtime_execution_preflight),
                )
            }
        }
        Err(_) => (
            HostSupport::Indeterminate,
            single_finding(
                AuditCheck::Host,
                CheckStatus::Indeterminate,
                FindingReason::HostUnavailable,
                None,
            ),
            None,
        ),
    }
}

/// One capability a host row consumes. Each maps to exactly one stable
/// finding reason when absent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HostCapability {
    DisplaySession,
    TranslatorInterpreter,
    /// 4 KB memory pages for the translator: FEX on the desktop's ARM64
    /// row, the app's Wine and Box64 on the Android host.
    TranslatorPageSize,
    TranslatorRootFs,
    DirectGuest,
    X11Display,
    WineLibrary(WineLibrary),
    VulkanLoader,
    VulkanDriver,
    HardwareGraphics,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RowArchitecture {
    Aarch64,
    X86_64,
}

/// One artifact/host row: the platform it serves, the Launcher artifacts it
/// admits (the standalone executable), whether it is certified in
/// this build, and the capabilities it requires. Admission is a lookup over this table rather than CPU checks
/// spread through policy.
struct HostRow {
    architecture: RowArchitecture,
    /// The Android host (injected host facts, `os_id` "android").
    android: bool,
    artifacts: &'static [LauncherArtifactEvidence],
    certified: bool,
    capabilities: &'static [HostCapability],
}

/// The `os_id` the Android host's probe reports from the app's host facts
/// (Android has no os-release).
const ANDROID_OS_ID: &str = "android";

const HOST_ROWS: [HostRow; 3] = [
    HostRow {
        architecture: RowArchitecture::Aarch64,
        android: false,
        artifacts: &[LauncherArtifactEvidence::NativeAarch64StandaloneBinary],
        certified: true,
        capabilities: &[
            HostCapability::DisplaySession,
            HostCapability::TranslatorInterpreter,
            HostCapability::TranslatorPageSize,
            HostCapability::TranslatorRootFs,
            // No Vulkan or hardware-graphics gate, as on the x86-64 row: the
            // game draws with OpenGL (D2GL) or DirectDraw, the Launcher's own
            // window proves its own graphics, and software rendering is
            // reported on the Play screen without blocking.
        ],
    },
    HostRow {
        architecture: RowArchitecture::X86_64,
        android: false,
        artifacts: &[LauncherArtifactEvidence::NativeX86_64StandaloneBinary],
        // Certified in x86-64 builds, the only builds that run on these
        // hosts; a build for another architecture reports the row's
        // diagnostics but keeps the host Unsupported.
        certified: cfg!(target_arch = "x86_64"),
        capabilities: &[
            HostCapability::DisplaySession,
            HostCapability::DirectGuest,
            HostCapability::X11Display,
            HostCapability::WineLibrary(WineLibrary::X11),
            HostCapability::WineLibrary(WineLibrary::Xext),
            HostCapability::WineLibrary(WineLibrary::FreeType),
            HostCapability::WineLibrary(WineLibrary::Fontconfig),
            HostCapability::WineLibrary(WineLibrary::OpenGl),
            // No Vulkan or hardware-graphics gate (see the ARM64 row).
        ],
    },
    // Android ARM64: the standalone ARM64 Launcher inside the Android app,
    // run as its engine, with the app's ARM64 Wine and its host facts.
    // No display session: Check needs no X screen, and the app starts its
    // X screen for Install and Play.
    HostRow {
        architecture: RowArchitecture::Aarch64,
        android: true,
        artifacts: &[LauncherArtifactEvidence::NativeAarch64StandaloneBinary],
        certified: true,
        // The app provides the runtime, its ARM64 Wine; the runtime
        // preflight shows that it runs. A kernel whose pages are not 4 KB
        // is told before setup, not by a failed preflight.
        capabilities: &[HostCapability::TranslatorPageSize],
    },
];

/// Capabilities still reported for an architecture without a row so the user
/// sees every independent gap, not only the architecture.
const UNKNOWN_ROW_CAPABILITIES: [HostCapability; 4] = [
    HostCapability::DisplaySession,
    HostCapability::VulkanLoader,
    HostCapability::VulkanDriver,
    HostCapability::HardwareGraphics,
];

fn host_finding(reason: FindingReason) -> AuditFinding {
    AuditFinding {
        check: AuditCheck::Host,
        reason,
        path: None,
    }
}

fn host_prerequisite_findings(
    host: &HostEvidence,
    launcher_artifact: LauncherArtifactEvidence,
) -> Vec<AuditFinding> {
    row_findings(&HOST_ROWS, host, launcher_artifact)
}

/// [`host_prerequisite_findings`] with the host `rows`.
fn row_findings(
    rows: &[HostRow],
    host: &HostEvidence,
    launcher_artifact: LauncherArtifactEvidence,
) -> Vec<AuditFinding> {
    let row_architecture = match HostPlatform::from_architecture(&host.architecture) {
        HostPlatform::Aarch64 => Some(RowArchitecture::Aarch64),
        HostPlatform::X86_64 => Some(RowArchitecture::X86_64),
        HostPlatform::Other(_) => None,
    };
    let android = host.os_id == ANDROID_OS_ID;
    let Some(row) = rows
        .iter()
        .find(|row| Some(row.architecture) == row_architecture && row.android == android)
    else {
        let mut findings = vec![host_finding(FindingReason::ArchitectureUnsupported)];
        findings.extend(
            UNKNOWN_ROW_CAPABILITIES
                .iter()
                .filter_map(|capability| capability_gap(host, *capability))
                .map(host_finding),
        );
        findings.push(host_finding(FindingReason::GuestExecutionUnavailable));
        return findings;
    };

    let mut findings = row
        .capabilities
        .iter()
        .filter_map(|capability| capability_gap(host, *capability))
        .map(host_finding)
        .collect::<Vec<_>>();
    if !row.artifacts.contains(&launcher_artifact) {
        findings.push(host_finding(FindingReason::LauncherArtifactUnverified));
    }
    if !row.certified {
        findings.push(host_finding(FindingReason::HostRowUncertified));
    }
    findings
}

fn capability_gap(host: &HostEvidence, capability: HostCapability) -> Option<FindingReason> {
    use GuestExecutionEvidence::{AppProvided, Native, Translated};

    let guest = &host.guest_execution;
    // Translation-detail and direct-execution-detail gaps are reported only
    // when the matching evidence variant exists; otherwise the interpreter or
    // `DirectGuest` capability is the single fail-closed finding.
    let (satisfied, reason) = match capability {
        HostCapability::DisplaySession => (
            matches!(
                host.display_session,
                DisplaySession::Wayland | DisplaySession::X11
            ),
            FindingReason::DisplaySessionUnavailable,
        ),
        HostCapability::TranslatorInterpreter => (
            matches!(
                guest,
                Translated {
                    interpreter_present: true,
                    ..
                }
            ),
            FindingReason::FexUnavailable,
        ),
        HostCapability::TranslatorPageSize => (
            !matches!(
                guest,
                Translated {
                    page_size_4k: false,
                    ..
                } | AppProvided {
                    page_size_4k: false
                }
            ),
            FindingReason::PageSizeUnsupported,
        ),
        HostCapability::TranslatorRootFs => (
            !matches!(
                guest,
                Translated {
                    interpreter_present: true,
                    page_size_4k: true,
                    rootfs_ready: false,
                    ..
                }
            ),
            FindingReason::FexRootFsUnavailable,
        ),
        HostCapability::DirectGuest => (
            matches!(guest, Native { .. }),
            FindingReason::GuestExecutionUnavailable,
        ),
        HostCapability::X11Display => (
            !matches!(
                guest,
                Native {
                    x11_display_available: false,
                    ..
                }
            ),
            FindingReason::X11DisplayUnavailable,
        ),
        // Wine starts without them (it loads them on demand), so only this
        // check tells the user before the prefix or the game fails.
        HostCapability::WineLibrary(library) => (
            !matches!(
                guest,
                Native {
                    missing_wine_libraries,
                    ..
                } if missing_wine_libraries.contains(&library)
            ),
            FindingReason::WineLibraryUnavailable(library),
        ),
        HostCapability::VulkanLoader => {
            (host.vulkan_loader, FindingReason::VulkanLoaderUnavailable)
        }
        HostCapability::VulkanDriver => {
            (host.vulkan_driver, FindingReason::VulkanDriverUnavailable)
        }
        HostCapability::HardwareGraphics => (
            host.graphics_execution
                .as_ref()
                .is_some_and(|graphics| !graphics.is_software_emulated),
            FindingReason::HardwareGraphicsUnavailable,
        ),
    };
    (!satisfied).then_some(reason)
}

fn check_runtime(root: &Directory, policy: &AuditPolicy) -> CheckResult {
    let base = format!("runner/{}", policy.runtime_identity);
    aggregate(
        AuditCheck::Runtime,
        [
            inspect_directory(root, &base),
            inspect_exact_text(
                root,
                &format!("{base}/.pd2launcher-archive.sha512"),
                &policy.runtime_files.archive_receipt,
            ),
            inspect_executable(root, &format!("{base}/bin/wine")),
            inspect_sha256(root, &format!("{base}/bin/wine"), policy.runtime_files.wine),
            inspect_executable(
                root,
                &format!("{base}/bin/{}", policy.runtime_files.loader_name),
            ),
            inspect_sha256(
                root,
                &format!("{base}/bin/{}", policy.runtime_files.loader_name),
                policy.runtime_files.wine_preloader,
            ),
        ],
    )
}

fn bind_runtime_preflight(
    root: &Directory,
    policy: &AuditPolicy,
) -> Result<RuntimePreflightHandles, FindingReason> {
    let base = format!("runner/{}", policy.runtime_identity);
    bind_runtime_handles(
        root,
        &base,
        bind_runtime_file(
            root,
            &format!("{base}/bin/wine"),
            Some(policy.runtime_files.wine),
        )?,
        bind_runtime_file(
            root,
            &format!("{base}/bin/{}", policy.runtime_files.loader_name),
            Some(policy.runtime_files.wine_preloader),
        )?,
    )
}

/// A custom runtime, or the Android app's, is executed through `bin/wine`
/// itself; its identity is not pinned, but the preflight still runs the
/// exact retained descriptor.
fn bind_unpinned_runtime_preflight(
    root: &Directory,
    base: &str,
) -> Result<RuntimePreflightHandles, FindingReason> {
    let wine = format!("{base}/bin/wine");
    bind_runtime_handles(
        root,
        base,
        bind_runtime_file(root, &wine, None)?,
        bind_runtime_file(root, &wine, None)?,
    )
}

fn bind_runtime_handles(
    root: &Directory,
    base: &str,
    wine: File,
    wine_preloader: File,
) -> Result<RuntimePreflightHandles, FindingReason> {
    Ok(RuntimePreflightHandles {
        wine,
        wine_preloader,
        prefix: open_managed_node(
            root,
            &relative_path("prefix"),
            OFlags::RDONLY | OFlags::DIRECTORY,
        )?
        .ok_or(FindingReason::Missing)?,
        runtime_bin: open_managed_node(
            root,
            &relative_path(&format!("{base}/bin")),
            OFlags::RDONLY | OFlags::DIRECTORY,
        )?
        .ok_or(FindingReason::Missing)?,
    })
}

fn bind_runtime_file(
    root: &Directory,
    relative: &str,
    expected: Option<FileSha256Expectation>,
) -> Result<File, FindingReason> {
    let relative = relative_path(relative);
    let mut file =
        open_managed_node(root, &relative, OFlags::RDONLY)?.ok_or(FindingReason::Missing)?;
    let metadata = file.metadata().map_err(|_| FindingReason::Unreadable)?;
    if !metadata.is_file() {
        return Err(FindingReason::UnexpectedType);
    }
    if metadata.permissions().mode() & 0o111 == 0 {
        return Err(FindingReason::NotExecutable);
    }
    let Some(expected) = expected else {
        return Ok(file);
    };
    if metadata.len() != expected.size {
        return Err(FindingReason::SizeMismatch);
    }
    if stream_digest::<Sha256, 32>(&mut file)? != expected.digest.0 {
        return Err(FindingReason::DigestMismatch);
    }
    Ok(file)
}

/// Custom-source runtime: presence and executable structure only.
/// The receipt records provenance; there is no pinned identity to compare.
fn check_custom_runtime(root: &Directory) -> CheckResult {
    let (receipt, host) = inspect_custom_receipt(
        root,
        &format!(
            "{CUSTOM_RUNTIME_BASE}/{}",
            crate::runtime::CUSTOM_RUNTIME_RECEIPT
        ),
        crate::sources::DownloadArtifact::Wine,
    );
    let mut result = aggregate(
        AuditCheck::Runtime,
        [
            inspect_directory(root, CUSTOM_RUNTIME_BASE),
            receipt,
            inspect_executable(root, &format!("{CUSTOM_RUNTIME_BASE}/bin/wine")),
            inspect_magic(root, &format!("{CUSTOM_RUNTIME_BASE}/bin/wine"), b"\x7fELF"),
            inspect_executable(root, &format!("{CUSTOM_RUNTIME_BASE}/bin/wineserver")),
        ],
    );
    result
        .custom_sources
        .extend(host.map(|host| CustomSourceEvidence {
            component: CustomComponent::WineRuntime,
            host,
        }));
    result
}

/// The Android app's runtime: presence and executable structure only. The
/// app made it for its own data folder, so it has no pinned digest; the
/// preflight checks its exact version.
fn check_app_runtime(root: &Directory) -> CheckResult {
    aggregate(
        AuditCheck::Runtime,
        [
            inspect_directory(root, APP_RUNTIME_BASE),
            inspect_executable(root, &format!("{APP_RUNTIME_BASE}/bin/wine")),
            inspect_magic(root, &format!("{APP_RUNTIME_BASE}/bin/wine"), b"\x7fELF"),
            inspect_executable(root, &format!("{APP_RUNTIME_BASE}/bin/wineserver")),
        ],
    )
}

/// With the built-in Wine source configured, a runtime installed from a
/// custom source (and no pinned runtime) is reported for replacement instead
/// of being accepted or silently treated as missing.
fn unconfigured_custom_runtime(root: &Directory, policy: &AuditPolicy) -> Option<CheckResult> {
    let custom_receipt = format!(
        "{CUSTOM_RUNTIME_BASE}/{}",
        crate::runtime::CUSTOM_RUNTIME_RECEIPT
    );
    let pinned_receipt = format!(
        "runner/{}/{}",
        policy.runtime_identity,
        crate::runtime::BUILTIN_RUNTIME_RECEIPT
    );
    (present(root, &custom_receipt) && absent(root, &pinned_receipt)).then(|| {
        single_finding(
            AuditCheck::Runtime,
            CheckStatus::Invalid,
            FindingReason::CustomSourceNotConfigured,
            Some(relative_path(CUSTOM_RUNTIME_BASE)),
        )
    })
}

fn present(root: &Directory, relative: &str) -> bool {
    matches!(safe_metadata(root, &relative_path(relative)), Ok(Some(_)))
}

fn absent(root: &Directory, relative: &str) -> bool {
    matches!(safe_metadata(root, &relative_path(relative)), Ok(None))
}

/// The prefix check: Wine's own prefix plus each Launcher-installed prefix
/// component, reported separately so an existing prefix can be completed in
/// place instead of recreated. `android`: the Android host, whose prefix has
/// no Wine Gecko and no Wine Mono.
fn check_prefix(
    root: &Directory,
    policy: &AuditPolicy,
    request: &AuditRequest,
    android: bool,
) -> (CheckResult, PrefixComponents) {
    let base = aggregate(
        AuditCheck::Prefix,
        [
            inspect_directory(root, "prefix/drive_c/windows"),
            inspect_regular(root, "prefix/system.reg"),
            inspect_regular(root, "prefix/user.reg"),
            inspect_regular(root, "prefix/userdef.reg"),
        ],
    );
    let (gecko, mono, dotnet) = if android {
        // The Android app's prefix has no Wine Gecko and no Wine Mono: no
        // program the Launcher runs there needs them, and every Wine
        // process there has mscoree and mshtml off. It reaches the app's
        // .NET (for the official PD2 launcher) through a link.
        (
            satisfied(AuditCheck::Prefix),
            satisfied(AuditCheck::Prefix),
            check_app_dotnet(&request.managed_root),
        )
    } else {
        (
            check_gecko(root, policy, request.custom_source(CustomComponent::Gecko)),
            check_prefix_dependency(
                root,
                PrefixDependency::WineMono,
                request.custom_source(CustomComponent::WineMono),
            ),
            // For the official PD2 launcher.
            check_prefix_dependency(
                root,
                PrefixDependency::DotnetDesktop,
                request.custom_source(CustomComponent::DotnetDesktop),
            ),
        )
    };
    let components = PrefixComponents {
        base: base.status,
        gecko: gecko.status,
        mono: mono.status,
        dotnet: dotnet.status,
    };
    // On Android only Wine's own prefix and the .NET Desktop Runtime make the
    // status: accepted Gecko and Mono parts among missing ones would make a
    // prefix that is not there (a stop before its first Wine start) Invalid
    // instead of Missing.
    let mut parts = if android {
        vec![base, dotnet]
    } else {
        vec![base, gecko, mono, dotnet]
    };
    // Only a rejected bridge changes the prefix's status.
    if request.xinput_bridge {
        parts.extend(check_xinput_bridge(root));
    }
    let statuses = parts.iter().map(|part| part.status).collect::<Vec<_>>();
    let status = if statuses.contains(&CheckStatus::Indeterminate) {
        CheckStatus::Indeterminate
    } else if statuses
        .iter()
        .all(|status| *status == CheckStatus::Satisfied)
    {
        CheckStatus::Satisfied
    } else if statuses
        .iter()
        .all(|status| *status == CheckStatus::Missing)
    {
        CheckStatus::Missing
    } else {
        CheckStatus::Invalid
    };
    let mut result = CheckResult {
        check: AuditCheck::Prefix,
        status,
        findings: Vec::new(),
        custom_sources: Vec::new(),
    };
    for part in parts {
        result.findings.extend(part.findings);
        result.custom_sources.extend(part.custom_sources);
    }
    (result, components)
}

/// The Android app's .NET as the prefix reaches it: its link (which the
/// safe walk never follows) leads to the app's Windows Desktop runtime.
fn check_app_dotnet(managed_root: &Path) -> CheckResult {
    let desktop = managed_root
        .join(crate::runtime::APP_DOTNET_LINK)
        .join("shared/Microsoft.WindowsDesktop.App/6.0.36");
    match std::fs::metadata(&desktop) {
        Ok(metadata) if metadata.is_dir() => satisfied(AuditCheck::Prefix),
        _ => single_finding(
            AuditCheck::Prefix,
            CheckStatus::Missing,
            FindingReason::Missing,
            Some(relative_path(crate::runtime::APP_DOTNET_LINK)),
        ),
    }
}

/// The Android host's XInput bridge: `None` for
/// the sealed one, or one not installed yet (absent, or Wine's own DLL),
/// which the game start replaces with it. Any other `xinput1_4.dll` is
/// rejected.
fn check_xinput_bridge(root: &Directory) -> Option<CheckResult> {
    let relative = relative_path(crate::install::XINPUT_BRIDGE_PATH);
    let evidence = match safe_read(root, &relative, MAX_XINPUT_DLL_BYTES) {
        Ok(None) => return None,
        Ok(Some(bytes)) if is_xinput_bridge(&bytes) || is_wine_dll(&bytes) => return None,
        Ok(Some(_)) => ItemEvidence::invalid(Some(relative), FindingReason::DigestMismatch),
        Err(reason) => ItemEvidence::indeterminate(Some(relative), reason),
    };
    Some(aggregate(AuditCheck::Prefix, [evidence]))
}

fn is_xinput_bridge(bytes: &[u8]) -> bool {
    let expected =
        Sha256Digest::parse_hex(crate::install::XINPUT_BRIDGE_SHA256).expect("static digest");
    bytes.len() == crate::install::XINPUT_BRIDGE_SIZE
        && Sha256::digest(bytes).as_slice() == expected.0.as_slice()
}

/// A DLL `wineboot` copied into the prefix: Wine marks its own after the
/// DOS header.
fn is_wine_dll(bytes: &[u8]) -> bool {
    bytes.starts_with(b"MZ")
        && bytes.get(0x40..).is_some_and(|stub| {
            stub.starts_with(b"Wine builtin DLL") || stub.starts_with(b"Wine placeholder DLL")
        })
}

fn check_gecko(root: &Directory, policy: &AuditPolicy, custom_source: bool) -> CheckResult {
    if absent(root, BUILTIN_GECKO_RECEIPT) && present(root, CUSTOM_GECKO_RECEIPT) {
        if !custom_source {
            return single_finding(
                AuditCheck::Prefix,
                CheckStatus::Invalid,
                FindingReason::CustomSourceNotConfigured,
                Some(relative_path(CUSTOM_GECKO_RECEIPT)),
            );
        }
        let (receipt, host) = inspect_custom_receipt(
            root,
            CUSTOM_GECKO_RECEIPT,
            crate::sources::DownloadArtifact::Gecko,
        );
        let mut result = aggregate(AuditCheck::Prefix, [receipt]);
        result
            .custom_sources
            .extend(host.map(|host| CustomSourceEvidence {
                component: CustomComponent::Gecko,
                host,
            }));
        return result;
    }
    aggregate(
        AuditCheck::Prefix,
        [
            inspect_exact_text(
                root,
                BUILTIN_GECKO_RECEIPT,
                &policy.runtime_files.gecko_installer_receipt,
            ),
            inspect_sha256(
                root,
                "prefix/drive_c/windows/syswow64/gecko/2.47.4/wine_gecko/xul.dll",
                policy.runtime_files.gecko_xul,
            ),
        ],
    )
}

/// A prefix component the Launcher installs after Gecko.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PrefixDependency {
    WineMono,
    DotnetDesktop,
}

impl PrefixDependency {
    const fn component(self) -> CustomComponent {
        match self {
            Self::WineMono => CustomComponent::WineMono,
            Self::DotnetDesktop => CustomComponent::DotnetDesktop,
        }
    }

    const fn artifact(self) -> crate::sources::DownloadArtifact {
        match self {
            Self::WineMono => crate::sources::DownloadArtifact::WineMono,
            Self::DotnetDesktop => crate::sources::DownloadArtifact::DotnetDesktop,
        }
    }

    const fn receipts(self) -> (&'static str, &'static str) {
        match self {
            Self::WineMono => (BUILTIN_MONO_RECEIPT, CUSTOM_MONO_RECEIPT),
            Self::DotnetDesktop => (
                BUILTIN_DOTNET_DESKTOP_RECEIPT,
                CUSTOM_DOTNET_DESKTOP_RECEIPT,
            ),
        }
    }

    const fn builtin_sha256(self) -> &'static str {
        match self {
            Self::WineMono => WINE_MONO_MSI_SHA256,
            Self::DotnetDesktop => DOTNET_DESKTOP_INSTALLER_SHA256,
        }
    }

    /// The pinned version's installed tree, or any version's for a custom
    /// source.
    const fn installed_tree(self, custom: bool) -> &'static str {
        match (self, custom) {
            (Self::WineMono, _) => "prefix/drive_c/windows/mono/mono-2.0",
            (Self::DotnetDesktop, false) => {
                "prefix/drive_c/Program Files (x86)/dotnet/shared/Microsoft.WindowsDesktop.App/6.0.36"
            }
            (Self::DotnetDesktop, true) => {
                "prefix/drive_c/Program Files (x86)/dotnet/shared/Microsoft.WindowsDesktop.App"
            }
        }
    }
}

fn check_prefix_dependency(
    root: &Directory,
    dependency: PrefixDependency,
    custom_source: bool,
) -> CheckResult {
    let (builtin_receipt, custom_receipt) = dependency.receipts();
    if absent(root, builtin_receipt) && present(root, custom_receipt) {
        if !custom_source {
            return single_finding(
                AuditCheck::Prefix,
                CheckStatus::Invalid,
                FindingReason::CustomSourceNotConfigured,
                Some(relative_path(custom_receipt)),
            );
        }
        let (receipt, host) = inspect_custom_receipt(root, custom_receipt, dependency.artifact());
        let mut result = aggregate(
            AuditCheck::Prefix,
            [
                receipt,
                inspect_directory(root, dependency.installed_tree(true)),
            ],
        );
        result
            .custom_sources
            .extend(host.map(|host| CustomSourceEvidence {
                component: dependency.component(),
                host,
            }));
        return result;
    }
    aggregate(
        AuditCheck::Prefix,
        [
            inspect_exact_text(root, builtin_receipt, dependency.builtin_sha256()),
            inspect_directory(root, dependency.installed_tree(false)),
        ],
    )
}

/// Diablo II is installed when its files are: Blizzard's installers or an
/// import put them there. An `imported` game (on the Android host) is
/// proven by the files its import checked.
fn check_base_game(game: &GameTree<'_>, activity: ActivityEvidence, imported: bool) -> CheckResult {
    let files: &[&str] = if imported {
        &crate::game_folder::IMPORT_BASE_FILES
    } else {
        &[
            "Game.exe",
            "Diablo II.exe",
            "d2data.mpq",
            "d2music.mpq",
            "d2sfx.mpq",
            "d2speech.mpq",
            "d2video.mpq",
        ]
    };
    check_game_files(AuditCheck::BaseGame, game, files, activity)
}

/// Lord of Destruction is installed when its files are; an `imported` game's
/// by the files its import checked.
fn check_expansion(game: &GameTree<'_>, activity: ActivityEvidence, imported: bool) -> CheckResult {
    let files: &[&str] = if imported {
        &crate::game_folder::IMPORT_EXPANSION_FILES
    } else {
        &["d2exp.mpq", "d2xmusic.mpq", "d2xtalk.mpq", "d2xvideo.mpq"]
    };
    check_game_files(AuditCheck::Expansion, game, files, activity)
}

/// A game is proven by its files alone, each a regular file: no install
/// log, registry value or import record, and no version. The game runs
/// from its folder without them.
fn check_game_files(
    check: AuditCheck,
    game: &GameTree<'_>,
    files: &[&str],
    activity: ActivityEvidence,
) -> CheckResult {
    let mut evidence: Vec<_> = files
        .iter()
        .map(|file| inspect_regular(game.directory, &game.path(file)))
        .collect();
    if activity == ActivityEvidence::InstallerRunning {
        evidence.push(ItemEvidence::invalid(None, FindingReason::ProcessActive));
    }
    aggregate(check, evidence)
}

/// Every file of the accepted manifest, each with the digest the manifest
/// names; `as_installed` (update checks off) asks only that each is there,
/// but for `Game.exe`, which the launch checks against its digest.
fn check_client(
    game: &GameTree<'_>,
    manifest: Option<&AcceptedClientManifest>,
    as_installed: bool,
) -> CheckResult {
    let Some(manifest) = manifest else {
        return single_finding(
            AuditCheck::Client,
            CheckStatus::Missing,
            FindingReason::ManifestUnavailable,
            None,
        );
    };

    aggregate(
        AuditCheck::Client,
        manifest.files.iter().map(|(path, digest)| {
            let relative = game.path(&format!("ProjectD2/{}", path.as_str()));
            if (as_installed && path.as_str() != "Game.exe")
                || is_known_protected(path.as_str())
                || is_official_launcher_file(path.as_str())
            {
                inspect_regular(game.directory, &relative)
            } else {
                inspect_md5(game.directory, &relative, *digest)
            }
        }),
    )
}

fn protected_file_inventory(
    game: Option<&GameTree<'_>>,
    manifest: Option<&AcceptedClientManifest>,
) -> (ProtectedInventoryStatus, Vec<ProtectedFileInventoryEntry>) {
    let project_d2 = game.map_or_else(
        || format!("{}ProjectD2", managed_game_base(crate::android_host())),
        |game| game.path("ProjectD2"),
    );
    let mut entries: Vec<_> = PROTECTED_FILES
        .into_iter()
        .map(|file| {
            let path = relative_path(&format!("{project_d2}/{file}"));
            let state = match game {
                None => ProtectedFileState::Indeterminate,
                Some(game) => match safe_metadata(game.directory, &path) {
                    Ok(Some(metadata)) if metadata.is_file() => ProtectedFileState::Present,
                    Ok(None) => ProtectedFileState::Missing,
                    Ok(Some(_)) | Err(_) => ProtectedFileState::Indeterminate,
                },
            };
            ProtectedFileInventoryEntry {
                path,
                kind: ProtectedFileKind::Known,
                state,
            }
        })
        .collect();

    let Some(game) = game else {
        return (ProtectedInventoryStatus::Indeterminate, entries);
    };
    let accepted: BTreeSet<_> = manifest
        .into_iter()
        .flat_map(|manifest| manifest.files.keys())
        .map(|path| path.as_str().to_owned())
        .collect();
    match collect_unknown_project_files(game.directory, &project_d2, &accepted) {
        Ok(mut unknown) => {
            entries.append(&mut unknown);
            entries.sort_by(|left, right| left.path.cmp(&right.path));
            let status = if entries
                .iter()
                .any(|entry| entry.state == ProtectedFileState::Indeterminate)
            {
                ProtectedInventoryStatus::Indeterminate
            } else {
                ProtectedInventoryStatus::Complete
            };
            (status, entries)
        }
        Err(()) => {
            for entry in &mut entries {
                entry.state = ProtectedFileState::Indeterminate;
            }
            (ProtectedInventoryStatus::Indeterminate, entries)
        }
    }
}

fn collect_unknown_project_files(
    root: &Directory,
    project_d2: &str,
    accepted: &BTreeSet<String>,
) -> Result<Vec<ProtectedFileInventoryEntry>, ()> {
    let project_relative = relative_path(project_d2);
    let project =
        match open_managed_node(root, &project_relative, OFlags::RDONLY | OFlags::DIRECTORY)
            .map_err(|_| ())?
        {
            None => return Ok(Vec::new()),
            Some(project) => project,
        };

    let mut output = Vec::new();
    let mut visited = 0;
    walk_project_files(
        &project,
        project_d2,
        "",
        0,
        accepted,
        &mut visited,
        &mut output,
    )?;
    Ok(output)
}

fn walk_project_files(
    directory: &File,
    project_d2: &str,
    project_relative: &str,
    depth: usize,
    accepted: &BTreeSet<String>,
    visited: &mut usize,
    output: &mut Vec<ProtectedFileInventoryEntry>,
) -> Result<(), ()> {
    if depth > MAX_PROTECTED_DEPTH || *visited >= MAX_PROTECTED_FILES {
        return Err(());
    }
    let remaining = MAX_PROTECTED_FILES - *visited;
    let mut names = {
        let mut buffer = vec![MaybeUninit::uninit(); 64 * 1024];
        let mut raw_entries = RawDir::new(directory, &mut buffer);
        let mut names = Vec::new();
        while let Some(entry) = raw_entries.next() {
            let entry = entry.map_err(|_| ())?;
            if matches!(entry.file_name().to_bytes(), b"." | b"..") {
                continue;
            }
            // The Launcher's own scratch: an interrupted write's temporary
            // file, or the retained folder earlier versions kept.
            if depth == 0
                && crate::safe_fs::is_launcher_scratch(
                    <std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(
                        entry.file_name().to_bytes(),
                    ),
                )
            {
                continue;
            }
            if names.len() >= remaining {
                return Err(());
            }
            names.push(
                std::str::from_utf8(entry.file_name().to_bytes())
                    .map_err(|_| ())?
                    .to_owned(),
            );
        }
        names
    };
    *visited += names.len();
    names.sort();

    for name in names {
        let child_relative = if project_relative.is_empty() {
            name.clone()
        } else {
            format!("{project_relative}/{name}")
        };
        RelativeGamePath::parse(&child_relative).map_err(|_| ())?;

        if child_relative == ".pd2launcher" || child_relative.starts_with(".pd2launcher/") {
            continue;
        }
        let child = crate::safe_fs::openat_beneath(directory, Path::new(&name), OFlags::RDONLY)
            .map(File::from)
            .map_err(|_| ())?;
        let metadata = child.metadata().map_err(|_| ())?;
        if metadata.is_dir() {
            walk_project_files(
                &child,
                project_d2,
                &child_relative,
                depth + 1,
                accepted,
                visited,
                output,
            )?;
            continue;
        }
        if accepted.contains(&child_relative) || is_known_protected(&child_relative) {
            continue;
        }

        if !metadata.is_file() {
            return Err(());
        }

        let path = relative_path(&format!("{project_d2}/{child_relative}"));
        output.push(ProtectedFileInventoryEntry {
            path,
            kind: if is_official_launcher_file(&child_relative) {
                ProtectedFileKind::OfficialLauncher
            } else {
                ProtectedFileKind::Unknown
            },
            state: ProtectedFileState::Present,
        });
    }
    Ok(())
}

fn is_known_protected(project_relative: &str) -> bool {
    PROTECTED_FILES.contains(&project_relative)
}

fn check_transaction(probe: &dyn TransactionProbe) -> CheckResult {
    match probe.probe() {
        Err(_) => single_finding(
            AuditCheck::Transaction,
            CheckStatus::Indeterminate,
            FindingReason::UnsafeTransaction,
            None,
        ),
        Ok(transaction) => match transaction {
            TransactionEvidence::Clear => CheckResult {
                check: AuditCheck::Transaction,
                status: CheckStatus::Satisfied,
                findings: Vec::new(),
                custom_sources: Vec::new(),
            },
            TransactionEvidence::Repairable => single_finding(
                AuditCheck::Transaction,
                CheckStatus::Invalid,
                FindingReason::RecoverableTransaction,
                None,
            ),
            TransactionEvidence::Unsafe => single_finding(
                AuditCheck::Transaction,
                CheckStatus::Indeterminate,
                FindingReason::UnsafeTransaction,
                None,
            ),
        },
    }
}

/// The Android host's log line for an audit: its status and findings, only when they differ
/// from the last audit's, so a steady installation adds nothing.
fn log_android_audit(status: InstallationStatus, findings: &[AuditFinding]) {
    static LAST: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());
    let line = android_audit_line(status, findings);
    let mut last = LAST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if *last != line {
        eprintln!("pd2-launcher: audit {line}");
        *last = line;
    }
}

/// [`log_android_audit`]'s line. A client with no accepted manifest yet is
/// a client not recorded yet (before Install Project Diablo 2, or an import
/// before it records the copied client), not a fault, and the line says so.
fn android_audit_line(status: InstallationStatus, findings: &[AuditFinding]) -> String {
    format!(
        "{status:?}: {}",
        findings
            .iter()
            .map(|finding| match (&finding.path, finding.reason) {
                (Some(path), _) => {
                    format!("{:?} {:?} {}", finding.check, finding.reason, path.as_str())
                }
                (None, FindingReason::ManifestUnavailable)
                    if finding.check == AuditCheck::Client =>
                {
                    "Client NotRecordedYet".to_owned()
                }
                (None, _) => format!("{:?} {:?}", finding.check, finding.reason),
            })
            .collect::<Vec<_>>()
            .join("; ")
    )
}

/// `game_entry`: on the Android host, beside a started base-game marker, the
/// game folder holds an entry other than its `Save` link
/// ([`GameTree::holds_an_entry`]); false elsewhere.
fn classify_installation(
    checks: &[CheckStatus; CHECK_COUNT],
    prefix_base: CheckStatus,
    marker: MarkerState,
    game_entry: bool,
) -> InstallationStatus {
    match checks[AuditCheck::Transaction.index()] {
        CheckStatus::Indeterminate => return InstallationStatus::Indeterminate,
        CheckStatus::Invalid => return InstallationStatus::RepairRequired,
        CheckStatus::Missing => return InstallationStatus::Indeterminate,
        CheckStatus::Satisfied => {}
    }
    // A marker beside incomplete evidence for its own stage is a known
    // interruption, not unexplained evidence. Missing evidence is still
    // resumable. Complete evidence means only the clear was lost when the
    // marker is finished; a game stage's step that has not returned may
    // have left its last file short.
    match marker {
        MarkerState::Absent => {}
        MarkerState::Invalid => return InstallationStatus::Indeterminate,
        MarkerState::Present(stage) | MarkerState::Finished(stage) => {
            let finished = matches!(marker, MarkerState::Finished(_));
            let invalid = |check: AuditCheck| checks[check.index()] == CheckStatus::Invalid;
            let written = |check: AuditCheck| {
                matches!(
                    checks[check.index()],
                    CheckStatus::Satisfied | CheckStatus::Invalid
                )
            };
            let interrupted = match stage {
                InterruptedStage::Prefix => invalid(AuditCheck::Prefix),
                // Both game stages write in the one game folder: an import
                // copies both games, and Lord of Destruction's installer
                // writes `Game.exe` again. Invalid evidence of either game
                // beside a game stage's marker is that stage's
                // interruption.
                _ if invalid(AuditCheck::BaseGame) || invalid(AuditCheck::Expansion) => true,
                // The files are the only proof, and a file is there but
                // short while it is written: a step that has not returned
                // stopped part way once it wrote any of its files. Diablo
                // II's files are there before Lord of Destruction's step.
                // On the Android host any entry of the game folder counts
                // too: a stop late in Start over's removal of the folder
                // (it writes this marker first) can leave only files that
                // no check names, as `ProjectD2/`.
                InterruptedStage::BaseGame => {
                    !finished
                        && (written(AuditCheck::BaseGame)
                            || written(AuditCheck::Expansion)
                            || game_entry)
                }
                // A started Lord of Destruction marker beside the PD2
                // client can only be one an earlier release left: this
                // one never installs the client beside it, and such a
                // release proved those files by their install log.
                InterruptedStage::Expansion => {
                    !finished
                        && written(AuditCheck::Expansion)
                        && checks[AuditCheck::Client.index()] != CheckStatus::Satisfied
                }
            };
            if interrupted {
                return InstallationStatus::StageInterrupted(stage);
            }
        }
    }

    let base = checks[AuditCheck::BaseGame.index()];
    if base != CheckStatus::Satisfied {
        return if base == CheckStatus::Missing {
            InstallationStatus::NeedsBaseGame
        } else {
            InstallationStatus::Indeterminate
        };
    }
    let expansion = checks[AuditCheck::Expansion.index()];
    if expansion != CheckStatus::Satisfied {
        return if expansion == CheckStatus::Missing {
            InstallationStatus::NeedsExpansion
        } else {
            InstallationStatus::Indeterminate
        };
    }
    match checks[AuditCheck::Client.index()] {
        CheckStatus::Satisfied => {}
        CheckStatus::Indeterminate => return InstallationStatus::Indeterminate,
        CheckStatus::Missing | CheckStatus::Invalid => return InstallationStatus::NeedsClient,
    }

    let host = checks[AuditCheck::Host.index()];
    let runtime = checks[AuditCheck::Runtime.index()];
    let prefix = checks[AuditCheck::Prefix.index()];
    if host != CheckStatus::Satisfied || runtime != CheckStatus::Satisfied {
        return InstallationStatus::Indeterminate;
    }
    match prefix {
        CheckStatus::Satisfied => InstallationStatus::Ready,
        // The game, the runtime, and Wine's own prefix are established; only
        // a Launcher-installed prefix component is missing or must be
        // replaced, as when an older installation lacks Wine Mono.
        CheckStatus::Missing | CheckStatus::Invalid if prefix_base == CheckStatus::Satisfied => {
            InstallationStatus::NeedsRuntime
        }
        _ => InstallationStatus::Indeterminate,
    }
}

#[derive(Clone, Debug)]
struct ItemEvidence {
    status: CheckStatus,
    path: Option<RelativeGamePath>,
    reason: FindingReason,
}

impl ItemEvidence {
    fn satisfied() -> Self {
        Self {
            status: CheckStatus::Satisfied,
            path: None,
            reason: FindingReason::Missing,
        }
    }

    fn missing(path: RelativeGamePath) -> Self {
        Self {
            status: CheckStatus::Missing,
            path: Some(path),
            reason: FindingReason::Missing,
        }
    }

    fn invalid(path: Option<RelativeGamePath>, reason: FindingReason) -> Self {
        Self {
            status: CheckStatus::Invalid,
            path,
            reason,
        }
    }

    fn indeterminate(path: Option<RelativeGamePath>, reason: FindingReason) -> Self {
        Self {
            status: CheckStatus::Indeterminate,
            path,
            reason,
        }
    }
}

fn aggregate(check: AuditCheck, evidence: impl IntoIterator<Item = ItemEvidence>) -> CheckResult {
    let evidence: Vec<_> = evidence.into_iter().collect();
    let all_missing = evidence
        .iter()
        .all(|item| item.status == CheckStatus::Missing);
    let any_missing = evidence
        .iter()
        .any(|item| item.status == CheckStatus::Missing);
    let status = if evidence
        .iter()
        .any(|item| item.status == CheckStatus::Indeterminate)
    {
        CheckStatus::Indeterminate
    } else if evidence
        .iter()
        .any(|item| item.status == CheckStatus::Invalid)
        || (any_missing && !all_missing)
    {
        CheckStatus::Invalid
    } else if all_missing {
        CheckStatus::Missing
    } else {
        CheckStatus::Satisfied
    };
    let findings = evidence
        .into_iter()
        .filter(|item| item.status != CheckStatus::Satisfied)
        .map(|item| AuditFinding {
            check,
            reason: item.reason,
            path: item.path,
        })
        .collect();
    CheckResult {
        check,
        status,
        findings,
        custom_sources: Vec::new(),
    }
}

/// A part of `check` that is accepted without inspection.
fn satisfied(check: AuditCheck) -> CheckResult {
    CheckResult {
        check,
        status: CheckStatus::Satisfied,
        findings: Vec::new(),
        custom_sources: Vec::new(),
    }
}

fn single_finding(
    check: AuditCheck,
    status: CheckStatus,
    reason: FindingReason,
    path: Option<RelativeGamePath>,
) -> CheckResult {
    CheckResult {
        check,
        status,
        findings: vec![AuditFinding {
            check,
            reason,
            path,
        }],
        custom_sources: Vec::new(),
    }
}

fn inspect_directory(root: &Directory, relative: &str) -> ItemEvidence {
    let relative = relative_path(relative);
    match safe_metadata(root, &relative) {
        Ok(Some(metadata)) if metadata.is_dir() => ItemEvidence::satisfied(),
        Ok(Some(_)) => ItemEvidence::invalid(Some(relative), FindingReason::UnexpectedType),
        Ok(None) => ItemEvidence::missing(relative),
        Err(reason) => ItemEvidence::indeterminate(Some(relative), reason),
    }
}

fn inspect_regular(root: &Directory, relative: &str) -> ItemEvidence {
    let relative = relative_path(relative);
    match safe_metadata(root, &relative) {
        Ok(Some(metadata)) if metadata.is_file() => ItemEvidence::satisfied(),
        Ok(Some(_)) => ItemEvidence::invalid(Some(relative), FindingReason::UnexpectedType),
        Ok(None) => ItemEvidence::missing(relative),
        Err(reason) => ItemEvidence::indeterminate(Some(relative), reason),
    }
}

fn inspect_executable(root: &Directory, relative: &str) -> ItemEvidence {
    let relative = relative_path(relative);
    match safe_metadata(root, &relative) {
        Ok(Some(metadata)) if !metadata.is_file() => {
            ItemEvidence::invalid(Some(relative), FindingReason::UnexpectedType)
        }
        Ok(Some(metadata)) if metadata.permissions().mode() & 0o111 == 0 => {
            ItemEvidence::invalid(Some(relative), FindingReason::NotExecutable)
        }
        Ok(Some(_)) => ItemEvidence::satisfied(),
        Ok(None) => ItemEvidence::missing(relative),
        Err(reason) => ItemEvidence::indeterminate(Some(relative), reason),
    }
}

/// Which kinds of provenance receipt an installed component carries,
/// independent of the configured download sources.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct InstalledProvenance {
    pub(crate) pinned: bool,
    pub(crate) custom_host: Option<String>,
}

/// Receipts of the installed Wine runtime, Gecko, Wine Mono, and .NET
/// Desktop Runtime, for showing the installed source beside the configured
/// one in Diagnostics.
pub(crate) fn installed_provenance(
    root: &Directory,
    component: CustomComponent,
) -> InstalledProvenance {
    // The pinned runtime this installation uses: the row's, or its earlier
    // one kept for a prefix that runtime made.
    let runtime_receipt = format!(
        "runner/{}/{}",
        crate::runtime::InstalledRuntime::select(root, false).directory(),
        crate::runtime::BUILTIN_RUNTIME_RECEIPT
    );
    let custom_runtime_receipt = format!(
        "{CUSTOM_RUNTIME_BASE}/{}",
        crate::runtime::CUSTOM_RUNTIME_RECEIPT
    );
    let (pinned, custom, artifact) = match component {
        CustomComponent::WineRuntime => (
            runtime_receipt.as_str(),
            custom_runtime_receipt.as_str(),
            crate::sources::DownloadArtifact::Wine,
        ),
        CustomComponent::Gecko => (
            BUILTIN_GECKO_RECEIPT,
            CUSTOM_GECKO_RECEIPT,
            crate::sources::DownloadArtifact::Gecko,
        ),
        CustomComponent::WineMono => (
            BUILTIN_MONO_RECEIPT,
            CUSTOM_MONO_RECEIPT,
            crate::sources::DownloadArtifact::WineMono,
        ),
        CustomComponent::DotnetDesktop => (
            BUILTIN_DOTNET_DESKTOP_RECEIPT,
            CUSTOM_DOTNET_DESKTOP_RECEIPT,
            crate::sources::DownloadArtifact::DotnetDesktop,
        ),
    };
    InstalledProvenance {
        pinned: present(root, pinned),
        custom_host: inspect_custom_receipt(root, custom, artifact).1,
    }
}

/// A bounded, well-formed custom-source receipt for `artifact`, with the
/// redacted source host when it is valid.
fn inspect_custom_receipt(
    root: &Directory,
    relative: &str,
    artifact: crate::sources::DownloadArtifact,
) -> (ItemEvidence, Option<String>) {
    let relative = relative_path(relative);
    match safe_read(root, &relative, MAX_CUSTOM_RECEIPT_BYTES) {
        Ok(Some(bytes)) => match crate::sources::CustomSourceReceipt::decode(&bytes, artifact) {
            Some(receipt) => (ItemEvidence::satisfied(), Some(receipt.host())),
            None => (
                ItemEvidence::invalid(Some(relative), FindingReason::DigestMismatch),
                None,
            ),
        },
        Ok(None) => (ItemEvidence::missing(relative), None),
        Err(reason) => (ItemEvidence::indeterminate(Some(relative), reason), None),
    }
}

/// The file starts with `magic` (executable structure without a pin).
fn inspect_magic(root: &Directory, relative: &str, magic: &[u8]) -> ItemEvidence {
    let relative = relative_path(relative);
    let file = match open_managed_node(root, &relative, OFlags::RDONLY) {
        Ok(Some(file)) => file,
        Ok(None) => return ItemEvidence::missing(relative),
        Err(reason) => return ItemEvidence::indeterminate(Some(relative), reason),
    };
    match file.metadata() {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => return ItemEvidence::invalid(Some(relative), FindingReason::UnexpectedType),
        Err(_) => return ItemEvidence::indeterminate(Some(relative), FindingReason::Unreadable),
    }
    let mut observed = vec![0_u8; magic.len()];
    match file.take(magic.len() as u64).read_exact(&mut observed) {
        Ok(()) if observed == magic => ItemEvidence::satisfied(),
        Ok(()) | Err(_) => ItemEvidence::invalid(Some(relative), FindingReason::UnexpectedType),
    }
}

fn inspect_exact_text(root: &Directory, relative: &str, expected: &str) -> ItemEvidence {
    let relative = relative_path(relative);
    match safe_read(root, &relative, MAX_RECEIPT_BYTES) {
        Ok(Some(bytes)) if bytes == expected.as_bytes() => ItemEvidence::satisfied(),
        Ok(Some(_)) => ItemEvidence::invalid(Some(relative), FindingReason::DigestMismatch),
        Ok(None) => ItemEvidence::missing(relative),
        Err(reason) => ItemEvidence::indeterminate(Some(relative), reason),
    }
}

fn inspect_sha256(
    root: &Directory,
    relative: &str,
    expected: FileSha256Expectation,
) -> ItemEvidence {
    inspect_digest::<Sha256, 32>(root, relative, expected.digest.0, Some(expected.size))
}

fn inspect_md5(root: &Directory, relative: &str, expected: Md5Digest) -> ItemEvidence {
    inspect_digest::<Md5, 16>(root, relative, expected.0, None)
}

fn inspect_digest<D, const N: usize>(
    root: &Directory,
    relative: &str,
    expected: [u8; N],
    expected_size: Option<u64>,
) -> ItemEvidence
where
    D: Digest + Default,
{
    let relative = relative_path(relative);
    match safe_digest::<D, N>(root, &relative, expected_size) {
        Ok(Some(digest)) if digest == expected => ItemEvidence::satisfied(),
        Ok(Some(_)) => ItemEvidence::invalid(Some(relative), FindingReason::DigestMismatch),
        Ok(None) => ItemEvidence::missing(relative),
        Err(DigestFailure::Finding(reason)) => ItemEvidence::indeterminate(Some(relative), reason),
        Err(DigestFailure::SizeMismatch) => {
            ItemEvidence::invalid(Some(relative), FindingReason::SizeMismatch)
        }
    }
}

enum DigestFailure {
    Finding(FindingReason),
    SizeMismatch,
}

fn safe_digest<D, const N: usize>(
    root: &Directory,
    relative: &RelativeGamePath,
    expected_size: Option<u64>,
) -> Result<Option<[u8; N]>, DigestFailure>
where
    D: Digest + Default,
{
    let file = open_managed_node(root, relative, OFlags::RDONLY).map_err(DigestFailure::Finding)?;
    let Some(mut file) = file else {
        return Ok(None);
    };
    let metadata = file
        .metadata()
        .map_err(|_| DigestFailure::Finding(FindingReason::Unreadable))?;
    if !metadata.is_file() {
        return Err(DigestFailure::Finding(FindingReason::UnexpectedType));
    }
    if expected_size.is_some_and(|size| metadata.len() != size) {
        return Err(DigestFailure::SizeMismatch);
    }

    stream_digest::<D, N>(&mut file)
        .map(Some)
        .map_err(DigestFailure::Finding)
}

fn stream_digest<D, const N: usize>(file: &mut File) -> Result<[u8; N], FindingReason>
where
    D: Digest + Default,
{
    let mut hasher = D::default();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|_| FindingReason::Unreadable)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    hasher
        .finalize()
        .as_slice()
        .try_into()
        .map_err(|_| FindingReason::Unreadable)
}

fn safe_read(
    root: &Directory,
    relative: &RelativeGamePath,
    limit: u64,
) -> Result<Option<Vec<u8>>, FindingReason> {
    let file = open_managed_node(root, relative, OFlags::RDONLY)?;
    let Some(file) = file else {
        return Ok(None);
    };
    let metadata = file.metadata().map_err(|_| FindingReason::Unreadable)?;
    if !metadata.is_file() {
        return Err(FindingReason::UnexpectedType);
    }
    if metadata.len() > limit {
        return Err(FindingReason::Unreadable);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| FindingReason::Unreadable)?;
    if bytes.len() as u64 > limit {
        return Err(FindingReason::Unreadable);
    }
    Ok(Some(bytes))
}

fn safe_metadata(
    root: &Directory,
    relative: &RelativeGamePath,
) -> Result<Option<fs::Metadata>, FindingReason> {
    open_managed_node(root, relative, OFlags::PATH)?
        .map(|file| file.metadata().map_err(|_| FindingReason::Unreadable))
        .transpose()
}

fn open_managed_node(
    root: &Directory,
    relative: &RelativeGamePath,
    flags: OFlags,
) -> Result<Option<File>, FindingReason> {
    match root.open_beneath(Path::new(relative.as_str()), flags) {
        Ok(file) => Ok(Some(file)),
        Err(error) => map_managed_io_error(error),
    }
}

fn map_managed_io_error(error: io::Error) -> Result<Option<File>, FindingReason> {
    match error.raw_os_error() {
        Some(code) if code == rustix::io::Errno::NOENT.raw_os_error() => Ok(None),
        Some(code) if code == rustix::io::Errno::NOTDIR.raw_os_error() => {
            Err(FindingReason::UnexpectedType)
        }
        Some(code)
            if code == rustix::io::Errno::LOOP.raw_os_error()
                || code == rustix::io::Errno::XDEV.raw_os_error() =>
        {
            Err(FindingReason::UnsafePath)
        }
        _ => Err(FindingReason::Unreadable),
    }
}

fn relative_path(path: &str) -> RelativeGamePath {
    RelativeGamePath::parse(path).expect("auditor paths are static and portable")
}

fn parse_hex<const N: usize>(value: &str) -> Result<[u8; N], InvalidDigest> {
    if value.len() != N * 2 {
        return Err(InvalidDigest);
    }
    let mut bytes = [0_u8; N];
    for (index, pair) in value.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        let high = hex_nibble(pair[0]).ok_or(InvalidDigest)?;
        let low = hex_nibble(pair[1]).ok_or(InvalidDigest)?;
        bytes[index] = (high << 4) | low;
    }
    Ok(bytes)
}

fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Start over on the Android host that a stop left after it removed the
    /// app's prefix state (the game folder and the prefix whole,
    /// `install::tests::
    /// on_android_a_start_over_stopped_after_the_prefix_state_leaves_the_marker`):
    /// every check passes, as for the Ready installation it was, and the
    /// started base-game marker that Start over writes first makes it an
    /// interrupted step, which only Start over resolves. With part of the game
    /// gone it is one too; with all of it gone the marker is the start of an
    /// install.
    #[test]
    fn a_start_over_stopped_after_the_prefix_state_is_an_interrupted_step() {
        let mut checks = [CheckStatus::Satisfied; CHECK_COUNT];
        let started = MarkerState::Present(InterruptedStage::BaseGame);
        let interrupted = InstallationStatus::StageInterrupted(InterruptedStage::BaseGame);
        assert_eq!(
            classify_installation(&checks, CheckStatus::Satisfied, MarkerState::Absent, false),
            InstallationStatus::Ready
        );
        assert_eq!(
            classify_installation(&checks, CheckStatus::Satisfied, started, true),
            interrupted
        );
        // The prefix gone before the game folder.
        checks[AuditCheck::Prefix.index()] = CheckStatus::Missing;
        assert_eq!(
            classify_installation(&checks, CheckStatus::Missing, started, true),
            interrupted
        );
        // A stop while the game folder went.
        checks[AuditCheck::BaseGame.index()] = CheckStatus::Invalid;
        assert_eq!(
            classify_installation(&checks, CheckStatus::Missing, started, true),
            interrupted
        );
        for check in [
            AuditCheck::BaseGame,
            AuditCheck::Expansion,
            AuditCheck::Client,
        ] {
            checks[check.index()] = CheckStatus::Missing;
        }
        // No entry left in the game folder: the start of an install.
        assert_eq!(
            classify_installation(&checks, CheckStatus::Missing, started, false),
            InstallationStatus::NeedsBaseGame
        );
    }

    /// A stop late in an Android Start over's removal of the game folder:
    /// only `ProjectD2/`, or a stray file, is left beside its `Save` link,
    /// none of them a file that the base-game or expansion check names, and
    /// the started base-game marker stands. Both checks say Missing; the
    /// entry makes it an interrupted step all the same (before, the audit said
    /// NeedsBaseGame, and Fast download and an import then refused the game
    /// folder that is not empty). Only the `Save` link, or a game folder that
    /// is not there, is no entry; a real `Save` folder is one.
    #[test]
    fn a_game_folder_with_only_unnamed_entries_beside_a_started_marker_is_interrupted() {
        let started = MarkerState::Present(InterruptedStage::BaseGame);
        for left in [
            "ProjectD2/BH.json",
            "stray.txt",
            "Save/hero.d2s",
            "none",
            "the Save link",
        ] {
            let fixture = tempfile::tempdir().unwrap();
            let game = fixture.path().join("Diablo II");
            fs::create_dir_all(&game).unwrap();
            match left {
                "none" => fs::remove_dir(&game).unwrap(),
                "the Save link" => {
                    std::os::unix::fs::symlink(fixture.path(), game.join("Save")).unwrap()
                }
                file => {
                    fs::create_dir_all(game.join(file).parent().unwrap()).unwrap();
                    fs::write(game.join(file), b"left").unwrap();
                    if !file.starts_with("Save/") {
                        std::os::unix::fs::symlink(fixture.path(), game.join("Save")).unwrap();
                    }
                }
            }
            let root = Directory::open(fixture.path()).unwrap();
            let tree = GameTree::managed_on(&root, true);
            let mut checks = [CheckStatus::Missing; CHECK_COUNT];
            for check in [
                AuditCheck::Host,
                AuditCheck::Runtime,
                AuditCheck::Transaction,
            ] {
                checks[check.index()] = CheckStatus::Satisfied;
            }
            checks[AuditCheck::BaseGame.index()] =
                check_base_game(&tree, ActivityEvidence::Idle, false).status;
            checks[AuditCheck::Expansion.index()] =
                check_expansion(&tree, ActivityEvidence::Idle, false).status;
            assert_eq!(
                checks[AuditCheck::BaseGame.index()],
                CheckStatus::Missing,
                "{left}"
            );
            assert_eq!(
                checks[AuditCheck::Expansion.index()],
                CheckStatus::Missing,
                "{left}"
            );
            let entry = !matches!(left, "none" | "the Save link");
            assert_eq!(tree.holds_an_entry(), entry, "{left}");
            assert_eq!(
                classify_installation(
                    &checks,
                    CheckStatus::Missing,
                    started,
                    tree.holds_an_entry()
                ),
                if entry {
                    InstallationStatus::StageInterrupted(InterruptedStage::BaseGame)
                } else {
                    InstallationStatus::NeedsBaseGame
                },
                "{left}"
            );
            // The desktop gives no game entry: there the same checks are the
            // start of an install, as before.
            assert_eq!(
                classify_installation(&checks, CheckStatus::Missing, started, false),
                InstallationStatus::NeedsBaseGame,
                "{left}"
            );
        }
    }

    fn android_host(guest_execution: GuestExecutionEvidence) -> HostEvidence {
        // The Android host facts as the probe maps them, with no Android
        // game environment running.
        HostEvidence {
            os_id: "android".to_owned(),
            os_version: "33".to_owned(),
            os_variant: Some("Retroid Pocket Nova".to_owned()),
            architecture: "aarch64".to_owned(),
            kernel_machine: "aarch64".to_owned(),
            kernel_release: "5.15.153-android13-8".to_owned(),
            display_session: DisplaySession::Unknown,
            guest_execution,
            fex_package: None,
            vulkan_loader: false,
            vulkan_driver: false,
            graphics_execution: None,
            runtime_execution_preflight: RuntimeExecutionEvidence::Unavailable,
        }
    }

    fn reasons(host: &HostEvidence) -> Vec<FindingReason> {
        host_prerequisite_findings(
            host,
            LauncherArtifactEvidence::NativeAarch64StandaloneBinary,
        )
        .into_iter()
        .map(|finding| finding.reason)
        .collect()
    }

    #[test]
    fn the_android_prefix_rejects_a_changed_xinput_bridge() {
        let fixture = tempfile::tempdir().unwrap();
        let installed = fixture.path().join(crate::install::XINPUT_BRIDGE_PATH);
        std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
        let root = Directory::open(fixture.path()).unwrap();
        let status = |root: &Directory| {
            check_xinput_bridge(root).map_or(CheckStatus::Satisfied, |result| result.status)
        };
        // Not installed yet, which the game start does: absent, or Wine's own.
        assert_eq!(status(&root), CheckStatus::Satisfied);
        let mut wine = b"MZ".to_vec();
        wine.resize(0x40, 0);
        wine.extend_from_slice(b"Wine builtin DLL");
        wine.resize(77_931, 0);
        std::fs::write(&installed, &wine).unwrap();
        assert_eq!(status(&root), CheckStatus::Satisfied);
        crate::install::install_xinput_bridge(&root).unwrap();
        assert_eq!(status(&root), CheckStatus::Satisfied);

        let mut changed = std::fs::read(&installed).unwrap();
        changed[0x200] ^= 1;
        std::fs::write(&installed, &changed).unwrap();
        let result = check_xinput_bridge(&root).unwrap();
        assert_eq!(result.status, CheckStatus::Invalid);
        assert_eq!(result.findings[0].reason, FindingReason::DigestMismatch);
        // Any other DLL in its place is rejected too.
        std::fs::write(&installed, b"MZ someone else's xinput1_4.dll").unwrap();
        assert_eq!(status(&root), CheckStatus::Invalid);
        // The game start puts the sealed bridge back.
        crate::install::install_xinput_bridge(&root).unwrap();
        assert_eq!(status(&root), CheckStatus::Satisfied);
    }

    #[test]
    fn the_android_prefix_needs_no_wine_gecko_or_mono() {
        let fixture = tempfile::tempdir().unwrap();
        let prefix = fixture.path().join("prefix");
        std::fs::create_dir_all(prefix.join("drive_c/windows")).unwrap();
        for hive in ["system.reg", "user.reg", "userdef.reg"] {
            std::fs::write(prefix.join(hive), b"WINE REGISTRY").unwrap();
        }
        let root = Directory::open(fixture.path()).unwrap();
        let policy = AuditPolicy::pinned();
        let request = AuditRequest::new(fixture.path().to_owned(), ActivityEvidence::Idle);
        // Neither Gecko's nor Mono's receipt or files: accepted on Android,
        // missing on the desktop, which installs both.
        let (_, android) = check_prefix(&root, &policy, &request, true);
        assert_eq!(android.base, CheckStatus::Satisfied);
        assert_eq!(android.gecko, CheckStatus::Satisfied);
        assert_eq!(android.mono, CheckStatus::Satisfied);
        assert_eq!(android.dotnet, CheckStatus::Missing);
        let (_, desktop) = check_prefix(&root, &policy, &request, false);
        assert_eq!(desktop.gecko, CheckStatus::Missing);
        assert_eq!(desktop.mono, CheckStatus::Missing);
        // A leftover receipt of an older release changes nothing on Android.
        std::fs::write(prefix.join(".pd2launcher-mono.sha256"), b"older").unwrap();
        let (_, android) = check_prefix(&root, &policy, &request, true);
        assert_eq!(android.mono, CheckStatus::Satisfied);
    }

    #[test]
    fn a_missing_android_prefix_is_missing_not_invalid() {
        let fixture = tempfile::tempdir().unwrap();
        let root = Directory::open(fixture.path()).unwrap();
        let policy = AuditPolicy::pinned();
        let request = AuditRequest::new(fixture.path().to_owned(), ActivityEvidence::Idle);
        // No prefix at all: a stop before Wine's first start in a new one.
        for android in [true, false] {
            let (result, components) = check_prefix(&root, &policy, &request, android);
            assert_eq!(result.status, CheckStatus::Missing, "android: {android}");
            assert_eq!(components.base, CheckStatus::Missing);
            assert_eq!(components.dotnet, CheckStatus::Missing);
        }
    }

    #[test]
    fn an_android_host_needs_no_display_session_and_no_fex() {
        // The app's ARM64 Wine: the runtime preflight is its evidence.
        let ready = android_host(GuestExecutionEvidence::AppProvided { page_size_4k: true });
        assert_eq!(reasons(&ready), []);
        // Only the standalone ARM64 Launcher runs inside the app.
        assert_eq!(
            host_prerequisite_findings(
                &ready,
                LauncherArtifactEvidence::NativeX86_64StandaloneBinary
            )
            .into_iter()
            .map(|finding| finding.reason)
            .collect::<Vec<_>>(),
            [FindingReason::LauncherArtifactUnverified]
        );
    }

    #[test]
    fn a_kernel_without_4_kb_pages_is_a_host_finding_on_the_android_host_too() {
        // Wine and Box64 need 4 KB pages: the player is told before setup.
        let pages = |page_size_4k| {
            reasons(&android_host(GuestExecutionEvidence::AppProvided {
                page_size_4k,
            }))
        };
        assert_eq!(pages(false), [FindingReason::PageSizeUnsupported]);
        assert_eq!(pages(true), []);
        // The desktop's ARM64 row keeps its own check, for FEX.
        let desktop = |page_size_4k| HostEvidence {
            os_id: "rocknix".to_owned(),
            display_session: DisplaySession::Wayland,
            ..android_host(GuestExecutionEvidence::Translated {
                interpreter_present: true,
                page_size_4k,
                rootfs_ready: page_size_4k,
                pending: false,
            })
        };
        assert_eq!(
            reasons(&desktop(false)),
            [FindingReason::PageSizeUnsupported]
        );
        assert_eq!(reasons(&desktop(true)), []);
    }

    #[test]
    fn an_uncertified_row_reports_only_its_certification_gap() {
        // The x86-64 row is uncertified in builds for another architecture,
        // which the checks do not run; the same row table shows the gap.
        let host = HostEvidence {
            os_id: "rocknix".to_owned(),
            display_session: DisplaySession::Wayland,
            ..android_host(GuestExecutionEvidence::Translated {
                interpreter_present: true,
                page_size_4k: true,
                rootfs_ready: true,
                pending: false,
            })
        };
        let artifact = LauncherArtifactEvidence::NativeAarch64StandaloneBinary;
        let reasons = |rows: &[HostRow]| {
            row_findings(rows, &host, artifact)
                .into_iter()
                .map(|finding| finding.reason)
                .collect::<Vec<_>>()
        };
        assert_eq!(reasons(&HOST_ROWS), []);
        let uncertified = HostRow {
            certified: false,
            ..HOST_ROWS[0]
        };
        assert_eq!(reasons(&[uncertified]), [FindingReason::HostRowUncertified]);
    }

    #[test]
    fn a_desktop_arm64_host_still_needs_a_display_session() {
        let desktop = HostEvidence {
            os_id: "rocknix".to_owned(),
            ..android_host(GuestExecutionEvidence::Translated {
                interpreter_present: true,
                page_size_4k: true,
                rootfs_ready: true,
                pending: false,
            })
        };
        assert_eq!(
            reasons(&desktop),
            [FindingReason::DisplaySessionUnavailable]
        );
    }

    /// An imported game (Android) is proven by the files its import
    /// checked: no music, speech or video archive is asked for; a game that
    /// Blizzard's installers put there, by all of them.
    #[test]
    fn an_imported_game_is_proven_by_the_files_its_import_checked() {
        let folder = tempfile::TempDir::new().unwrap();
        for name in crate::game_folder::IMPORT_BASE_FILES
            .into_iter()
            .chain(crate::game_folder::IMPORT_EXPANSION_FILES)
        {
            std::fs::write(folder.path().join(name), b"x").unwrap();
        }
        let directory = Directory::open(folder.path()).unwrap();
        let game = GameTree {
            directory: &directory,
            base: "",
            names: BTreeSet::new(),
        };
        let satisfied = |result: CheckResult| result.status == CheckStatus::Satisfied;
        assert!(satisfied(check_base_game(
            &game,
            ActivityEvidence::Idle,
            true
        )));
        assert!(satisfied(check_expansion(
            &game,
            ActivityEvidence::Idle,
            true
        )));
        assert!(!satisfied(check_base_game(
            &game,
            ActivityEvidence::Idle,
            false
        )));
        assert!(!satisfied(check_expansion(
            &game,
            ActivityEvidence::Idle,
            false
        )));
        std::fs::remove_file(folder.path().join("d2char.mpq")).unwrap();
        assert!(!satisfied(check_base_game(
            &game,
            ActivityEvidence::Idle,
            true
        )));
    }

    #[test]
    fn game_file_lookup_prefers_the_exact_name_whatever_the_listing_order() {
        let folder = tempfile::TempDir::new().unwrap();
        let directory = Directory::open(folder.path()).unwrap();
        let lookup = |names: &[&str], file: &str| {
            GameTree {
                directory: &directory,
                base: "",
                names: names.iter().map(|name| (*name).to_owned()).collect(),
            }
            .path(file)
        };

        for names in [
            ["Patch_D2.mpq", "patch_d2.mpq"],
            ["patch_d2.mpq", "Patch_D2.mpq"],
        ] {
            assert_eq!(lookup(&names, "Patch_D2.mpq"), "Patch_D2.mpq");
        }
        assert_eq!(lookup(&["patch_d2.mpq"], "Patch_D2.mpq"), "patch_d2.mpq");
        assert_eq!(
            lookup(&["PATCH_D2.MPQ", "patch_d2.mpq"], "Patch_D2.mpq"),
            "PATCH_D2.MPQ"
        );
        assert_eq!(lookup(&[], "Game.exe"), "Game.exe");
        assert_eq!(
            lookup(&["projectd2"], "ProjectD2/d2gl.json"),
            "projectd2/d2gl.json"
        );
    }

    #[test]
    fn the_android_audit_line_says_a_client_without_a_record_is_not_recorded_yet() {
        let finding = |check, reason, path: Option<&str>| AuditFinding {
            check,
            reason,
            path: path.map(|path| RelativeGamePath::parse(path).expect("test path")),
        };
        assert_eq!(
            android_audit_line(
                InstallationStatus::NeedsClient,
                &[finding(
                    AuditCheck::Client,
                    FindingReason::ManifestUnavailable,
                    None
                )],
            ),
            "NeedsClient: Client NotRecordedYet"
        );
        // Every other finding keeps its check, reason and path.
        assert_eq!(
            android_audit_line(
                InstallationStatus::NeedsClient,
                &[
                    finding(
                        AuditCheck::Client,
                        FindingReason::Missing,
                        Some("ProjectD2/Game.exe")
                    ),
                    finding(
                        AuditCheck::Transaction,
                        FindingReason::UnsafeTransaction,
                        None
                    ),
                ],
            ),
            "NeedsClient: Client Missing ProjectD2/Game.exe; Transaction UnsafeTransaction"
        );
    }
}
