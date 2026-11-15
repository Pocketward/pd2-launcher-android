//! Fail-closed command-line inputs for unattended installation testing.

use std::{ffi::OsString, fmt, path::PathBuf};

use zeroize::Zeroize;

use crate::{
    presentation::{AuthorizationInput, ImportModeView},
    startup_credentials::{StartupInputs, StartupInstallerSource},
};

pub(crate) const USAGE: &str = "PD2 Launcher\n\nUsage:\n  pd2-launcher\n  pd2-launcher --save-report\n  pd2-launcher --unattended-install [--installer-dir PATH | --download-installers | --existing-install PATH [--existing-install-mode copy|in-place]] [--d2-key KEY] [--lod-key KEY] --accept-vendor-terms [--install-dir PATH] [--start-over] [--launch-after-ready | --open-official-launcher-after-ready]\n\n--save-report opens the Launcher, saves a problem report (as Diagnostics → Save report does) into ~/.pd2launcher/logs/reports, opens that folder, prints the report's path, and closes. The report never includes CD keys (docs/DIAGNOSTICS.md).\n\nUnattended mode runs every setup step in order: Download, Install Diablo II, then Install Project Diablo 2. --existing-install uses a Diablo II + Lord of Destruction the user already has, of any version and as it is (the folder that contains Game.exe), instead of Blizzard's installers, and needs no CD keys: copy (the default) copies it into the launcher's game folder; in-place uses it where it is and adds Project Diablo 2 to that folder. --download-installers fetches the official D2 and LoD downloader files from Blizzard instead of using a local folder. --open-official-launcher-after-ready opens the official PD2 launcher, like its button, once the installation is Ready; an official launcher that is not verified needs confirmation in the Launcher, so the run ends without opening it (exit status 5). An interrupted installation stops unattended mode with instructions (exit status 3); --start-over confirms Start over for it instead: the Managed prefix is removed (downloads and the current Wine runtime are kept) and the installation runs again. A failed installation stops it with the reason, such as a rejected CD key (exit status 7). --install-dir PATH installs into PATH/pd2-launcher, which must be new or empty, and only while the current location holds no installation (exit status 4 when refused).\n\nWhen installer or key arguments are omitted, D2_INSTALLER_SOURCE (download or folder), D2_INSTALLER_DIR, D2_KEY, and D2_LOD_KEY are loaded from the process environment or .env.local. PD2_URL_* variables override download sources (unverified).";

pub(crate) enum ArgumentMode {
    Interactive,
    /// `--save-report`: the interactive Launcher saves a report and closes.
    SaveReport,
    Help,
    Unattended(AutomationRequest),
    /// `--engine`: the Launcher engine the Android app drives through the
    /// Control channel on standard input. Not in [`USAGE`]: only the Android
    /// app starts it.
    Engine,
    /// `--stop-wine`: `wineserver -k` for the Managed prefix through the
    /// engine's guest executor, then exit. Not in [`USAGE`]: only the
    /// Android app runs it, when it stops the game environment.
    StopWine,
    /// `--remove-downloads`: remove the installer downloads under the
    /// installation lease, then exit. Not in [`USAGE`]: only the Android
    /// app runs it, when the player frees that space.
    RemoveDownloads,
}

pub(crate) struct AutomationRequest {
    installer: StartupInstallerSource,
    /// Blizzard's installers need both keys; an existing installation none.
    authorization: Option<AuthorizationInput>,
    import_mode: ImportModeView,
    after_ready: AfterReady,
    start_over: bool,
    install_dir: Option<PathBuf>,
}

/// Exit status of an unattended run that stopped at an interrupted
/// installation without `--start-over`.
pub(crate) const INTERRUPTED_EXIT_STATUS: u8 = 3;
/// Exit status of an unattended run whose `--install-dir` was refused.
pub(crate) const LOCATION_EXIT_STATUS: u8 = 4;
/// Exit status of an unattended run that did not open an unverified
/// official launcher (it needs the user's confirmation).
pub(crate) const OFFICIAL_LAUNCHER_EXIT_STATUS: u8 = 5;

/// Exit status of an unattended installation that failed.
pub(crate) const INSTALL_FAILED_EXIT_STATUS: u8 = 7;
/// Exit status of a Launcher whose window could not open.
pub(crate) const STARTUP_FAILURE_EXIT_STATUS: u8 = 6;
/// Exit status of `--save-report` when the report could not be saved.
pub(crate) const REPORT_FAILED_EXIT_STATUS: u8 = 8;

static SAVE_REPORT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `--save-report` was given.
pub(crate) fn request_report() {
    SAVE_REPORT.store(true, std::sync::atomic::Ordering::SeqCst);
}

pub(crate) fn report_requested() -> bool {
    SAVE_REPORT.load(std::sync::atomic::Ordering::SeqCst)
}

static UNATTENDED_EXIT_STATUS: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub(crate) fn set_unattended_exit_status(status: u8) {
    UNATTENDED_EXIT_STATUS.store(status, std::sync::atomic::Ordering::SeqCst);
}

pub(crate) fn unattended_exit_status() -> u8 {
    UNATTENDED_EXIT_STATUS.load(std::sync::atomic::Ordering::SeqCst)
}

/// What unattended mode does once the installation is Ready.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AfterReady {
    Nothing,
    LaunchGame,
    /// Open the official PD2 launcher through its button's intent.
    OpenOfficialLauncher,
}

impl AutomationRequest {
    pub(crate) fn into_parts(
        self,
    ) -> (
        StartupInstallerSource,
        Option<AuthorizationInput>,
        AfterReady,
    ) {
        (self.installer, self.authorization, self.after_ready)
    }

    /// `--existing-install-mode`: how an existing installation is used.
    pub(crate) fn import_mode(&self) -> ImportModeView {
        self.import_mode
    }

    /// The user confirmed Start over for an interrupted installation.
    pub(crate) fn start_over(&self) -> bool {
        self.start_over
    }

    /// `--install-dir`: the parent folder of a fresh managed root.
    pub(crate) fn install_dir(&self) -> Option<&PathBuf> {
        self.install_dir.as_ref()
    }

    /// An unattended download install with the given authorization, built
    /// without reading the environment or `.env.local`.
    #[cfg(test)]
    pub(crate) fn download_for_tests(authorization: AuthorizationInput) -> Self {
        Self {
            installer: StartupInstallerSource::BlizzardDownload,
            authorization: Some(authorization),
            import_mode: ImportModeView::Copy,
            after_ready: AfterReady::Nothing,
            start_over: false,
            install_dir: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArgumentError {
    message: &'static str,
}

impl ArgumentError {
    const fn new(message: &'static str) -> Self {
        Self { message }
    }
}

/// A build without the window (no `gui` feature) refuses every mode that
/// opens it.
#[cfg(not(feature = "gui"))]
pub(crate) const WINDOW_UNAVAILABLE: ArgumentError = ArgumentError::new(
    "This PD2 Launcher has no window: it runs only as the Launcher engine of the Android app.",
);

impl fmt::Display for ArgumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for ArgumentError {}

struct SecretArgument(String);

impl SecretArgument {
    fn into_string(mut self) -> String {
        std::mem::take(&mut self.0)
    }
}

impl Drop for SecretArgument {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

pub(crate) fn parse_arguments(
    arguments: impl IntoIterator<Item = OsString>,
) -> Result<ArgumentMode, ArgumentError> {
    parse_arguments_with(arguments, || {
        StartupInputs::load(crate::startup_credentials::LaunchMode::Unattended)
    })
}

/// Parse with an explicit fallback source for omitted installer and key
/// values, so tests never read the developer's environment or `.env.local`.
fn parse_arguments_with(
    arguments: impl IntoIterator<Item = OsString>,
    load_fallback: impl FnOnce() -> StartupInputs,
) -> Result<ArgumentMode, ArgumentError> {
    let arguments = arguments.into_iter().collect::<Vec<_>>();
    if arguments.is_empty() {
        return Ok(ArgumentMode::Interactive);
    }
    if arguments.len() == 1 && arguments[0] == "--help" {
        return Ok(ArgumentMode::Help);
    }
    if arguments.len() == 1 && arguments[0] == "--save-report" {
        return Ok(ArgumentMode::SaveReport);
    }
    if arguments.len() == 1 && arguments[0] == "--engine" {
        return Ok(ArgumentMode::Engine);
    }
    if arguments.len() == 1 && arguments[0] == "--stop-wine" {
        return Ok(ArgumentMode::StopWine);
    }
    if arguments.len() == 1 && arguments[0] == "--remove-downloads" {
        return Ok(ArgumentMode::RemoveDownloads);
    }

    let mut unattended = false;
    let mut installer_directory = None;
    let mut download_installers = false;
    let mut base_key = None;
    let mut expansion_key = None;
    let mut vendor_terms_accepted = false;
    let mut launch_after_ready = false;
    let mut open_official_launcher_after_ready = false;
    let mut start_over = false;
    let mut install_dir = None;
    let mut existing_install = None;
    let mut import_mode = None;
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        match argument.to_str() {
            Some("--unattended-install") if !unattended => unattended = true,
            Some("--unattended-install") => {
                return Err(ArgumentError::new(
                    "--unattended-install may be supplied only once",
                ));
            }
            Some("--installer-dir") if installer_directory.is_none() => {
                index += 1;
                let value = arguments.get(index).ok_or_else(|| {
                    ArgumentError::new("--installer-dir requires an absolute path")
                })?;
                let path = PathBuf::from(value);
                if !path.is_absolute() {
                    return Err(ArgumentError::new(
                        "--installer-dir requires an absolute path",
                    ));
                }
                installer_directory = Some(path);
            }
            Some("--installer-dir") => {
                return Err(ArgumentError::new(
                    "--installer-dir may be supplied only once",
                ));
            }
            Some("--download-installers") if !download_installers => download_installers = true,
            Some("--download-installers") => {
                return Err(ArgumentError::new(
                    "--download-installers may be supplied only once",
                ));
            }
            Some("--d2-key") if base_key.is_none() => {
                index += 1;
                base_key = Some(SecretArgument(
                    arguments
                        .get(index)
                        .and_then(|value| value.to_str())
                        .ok_or_else(|| ArgumentError::new("--d2-key requires an ASCII key"))?
                        .to_owned(),
                ));
            }
            Some("--d2-key") => {
                return Err(ArgumentError::new("--d2-key may be supplied only once"));
            }
            Some("--lod-key") if expansion_key.is_none() => {
                index += 1;
                expansion_key = Some(SecretArgument(
                    arguments
                        .get(index)
                        .and_then(|value| value.to_str())
                        .ok_or_else(|| ArgumentError::new("--lod-key requires an ASCII key"))?
                        .to_owned(),
                ));
            }
            Some("--lod-key") => {
                return Err(ArgumentError::new("--lod-key may be supplied only once"));
            }
            Some("--accept-vendor-terms") if !vendor_terms_accepted => {
                vendor_terms_accepted = true;
            }
            Some("--accept-vendor-terms") => {
                return Err(ArgumentError::new(
                    "--accept-vendor-terms may be supplied only once",
                ));
            }
            Some("--launch-after-ready") if !launch_after_ready => launch_after_ready = true,
            Some("--launch-after-ready") => {
                return Err(ArgumentError::new(
                    "--launch-after-ready may be supplied only once",
                ));
            }
            Some("--open-official-launcher-after-ready") if !open_official_launcher_after_ready => {
                open_official_launcher_after_ready = true;
            }
            Some("--open-official-launcher-after-ready") => {
                return Err(ArgumentError::new(
                    "--open-official-launcher-after-ready may be supplied only once",
                ));
            }
            Some("--install-dir") if install_dir.is_none() => {
                index += 1;
                let path = arguments
                    .get(index)
                    .map(PathBuf::from)
                    .filter(|path| path.is_absolute())
                    .ok_or_else(|| ArgumentError::new("--install-dir requires an absolute path"))?;
                install_dir = Some(path);
            }
            Some("--install-dir") => {
                return Err(ArgumentError::new(
                    "--install-dir may be supplied only once",
                ));
            }
            Some("--existing-install") if existing_install.is_none() => {
                index += 1;
                let path = arguments
                    .get(index)
                    .map(PathBuf::from)
                    .filter(|path| path.is_absolute())
                    .ok_or_else(|| {
                        ArgumentError::new("--existing-install requires an absolute path")
                    })?;
                existing_install = Some(path);
            }
            Some("--existing-install") => {
                return Err(ArgumentError::new(
                    "--existing-install may be supplied only once",
                ));
            }
            Some("--existing-install-mode") if import_mode.is_none() => {
                index += 1;
                import_mode = Some(
                    match arguments.get(index).and_then(|value| value.to_str()) {
                        Some("copy") => ImportModeView::Copy,
                        Some("in-place") => ImportModeView::InPlace,
                        _ => {
                            return Err(ArgumentError::new(
                                "--existing-install-mode must be copy or in-place",
                            ));
                        }
                    },
                );
            }
            Some("--existing-install-mode") => {
                return Err(ArgumentError::new(
                    "--existing-install-mode may be supplied only once",
                ));
            }
            Some("--start-over") if !start_over => start_over = true,
            Some("--start-over") => {
                return Err(ArgumentError::new("--start-over may be supplied only once"));
            }
            Some("--help") => {
                return Err(ArgumentError::new("--help must be supplied by itself"));
            }
            _ => return Err(ArgumentError::new("unknown launcher argument")),
        }
        index += 1;
    }

    if !unattended {
        return Err(ArgumentError::new(
            "automation arguments require --unattended-install",
        ));
    }
    if !vendor_terms_accepted {
        return Err(ArgumentError::new(
            "unattended installation requires explicit --accept-vendor-terms",
        ));
    }
    if launch_after_ready && open_official_launcher_after_ready {
        return Err(ArgumentError::new(
            "choose either --launch-after-ready or --open-official-launcher-after-ready",
        ));
    }
    if download_installers && installer_directory.is_some() {
        return Err(ArgumentError::new(
            "choose either --installer-dir or --download-installers",
        ));
    }
    if existing_install.is_some() && (download_installers || installer_directory.is_some()) {
        return Err(ArgumentError::new(
            "choose either --existing-install or a way to get Blizzard's installers",
        ));
    }
    if import_mode.is_some() && existing_install.is_none() {
        return Err(ArgumentError::new(
            "--existing-install-mode requires --existing-install",
        ));
    }

    let (mut fallback_base, mut fallback_expansion, fallback_installer) =
        load_fallback().into_parts();
    let base_key = base_key.map_or_else(
        || std::mem::take(&mut fallback_base),
        SecretArgument::into_string,
    );
    let expansion_key = expansion_key.map_or_else(
        || std::mem::take(&mut fallback_expansion),
        SecretArgument::into_string,
    );
    fallback_base.zeroize();
    fallback_expansion.zeroize();

    // An explicit argument wins; otherwise D2_INSTALLER_SOURCE=download or
    // D2_INSTALLER_DIR from the environment/.env.local. With neither, the
    // unattended mode keeps failing closed.
    let existing = existing_install.is_some();
    let installer = match if let Some(path) = existing_install {
        Some(StartupInstallerSource::ExistingInstall(path))
    } else if download_installers {
        Some(StartupInstallerSource::BlizzardDownload)
    } else {
        installer_directory.map(StartupInstallerSource::Directory)
    } {
        Some(installer) => installer,
        None => fallback_installer
            .map_err(|_| {
                ArgumentError::new("D2_INSTALLER_SOURCE must be \"download\" or \"folder\"")
            })?
            .ok_or_else(|| {
                ArgumentError::new("unattended installation requires installer media")
            })?,
    };
    // An existing installation needs no keys; any given are not used.
    let authorization = if existing {
        let (mut base_key, mut expansion_key) = (base_key, expansion_key);
        base_key.zeroize();
        expansion_key.zeroize();
        None
    } else {
        // Each key as the installers check it, before anything is downloaded.
        if let Some(message) = key_problem(&base_key, &expansion_key) {
            let (mut base_key, mut expansion_key) = (base_key, expansion_key);
            base_key.zeroize();
            expansion_key.zeroize();
            return Err(ArgumentError::new(message));
        }
        Some(
            AuthorizationInput::new(base_key, expansion_key, vendor_terms_accepted).map_err(
                |_| ArgumentError::new("unattended installation requires two valid product keys"),
            )?,
        )
    };

    let after_ready = if launch_after_ready {
        AfterReady::LaunchGame
    } else if open_official_launcher_after_ready {
        AfterReady::OpenOfficialLauncher
    } else {
        AfterReady::Nothing
    };
    Ok(ArgumentMode::Unattended(AutomationRequest {
        installer,
        authorization,
        import_mode: import_mode.unwrap_or(ImportModeView::Copy),
        after_ready,
        start_over,
        install_dir,
    }))
}

/// Why the installers would refuse a given key, in words without the key;
/// a missing key is left to the authorization's own error.
fn key_problem(base_key: &str, expansion_key: &str) -> Option<&'static str> {
    use crate::blizzard::{KeyField, KeyProblem, check_key};
    if base_key.is_empty() || expansion_key.is_empty() {
        return None;
    }
    match check_key(KeyField::BaseGame, base_key.as_bytes()) {
        Ok(_) => {}
        Err(KeyProblem::ExpansionKeyInBaseField) => {
            return Some("the Diablo II CD key (--d2-key or D2_KEY) is a Lord of Destruction key");
        }
        Err(_) => {
            return Some(
                "the Diablo II CD key (--d2-key or D2_KEY) is not one the Diablo II installer takes: 26 letters and numbers, as the Blizzard account shows it",
            );
        }
    }
    match check_key(KeyField::Expansion, expansion_key.as_bytes()) {
        Ok(_) => None,
        Err(KeyProblem::BaseKeyInExpansionField) => {
            Some("the Lord of Destruction CD key (--lod-key or D2_LOD_KEY) is a Diablo II key")
        }
        Err(_) => Some(
            "the Lord of Destruction CD key (--lod-key or D2_LOD_KEY) is not one the Lord of Destruction installer takes: 26 letters and numbers, as the Blizzard account shows it",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blizzard::{TEST_BASE_KEY, TEST_EXPANSION_KEY};

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn explicit_unattended_arguments_form_a_redacted_request() {
        let mode = parse_arguments(args(&[
            "--unattended-install",
            "--installer-dir",
            "/home/test/Downloads",
            "--d2-key",
            TEST_BASE_KEY,
            "--lod-key",
            TEST_EXPANSION_KEY,
            "--accept-vendor-terms",
            "--launch-after-ready",
        ]))
        .unwrap();
        let ArgumentMode::Unattended(request) = mode else {
            panic!("expected unattended mode");
        };
        let (installer, authorization, after_ready) = request.into_parts();
        assert_eq!(
            installer,
            StartupInstallerSource::Directory(PathBuf::from("/home/test/Downloads"))
        );
        assert_eq!(after_ready, AfterReady::LaunchGame);
        assert_eq!(
            format!(
                "{:?}",
                authorization.expect("keys for Blizzard's installers")
            ),
            "AuthorizationInput { base_key: [REDACTED], expansion_key: [REDACTED], vendor_terms_accepted: true }"
        );
    }

    #[test]
    fn save_report_stands_alone() {
        let parse = |values: &[&str]| {
            parse_arguments_with(args(values), || StartupInputs::for_tests(None, None, false))
        };
        assert!(matches!(
            parse(&["--save-report"]),
            Ok(ArgumentMode::SaveReport)
        ));
        assert!(parse(&["--save-report", "--unattended-install"]).is_err());
        assert!(USAGE.contains("--save-report"));
    }

    #[test]
    fn engine_mode_stands_alone_and_is_not_advertised() {
        let parse = |values: &[&str]| {
            parse_arguments_with(args(values), || StartupInputs::for_tests(None, None, false))
        };
        assert!(matches!(parse(&["--engine"]), Ok(ArgumentMode::Engine)));
        assert!(parse(&["--engine", "--unattended-install"]).is_err());
        assert!(!USAGE.contains("--engine"));
        assert!(matches!(
            parse(&["--stop-wine"]),
            Ok(ArgumentMode::StopWine)
        ));
        assert!(parse(&["--stop-wine", "--engine"]).is_err());
        assert!(!USAGE.contains("--stop-wine"));
        assert!(matches!(
            parse(&["--remove-downloads"]),
            Ok(ArgumentMode::RemoveDownloads)
        ));
        assert!(parse(&["--remove-downloads", "--engine"]).is_err());
        assert!(!USAGE.contains("--remove-downloads"));
    }

    #[test]
    fn an_existing_installation_needs_no_keys_and_names_how_it_is_used() {
        let parse = |values: &[&str]| {
            parse_arguments_with(args(values), || StartupInputs::for_tests(None, None, false))
        };
        let Ok(ArgumentMode::Unattended(request)) = parse(&[
            "--unattended-install",
            "--accept-vendor-terms",
            "--existing-install",
            "/home/test/Games/Diablo II",
            "--existing-install-mode",
            "in-place",
        ]) else {
            panic!("expected unattended mode");
        };
        assert_eq!(request.import_mode(), ImportModeView::InPlace);
        let (installer, authorization, _) = request.into_parts();
        assert_eq!(
            installer,
            StartupInstallerSource::ExistingInstall(PathBuf::from("/home/test/Games/Diablo II"))
        );
        assert!(authorization.is_none(), "no CD keys are needed");
        // Copy is the default; keys given anyway are not used.
        let Ok(ArgumentMode::Unattended(request)) = parse(&[
            "--unattended-install",
            "--accept-vendor-terms",
            "--d2-key",
            TEST_BASE_KEY,
            "--existing-install",
            "/home/test/Games/Diablo II",
        ]) else {
            panic!("expected unattended mode");
        };
        assert_eq!(request.import_mode(), ImportModeView::Copy);
        assert!(request.into_parts().1.is_none());
        for (values, message) in [
            (
                &[
                    "--unattended-install",
                    "--accept-vendor-terms",
                    "--existing-install",
                    "relative",
                ][..],
                "--existing-install requires an absolute path",
            ),
            (
                &[
                    "--unattended-install",
                    "--accept-vendor-terms",
                    "--existing-install",
                    "/games/d2",
                    "--download-installers",
                ][..],
                "choose either --existing-install or a way to get Blizzard's installers",
            ),
            (
                &[
                    "--unattended-install",
                    "--accept-vendor-terms",
                    "--existing-install-mode",
                    "copy",
                ][..],
                "--existing-install-mode requires --existing-install",
            ),
            (
                &[
                    "--unattended-install",
                    "--accept-vendor-terms",
                    "--existing-install",
                    "/games/d2",
                    "--existing-install-mode",
                    "move",
                ][..],
                "--existing-install-mode must be copy or in-place",
            ),
        ] {
            match parse(values) {
                Err(error) => assert_eq!(error.to_string(), message),
                Ok(_) => panic!("{message}"),
            }
        }
    }

    fn keys() -> [&'static str; 4] {
        ["--d2-key", TEST_BASE_KEY, "--lod-key", TEST_EXPANSION_KEY]
    }

    fn unattended_with(
        extra: &[&str],
        fallback: StartupInputs,
    ) -> Result<StartupInstallerSource, ArgumentError> {
        let mut values = vec!["--unattended-install", "--accept-vendor-terms"];
        values.extend(keys());
        values.extend(extra);
        match parse_arguments_with(args(&values), || fallback)? {
            ArgumentMode::Unattended(request) => Ok(request.into_parts().0),
            _ => panic!("expected unattended mode"),
        }
    }

    #[test]
    fn unattended_installers_come_from_a_flag_folder_or_environment_choice() {
        assert_eq!(
            unattended_with(
                &["--download-installers"],
                StartupInputs::for_tests(None, None, false)
            ),
            Ok(StartupInstallerSource::BlizzardDownload)
        );
        // D2_INSTALLER_SOURCE=download from the environment or .env.local.
        assert_eq!(
            unattended_with(&[], StartupInputs::for_tests(None, None, true)),
            Ok(StartupInstallerSource::BlizzardDownload)
        );
        assert_eq!(
            unattended_with(
                &[],
                StartupInputs::for_tests(None, Some(PathBuf::from("/media/d2")), false)
            ),
            Ok(StartupInstallerSource::Directory(PathBuf::from(
                "/media/d2"
            )))
        );
        // An explicit folder wins over an environment download choice.
        assert_eq!(
            unattended_with(
                &["--installer-dir", "/media/explicit"],
                StartupInputs::for_tests(None, None, true)
            ),
            Ok(StartupInstallerSource::Directory(PathBuf::from(
                "/media/explicit"
            )))
        );
        // An unknown D2_INSTALLER_SOURCE is reported, never a silent folder;
        // an explicit argument still wins over it.
        assert_eq!(
            unattended_with(&[], StartupInputs::with_invalid_installer_source())
                .unwrap_err()
                .to_string(),
            "D2_INSTALLER_SOURCE must be \"download\" or \"folder\""
        );
        assert_eq!(
            unattended_with(
                &["--download-installers"],
                StartupInputs::with_invalid_installer_source()
            ),
            Ok(StartupInstallerSource::BlizzardDownload)
        );
        // Neither folder nor download: the existing fail-closed error.
        assert_eq!(
            unattended_with(&[], StartupInputs::for_tests(None, None, false))
                .unwrap_err()
                .to_string(),
            "unattended installation requires installer media"
        );
        assert_eq!(
            unattended_with(
                &["--download-installers", "--installer-dir", "/media/d2"],
                StartupInputs::for_tests(None, None, false)
            )
            .unwrap_err()
            .to_string(),
            "choose either --installer-dir or --download-installers"
        );
        assert!(
            unattended_with(
                &["--download-installers", "--download-installers"],
                StartupInputs::for_tests(None, None, false)
            )
            .is_err()
        );
        let fallback_keys = parse_arguments_with(
            args(&[
                "--unattended-install",
                "--accept-vendor-terms",
                "--download-installers",
            ]),
            || StartupInputs::for_tests(Some((TEST_BASE_KEY, TEST_EXPANSION_KEY)), None, false),
        );
        assert!(matches!(fallback_keys, Ok(ArgumentMode::Unattended(_))));
    }

    #[test]
    fn the_official_launcher_can_be_opened_after_ready_instead_of_the_game() {
        let request = |extra: &[&str]| {
            let mut values = vec![
                "--unattended-install",
                "--accept-vendor-terms",
                "--download-installers",
            ];
            values.extend(keys());
            values.extend(extra);
            parse_arguments_with(args(&values), || {
                StartupInputs::for_tests(None, None, false)
            })
        };
        let Ok(ArgumentMode::Unattended(parsed)) =
            request(&["--open-official-launcher-after-ready"])
        else {
            panic!("expected unattended mode");
        };
        assert_eq!(parsed.into_parts().2, AfterReady::OpenOfficialLauncher);
        let Ok(ArgumentMode::Unattended(parsed)) = request(&[]) else {
            panic!("expected unattended mode");
        };
        assert_eq!(parsed.into_parts().2, AfterReady::Nothing);
        assert_eq!(
            request(&[
                "--open-official-launcher-after-ready",
                "--launch-after-ready"
            ])
            .err()
            .unwrap()
            .to_string(),
            "choose either --launch-after-ready or --open-official-launcher-after-ready"
        );
        assert!(
            request(&[
                "--open-official-launcher-after-ready",
                "--open-official-launcher-after-ready"
            ])
            .is_err()
        );
    }

    #[test]
    fn start_over_is_an_explicit_unattended_flag() {
        let parse = |extra: &[&str]| {
            let mut values = vec![
                "--unattended-install",
                "--accept-vendor-terms",
                "--download-installers",
            ];
            values.extend(keys());
            values.extend(extra);
            parse_arguments_with(args(&values), || {
                StartupInputs::for_tests(None, None, false)
            })
        };
        let Ok(ArgumentMode::Unattended(plain)) = parse(&[]) else {
            panic!("expected unattended mode");
        };
        assert!(!plain.start_over());
        let Ok(ArgumentMode::Unattended(confirmed)) = parse(&["--start-over"]) else {
            panic!("expected unattended mode");
        };
        assert!(confirmed.start_over());
        assert!(parse(&["--start-over", "--start-over"]).is_err());
        let Ok(ArgumentMode::Unattended(located)) = parse(&["--install-dir", "/run/media/card"])
        else {
            panic!("expected unattended mode");
        };
        assert_eq!(
            located.install_dir(),
            Some(&PathBuf::from("/run/media/card"))
        );
        assert!(plain.install_dir().is_none());
        assert!(parse(&["--install-dir", "relative"]).is_err());
        assert!(parse(&["--install-dir"]).is_err());
        assert!(parse(&["--install-dir", "/a", "--install-dir", "/b"]).is_err());
        assert_eq!(
            parse_arguments(args(&["--start-over"]))
                .err()
                .unwrap()
                .to_string(),
            "automation arguments require --unattended-install"
        );
    }

    #[test]
    fn unattended_arguments_fail_closed_without_mode_consent_or_valid_values() {
        assert_eq!(
            parse_arguments(args(&["--launch-after-ready"]))
                .err()
                .expect("automation without mode must fail")
                .to_string(),
            "automation arguments require --unattended-install"
        );
        assert_eq!(
            parse_arguments(args(&[
                "--unattended-install",
                "--installer-dir",
                "/home/test/Downloads",
                "--d2-key",
                TEST_BASE_KEY,
                "--lod-key",
                TEST_EXPANSION_KEY,
            ]))
            .err()
            .expect("automation without consent must fail")
            .to_string(),
            "unattended installation requires explicit --accept-vendor-terms"
        );
        assert!(
            parse_arguments(args(&[
                "--unattended-install",
                "--installer-dir",
                "relative",
                "--d2-key",
                TEST_BASE_KEY,
                "--lod-key",
                TEST_EXPANSION_KEY,
                "--accept-vendor-terms",
            ]))
            .is_err()
        );
        assert!(parse_arguments(args(&["--unknown"])).is_err());
    }

    #[test]
    fn a_key_the_installers_refuse_fails_before_any_download() {
        // Of the right form, but no key of the product: refused while the
        // arguments are read, so no job (and no download) starts.
        for (base, expansion, message) in [
            (
                "ZZZZ-ZZZZ-ZZZZ-ZZZZ-ZZZZ-ZZZZ-ZZ",
                TEST_EXPANSION_KEY,
                "the Diablo II CD key (--d2-key or D2_KEY) is not one the Diablo II installer takes: 26 letters and numbers, as the Blizzard account shows it",
            ),
            (
                TEST_BASE_KEY,
                "2222-2222-2222-2222-2222-2222-22",
                "the Lord of Destruction CD key (--lod-key or D2_LOD_KEY) is not one the Lord of Destruction installer takes: 26 letters and numbers, as the Blizzard account shows it",
            ),
            (
                "BCDF-GHJK-MNPR-TVWX",
                TEST_EXPANSION_KEY,
                "the Diablo II CD key (--d2-key or D2_KEY) is not one the Diablo II installer takes: 26 letters and numbers, as the Blizzard account shows it",
            ),
        ] {
            let error = parse_arguments(args(&[
                "--unattended-install",
                "--accept-vendor-terms",
                "--download-installers",
                "--d2-key",
                base,
                "--lod-key",
                expansion,
            ]))
            .err()
            .expect("a refused key stops unattended mode");
            assert_eq!(error.to_string(), message);
            assert!(!error.to_string().contains("ZZZZ") && !error.to_string().contains("2222"));
        }
    }
}
