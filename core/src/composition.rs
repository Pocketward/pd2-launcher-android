//! Process composition root.
//!
//! This is the only module that will construct concrete adapters and connect
//! them to launcher workflows.

use std::sync::Arc;

use crate::{
    InstallationStatus,
    audit::GraphicsExecutionEvidence,
    control_channel::HostFacts,
    coordinator::ServiceRuntime,
    host_execution::{GuestExecutor, SystemHostExecution, SystemHostProbe},
    persistence::LauncherPaths,
    presentation::{
        DomainEvent, HostView, OperationPhase, OperationStatusView, OperationView, SequencedEvent,
        ServiceEndpoint, UiSnapshot,
    },
    startup_credentials::LaunchMode,
};
#[cfg(feature = "gui")]
use crate::{
    automation::AutomationRequest,
    presentation::{self, GraphicsSnapshot},
};

mod production;

use production::ProductionBackend;

/// Start the native launcher interface.
#[cfg(feature = "gui")]
pub fn run() {
    run_with_automation_inner(None);
}

#[cfg(feature = "gui")]
pub(crate) fn run_with_automation(request: AutomationRequest) {
    run_with_automation_inner(Some(request));
}

#[cfg(feature = "gui")]
fn run_with_automation_inner(request: Option<AutomationRequest>) {
    // Only the unattended CLI reads `.env.local`; the interactive Launcher
    // uses the process environment alone.
    let mode = if request.is_some() {
        LaunchMode::Unattended
    } else {
        LaunchMode::Interactive
    };
    // Debug builds only: a canned screen for layout review, with no service.
    #[cfg(debug_assertions)]
    if request.is_none()
        && let Some(snapshot) = presentation::preview::snapshot()
    {
        let (presentation, service) = presentation::service_bridge(snapshot);
        presentation::run(presentation, None, mode, move |_| {
            Box::new(service) as Box<dyn Send>
        });
        return;
    }
    // The setup record, read before the first snapshot: the page the window
    // opens on while the first check runs.
    let setup_record = LauncherPaths::discover()
        .map(|paths| crate::persistence::load_setup_record(&paths.setup_record_file()))
        .unwrap_or_default();
    let (presentation, service) = presentation::service_bridge(
        presentation::UiSnapshot::initial().with_setup_record(setup_record),
    );
    presentation::run(presentation, request, mode, move |graphics| {
        start_service(
            service,
            graphics.map(graphics_execution_evidence),
            mode,
            None,
        )
    });
}

/// Start the Launcher engine (`--engine`): the Android app drives it through
/// the Control channel on standard input instead of a window. Returns the
/// process exit status.
pub(crate) fn run_engine() -> u8 {
    crate::control_channel::run_engine(|host_facts, service| {
        // Logged so a run can be matched to its device.
        eprintln!("pd2-launcher: engine started on {}", host_facts.summary());
        start_service(service, None, LaunchMode::Engine, Some(host_facts.clone()))
    })
}

/// The variable the Android app sets to `1` for every engine process it
/// starts; the desktop never sets it.
const ANDROID_APP: &str = "PD2_ANDROID_APP";

/// Whether `environment` is the Android app's engine environment
/// ([`ANDROID_APP`]). The app-only switches below put the process on the
/// Android host, so they refuse without it, before they touch anything.
fn android_app_environment(
    environment: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> bool {
    environment
        .into_iter()
        .any(|(name, value)| name == ANDROID_APP && value == "1")
}

/// `--stop-wine` (the Android app only): `wineserver -k` for the Managed
/// prefix with the app's Wine, as the engine runs its guests. The app
/// runs it when it stops the game environment and after an engine exit, so
/// no Wine process outlives them. Returns the process exit status: 1
/// outside the Android app's environment.
pub(crate) fn stop_wine() -> u8 {
    stop_wine_in(std::env::vars_os().collect())
}

fn stop_wine_in(environment: Vec<(std::ffi::OsString, std::ffi::OsString)>) -> u8 {
    if !android_app_environment(environment.iter().cloned()) {
        eprintln!("pd2-launcher: --stop-wine refused: only the Android app runs it");
        return 1;
    }
    let Ok(paths) = LauncherPaths::discover() else {
        eprintln!("pd2-launcher: wineserver -k not run: no Launcher home");
        return 1;
    };
    crate::use_android_host(paths.home());
    let host = SystemHostExecution::new(guest_executor(true, Some(&paths)));
    match crate::process::stop_wine(&paths.managed_root(), &host) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("pd2-launcher: wineserver -k failed: {error:?}");
            1
        }
    }
}

/// `--remove-downloads` (the Android app only): the player removes the
/// installer downloads once the game works (`downloads::INSTALLER_DOWNLOADS`).
/// Exit status 0 when they are gone (or there were none), 2 while an
/// install, repair or the game holds the installation, 1 on a failure and
/// outside the Android app's environment.
pub(crate) fn remove_downloads() -> u8 {
    remove_downloads_in(std::env::vars_os())
}

fn remove_downloads_in(
    environment: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> u8 {
    if !android_app_environment(environment) {
        eprintln!("pd2-launcher: --remove-downloads refused: only the Android app runs it");
        return 1;
    }
    let Ok(paths) = LauncherPaths::discover() else {
        eprintln!("pd2-launcher: downloads not removed: no Launcher home");
        return 1;
    };
    crate::use_android_host(paths.home());
    remove_downloads_at(&paths.managed_root())
}

fn remove_downloads_at(managed_root: &std::path::Path) -> u8 {
    use crate::safe_fs::Directory;
    let Ok(root) = Directory::open(managed_root) else {
        eprintln!("pd2-launcher: downloads not removed: no installation");
        return 1;
    };
    // The installation lease: a running job or game holds it.
    let lock = match root.open_file(std::path::Path::new(".pd2launcher.lock")) {
        Ok(lock) if std::fs::File::try_lock(&lock).is_ok() => lock,
        _ => {
            eprintln!("pd2-launcher: downloads not removed: the installation is in use");
            return 2;
        }
    };
    let result = crate::downloads::remove_installer_downloads(&root);
    let _ = std::fs::File::unlock(&lock);
    match result {
        Ok(removed) => {
            eprintln!("pd2-launcher: installer downloads removed ({removed})");
            0
        }
        Err(error) => {
            eprintln!("pd2-launcher: downloads not removed: {error}");
            1
        }
    }
}

/// Build the production backend and start its service runtime. A service
/// that cannot start sends its failure as the first snapshot instead. The
/// returned value keeps the runtime running until it is dropped, which
/// requests a safe stop and joins the runtime's worker. `android` is the
/// Android host's facts, given only to the Launcher engine.
fn start_service(
    service: ServiceEndpoint,
    graphics: Option<GraphicsExecutionEvidence>,
    mode: LaunchMode,
    android: Option<HostFacts>,
) -> Box<dyn Send> {
    let failure_events = service.event_sender.clone();
    let paths = LauncherPaths::discover().ok();
    if android.is_some() {
        // Before the backend's first lookup.
        crate::use_android_host(paths.as_ref().and_then(|paths| paths.home()));
    }
    let guest = guest_executor(android.is_some(), paths.as_ref());
    let mut probe = SystemHostProbe::new(graphics, guest.clone());
    if let Some(facts) = android {
        probe = probe.with_android_host(facts);
    }
    let backend = Arc::new(ProductionBackend::new(
        probe,
        SystemHostExecution::new(guest),
        mode,
        paths,
    ));
    match ServiceRuntime::start(service, backend) {
        Ok(runtime) => Box::new(runtime) as Box<dyn Send>,
        Err(error) => {
            let snapshot = UiSnapshot::initial()
                .with_host(HostView::Unsupported)
                .with_installation(InstallationStatus::Indeterminate)
                .with_operation(Some(
                    OperationView::read_only(
                        OperationPhase::HostAudit,
                        OperationStatusView::Failed,
                        "Launcher service could not start",
                    )
                    .with_evidence(vec![error.code().to_owned()])
                    // Shown as the launcher's own failure, never as a
                    // missing system package.
                    .with_failure(error.code()),
                ));
            let _ = failure_events.try_send(SequencedEvent::new(
                1,
                DomainEvent::FullSnapshot(Box::new(snapshot)),
            ));
            Box::new(()) as Box<dyn Send>
        }
    }
}

/// The guest executor, selected once from the compile target and shared by
/// the host preflight and every guest process start. On ARM64 the FEX set in
/// Settings comes first, and the built-in FEX replaces it once chosen and
/// installed; the paths are the backend's own, so a moved
/// installation is followed. An Android host never uses FEX: the app's
/// runtime is an ARM64 Wine, which runs directly.
fn guest_executor(android_host: bool, paths: Option<&LauncherPaths>) -> GuestExecutor {
    if android_host {
        return GuestExecutor::Direct;
    }
    let guest = GuestExecutor::discover(saved_fex().as_deref());
    match paths {
        Some(paths) => guest.with_choice(paths.clone()),
        None => guest,
    }
}

/// Evaluate the same build identity used by the host audit, without
/// constructing GPUI or any production service. This is intentionally narrow
/// and exists only for post-build verification.
#[doc(hidden)]
pub fn package_identity_probe() -> Option<&'static str> {
    production::portable_identity(production::launcher_artifact())
}

#[cfg(feature = "gui")]
fn graphics_execution_evidence(graphics: GraphicsSnapshot) -> GraphicsExecutionEvidence {
    GraphicsExecutionEvidence::new(
        graphics.is_software_emulated,
        graphics.device_name,
        graphics.driver_name,
        graphics.driver_info,
    )
}

/// The FEX the player set in Settings (ARM64 builds); an unreadable
/// settings file means none.
fn saved_fex() -> Option<std::path::PathBuf> {
    let paths = crate::persistence::LauncherPaths::discover().ok()?;
    crate::persistence::SettingsStore::new(paths.settings_file())
        .load()
        .ok()?
        .fex()
        .map(std::path::Path::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_environment() -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
        vec![("PD2_ANDROID_APP".into(), "1".into())]
    }

    #[test]
    fn downloads_are_removed_only_while_the_installation_is_free() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        let tome = root.join("cache/artifacts/d2-payload/Installer Tome.mpq");
        std::fs::create_dir_all(tome.parent().unwrap()).unwrap();
        std::fs::write(&tome, b"x").unwrap();
        std::fs::write(root.join(".pd2launcher.lock"), b"").unwrap();
        // An install, repair or the game holds the lease: refused.
        let held = std::fs::File::open(root.join(".pd2launcher.lock")).unwrap();
        held.try_lock().unwrap();
        assert_eq!(remove_downloads_at(root), 2);
        assert!(tome.is_file());
        held.unlock().unwrap();
        assert_eq!(remove_downloads_at(root), 0);
        assert!(!root.join("cache/artifacts/d2-payload").exists());
        assert_eq!(remove_downloads_at(root), 0);
        // No lock file (no installation yet) is refused like a busy one.
        let bare = tempfile::tempdir().unwrap();
        assert_eq!(remove_downloads_at(bare.path()), 2);
    }

    #[test]
    fn the_android_app_switches_refuse_outside_its_environment() {
        // The desktop's environment, and one whose PD2_ANDROID_APP is not 1.
        let refused: [Vec<(std::ffi::OsString, std::ffi::OsString)>; 2] =
            [Vec::new(), vec![("PD2_ANDROID_APP".into(), "0".into())]];
        for environment in refused {
            assert!(!android_app_environment(environment.clone()));
            assert_eq!(stop_wine_in(environment.clone()), 1);
            assert_eq!(remove_downloads_in(environment), 1);
        }
        // Refused before anything else: the process never went onto the
        // Android host.
        assert!(!crate::android_host());
        assert!(android_app_environment(app_environment()));
    }

    #[test]
    fn an_android_host_runs_its_guests_directly_never_through_fex() {
        // A fresh managed root: on the desktop's ARM64 row the built-in FEX
        // is the step 1 default there, reported pending until Download.
        let fixture = tempfile::tempdir().unwrap();
        let paths = LauncherPaths::from_home(fixture.path().join("launcher"));
        let guest = guest_executor(true, Some(&paths));
        assert_eq!(guest, GuestExecutor::Direct);
        assert_eq!(guest.interpreter(), None);
        assert_eq!(guest_executor(true, None), GuestExecutor::Direct);
    }
}
