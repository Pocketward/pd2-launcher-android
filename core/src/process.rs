//! Direct Game client supervision.
//!
//! This workflow accepts fresh audit evidence, owns the no-mutation and
//! single-game leases for the process lifetime, and delegates one typed,
//! shell-free request to Host execution.

use std::{
    env,
    ffi::{OsStr, OsString},
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use md5::{Digest as _, Md5};
use rustix::fs::OFlags;

use crate::{
    InstallationStatus, LaunchPreflight, android_host,
    host_execution::{HostExecution, ProcessOutput, ProcessRequest, SupervisedProcess},
    safe_fs::Directory,
};

const CHILD_INSTALLATION_LEASE_FD: i32 = 210;
const CHILD_GAME_LEASE_FD: i32 = 211;
const CHILD_PRELOADER_FD: i32 = 212;
const CHILD_WINE_PROOF_FD: i32 = 213;
const CHILD_GAME_EXECUTABLE_FD: i32 = 214;
const CHILD_MANAGED_ROOT_FD: i32 = 215;
const CHILD_RUNTIME_BIN_FD: i32 = 216;
const CHILD_GL_PROBE_FD: i32 = 217;
const CHILD_WINESERVER_FD: i32 = 218;
/// `wineserver -k` under FEX ends within a second or two; the Android app
/// waits 10 s for the whole `--stop-wine` run.
const STOP_WINE_TIMEOUT: Duration = Duration::from_secs(8);
/// A cold Wine start in the prefix takes a few seconds; the probe itself
/// is instant once OpenGL answers.
const GL_PROBE_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(test)]
const WINDOWS_GAME_EXECUTABLE: &str = "Game.exe";
const PROCESS_OUTPUT_LIMIT: usize = 8 * 1024;
const DIAGNOSTIC_LIMIT: usize = 16 * 1024;
/// Wine DLL overrides of the game launch profile.
/// `winedbg.exe=d`: a crashed game ends instead of waiting on Wine's crash
/// debugger.
pub(crate) const GAME_DLL_OVERRIDES: &str = "winemenubuilder.exe=;winedbg.exe=d";
/// What the Android host's game adds to [`GAME_DLL_OVERRIDES`]. `ddraw=n`:
/// PD2's own DirectDraw wrapper; with Wine's builtin ddraw.dll SGD2FreeRes
/// filters its resolutions to nothing and a character's first game aborts.
/// Every Wine process on Android, the game's too, also goes without
/// `nsiproxy.sys` (`wine_environment::compose`).
const ANDROID_GAME_DLL_OVERRIDES: &str = ";ddraw=n";

/// The game's DLL overrides on the Android host (`android`) or the desktop.
pub(crate) fn game_dll_overrides(android: bool) -> String {
    if android {
        format!("{GAME_DLL_OVERRIDES}{ANDROID_GAME_DLL_OVERRIDES}")
    } else {
        GAME_DLL_OVERRIDES.to_owned()
    }
}

/// What the game's Wine gets from the Launcher (see `wine_environment`):
/// its prefix, wineserver, debug channels, DLL overrides, cache, home,
/// path, and locale, and on ARM64 FEX's strict settings.
pub(crate) fn game_wine_settings(
    home: impl Into<OsString>,
) -> Vec<crate::wine_environment::Setting> {
    use crate::wine_environment::{Kind, Setting};
    let mut settings = vec![
        Setting::new(
            "WINEPREFIX",
            format!("/proc/self/fd/{CHILD_MANAGED_ROOT_FD}/prefix"),
            Kind::Required,
        ),
        // Wine runs the wineserver beside the program it was started as;
        // started through /proc/self/fd/N it would look there.
        Setting::new(
            "WINESERVER",
            format!("/proc/self/fd/{CHILD_RUNTIME_BIN_FD}/wineserver"),
            Kind::Required,
        ),
        Setting::new(
            "WINEDEBUG",
            crate::dev_build::GAME_WINEDEBUG,
            Kind::Preference,
        ),
        Setting::new(
            "WINEDLLOVERRIDES",
            game_dll_overrides(android_host()),
            Kind::Preference,
        ),
        Setting::new("HOME", home, Kind::Default),
        Setting::new("LANG", "C.UTF-8", Kind::Default),
        Setting::new("PATH", "/usr/bin:/usr/sbin", Kind::Default),
        Setting::new(
            "XDG_CACHE_HOME",
            format!(
                "/proc/self/fd/{CHILD_MANAGED_ROOT_FD}/{}",
                crate::persistence::WINE_CACHE_DIRECTORY
            ),
            // The installation's own cache: Wine never uses ~/.cache/wine.
            Kind::Required,
        ),
    ];
    settings.extend(crate::install::fex_strict_settings());
    settings
}

mod d2gl;
mod ddraw;

/// Whether D2GL has not written its configuration yet: the game has never
/// been launched with it, so its first launch opens at the wrong size.
pub(crate) fn first_d2gl_launch_pending(root: &Directory) -> bool {
    crate::game_folder::GameFolder::resolve(root)
        .and_then(|folder| {
            folder
                .open(root)
                .map_err(|_| crate::game_folder::GameFolderError::RecordInvalid)
        })
        .is_ok_and(|folder| d2gl::configuration_absent(&folder))
}

pub(crate) use d2gl::{D2glSetup, GraphicsProfile, pin_dlls as pin_d2gl_dlls};

/// What a launch does to D2GL's configuration in the game `folder` before
/// the game starts. On the desktop D2GL owns its configuration: the
/// Launcher writes nothing and only reports an absent one, so it can explain
/// D2GL's first launch. A Glide launch on the `android` host starts only
/// once `d2gl.json` names the DLLs PD2 ships ([`pin_d2gl_dlls`]); then the
/// Launcher writes the host's other values ([`d2gl::configure`]), which
/// never blocks the launch. No host writes D2GL's window size.
fn prepare_d2gl(
    folder: &Directory,
    options: &GameLaunchOptions,
    android: bool,
) -> Result<D2glSetup, GameLaunchError> {
    if options.renderer != GameRenderer::Glide {
        return Ok(D2glSetup::NotRequested);
    }
    if android {
        pin_d2gl_dlls(folder).map_err(|error| {
            eprintln!("pd2-launcher: game not started, d2gl.json may name other DLLs: {error}");
            GameLaunchError::LaunchPreflightFailed
        })?;
        return Ok(d2gl::configure(folder, options.graphics_profile));
    }
    Ok(if d2gl::configuration_absent(folder) {
        D2glSetup::ConfigurationAbsent
    } else {
        D2glSetup::Unchanged
    })
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum GameRenderer {
    #[default]
    Glide,
    DirectDraw,
}

impl GameRenderer {
    fn argument(self) -> &'static str {
        match self {
            Self::Glide => "-3dfx",
            Self::DirectDraw => "-ddraw",
        }
    }
}

/// Typed settings for the direct Game client profile.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GameLaunchOptions {
    renderer: GameRenderer,
    /// cnc-ddraw's `renderer=` for a DirectDraw launch, or `None` to keep it.
    direct_draw_renderer: Option<&'static str>,
    skip_to_bnet: bool,
    sound_in_background: bool,
    /// The player's own start options, each one argument after the
    /// Launcher's own (the Android app's Custom start options).
    custom_arguments: Vec<String>,
    /// Another program than `ProjectD2/Game.exe` for Play, a path in the game
    /// folder ([`crate::persistence::program_path`]): it starts without
    /// PD2's checks and setup ([`GameSupervisor::start`]). `None`: PD2's own.
    program: Option<String>,
    /// The Android app's D2GL graphics profile for this Play; none on the
    /// desktop.
    graphics_profile: Option<GraphicsProfile>,
    /// Box64's strict x86 memory order for the game on the Android host
    /// ([`crate::box64::Run::Game`]).
    strict_memory_order: bool,
    /// The Android app's Detailed logging for the game's Wine
    /// ([`crate::wine_environment::detailed_settings`]).
    detailed_logging: bool,
}

impl Default for GameLaunchOptions {
    fn default() -> Self {
        Self {
            renderer: GameRenderer::Glide,
            direct_draw_renderer: None,
            skip_to_bnet: true,
            sound_in_background: false,
            custom_arguments: Vec::new(),
            program: None,
            graphics_profile: None,
            strict_memory_order: false,
            detailed_logging: false,
        }
    }
}

impl GameLaunchOptions {
    pub(crate) fn new(renderer: GameRenderer, skip_to_bnet: bool) -> Self {
        Self {
            renderer,
            direct_draw_renderer: None,
            skip_to_bnet,
            sound_in_background: false,
            custom_arguments: Vec::new(),
            program: None,
            graphics_profile: None,
            strict_memory_order: false,
            detailed_logging: false,
        }
    }

    /// How PD2's DirectDraw wrapper draws (cnc-ddraw's `renderer=`).
    pub(crate) fn with_direct_draw_renderer(mut self, renderer: Option<&'static str>) -> Self {
        self.direct_draw_renderer = renderer;
        self
    }

    pub(crate) fn with_sound_in_background(mut self, enabled: bool) -> Self {
        self.sound_in_background = enabled;
        self
    }

    /// The player's own start options, split on whitespace; Game.exe gets
    /// each one as an argument after the Launcher's own, never through a
    /// shell.
    pub(crate) fn with_custom_arguments(mut self, arguments: &str) -> Self {
        self.custom_arguments = arguments.split_whitespace().map(str::to_owned).collect();
        self
    }

    /// The program Play starts, a path in the game folder that
    /// [`crate::persistence::program_path`] accepted; PD2's own
    /// ([`crate::persistence::DEFAULT_GAME_PROGRAM`]) keeps every check.
    pub(crate) fn with_program(mut self, path: &str) -> Self {
        self.program = (path != crate::persistence::DEFAULT_GAME_PROGRAM).then(|| path.to_owned());
        self
    }

    /// The D2GL graphics profile a Glide launch writes into `d2gl.json` on
    /// the Android host when it is not the one last applied.
    pub(crate) fn with_graphics_profile(mut self, profile: GraphicsProfile) -> Self {
        self.graphics_profile = Some(profile);
        self
    }

    /// Whether the game gets Box64's strict x86 memory order on the Android
    /// host (the app's Strict CPU emulation); the desktop ignores it.
    pub(crate) fn with_strict_memory_order(mut self, enabled: bool) -> Self {
        self.strict_memory_order = enabled;
        self
    }

    /// Whether the game's Wine logs in detail (the Android app's Detailed
    /// logging).
    pub(crate) fn with_detailed_logging(mut self, enabled: bool) -> Self {
        self.detailed_logging = enabled;
        self
    }
}

/// Fresh read-only evidence used immediately before process creation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct GameAudit {
    installation: InstallationStatus,
    launch_preflight: LaunchPreflight,
    game_executable_md5: Option<[u8; 16]>,
    runtime: crate::runtime::InstalledRuntime,
}

impl GameAudit {
    pub(crate) fn new(installation: InstallationStatus, launch_preflight: LaunchPreflight) -> Self {
        Self {
            installation,
            launch_preflight,
            game_executable_md5: None,
            runtime: crate::runtime::InstalledRuntime::BuiltIn(crate::runtime::current()),
        }
    }

    /// The runtime the audit accepted and preflighted; the launch executes
    /// exactly this runtime and never re-detects one.
    pub(crate) fn with_runtime(mut self, runtime: crate::runtime::InstalledRuntime) -> Self {
        self.runtime = runtime;
        self
    }

    pub(crate) fn with_game_executable_md5(mut self, digest: [u8; 16]) -> Self {
        self.game_executable_md5 = Some(digest);
        self
    }
}

pub(crate) trait GameAuditor: Send + Sync {
    fn audit_for_launch(
        &self,
        managed_root: &Path,
        retained_root: &Directory,
    ) -> Result<GameAudit, GameAuditFailure>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct GameAuditFailure;

/// Stable failures that retain no adapter strings or absolute paths.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GameLaunchError {
    AlreadyRunning,
    InstallationBusy,
    AuditUnavailable,
    InstallationNotReady,
    LaunchPreflightFailed,
    UnsafeManagedInstallation,
    EnvironmentUnavailable,
    ProcessSpawnFailed,
    ProcessControlFailed,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct GameDiagnostics {
    text: String,
    truncated: bool,
}

impl GameDiagnostics {
    #[cfg(test)]
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    #[cfg(test)]
    pub(crate) fn truncated(&self) -> bool {
        self.truncated
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GameExitReport {
    success: bool,
    code: Option<i32>,
    diagnostics: GameDiagnostics,
}

impl GameExitReport {
    #[cfg(test)]
    pub(crate) fn success(&self) -> bool {
        self.success
    }

    #[cfg(test)]
    pub(crate) fn code(&self) -> Option<i32> {
        self.code
    }

    #[cfg(test)]
    pub(crate) fn diagnostics(&self) -> &GameDiagnostics {
        &self.diagnostics
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum GameState {
    Idle,
    Running,
    Exited(GameExitReport),
    SupervisionFailed,
}

/// Owns exactly one direct Game client process.
pub(crate) struct GameSupervisor {
    auditor: Arc<dyn GameAuditor>,
    host: Arc<dyn HostExecution>,
    active: Option<ActiveGame>,
    state: GameState,
}

impl GameSupervisor {
    pub(crate) fn new(auditor: Arc<dyn GameAuditor>, host: Arc<dyn HostExecution>) -> Self {
        Self {
            auditor,
            host,
            active: None,
            state: GameState::Idle,
        }
    }

    pub(crate) fn state(&self) -> &GameState {
        &self.state
    }

    pub(crate) fn start(
        &mut self,
        managed_root: &Path,
        options: GameLaunchOptions,
    ) -> Result<D2glSetup, GameLaunchError> {
        if self.active.is_some() {
            return Err(GameLaunchError::AlreadyRunning);
        }

        let root = Directory::open(managed_root)
            .map_err(|_| GameLaunchError::UnsafeManagedInstallation)?;
        let paths = GamePaths::validate(managed_root, &root)?;
        let leases = GameLeases::acquire(&root)?;
        // The XInput bridge, for a prefix that lacks it (an older one, or one
        // made after Start over), before the audit checks it.
        if android_host() {
            crate::install::install_xinput_bridge(&root)
                .map_err(|_| GameLaunchError::UnsafeManagedInstallation)?;
        }
        let evidence = self
            .auditor
            .audit_for_launch(&paths.root, &root)
            .map_err(|_| GameLaunchError::AuditUnavailable)?;
        if evidence.installation != InstallationStatus::Ready {
            return Err(GameLaunchError::InstallationNotReady);
        }
        if evidence.launch_preflight != LaunchPreflight::Passed {
            return Err(GameLaunchError::LaunchPreflightFailed);
        }

        // Recheck types after audit while the shared installation lease keeps
        // every conforming mutator excluded.
        paths.validate_launch_directories(&root)?;
        // A folder used in place on the Android host is on the G: drive
        // (`in_place_drive`), linked again should it be missing.
        if let Some(folder) = &paths.in_place_drive {
            match crate::in_place_drive::link(&root, folder) {
                Ok(true) => eprintln!(
                    "pd2-launcher: the G: drive links to {}",
                    folder.parent().unwrap_or(folder).display()
                ),
                Ok(false) => {}
                Err(error) => eprintln!("pd2-launcher: the G: drive was not linked: {error}"),
            }
        }
        // The Android host's game folder is outside the prefix: its link
        // there, made again should it be missing.
        if android_host() && matches!(paths.game_folder, crate::game_folder::GameFolder::Managed) {
            match crate::game_folder::link_game(&root) {
                Ok(true) => eprintln!("pd2-launcher: the prefix's game folder link was made again"),
                Ok(false) => {}
                Err(error) => {
                    eprintln!("pd2-launcher: the game folder's link was not made: {error}")
                }
            }
        }
        // Another program than PD2's starts as it is: PD2's d2gl.json and
        // ddraw.ini stay as they are, and its file is not checked.
        let d2gl_setup = if options.program.is_some() {
            D2glSetup::NotRequested
        } else {
            prepare_d2gl(&paths.folder, &options, android_host())?
        };
        // How DirectDraw mode draws; a failure keeps PD2's own setting.
        if options.program.is_none() {
            if options.renderer == GameRenderer::DirectDraw
                && let Some(renderer) = options.direct_draw_renderer
            {
                match ddraw::set_renderer(&paths.folder, renderer) {
                    // The Android app reads this line ([`ddraw::draws_with_line`]).
                    Ok(changed) => eprintln!("{}", ddraw::draws_with_line(renderer, changed)),
                    Err(error) => {
                        eprintln!("pd2-launcher: ddraw.ini left as it is: {error}");
                    }
                }
            } else {
                // A start without the Software driver's `gdi` after it: the lines from before it again.
                match ddraw::leave_software(&paths.folder) {
                    Ok(true) => eprintln!(
                        "pd2-launcher: ddraw.ini has its renderer= and adjmouse= lines from before the software renderer again"
                    ),
                    Ok(false) => {}
                    Err(error) => {
                        eprintln!("pd2-launcher: ddraw.ini's software values not put back: {error}")
                    }
                }
            }
        }
        let runtime = RuntimeHandles::open(&root, evidence.runtime)?;
        let (program, game) = match &options.program {
            Some(path) => (Path::new(path), open_other_game(&paths, Path::new(path))?),
            None => {
                let expected_game = evidence
                    .game_executable_md5
                    .ok_or(GameLaunchError::LaunchPreflightFailed)?;
                (
                    Path::new(crate::persistence::DEFAULT_GAME_PROGRAM),
                    open_verified_game(&paths, expected_game)?,
                )
            }
        };
        let request = build_game_request(&root, &paths, &options, &leases, runtime, program, game)?;
        let process = self
            .host
            .spawn(request)
            .map_err(|_| GameLaunchError::ProcessSpawnFailed)?;
        let arguments = std::iter::once(options.renderer.argument())
            .chain(options.custom_arguments.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ");
        match &options.program {
            Some(path) => eprintln!(
                "pd2-launcher: game started ({arguments}): {path}, not PD2's program, without PD2's checks"
            ),
            None => eprintln!("pd2-launcher: game started ({arguments})"),
        }
        self.active = Some(ActiveGame {
            process,
            logs: GameLogs::start(&paths.root),
            managed_root: paths.root,
            _leases: leases,
        });
        self.state = GameState::Running;
        Ok(d2gl_setup)
    }

    pub(crate) fn poll(&mut self) -> GameState {
        let Some(active) = self.active.as_mut() else {
            return self.state.clone();
        };
        match active.process.try_wait() {
            Ok(None) => {
                active.logs.print_new(false);
                GameState::Running
            }
            Ok(Some(exit)) => {
                active.logs.print_new(true);
                let diagnostics = render_diagnostics(
                    active.process.output(),
                    &active.managed_root,
                    env::var_os("HOME").as_deref().map(Path::new),
                );
                self.active.take();
                self.state = GameState::Exited(GameExitReport {
                    success: exit.success(),
                    code: exit.code(),
                    diagnostics,
                });
                self.state.clone()
            }
            Err(_) => {
                // A failed observation is not proof that the process exited.
                // Retain ownership and both leases so mutation cannot race a
                // Game client that may still be running.
                self.state = GameState::SupervisionFailed;
                self.state.clone()
            }
        }
    }

    /// The OpenGL renderer the game would draw with: the sealed probe runs
    /// through the same runtime, prefix, and session as the game, under
    /// the same leases, and reports `GL_RENDERER`. It never starts while
    /// the game runs. `None`: the probe ran but got no OpenGL context.
    pub(crate) fn probe_renderer(
        &mut self,
        managed_root: &Path,
        probe: File,
    ) -> Result<Option<String>, GameLaunchError> {
        if self.active.is_some() {
            return Err(GameLaunchError::AlreadyRunning);
        }
        let root = Directory::open(managed_root)
            .map_err(|_| GameLaunchError::UnsafeManagedInstallation)?;
        let paths = GamePaths::validate(managed_root, &root)?;
        let leases = GameLeases::acquire(&root)?;
        let evidence = self
            .auditor
            .audit_for_launch(&paths.root, &root)
            .map_err(|_| GameLaunchError::AuditUnavailable)?;
        if evidence.installation != InstallationStatus::Ready {
            return Err(GameLaunchError::InstallationNotReady);
        }
        if evidence.launch_preflight != LaunchPreflight::Passed {
            return Err(GameLaunchError::LaunchPreflightFailed);
        }
        paths.validate_launch_directories(&root)?;
        let runtime = RuntimeHandles::open(&root, evidence.runtime)?;
        let working_directory = root
            .try_clone()
            .map_err(|_| GameLaunchError::UnsafeManagedInstallation)?;
        // The probe is a sealed memfd with no path of its own. With a Wine
        // session already running, Wine-GE starts an extensionless program
        // in another process, where /proc/self names that process; so Wine
        // is given this process's descriptor, held open until the probe ends.
        let retained = probe
            .try_clone()
            .map_err(|_| GameLaunchError::UnsafeManagedInstallation)?;
        // On the Android host it runs as a checked copy under its own name
        // instead, removed when `copy` drops: Wine on Android 17 cannot start
        // a program by another process's descriptor.
        let copy = if crate::android_host() {
            Some(
                crate::install::android_gl_probe_copy(&root, managed_root, &probe)
                    .map_err(|_| GameLaunchError::UnsafeManagedInstallation)?,
            )
        } else {
            None
        };
        let program = copy.as_ref().map_or_else(
            || format!("/proc/{}/fd/{}", std::process::id(), retained.as_raw_fd()),
            |(program, _)| program.to_string_lossy().into_owned(),
        );
        let request = game_profile_request(
            &root,
            &paths.root,
            &leases,
            runtime,
            &program,
            (&paths.root, working_directory),
            Vec::new(),
            false,
        )?
        .with_inherited_file(probe, CHILD_GL_PROBE_FD)
        .map_err(|_| GameLaunchError::UnsafeManagedInstallation)?;
        let mut process = self
            .host
            .spawn(request)
            .map_err(|_| GameLaunchError::ProcessSpawnFailed)?;
        let deadline = Instant::now() + GL_PROBE_TIMEOUT;
        let exit = loop {
            match process.try_wait() {
                Ok(Some(exit)) => break exit,
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(25)),
                _ => {
                    let _ = process.terminate();
                    let _ = process.wait();
                    return Err(GameLaunchError::ProcessControlFailed);
                }
            }
        };
        drop(retained);
        drop(copy);
        let output = process.output();
        Ok(String::from_utf8_lossy(output.stdout())
            .lines()
            .find_map(|line| line.strip_prefix("renderer="))
            .map(str::trim)
            .filter(|renderer| exit.success() && !renderer.is_empty())
            .map(str::to_owned))
    }

    /// Terminate only the Launcher-owned child; leases remain held until poll
    /// observes completion.
    pub(crate) fn request_stop(&mut self) -> Result<(), GameLaunchError> {
        let Some(active) = self.active.as_mut() else {
            return Ok(());
        };
        active
            .process
            .terminate()
            .map_err(|_| GameLaunchError::ProcessControlFailed)
    }
}

struct ActiveGame {
    process: Box<dyn SupervisedProcess>,
    managed_root: PathBuf,
    logs: GameLogs,
    _leases: GameLeases,
}

struct GamePaths {
    root: PathBuf,
    /// The game folder's own path, which Wine sees.
    game_path: PathBuf,
    /// Which game folder: the Managed one, or a folder used in place.
    game_folder: crate::game_folder::GameFolder,
    /// The game folder: the Managed one, or a folder used in place proven
    /// to be the recorded one.
    folder: Directory,
    /// A folder used in place on the Android host, on the `G:` drive
    /// (`in_place_drive`).
    in_place_drive: Option<PathBuf>,
}

impl GamePaths {
    fn validate(root: &Path, retained: &Directory) -> Result<Self, GameLaunchError> {
        if !root.is_absolute() {
            return Err(GameLaunchError::UnsafeManagedInstallation);
        }
        let folder = crate::game_folder::GameFolder::resolve(retained)
            .map_err(|_| GameLaunchError::UnsafeManagedInstallation)?;
        Ok(Self {
            root: root.to_owned(),
            game_path: folder.host_path(root),
            folder: folder
                .open(retained)
                .map_err(|_| GameLaunchError::UnsafeManagedInstallation)?,
            in_place_drive: crate::in_place_drive::folder(android_host(), &folder),
            game_folder: folder,
        })
    }

    fn validate_launch_directories(&self, root: &Directory) -> Result<(), GameLaunchError> {
        root.open_directory(Path::new("prefix"))
            .map_err(|_| GameLaunchError::UnsafeManagedInstallation)?;
        self.folder
            .open_directory(Path::new("ProjectD2"))
            .map_err(|_| GameLaunchError::UnsafeManagedInstallation)?;
        self.folder
            .open_file(Path::new("ProjectD2/Game.exe"))
            .map(|_| ())
            .map_err(|_| GameLaunchError::UnsafeManagedInstallation)
    }
}

/// Runtime entries retained after the fresh audit. The guest executor starts
/// the exact loader descriptor (`wine-preloader`, or `wine` for a runtime
/// without one). Wine's preloader requires a loader path whose basename is
/// `wine`, so that path is rooted at the retained bin directory while the
/// verified Wine file descriptor remains inherited as proof.
struct RuntimeHandles {
    installed: crate::runtime::InstalledRuntime,
    wine_preloader: File,
    wine: File,
    bin: Directory,
}

impl RuntimeHandles {
    /// Opens the runtime the fresh audit accepted: the pinned runtime, or
    /// the custom-source runtime while the Wine source is custom.
    fn open(
        root: &Directory,
        installed: crate::runtime::InstalledRuntime,
    ) -> Result<Self, GameLaunchError> {
        let bin_path = Path::new("runner").join(installed.directory()).join("bin");
        let wine_preloader = open_executable_beneath(root, bin_path.join(installed.loader_name()))?;
        let wine = open_executable_beneath(root, bin_path.join("wine"))?;
        let bin = root
            .open_directory(&bin_path)
            .map_err(|_| GameLaunchError::UnsafeManagedInstallation)?;
        Ok(Self {
            installed,
            wine_preloader,
            wine,
            bin,
        })
    }
}

fn open_executable_beneath(root: &Directory, path: PathBuf) -> Result<File, GameLaunchError> {
    let descriptor = root
        .open_beneath(&path, OFlags::RDONLY)
        .map_err(|_| GameLaunchError::UnsafeManagedInstallation)?;
    let metadata = descriptor
        .metadata()
        .map_err(|_| GameLaunchError::UnsafeManagedInstallation)?;
    if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
        Ok(descriptor)
    } else {
        Err(GameLaunchError::UnsafeManagedInstallation)
    }
}

fn open_verified_game(paths: &GamePaths, expected: [u8; 16]) -> Result<File, GameLaunchError> {
    let mut game = paths
        .folder
        .open_beneath(Path::new("ProjectD2/Game.exe"), OFlags::RDONLY)
        .map_err(|_| GameLaunchError::UnsafeManagedInstallation)?;
    if !game
        .metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.nlink() == 1)
    {
        return Err(GameLaunchError::UnsafeManagedInstallation);
    }
    let mut hasher = Md5::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = game
            .read(&mut buffer)
            .map_err(|_| GameLaunchError::UnsafeManagedInstallation)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    if <[u8; 16]>::from(hasher.finalize()) != expected {
        return Err(GameLaunchError::LaunchPreflightFailed);
    }
    game.seek(SeekFrom::Start(0))
        .map_err(|_| GameLaunchError::UnsafeManagedInstallation)?;
    Ok(game)
}

/// Another program than PD2's Game.exe, at `program` in the game folder:
/// opened beneath it with no symbolic link on the way, a regular file that
/// starts as a Windows program (`MZ`). Its bytes are not checked.
fn open_other_game(paths: &GamePaths, program: &Path) -> Result<File, GameLaunchError> {
    let mut game = paths
        .folder
        .open_beneath(program, OFlags::RDONLY)
        .map_err(|error| {
            eprintln!(
                "pd2-launcher: game not started, the game program {} does not open in the game folder (missing, or a symbolic link on its way): {error}",
                program.display()
            );
            GameLaunchError::UnsafeManagedInstallation
        })?;
    let mut magic = [0_u8; 2];
    if !game.metadata().is_ok_and(|metadata| metadata.is_file())
        || game.read_exact(&mut magic).is_err()
        || &magic != b"MZ"
    {
        eprintln!(
            "pd2-launcher: game not started, the game program {} is not a Windows program",
            program.display()
        );
        return Err(GameLaunchError::UnsafeManagedInstallation);
    }
    game.seek(SeekFrom::Start(0))
        .map_err(|_| GameLaunchError::UnsafeManagedInstallation)?;
    Ok(game)
}

fn is_plain_directory(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
}

struct GameLeases {
    installation: File,
    game: File,
}

impl GameLeases {
    fn acquire(root: &Directory) -> Result<Self, GameLaunchError> {
        let installation = root
            .open_file(Path::new(".pd2launcher.lock"))
            .map_err(|_| GameLaunchError::InstallationBusy)?;
        File::try_lock_shared(&installation).map_err(|_| GameLaunchError::InstallationBusy)?;

        let game = match root.open_or_create_file(OsStr::new(".pd2launcher.game.lock")) {
            Ok(game) => game,
            Err(error) => {
                let _ = File::unlock(&installation);
                return Err(match error.kind() {
                    io::ErrorKind::AlreadyExists => GameLaunchError::AlreadyRunning,
                    _ => GameLaunchError::InstallationBusy,
                });
            }
        };
        if File::try_lock(&game).is_err() {
            let _ = File::unlock(&installation);
            return Err(GameLaunchError::AlreadyRunning);
        }
        Ok(Self { installation, game })
    }
}

impl Drop for GameLeases {
    fn drop(&mut self) {
        let _ = File::unlock(&self.game);
        let _ = File::unlock(&self.installation);
    }
}

fn build_game_request(
    root: &Directory,
    paths: &GamePaths,
    options: &GameLaunchOptions,
    leases: &GameLeases,
    runtime: RuntimeHandles,
    program: &Path,
    game: File,
) -> Result<ProcessRequest, GameLaunchError> {
    // The program's own folder in the game folder is its working directory
    // (ProjectD2 for PD2's Game.exe), and Wine starts it there by its name.
    let folder = program.parent().unwrap_or(Path::new(""));
    let name = program
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or(GameLaunchError::UnsafeManagedInstallation)?;
    let client_path = paths.game_path.join(folder);
    let project = if folder.as_os_str().is_empty() {
        paths.folder.try_clone()
    } else {
        paths.folder.open_directory(folder)
    }
    .map_err(|_| GameLaunchError::UnsafeManagedInstallation)?;
    // Wine must resolve the verified client from the retained ProjectD2
    // working directory. Passing the executable descriptor as argv[0]
    // makes the Windows-visible image path `Z:\\proc\\self\\fd\\214`;
    // ProjectDiablo then derives an invalid installation root and
    // Battle.net cannot verify the application version. The separately
    // inherited descriptor remains the pre-spawn integrity proof. Every
    // conforming Launcher mutator is excluded by the retained leases, and
    // the hostile same-UID post-proof race is outside the retained mutation
    // boundary.
    let mut request = game_profile_request(
        root,
        &paths.root,
        leases,
        runtime,
        name,
        (&client_path, project),
        crate::box64::settings(
            crate::box64::Run::Game {
                strict_memory_order: options.strict_memory_order,
            },
            crate::android_host(),
        ),
        options.detailed_logging,
    )?
    .with_argument(options.renderer.argument());
    for (name, value) in crate::dev_build::game_locale(root, &paths.root) {
        eprintln!("pd2-launcher: a dev build's debug file sets the game's {name}={value}");
        request = request.with_environment(name, value);
    }
    if options.skip_to_bnet {
        request = request.with_argument("-skiptobnet");
    }
    if options.sound_in_background {
        request = request.with_argument("-sndbkg");
    }
    for argument in &options.custom_arguments {
        request = request.with_argument(argument);
    }
    request
        .with_inherited_file(game, CHILD_GAME_EXECUTABLE_FD)
        .map_err(|_| GameLaunchError::UnsafeManagedInstallation)
}

/// `program` run through the managed runtime exactly as the game is: the
/// same prefix, Wine cache, DLL overrides, session variables, and leases,
/// with `extra` settings on top and, with `detailed`, Detailed logging's.
#[allow(clippy::too_many_arguments)]
fn game_profile_request(
    root: &Directory,
    managed_root: &Path,
    leases: &GameLeases,
    runtime: RuntimeHandles,
    program: &str,
    (working_directory, working_directory_handle): (&Path, Directory),
    extra: Vec<crate::wine_environment::Setting>,
    detailed: bool,
) -> Result<ProcessRequest, GameLaunchError> {
    let home = required_directory_environment("HOME")?;
    // The pinned preloader descriptor is the guest ELF; Host execution's
    // executor decides whether it is translated or executed directly.
    let loader_arguments = runtime
        .installed
        .loader_takes_wine_path()
        .then(|| format!("/proc/self/fd/{CHILD_RUNTIME_BIN_FD}/wine"));
    let mut request = ProcessRequest::guest(format!("/proc/self/fd/{CHILD_PRELOADER_FD}"))
        .with_arguments(loader_arguments)
        .with_argument(program)
        .with_working_directory_handle(
            working_directory,
            working_directory_handle
                .try_clone_file()
                .map_err(|_| GameLaunchError::UnsafeManagedInstallation)?,
        )
        .with_cleared_environment()
        .with_output_limit(PROCESS_OUTPUT_LIMIT)
        .with_detailed_output(detailed);
    let mut settings = game_wine_settings(home);
    settings.extend(extra);
    settings.extend(crate::wine_environment::detailed_settings(
        detailed,
        crate::dev_build::GAME_WINEDEBUG,
    ));
    let environment = crate::wine_environment::compose(env::vars_os(), &settings);
    if let Some(line) = crate::wine_environment::box64_line(program, &environment) {
        eprintln!("{line}");
    }
    for (name, value) in environment {
        request = request.with_environment(name, value);
    }
    // The game folder outside the Android host's prefix is
    // C:\Program Files (x86)\Diablo II to the game, not a folder on the
    // root drive (`game_folder::wine_pwd`).
    if let Some(pwd) = crate::game_folder::wine_pwd(managed_root, working_directory, android_host())
    {
        request = request.with_environment("PWD", pwd);
    }

    request = request
        .with_inherited_file(
            root.try_clone_file()
                .map_err(|_| GameLaunchError::UnsafeManagedInstallation)?,
            CHILD_MANAGED_ROOT_FD,
        )
        .map_err(|_| GameLaunchError::UnsafeManagedInstallation)?;
    request = request
        .with_inherited_file(
            leases
                .installation
                .try_clone()
                .map_err(|_| GameLaunchError::InstallationBusy)?,
            CHILD_INSTALLATION_LEASE_FD,
        )
        .map_err(|_| GameLaunchError::InstallationBusy)?;
    request = request
        .with_inherited_file(
            leases
                .game
                .try_clone()
                .map_err(|_| GameLaunchError::InstallationBusy)?,
            CHILD_GAME_LEASE_FD,
        )
        .map_err(|_| GameLaunchError::InstallationBusy)?;
    request = request
        .with_inherited_file(runtime.wine_preloader, CHILD_PRELOADER_FD)
        .map_err(|_| GameLaunchError::UnsafeManagedInstallation)?;
    request = request
        .with_inherited_file(runtime.wine, CHILD_WINE_PROOF_FD)
        .map_err(|_| GameLaunchError::UnsafeManagedInstallation)?;
    request
        .with_inherited_file(
            runtime
                .bin
                .try_clone_file()
                .map_err(|_| GameLaunchError::UnsafeManagedInstallation)?,
            CHILD_RUNTIME_BIN_FD,
        )
        .map_err(|_| GameLaunchError::UnsafeManagedInstallation)
}

/// `wineserver -k` for the Managed prefix at `managed_root`: Wine's server
/// and every Wine process of the prefix end. It runs the built-in runtime's
/// `wineserver` with the game's Wine environment through `host`, so on the
/// Android host it runs the app's ARM64 Wine like the game itself (the
/// app's `--stop-wine`, when it stops the game environment or cleans up
/// after the engine). Its exit status proves nothing (nonzero when no
/// server runs); it must only run and be reaped in time.
pub(crate) fn stop_wine(
    managed_root: &Path,
    host: &dyn HostExecution,
) -> Result<(), GameLaunchError> {
    let root =
        Directory::open(managed_root).map_err(|_| GameLaunchError::UnsafeManagedInstallation)?;
    let installed = crate::runtime::InstalledRuntime::select(&root, false);
    let bin_path = Path::new("runner").join(installed.directory()).join("bin");
    // Started through its own descriptor, as the installers start it: a
    // direct guest (the Android host's) is checked through the descriptor
    // it starts from, and a name in an inherited folder is not one. The
    // folder goes too, for WINESERVER.
    let wineserver = open_executable_beneath(&root, bin_path.join("wineserver"))?;
    let bin = root
        .open_directory(&bin_path)
        .map_err(|_| GameLaunchError::UnsafeManagedInstallation)?;
    let home = required_directory_environment("HOME")?;
    let mut request = ProcessRequest::guest(format!("/proc/self/fd/{CHILD_WINESERVER_FD}"))
        .with_argument("-k")
        .with_working_directory_handle(
            managed_root,
            root.try_clone_file()
                .map_err(|_| GameLaunchError::UnsafeManagedInstallation)?,
        )
        .with_cleared_environment()
        .with_output_limit(PROCESS_OUTPUT_LIMIT);
    for (name, value) in crate::wine_environment::compose(env::vars_os(), &game_wine_settings(home))
    {
        request = request.with_environment(name, value);
    }
    request = request
        .with_inherited_file(
            root.try_clone_file()
                .map_err(|_| GameLaunchError::UnsafeManagedInstallation)?,
            CHILD_MANAGED_ROOT_FD,
        )
        .map_err(|_| GameLaunchError::UnsafeManagedInstallation)?
        .with_inherited_file(
            bin.try_clone_file()
                .map_err(|_| GameLaunchError::UnsafeManagedInstallation)?,
            CHILD_RUNTIME_BIN_FD,
        )
        .map_err(|_| GameLaunchError::UnsafeManagedInstallation)?
        .with_inherited_file(wineserver, CHILD_WINESERVER_FD)
        .map_err(|_| GameLaunchError::UnsafeManagedInstallation)?;
    let mut process = host
        .spawn(request)
        .map_err(|_| GameLaunchError::ProcessSpawnFailed)?;
    let deadline = Instant::now() + STOP_WINE_TIMEOUT;
    loop {
        match process.try_wait() {
            Ok(Some(_)) => return Ok(()),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(25)),
            _ => {
                let _ = process.terminate();
                let _ = process.wait();
                return Err(GameLaunchError::ProcessControlFailed);
            }
        }
    }
}

/// Whether an OpenGL renderer draws in software: Mesa's llvmpipe,
/// softpipe, and swrast ("Software Rasterizer"), including through Zink
/// on lavapipe.
pub(crate) fn software_renderer(renderer: &str) -> bool {
    let renderer = renderer.to_ascii_lowercase();
    ["llvmpipe", "softpipe", "software rasterizer", "lavapipe"]
        .iter()
        .any(|software| renderer.contains(software))
}

fn required_directory_environment(name: &str) -> Result<OsString, GameLaunchError> {
    let value = env::var_os(name).ok_or(GameLaunchError::EnvironmentUnavailable)?;
    let path = Path::new(&value);
    if path.is_absolute() && is_plain_directory(path) {
        Ok(value)
    } else {
        Err(GameLaunchError::EnvironmentUnavailable)
    }
}

/// The game's own logs (D2GL's, cnc-ddraw's, the newest `D2*.txt`), printed
/// to the Launcher's stderr as they grow, so a session that is killed with
/// the game still running keeps what the game wrote: on some systems
/// (DroidDeck) standard error is the only log anyone can read.
struct GameLogs {
    folder: PathBuf,
    printed: std::collections::BTreeMap<PathBuf, u64>,
    next: Instant,
}

impl GameLogs {
    const EVERY: Duration = Duration::from_secs(3);
    const MAX_BYTES: u64 = 256 * 1024;

    /// What is already in the logs is from earlier runs and is skipped.
    fn start(managed_root: &Path) -> Self {
        let folder = match Directory::open(managed_root)
            .ok()
            .and_then(|root| crate::game_folder::GameFolder::read(&root).ok())
        {
            Some(crate::game_folder::GameFolder::External(external)) => external.path().to_owned(),
            _ => managed_root.join(crate::game_folder::managed_relative(android_host())),
        };
        let mut logs = Self {
            folder,
            printed: std::collections::BTreeMap::new(),
            next: Instant::now() + Self::EVERY,
        };
        for log in logs.files() {
            let length = fs::metadata(&log).map_or(0, |metadata| metadata.len());
            logs.printed.insert(log, length);
        }
        logs
    }

    fn files(&self) -> Vec<PathBuf> {
        let mut logs = Vec::new();
        for directory in [self.folder.join("ProjectD2"), self.folder.clone()] {
            let Ok(entries) = fs::read_dir(&directory) else {
                continue;
            };
            let mut reports = Vec::new();
            for entry in entries.flatten() {
                let lower = entry.file_name().to_string_lossy().to_ascii_lowercase();
                if lower == "d2gl.log" || lower == "ddraw.log" || lower == "cnc-ddraw.log" {
                    logs.push(entry.path());
                } else if lower.starts_with("d2") && lower.ends_with(".txt") {
                    let modified = entry
                        .metadata()
                        .and_then(|metadata| metadata.modified())
                        .ok();
                    reports.push((modified, entry.path()));
                }
            }
            reports.sort();
            logs.extend(reports.pop().map(|(_, path)| path));
        }
        logs
    }

    /// Print what was added since the last look; `now` skips the wait.
    fn print_new(&mut self, now: bool) {
        if !now && Instant::now() < self.next {
            return;
        }
        self.next = Instant::now() + Self::EVERY;
        for log in self.files() {
            let printed = self.printed.get(&log).copied().unwrap_or(0);
            let Ok(mut file) = File::open(&log) else {
                continue;
            };
            let length = file.metadata().map_or(0, |metadata| metadata.len());
            // A log rewritten by this run starts over.
            let from = if length < printed { 0 } else { printed };
            if length == from {
                continue;
            }
            let from = from.max(length.saturating_sub(Self::MAX_BYTES));
            let mut bytes = Vec::new();
            if file.seek(SeekFrom::Start(from)).is_err()
                || file.take(length - from).read_to_end(&mut bytes).is_err()
            {
                continue;
            }
            // Only whole lines; the rest is printed once it is finished.
            let Some(end) = bytes.iter().rposition(|byte| *byte == b'\n') else {
                continue;
            };
            let name = log
                .file_name()
                .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
            for line in String::from_utf8_lossy(&bytes[..=end]).lines() {
                if !line.trim().is_empty() {
                    eprintln!("pd2-launcher: [{name}] {}", game_log_line(line));
                }
            }
            self.printed.insert(log, from + end as u64 + 1);
        }
    }
}

/// A line of the game's own logs as the Launcher's stderr prints it: masked
/// like the report's copy (CD keys, the home folder, the user name, and
/// account, character and game names; a crash report's memory dump keeps its
/// hex words).
fn game_log_line(line: &str) -> String {
    crate::redact::mask_game_identities(&crate::activity::redacted_game_log(line))
}

fn render_diagnostics(
    output: ProcessOutput,
    managed_root: &Path,
    home: Option<&Path>,
) -> GameDiagnostics {
    let clean = |bytes: &[u8]| {
        let mut text = String::from_utf8_lossy(bytes).into_owned();
        text = redact_path(text, managed_root, "[MANAGED_ROOT]");
        if let Some(home) = home.filter(|home| *home != Path::new("/")) {
            text = redact_path(text, home, "[HOME]");
        }
        text.chars()
            .map(|character| {
                if character == '\n' || character == '\t' || !character.is_control() {
                    character
                } else {
                    '\u{fffd}'
                }
            })
            .collect::<String>()
    };
    let stdout = clean(output.stdout());
    let stderr = clean(output.stderr());
    // The headers and the line between the two streams.
    let heading = |stream: &str, name: &str| {
        if stream.is_empty() { 0 } else { name.len() + 2 }
    };
    let overhead = heading(&stdout, "stdout")
        + heading(&stderr, "stderr")
        + usize::from(!stdout.is_empty() && !stderr.is_empty());
    let room = DIAGNOSTIC_LIMIT.saturating_sub(overhead);
    let was_too_long = stdout.len() + stderr.len() > room;
    // When both do not fit, stderr comes first (its end is where a failed
    // game says what happened), and each stream loses its middle, never its
    // end.
    let stderr_room = if was_too_long {
        stderr
            .len()
            .min(room.saturating_sub(stdout.len().min(DIAGNOSTIC_STDOUT_KEPT)))
    } else {
        stderr.len()
    };
    let stdout_room = room.saturating_sub(stderr_room);
    let mut text = String::new();
    append_stream(&mut text, "stdout", &cut_middle(&stdout, stdout_room));
    append_stream(&mut text, "stderr", &cut_middle(&stderr, stderr_room));
    GameDiagnostics {
        text,
        truncated: was_too_long || output.stdout_truncated() || output.stderr_truncated(),
    }
}

/// The diagnostics keep at least this much of stdout when stderr is long.
const DIAGNOSTIC_STDOUT_KEPT: usize = 1024;

/// `text` in at most `limit` bytes: all of it when it fits, else its start
/// and its end with a line between them that says how much is left out.
fn cut_middle(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let marker = |left_out: usize| format!("\n[{left_out} bytes left out]\n");
    // The marker's own length, with the longest count it can name.
    let room = limit.saturating_sub(marker(text.len()).len());
    let mut head = room / 2;
    while !text.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail = text.len() - (room - room / 2);
    while !text.is_char_boundary(tail) {
        tail += 1;
    }
    format!("{}{}{}", &text[..head], marker(tail - head), &text[tail..])
}

/// The whole of one stream's text (already bounded: [`cut_middle`]).
fn append_stream(output: &mut String, name: &str, text: &str) {
    if text.is_empty() {
        return;
    }
    if !output.is_empty() {
        output.push('\n');
    }
    output.push_str(name);
    output.push_str(":\n");
    output.push_str(text);
}

fn redact_path(text: String, path: &Path, replacement: &str) -> String {
    let path = path.as_os_str().to_string_lossy();
    if path.is_empty() {
        text
    } else {
        let wine_path = format!("Z:{}", path.replace('/', "\\"));
        let lower_wine_path = format!("z:{}", path.replace('/', "\\"));
        text.replace(path.as_ref(), replacement)
            .replace(&wine_path, replacement)
            .replace(&lower_wine_path, replacement)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsString,
        io,
        os::unix::fs::{PermissionsExt, symlink},
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
    };

    use tempfile::TempDir;

    use super::*;
    use crate::host_execution::{ProcessExit, ProcessRequest};

    struct Fixture {
        directory: TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().expect("temporary managed root");
            let root = directory.path();
            fs::write(root.join(".pd2launcher.lock"), []).expect("installation lock");
            let runtime = root
                .join("runner")
                .join(crate::runtime::current().identity)
                .join("bin");
            let project_d2 = root.join("prefix/drive_c/Program Files (x86)/Diablo II/ProjectD2");
            fs::create_dir_all(&runtime).expect("runtime directories");
            fs::create_dir_all(&project_d2).expect("game directories");
            for executable in [
                runtime.join(crate::runtime::current().loader_name),
                runtime.join("wine"),
            ] {
                fs::write(&executable, b"test executable").expect("runtime fixture");
                let mut permissions = fs::metadata(&executable).expect("metadata").permissions();
                permissions.set_mode(0o700);
                fs::set_permissions(executable, permissions).expect("executable permissions");
            }
            fs::write(project_d2.join("Game.exe"), b"test game").expect("game fixture");
            Self { directory }
        }

        fn root(&self) -> &Path {
            self.directory.path()
        }

        fn installation_lock(&self) -> File {
            File::options()
                .read(true)
                .write(true)
                .open(self.root().join(".pd2launcher.lock"))
                .expect("open installation lock")
        }
    }

    struct FakeAuditor {
        result: Result<GameAudit, GameAuditFailure>,
        calls: AtomicUsize,
        saw_shared_lease: AtomicBool,
    }

    impl FakeAuditor {
        fn ready() -> Self {
            Self {
                result: Ok(
                    GameAudit::new(InstallationStatus::Ready, LaunchPreflight::Passed)
                        .with_game_executable_md5(Md5::digest(b"test game").into()),
                ),
                calls: AtomicUsize::new(0),
                saw_shared_lease: AtomicBool::new(false),
            }
        }
    }

    impl GameAuditor for FakeAuditor {
        fn audit_for_launch(
            &self,
            _managed_root: &Path,
            retained_root: &Directory,
        ) -> Result<GameAudit, GameAuditFailure> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let lock = retained_root
                .open_file(Path::new(".pd2launcher.lock"))
                .map_err(|_| GameAuditFailure)?;
            self.saw_shared_lease
                .store(File::try_lock(&lock).is_err(), Ordering::Relaxed);
            self.result
        }
    }

    #[derive(Debug)]
    struct RequestSnapshot {
        executable: PathBuf,
        guest: bool,
        arguments: Vec<OsString>,
        working_directory: Option<PathBuf>,
        clears_environment: bool,
        environment: Vec<(OsString, OsString)>,
        output_limit: Option<usize>,
        inherited_descriptors: Vec<i32>,
    }

    impl From<&ProcessRequest> for RequestSnapshot {
        fn from(request: &ProcessRequest) -> Self {
            Self {
                executable: request.executable().to_owned(),
                guest: request.is_guest(),
                arguments: request.arguments().to_vec(),
                working_directory: request.working_directory().map(Path::to_owned),
                clears_environment: request.clears_environment(),
                environment: request.environment().to_vec(),
                output_limit: request.output_limit(),
                inherited_descriptors: request.inherited_file_descriptors().collect(),
            }
        }
    }

    #[derive(Clone, Debug)]
    struct FakeHost {
        snapshot: Arc<Mutex<Option<RequestSnapshot>>>,
        process: Arc<Mutex<FakeProcessState>>,
        fail_spawn: bool,
    }

    impl FakeHost {
        fn new() -> Self {
            Self {
                snapshot: Arc::new(Mutex::new(None)),
                process: Arc::new(Mutex::new(FakeProcessState::default())),
                fail_spawn: false,
            }
        }

        fn failing() -> Self {
            Self {
                fail_spawn: true,
                ..Self::new()
            }
        }
    }

    impl HostExecution for FakeHost {
        fn spawn(&self, request: ProcessRequest) -> io::Result<Box<dyn SupervisedProcess>> {
            if self.fail_spawn {
                return Err(io::Error::other("synthetic spawn failure"));
            }
            *self
                .snapshot
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                Some(RequestSnapshot::from(&request));
            Ok(Box::new(FakeProcess {
                state: Arc::clone(&self.process),
            }))
        }
    }

    struct SubstitutingSpawnHost {
        game: PathBuf,
        observed: Arc<Mutex<Vec<u8>>>,
        process: Arc<Mutex<FakeProcessState>>,
    }

    impl HostExecution for SubstitutingSpawnHost {
        fn spawn(&self, request: ProcessRequest) -> io::Result<Box<dyn SupervisedProcess>> {
            let moved = self.game.with_extension("verified-away.exe");
            fs::rename(&self.game, &moved)?;
            fs::write(&self.game, b"attacker replacement")?;
            let mut descriptor = request.clone_inherited_file(CHILD_GAME_EXECUTABLE_FD)?;
            descriptor.read_to_end(&mut self.observed.lock().unwrap())?;
            Ok(Box::new(FakeProcess {
                state: Arc::clone(&self.process),
            }))
        }
    }

    #[derive(Clone, Debug, Default)]
    struct FakeProcessState {
        exit: Option<ProcessExit>,
        output: ProcessOutput,
        terminate_called: bool,
        wait_fails: bool,
    }

    struct FakeProcess {
        state: Arc<Mutex<FakeProcessState>>,
    }

    impl SupervisedProcess for FakeProcess {
        fn try_wait(&mut self) -> io::Result<Option<ProcessExit>> {
            let state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.wait_fails {
                Err(io::Error::other("synthetic supervision failure"))
            } else {
                Ok(state.exit)
            }
        }

        fn terminate(&mut self) -> io::Result<()> {
            self.state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .terminate_called = true;
            Ok(())
        }

        fn output(&self) -> ProcessOutput {
            self.state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .output
                .clone()
        }
    }

    fn environment_value<'a>(snapshot: &'a RequestSnapshot, name: &str) -> Option<&'a OsString> {
        snapshot
            .environment
            .iter()
            .find_map(|(candidate, value)| (candidate == name).then_some(value))
    }

    fn game_supervisor(auditor: Arc<FakeAuditor>, host: Arc<FakeHost>) -> GameSupervisor {
        GameSupervisor::new(auditor, host)
    }

    /// The renderer probe runs through the game's own profile: the same
    /// loader, prefix, session, and leases, with only the program replaced
    /// by the sealed helper's descriptor. It reads `GL_RENDERER` from the
    /// helper's output and leaves no game running.
    #[test]
    fn the_renderer_probe_runs_like_the_game_and_reads_the_renderer() {
        let fixture = Fixture::new();
        let auditor = Arc::new(FakeAuditor::ready());
        let host = Arc::new(FakeHost::new());
        {
            let mut process = host.process.lock().unwrap();
            process.exit = Some(ProcessExit::new(true, Some(0)));
            process.output = ProcessOutput::new(
                b"renderer=llvmpipe (LLVM 20.1.2, 256 bits)\nvendor=Mesa\nversion=4.5\n".to_vec(),
                Vec::new(),
                false,
                false,
            );
        }
        let mut supervisor = game_supervisor(Arc::clone(&auditor), Arc::clone(&host));
        let probe = tempfile::tempfile().unwrap();

        let renderer = supervisor
            .probe_renderer(fixture.root(), probe)
            .expect("the probe runs");

        assert_eq!(
            renderer.as_deref(),
            Some("llvmpipe (LLVM 20.1.2, 256 bits)")
        );
        assert!(auditor.saw_shared_lease.load(Ordering::Relaxed));
        assert_eq!(supervisor.state(), &GameState::Idle);
        let snapshots = host.snapshot.lock().unwrap();
        let request = snapshots.as_ref().expect("typed request was captured");
        let loader = crate::runtime::current()
            .loader_takes_wine_path
            .then(|| OsString::from(format!("/proc/self/fd/{CHILD_RUNTIME_BIN_FD}/wine")));
        let (probe_path, loader_arguments) = request.arguments.split_last().unwrap();
        assert_eq!(loader_arguments, loader.as_slice());
        // This process's retained descriptor, never the child's /proc/self.
        assert!(Path::new(probe_path).starts_with(format!("/proc/{}/fd", std::process::id())));
        assert_eq!(request.working_directory.as_deref(), Some(fixture.root()));
        assert_eq!(
            environment_value(request, "WINEDLLOVERRIDES"),
            Some(&OsString::from(GAME_DLL_OVERRIDES))
        );
        assert_eq!(
            request.inherited_descriptors,
            [
                CHILD_MANAGED_ROOT_FD,
                CHILD_INSTALLATION_LEASE_FD,
                CHILD_GAME_LEASE_FD,
                CHILD_PRELOADER_FD,
                CHILD_WINE_PROOF_FD,
                CHILD_RUNTIME_BIN_FD,
                CHILD_GL_PROBE_FD,
            ]
        );
        drop(snapshots);
        // The leases were released: the game can start right after.
        supervisor
            .start(fixture.root(), GameLaunchOptions::default())
            .expect("the game starts after the probe");
        // And no probe runs while it does.
        assert_eq!(
            supervisor.probe_renderer(fixture.root(), tempfile::tempfile().unwrap()),
            Err(GameLaunchError::AlreadyRunning)
        );
    }

    #[test]
    fn a_probe_without_an_opengl_context_reports_no_renderer() {
        let fixture = Fixture::new();
        let host = Arc::new(FakeHost::new());
        host.process.lock().unwrap().exit = Some(ProcessExit::new(false, Some(32)));
        let mut supervisor = game_supervisor(Arc::new(FakeAuditor::ready()), host);
        assert_eq!(
            supervisor.probe_renderer(fixture.root(), tempfile::tempfile().unwrap()),
            Ok(None)
        );
    }

    #[test]
    fn software_renderers_are_mesas_cpu_drivers() {
        for software in [
            "llvmpipe (LLVM 20.1.2, 256 bits)",
            "softpipe",
            "Software Rasterizer",
            "zink Vulkan 1.3(llvmpipe (LLVM 17.0.6, 256 bits) (MESA_LLVMPIPE))",
        ] {
            assert!(software_renderer(software), "{software}");
        }
        for hardware in [
            "NVIDIA GeForce RTX 5090/PCIe/SSE2",
            "AMD Radeon RX 7900 XTX (radeonsi, navi31, LLVM 19.1.7, DRM 3.61, 6.14.0)",
            "Mesa Intel(R) UHD Graphics 620 (KBL GT2)",
            "virgl (NVIDIA GeForce RTX 5090/PCIe/SSE2)",
        ] {
            assert!(!software_renderer(hardware), "{hardware}");
        }
    }

    #[test]
    fn ready_launch_uses_the_verified_canonical_game_profile() {
        let fixture = Fixture::new();
        let auditor = Arc::new(FakeAuditor::ready());
        let host = Arc::new(FakeHost::new());
        let mut supervisor = game_supervisor(Arc::clone(&auditor), Arc::clone(&host));

        supervisor
            .start(fixture.root(), GameLaunchOptions::default())
            .expect("Ready game starts");

        assert_eq!(supervisor.state(), &GameState::Running);
        assert_eq!(auditor.calls.load(Ordering::Relaxed), 1);
        assert!(auditor.saw_shared_lease.load(Ordering::Relaxed));
        let snapshots = host
            .snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let request = snapshots.as_ref().expect("typed request was captured");
        assert!(request.guest);
        assert_eq!(
            request.executable,
            PathBuf::from(format!("/proc/self/fd/{CHILD_PRELOADER_FD}"))
        );
        let loader = crate::runtime::current()
            .loader_takes_wine_path
            .then(|| OsString::from(format!("/proc/self/fd/{CHILD_RUNTIME_BIN_FD}/wine")));
        assert_eq!(
            request.arguments,
            loader
                .into_iter()
                .chain([
                    OsString::from(WINDOWS_GAME_EXECUTABLE),
                    OsString::from("-3dfx"),
                    OsString::from("-skiptobnet"),
                ])
                .collect::<Vec<_>>()
        );
        assert_eq!(
            request.working_directory.as_deref(),
            Some(
                fixture
                    .root()
                    .join("prefix/drive_c/Program Files (x86)/Diablo II/ProjectD2")
                    .as_path()
            )
        );
        assert!(request.clears_environment);
        assert_eq!(request.output_limit, Some(PROCESS_OUTPUT_LIMIT));
        assert_eq!(
            environment_value(request, "WINEPREFIX"),
            Some(&OsString::from(format!(
                "/proc/self/fd/{CHILD_MANAGED_ROOT_FD}/prefix"
            )))
        );
        // Wine never uses the user's ~/.cache/wine.
        assert_eq!(
            environment_value(request, "XDG_CACHE_HOME"),
            Some(&OsString::from(format!(
                "/proc/self/fd/{CHILD_MANAGED_ROOT_FD}/cache/xdg"
            )))
        );
        // The Launcher sets the locale; the host's does not reach Wine.
        assert_eq!(
            environment_value(request, "LANG"),
            Some(&OsString::from("C.UTF-8"))
        );
        assert!(environment_value(request, "DOTNET_ROOT").is_none());
        assert!(environment_value(request, "PROTONPATH").is_none());
        assert_eq!(
            request.inherited_descriptors,
            [
                CHILD_MANAGED_ROOT_FD,
                CHILD_INSTALLATION_LEASE_FD,
                CHILD_GAME_LEASE_FD,
                CHILD_PRELOADER_FD,
                CHILD_WINE_PROOF_FD,
                CHILD_RUNTIME_BIN_FD,
                CHILD_GAME_EXECUTABLE_FD,
            ]
        );
    }

    #[test]
    fn game_path_substitution_after_audit_cannot_reach_spawn() {
        struct SubstitutingAuditor;

        impl GameAuditor for SubstitutingAuditor {
            fn audit_for_launch(
                &self,
                managed_root: &Path,
                _retained_root: &Directory,
            ) -> Result<GameAudit, GameAuditFailure> {
                fs::write(
                    managed_root
                        .join("prefix/drive_c/Program Files (x86)/Diablo II/ProjectD2/Game.exe"),
                    b"substituted game",
                )
                .map_err(|_| GameAuditFailure)?;
                Ok(
                    GameAudit::new(InstallationStatus::Ready, LaunchPreflight::Passed)
                        .with_game_executable_md5(Md5::digest(b"test game").into()),
                )
            }
        }

        let fixture = Fixture::new();
        let host = Arc::new(FakeHost::new());
        let mut supervisor = GameSupervisor::new(Arc::new(SubstitutingAuditor), host.clone());
        assert_eq!(
            supervisor.start(fixture.root(), GameLaunchOptions::default()),
            Err(GameLaunchError::LaunchPreflightFailed)
        );
        assert!(host.snapshot.lock().unwrap().is_none());
    }

    #[test]
    fn canonical_game_launch_retains_the_verified_descriptor_proof() {
        let fixture = Fixture::new();
        let game = fixture
            .root()
            .join("prefix/drive_c/Program Files (x86)/Diablo II/ProjectD2/Game.exe");
        let observed = Arc::new(Mutex::new(Vec::new()));
        let host = Arc::new(SubstitutingSpawnHost {
            game: game.clone(),
            observed: Arc::clone(&observed),
            process: Arc::new(Mutex::new(FakeProcessState::default())),
        });
        let mut supervisor = GameSupervisor::new(Arc::new(FakeAuditor::ready()), host);

        supervisor
            .start(fixture.root(), GameLaunchOptions::default())
            .unwrap();
        assert_eq!(&*observed.lock().unwrap(), b"test game");
        assert_eq!(fs::read(game).unwrap(), b"attacker replacement");
    }

    #[test]
    fn hardlinked_game_executable_is_not_an_accepted_execution_identity() {
        let fixture = Fixture::new();
        let game = fixture
            .root()
            .join("prefix/drive_c/Program Files (x86)/Diablo II/ProjectD2/Game.exe");
        fs::hard_link(&game, fixture.root().join("game-hardlink.exe")).unwrap();
        let host = Arc::new(FakeHost::new());
        let mut supervisor = game_supervisor(Arc::new(FakeAuditor::ready()), host.clone());

        assert_eq!(
            supervisor.start(fixture.root(), GameLaunchOptions::default()),
            Err(GameLaunchError::UnsafeManagedInstallation)
        );
        assert!(host.snapshot.lock().unwrap().is_none());
    }

    #[test]
    fn launch_settings_select_directdraw_and_background_sound_without_online_shortcut() {
        let fixture = Fixture::new();
        let auditor = Arc::new(FakeAuditor::ready());
        let host = Arc::new(FakeHost::new());
        let mut supervisor = game_supervisor(auditor, Arc::clone(&host));

        supervisor
            .start(
                fixture.root(),
                GameLaunchOptions::new(GameRenderer::DirectDraw, false)
                    .with_sound_in_background(true),
            )
            .expect("Ready game starts");

        let snapshots = host
            .snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let request = snapshots.as_ref().expect("typed request was captured");
        let arguments = &request.arguments;
        assert!(arguments.iter().any(|argument| argument == "-ddraw"));
        // The desktop's overrides are unchanged.
        assert_eq!(
            environment_value(request, "WINEDLLOVERRIDES"),
            Some(&OsString::from(GAME_DLL_OVERRIDES))
        );
        assert!(arguments.iter().any(|argument| argument == "-sndbkg"));
        assert!(!arguments.iter().any(|argument| argument == "-skiptobnet"));
    }

    #[test]
    fn custom_arguments_follow_the_launchers_own_as_separate_arguments() {
        let fixture = Fixture::new();
        let host = Arc::new(FakeHost::new());
        let mut supervisor = game_supervisor(Arc::new(FakeAuditor::ready()), Arc::clone(&host));

        supervisor
            .start(
                fixture.root(),
                GameLaunchOptions::default()
                    .with_sound_in_background(true)
                    .with_custom_arguments("-direct   -txt"),
            )
            .expect("Ready game starts");

        let snapshots = host
            .snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let arguments = &snapshots.as_ref().expect("typed request").arguments;
        assert_eq!(
            arguments[arguments.len() - 5..],
            ["-3dfx", "-skiptobnet", "-sndbkg", "-direct", "-txt"].map(OsString::from)
        );
    }

    #[test]
    fn another_game_program_starts_from_its_own_folder_without_pd2s_checks() {
        let fixture = Fixture::new();
        let game = fixture
            .root()
            .join("prefix/drive_c/Program Files (x86)/Diablo II");
        fs::create_dir_all(game.join("ZYLPD2")).unwrap();
        fs::write(game.join("ZYLPD2/Client.exe"), b"MZ other client").unwrap();
        // PD2's own Game.exe does not match the audit: it is not used.
        fs::write(game.join("ProjectD2/Game.exe"), b"changed game").unwrap();
        let host = Arc::new(FakeHost::new());
        let mut supervisor = game_supervisor(Arc::new(FakeAuditor::ready()), Arc::clone(&host));

        let started = supervisor.start(
            fixture.root(),
            GameLaunchOptions::default()
                .with_program("ZYLPD2/Client.exe")
                .with_custom_arguments("-direct"),
        );

        assert_eq!(started, Ok(D2glSetup::NotRequested));
        let snapshots = host
            .snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let request = snapshots.as_ref().expect("typed request");
        assert_eq!(
            request.arguments[request.arguments.len() - 4..],
            ["Client.exe", "-3dfx", "-skiptobnet", "-direct"].map(OsString::from)
        );
        assert_eq!(
            request.working_directory.as_deref(),
            Some(game.join("ZYLPD2").as_path())
        );
    }

    #[test]
    fn another_game_program_must_be_a_windows_program_inside_the_game_folder() {
        let fixture = Fixture::new();
        let game = fixture
            .root()
            .join("prefix/drive_c/Program Files (x86)/Diablo II");
        fs::write(game.join("Script.exe"), b"#!/bin/sh").unwrap();
        let outside = fixture.root().join("Outside.exe");
        fs::write(&outside, b"MZ outside").unwrap();
        std::os::unix::fs::symlink(&outside, game.join("Linked.exe")).unwrap();
        for program in ["Missing.exe", "Script.exe", "Linked.exe"] {
            let host = Arc::new(FakeHost::new());
            let mut supervisor = game_supervisor(Arc::new(FakeAuditor::ready()), Arc::clone(&host));
            assert_eq!(
                supervisor.start(
                    fixture.root(),
                    GameLaunchOptions::default().with_program(program)
                ),
                Err(GameLaunchError::UnsafeManagedInstallation),
                "{program}"
            );
            assert!(host.snapshot.lock().unwrap().is_none(), "{program}");
        }
    }

    #[test]
    fn stop_wine_runs_the_runtimes_wineserver_with_the_games_wine_environment() {
        let fixture = Fixture::new();
        let wineserver = fixture
            .root()
            .join("runner")
            .join(crate::runtime::current().identity)
            .join("bin/wineserver");
        fs::write(&wineserver, b"test executable").expect("wineserver fixture");
        fs::set_permissions(&wineserver, fs::Permissions::from_mode(0o700))
            .expect("executable permissions");
        let host = Arc::new(FakeHost::new());
        // No server was running: `-k` exits nonzero, which proves nothing.
        host.process.lock().unwrap().exit = Some(ProcessExit::new(false, Some(1)));
        stop_wine(fixture.root(), host.as_ref()).expect("wineserver -k ran");
        let snapshots = host
            .snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let request = snapshots.as_ref().expect("typed request was captured");
        assert!(request.guest);
        // Its own descriptor: the only form a direct guest's check can open.
        assert_eq!(
            request.executable,
            PathBuf::from(format!("/proc/self/fd/{CHILD_WINESERVER_FD}"))
        );
        assert_eq!(request.arguments, [OsString::from("-k")]);
        // The game's prefix and wineserver, through the inherited folders.
        assert_eq!(
            environment_value(request, "WINEPREFIX"),
            Some(&OsString::from(format!(
                "/proc/self/fd/{CHILD_MANAGED_ROOT_FD}/prefix"
            )))
        );
        assert_eq!(
            environment_value(request, "WINESERVER"),
            Some(&OsString::from(format!(
                "/proc/self/fd/{CHILD_RUNTIME_BIN_FD}/wineserver"
            )))
        );
        assert_eq!(
            request.inherited_descriptors,
            [
                CHILD_MANAGED_ROOT_FD,
                CHILD_RUNTIME_BIN_FD,
                CHILD_WINESERVER_FD
            ]
        );
    }

    #[test]
    fn stop_wine_needs_the_runtimes_wineserver() {
        // The fixture's runtime has no wineserver.
        let fixture = Fixture::new();
        let host = Arc::new(FakeHost::new());
        assert_eq!(
            stop_wine(fixture.root(), host.as_ref()),
            Err(GameLaunchError::UnsafeManagedInstallation)
        );
        assert!(host.snapshot.lock().unwrap().is_none());
    }

    /// The direct executor's host (the Android host's), noting whether the
    /// last start failed on a program that was busy (ETXTBSY).
    struct DirectHost {
        host: crate::host_execution::SystemHostExecution,
        busy: AtomicBool,
    }

    impl HostExecution for DirectHost {
        fn spawn(&self, request: ProcessRequest) -> io::Result<Box<dyn SupervisedProcess>> {
            self.host.spawn(request).inspect_err(|error| {
                let busy = rustix::io::Errno::TXTBSY.raw_os_error();
                self.busy
                    .store(error.raw_os_error() == Some(busy), Ordering::Release);
            })
        }
    }

    #[test]
    fn stop_wine_starts_wineserver_through_the_direct_executors_check() {
        // The direct executor checks the image through the descriptor it
        // starts from (`direct_guest_check`), the check that refused a name
        // in an inherited folder on the phones. A native program stands in
        // for the app's ARM64 wineserver, so a build machine runs it.
        let fixture = Fixture::new();
        let wineserver = fixture
            .root()
            .join("runner")
            .join(crate::runtime::current().identity)
            .join("bin/wineserver");
        fs::copy("/usr/bin/true", &wineserver).expect("a native program as wineserver");
        let host = DirectHost {
            host: crate::host_execution::SystemHostExecution::new(
                crate::host_execution::GuestExecutor::Direct,
            ),
            busy: AtomicBool::new(false),
        };
        // A child another test forked while the copy was being written keeps
        // it open for writing until that child execs (ETXTBSY): only then
        // is it started again.
        let mut attempts = 0;
        loop {
            match stop_wine(fixture.root(), &host) {
                Ok(()) => break,
                Err(GameLaunchError::ProcessSpawnFailed)
                    if host.busy.swap(false, Ordering::AcqRel) && attempts < 50 =>
                {
                    attempts += 1;
                    thread::sleep(Duration::from_millis(20));
                }
                other => panic!("wineserver -k did not run: {other:?}"),
            }
        }
        // An image the check does not know never starts, not even through
        // the shell.
        let marker = fixture.root().join("shell-ran");
        fs::write(&wineserver, format!("touch '{}'\n", marker.display())).unwrap();
        assert_eq!(
            stop_wine(fixture.root(), &host),
            Err(GameLaunchError::ProcessSpawnFailed)
        );
        assert!(!marker.exists());
    }

    #[test]
    fn the_desktop_game_never_gets_box64_settings() {
        let fixture = Fixture::new();
        let host = Arc::new(FakeHost::new());
        let mut supervisor = game_supervisor(Arc::new(FakeAuditor::ready()), Arc::clone(&host));

        supervisor
            .start(
                fixture.root(),
                GameLaunchOptions::default().with_strict_memory_order(true),
            )
            .expect("Ready game starts");

        let snapshots = host
            .snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let request = snapshots.as_ref().expect("typed request was captured");
        let box64 = request
            .environment
            .iter()
            .filter(|(name, _)| name.to_string_lossy().starts_with("BOX64_"))
            .collect::<Vec<_>>();
        assert!(box64.is_empty(), "{box64:?}");
    }

    #[test]
    fn a_play_with_detailed_logging_gets_its_wine_channels_and_box64s_log() {
        for detailed in [false, true] {
            let fixture = Fixture::new();
            let host = Arc::new(FakeHost::new());
            let mut supervisor = game_supervisor(Arc::new(FakeAuditor::ready()), Arc::clone(&host));
            supervisor
                .start(
                    fixture.root(),
                    GameLaunchOptions::default().with_detailed_logging(detailed),
                )
                .expect("Ready game starts");
            let snapshots = host
                .snapshot
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let request = snapshots.as_ref().expect("typed request was captured");
            let winedebug = environment_value(request, "WINEDEBUG")
                .map(|value| value.to_string_lossy().into_owned())
                .unwrap_or_default();
            assert_eq!(winedebug.contains("+winsock"), detailed, "{winedebug}");
            assert_eq!(
                environment_value(request, "BOX64_LOG").is_some_and(|value| value == "1"),
                detailed
            );
        }
    }

    #[test]
    fn only_the_android_game_loads_pd2s_own_ddraw() {
        // PD2's own ddraw.dll, never Wine's.
        assert_eq!(
            game_dll_overrides(true),
            "winemenubuilder.exe=;winedbg.exe=d;ddraw=n"
        );
        assert_eq!(
            game_dll_overrides(false),
            "winemenubuilder.exe=;winedbg.exe=d"
        );
    }

    #[test]
    fn only_an_android_glide_launch_pins_d2gls_dlls_and_it_waits_for_them() {
        const PLANTED: &str = r#"{"screen": {"window_size_width": 800, "window_size_height": 600}, "other": {"load_dlls_late": "Save/notes.txt"}}"#;
        let game = TempDir::new().unwrap();
        let config = game.path().join("ProjectD2/d2gl.json");
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        let folder = Directory::open(game.path()).unwrap();
        let glide = GameLaunchOptions::default();
        let direct_draw = GameLaunchOptions::new(GameRenderer::DirectDraw, true);
        let stored = || -> serde_json::Value {
            serde_json::from_slice(&fs::read(&config).unwrap()).unwrap()
        };

        fs::write(&config, PLANTED).unwrap();
        assert_eq!(
            prepare_d2gl(&folder, &direct_draw, true),
            Ok(D2glSetup::NotRequested)
        );
        assert_eq!(fs::read_to_string(&config).unwrap(), PLANTED);
        assert_eq!(
            prepare_d2gl(&folder, &glide, false),
            Ok(D2glSetup::Unchanged)
        );
        assert_eq!(fs::read_to_string(&config).unwrap(), PLANTED);

        // The Android values follow the pin and keep its values; the window
        // size stays.
        assert_eq!(prepare_d2gl(&folder, &glide, true), Ok(D2glSetup::Written));
        assert_eq!(stored()["other"]["load_dlls_late"], "SGD2FreeRes.dll");
        assert_eq!(stored()["other"]["load_dlls_early"], "");
        assert_eq!(stored()["screen"]["unlock_cursor"], true);
        assert_eq!(stored()["screen"]["window_size_width"], 800);
        assert_eq!(stored()["screen"]["window_size_height"], 600);

        // Only a Glide launch on the Android host asks D2GL for OpenGL 3.3:
        // DirectDraw (any of its renderers, gdi too) and the desktop leave
        // the saved version as it is.
        const SAVED_4_6: &str = r#"{"screen": {"unlock_cursor": true, "window_fullscreen": true, "auto_minimize": false}, "other": {"gl_ver_major": 4, "gl_ver_minor": 6, "load_dlls_early": "", "load_dlls_late": "SGD2FreeRes.dll"}}"#;
        fs::write(&config, SAVED_4_6).unwrap();
        for renderer in [None, Some("gdi"), Some("opengl")] {
            assert_eq!(
                prepare_d2gl(
                    &folder,
                    &direct_draw.clone().with_direct_draw_renderer(renderer),
                    true
                ),
                Ok(D2glSetup::NotRequested)
            );
            assert_eq!(fs::read_to_string(&config).unwrap(), SAVED_4_6);
        }
        assert_eq!(
            prepare_d2gl(&folder, &glide, false),
            Ok(D2glSetup::Unchanged)
        );
        assert_eq!(fs::read_to_string(&config).unwrap(), SAVED_4_6);
        assert_eq!(prepare_d2gl(&folder, &glide, true), Ok(D2glSetup::Written));
        assert_eq!(
            fs::read_to_string(&config).unwrap(),
            SAVED_4_6.replace("4, \"gl_ver_minor\": 6", "3, \"gl_ver_minor\": 3")
        );
        assert_eq!(
            prepare_d2gl(&folder, &glide, true),
            Ok(D2glSetup::Unchanged)
        );

        // A configuration that cannot be checked keeps the game from starting.
        fs::remove_file(&config).unwrap();
        fs::create_dir(&config).unwrap();
        assert_eq!(
            prepare_d2gl(&folder, &glide, true),
            Err(GameLaunchError::LaunchPreflightFailed)
        );
        assert_eq!(
            prepare_d2gl(&folder, &glide, false),
            Ok(D2glSetup::Unchanged)
        );
    }

    #[test]
    fn the_desktop_never_changes_d2gl_json() {
        // D2GL sizes a full-screen game from the monitor, so its stored
        // window size, D2GL's default or the player's, stays as it is, as
        // does every other value, with or without a graphics profile.
        const IMPORTED: &str = r#"{"screen": {"window_size_width": 3840, "window_size_height": 2160, "unlock_cursor": false, "window_fullscreen": false}, "graphics": {"fxaa": true}}"#;
        for config in [
            "{\"screen\": {\"window_size_width\": 800, \"window_size_height\": 600, \"window_fullscreen\": true}}",
            IMPORTED,
            "{ not json",
        ] {
            for options in [
                GameLaunchOptions::default(),
                GameLaunchOptions::default().with_graphics_profile(GraphicsProfile::Performance),
            ] {
                let fixture = Fixture::new();
                let path = fixture
                    .root()
                    .join("prefix/drive_c/Program Files (x86)/Diablo II/ProjectD2/d2gl.json");
                fs::write(&path, config).expect("d2gl fixture");
                let host = Arc::new(FakeHost::new());
                let mut supervisor = game_supervisor(Arc::new(FakeAuditor::ready()), host);

                assert_eq!(
                    supervisor.start(fixture.root(), options),
                    Ok(D2glSetup::Unchanged)
                );
                assert_eq!(fs::read_to_string(&path).expect("read d2gl"), config);
                assert!(
                    !path
                        .parent()
                        .unwrap()
                        .join(".pd2launcher/d2gl-profile")
                        .exists()
                );
            }
        }
    }

    #[test]
    fn glide_launch_without_d2gl_configuration_reports_its_absence() {
        for (options, expected) in [
            (GameLaunchOptions::default(), D2glSetup::ConfigurationAbsent),
            (
                GameLaunchOptions::new(GameRenderer::DirectDraw, true),
                D2glSetup::NotRequested,
            ),
        ] {
            let fixture = Fixture::new();
            let config = fixture
                .root()
                .join("prefix/drive_c/Program Files (x86)/Diablo II/ProjectD2/d2gl.json");
            let host = Arc::new(FakeHost::new());
            let mut supervisor = game_supervisor(Arc::new(FakeAuditor::ready()), host);

            assert_eq!(supervisor.start(fixture.root(), options), Ok(expected));
            assert!(!config.exists(), "the Launcher never creates d2gl.json");
        }
    }

    #[test]
    fn running_game_blocks_second_launch_and_installation_mutation() {
        let fixture = Fixture::new();
        let auditor = Arc::new(FakeAuditor::ready());
        let host = Arc::new(FakeHost::new());
        let mut supervisor = game_supervisor(Arc::clone(&auditor), Arc::clone(&host));
        supervisor
            .start(fixture.root(), GameLaunchOptions::default())
            .expect("first launch");

        assert_eq!(
            supervisor.start(fixture.root(), GameLaunchOptions::default()),
            Err(GameLaunchError::AlreadyRunning)
        );
        let mutation_lock = fixture.installation_lock();
        assert!(File::try_lock(&mutation_lock).is_err());

        let other_auditor = Arc::new(FakeAuditor::ready());
        let other_host = Arc::new(FakeHost::new());
        let mut other = game_supervisor(other_auditor, other_host);
        assert_eq!(
            other.start(fixture.root(), GameLaunchOptions::default()),
            Err(GameLaunchError::AlreadyRunning)
        );
    }

    #[test]
    fn active_mutation_and_failed_preflight_block_spawn_and_release_leases() {
        let fixture = Fixture::new();
        let mutation_lock = fixture.installation_lock();
        File::try_lock(&mutation_lock).expect("exclusive mutator lease");
        let auditor = Arc::new(FakeAuditor::ready());
        let host = Arc::new(FakeHost::new());
        let mut supervisor = game_supervisor(Arc::clone(&auditor), Arc::clone(&host));
        assert_eq!(
            supervisor.start(fixture.root(), GameLaunchOptions::default()),
            Err(GameLaunchError::InstallationBusy)
        );
        File::unlock(&mutation_lock).expect("release mutator lease");

        let failed_auditor = Arc::new(FakeAuditor {
            result: Ok(GameAudit::new(
                InstallationStatus::Ready,
                LaunchPreflight::Failed,
            )),
            calls: AtomicUsize::new(0),
            saw_shared_lease: AtomicBool::new(false),
        });
        let mut supervisor = game_supervisor(failed_auditor, Arc::clone(&host));
        assert_eq!(
            supervisor.start(fixture.root(), GameLaunchOptions::default()),
            Err(GameLaunchError::LaunchPreflightFailed)
        );
        assert!(
            host.snapshot
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_none()
        );
        File::try_lock(&mutation_lock).expect("failed start releases shared lease");

        File::unlock(&mutation_lock).expect("release test lease");
        let not_ready_auditor = Arc::new(FakeAuditor {
            result: Ok(GameAudit::new(
                InstallationStatus::NeedsClient,
                LaunchPreflight::Failed,
            )),
            calls: AtomicUsize::new(0),
            saw_shared_lease: AtomicBool::new(false),
        });
        let mut supervisor = game_supervisor(not_ready_auditor, Arc::clone(&host));
        assert_eq!(
            supervisor.start(fixture.root(), GameLaunchOptions::default()),
            Err(GameLaunchError::InstallationNotReady)
        );

        let unavailable_auditor = Arc::new(FakeAuditor {
            result: Err(GameAuditFailure),
            calls: AtomicUsize::new(0),
            saw_shared_lease: AtomicBool::new(false),
        });
        let mut supervisor = game_supervisor(unavailable_auditor, host);
        assert_eq!(
            supervisor.start(fixture.root(), GameLaunchOptions::default()),
            Err(GameLaunchError::AuditUnavailable)
        );
    }

    #[test]
    fn full_streams_keep_the_end_of_stderr_and_the_start_and_end_of_stdout() {
        let full = |name: &str, filler: &str| {
            let mut text = format!("{name}-start\n");
            while text.len() < PROCESS_OUTPUT_LIMIT + 40 {
                text.push_str(filler);
            }
            text.push_str(&format!("{name}-end"));
            text.into_bytes()
        };
        // A carriage return becomes U+FFFD: three bytes for one.
        let output = ProcessOutput::new(full("out", "x\r"), full("err", "y\r"), true, true);
        let diagnostics = render_diagnostics(output, Path::new("/nowhere"), None);
        let text = diagnostics.text();
        assert!(text.len() <= DIAGNOSTIC_LIMIT, "{}", text.len());
        assert!(text.ends_with("err-end"), "{}", &text[text.len() - 40..]);
        assert!(
            text.contains("err-start") && text.contains("out-start") && text.contains("out-end")
        );
        assert!(text.contains("bytes left out]"));
        assert!(diagnostics.truncated());
        // What fits stays whole.
        let small = render_diagnostics(
            ProcessOutput::new(b"a".to_vec(), b"b".to_vec(), false, false),
            Path::new("/nowhere"),
            None,
        );
        assert_eq!(small.text(), "stdout:\na\nstderr:\nb");
        assert!(!small.truncated());
    }

    #[test]
    fn exit_is_bounded_redacted_and_releases_both_leases() {
        let fixture = Fixture::new();
        let auditor = Arc::new(FakeAuditor::ready());
        let host = Arc::new(FakeHost::new());
        let mut supervisor = game_supervisor(auditor, Arc::clone(&host));
        supervisor
            .start(fixture.root(), GameLaunchOptions::default())
            .expect("launch");
        let home = env::var_os("HOME").expect("test HOME");
        let output = format!(
            "managed={} wine-managed=Z:{} home={} wine-home=z:{} {}",
            fixture.root().display(),
            fixture.root().to_string_lossy().replace('/', "\\"),
            Path::new(&home).display(),
            Path::new(&home).to_string_lossy().replace('/', "\\"),
            "x".repeat(DIAGNOSTIC_LIMIT * 2)
        );
        {
            let mut process = host
                .process
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            process.exit = Some(ProcessExit::new(false, Some(17)));
            process.output = ProcessOutput::new(
                output.as_bytes().to_vec(),
                output.as_bytes().to_vec(),
                false,
                false,
            );
        }

        let state = supervisor.poll();
        let GameState::Exited(report) = state else {
            panic!("expected exited state");
        };
        assert!(!report.success());
        assert_eq!(report.code(), Some(17));
        assert!(report.diagnostics().truncated());
        assert!(report.diagnostics().text().len() <= DIAGNOSTIC_LIMIT);
        assert!(report.diagnostics().text().contains("[MANAGED_ROOT]"));
        assert!(report.diagnostics().text().contains("[HOME]"));
        assert!(
            !report
                .diagnostics()
                .text()
                .contains(&fixture.root().to_string_lossy().into_owned())
        );
        assert!(
            !report
                .diagnostics()
                .text()
                .contains(&Path::new(&home).to_string_lossy().into_owned())
        );

        let installation = fixture.installation_lock();
        File::try_lock(&installation).expect("exit releases no-mutation lease");
        let game = File::options()
            .read(true)
            .write(true)
            .open(fixture.root().join(".pd2launcher.game.lock"))
            .expect("open game lock");
        File::try_lock(&game).expect("exit releases single-game lease");
    }

    #[test]
    fn cancellation_is_owned_and_leases_last_until_observed_exit() {
        let fixture = Fixture::new();
        let auditor = Arc::new(FakeAuditor::ready());
        let host = Arc::new(FakeHost::new());
        let mut supervisor = game_supervisor(auditor, Arc::clone(&host));
        supervisor
            .start(
                fixture.root(),
                GameLaunchOptions::new(GameRenderer::DirectDraw, false),
            )
            .expect("launch");

        supervisor.request_stop().expect("owned stop request");
        assert!(
            host.process
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .terminate_called
        );
        let installation = fixture.installation_lock();
        assert!(File::try_lock(&installation).is_err());

        host.process
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .exit = Some(ProcessExit::new(false, None));
        assert!(matches!(supervisor.poll(), GameState::Exited(_)));
        File::try_lock(&installation).expect("observed exit releases lease");
    }

    #[test]
    fn supervision_failure_retains_exclusion_until_exit_is_proven() {
        let fixture = Fixture::new();
        let auditor = Arc::new(FakeAuditor::ready());
        let host = Arc::new(FakeHost::new());
        let mut supervisor = game_supervisor(auditor, Arc::clone(&host));
        supervisor
            .start(fixture.root(), GameLaunchOptions::default())
            .expect("launch");
        host.process
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .wait_fails = true;

        assert_eq!(supervisor.poll(), GameState::SupervisionFailed);
        assert_eq!(
            supervisor.start(fixture.root(), GameLaunchOptions::default()),
            Err(GameLaunchError::AlreadyRunning)
        );
        let installation = fixture.installation_lock();
        assert!(File::try_lock(&installation).is_err());

        {
            let mut process = host
                .process
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            process.wait_fails = false;
            process.exit = Some(ProcessExit::new(true, Some(0)));
        }
        assert!(matches!(supervisor.poll(), GameState::Exited(_)));
        File::try_lock(&installation).expect("proven exit releases lease");
    }

    #[test]
    fn unsafe_runtime_and_spawn_failure_fail_closed_without_stale_locks() {
        let fixture = Fixture::new();
        let runtime_wine = fixture
            .root()
            .join("runner")
            .join(crate::runtime::current().identity)
            .join("bin/wine");
        fs::remove_file(&runtime_wine).expect("remove fixture wine");
        symlink("/usr/bin/true", &runtime_wine).expect("substitute symlink");
        let auditor = Arc::new(FakeAuditor::ready());
        let host = Arc::new(FakeHost::new());
        let mut supervisor = game_supervisor(Arc::clone(&auditor), host);
        assert_eq!(
            supervisor.start(fixture.root(), GameLaunchOptions::default()),
            Err(GameLaunchError::UnsafeManagedInstallation)
        );

        fs::remove_file(&runtime_wine).expect("remove symlink");
        fs::write(&runtime_wine, b"test executable").expect("restore wine");
        let mut permissions = fs::metadata(&runtime_wine).expect("metadata").permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(runtime_wine, permissions).expect("restore permissions");
        let failing_host = Arc::new(FakeHost::failing());
        let mut supervisor = game_supervisor(auditor, failing_host);
        assert_eq!(
            supervisor.start(fixture.root(), GameLaunchOptions::default()),
            Err(GameLaunchError::ProcessSpawnFailed)
        );
        let installation = fixture.installation_lock();
        File::try_lock(&installation).expect("spawn failure releases shared lease");
    }

    #[test]
    fn the_game_logs_copy_hides_account_character_and_game_names() {
        let line =
            game_log_line("12:28:06 Joined game Baal-Run-07 with character: Sorceress account=Foo");
        assert!(!line.contains("Baal-Run-07"), "{line}");
        assert!(!line.contains("Sorceress"), "{line}");
        assert!(!line.contains("Foo"), "{line}");
        assert!(line.starts_with("12:28:06 Joined game"), "{line}");
    }

    #[test]
    fn the_game_logs_copy_keeps_a_dump_rows_hex_and_hides_a_key_in_it() {
        let row = "21:13:04.951  0201FF18: FFFFFFFF D87E244C AD144BBF F293C16F  TM.x..%.l......o";
        assert_eq!(game_log_line(row), row);
        assert_eq!(
            game_log_line("              0201FF28: 2468BCDE  GHJKMNPR"),
            format!("              0201FF28: {}", crate::redact::KEY_MASK)
        );
    }
}
