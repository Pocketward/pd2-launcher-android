//! PD2 Launcher modular-monolith interface.

#![forbid(unsafe_code)]
// Without the window (the `gui` feature), what only the window uses (most
// of the view model, unattended mode) is unused; the default build, with
// the window, still checks the whole crate for dead code.
#![cfg_attr(not(feature = "gui"), allow(dead_code))]

mod activity;
#[cfg(feature = "gui")]
mod app_menu;
pub mod audit;
mod automation;
mod blizzard;
mod box64;
mod builtin_fex;
mod composition;
mod control_channel;
mod coordinator;
mod dev_build;
mod diagnostics;
mod domain;
mod downloads;
mod game_folder;
mod game_language;
mod in_place_drive;
mod install;
mod location;
mod manifest;
mod network;
mod operation;
mod orphaned_wine;
mod persistence;
mod phone_fonts;
mod presentation;
mod process;
mod redact;
#[cfg(test)]
mod release_verification;
mod relocation;
mod report;
mod runtime;
mod safe_fs;
mod save_keeper;
mod sources;
mod stage_marker;
mod startup_credentials;
#[cfg(feature = "gui")]
mod startup_failure;
mod transaction;
mod update;
mod wine_environment;

pub mod host_execution;

/// Set once by the Launcher engine on the Android host, before its first
/// lookup; never on the desktop.
static ANDROID_HOST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The Launcher engine runs on the Android host: [`safe_fs`] avoids
/// `openat2` and opens `home`'s ancestors search-only, Wine's guests also
/// keep the Android values and load the XInput bridge, and a missing D2GL
/// configuration is written before the first Glide launch (an existing one
/// is made full screen with the cursor unlocked), and the DLLs it names are
/// set to PD2's own before D2GL can start.
pub(crate) fn use_android_host(home: Option<&std::path::Path>) {
    safe_fs::use_android_host(home);
    ANDROID_HOST.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Whether the Launcher engine runs on the Android host.
pub(crate) fn android_host() -> bool {
    ANDROID_HOST.load(std::sync::atomic::Ordering::Relaxed)
}

pub use domain::{
    InstallationStatus, InterruptedStage, InvalidRelativeGamePath, LaunchPreflight,
    RelativeGamePath, UpdateStatus,
};

pub use automation::ArgumentError;
pub use composition::package_identity_probe;
#[cfg(feature = "gui")]
pub use composition::run;

/// Process exit status for the finished run: nonzero when the window could
/// not open, when unattended mode stopped without finishing, or when the
/// Launcher engine ended on a Control channel failure.
pub fn exit_status() -> u8 {
    automation::unattended_exit_status()
}

/// Parse launcher arguments and start either the interactive or explicitly
/// authorized unattended workflow.
pub fn run_with_arguments(
    arguments: impl IntoIterator<Item = std::ffi::OsString>,
) -> Result<(), ArgumentError> {
    match automation::parse_arguments(arguments)? {
        #[cfg(feature = "gui")]
        automation::ArgumentMode::Interactive => {
            startup_failure::install_panic_hook();
            composition::run();
        }
        #[cfg(feature = "gui")]
        automation::ArgumentMode::SaveReport => {
            automation::request_report();
            startup_failure::install_panic_hook();
            composition::run();
        }
        automation::ArgumentMode::Help => println!("{}", automation::USAGE),
        #[cfg(feature = "gui")]
        automation::ArgumentMode::Unattended(request) => {
            startup_failure::install_panic_hook();
            composition::run_with_automation(request);
        }
        // A build without the window (no `gui` feature) is only the
        // Android app's Launcher engine.
        #[cfg(not(feature = "gui"))]
        automation::ArgumentMode::Interactive
        | automation::ArgumentMode::SaveReport
        | automation::ArgumentMode::Unattended(_) => return Err(automation::WINDOW_UNAVAILABLE),
        // No desktop: a failure goes to standard error and the Control
        // channel, never to a dialog.
        automation::ArgumentMode::Engine => {
            automation::set_unattended_exit_status(composition::run_engine());
        }
        automation::ArgumentMode::StopWine => {
            automation::set_unattended_exit_status(composition::stop_wine());
        }
        automation::ArgumentMode::RemoveDownloads => {
            automation::set_unattended_exit_status(composition::remove_downloads());
        }
    }
    Ok(())
}
