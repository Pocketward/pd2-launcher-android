//! Reporting a Launcher that could not open its window.
//!
//! Without a window the user would see nothing at all: a double-clicked
//! executable that finds no graphics driver to draw with, or no font, simply
//! vanishes. A failure to open the window, or a panic before it opened, is
//! written to `~/.pd2launcher/logs/startup-error.txt` and announced with the
//! first available desktop dialog or notification. The announcement names
//! only what the failure and this computer show.

use std::{
    ffi::OsString,
    fs::{self, OpenOptions},
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
};

use crate::persistence::LauncherPaths;

const REPORT_NAME: &str = "startup-error.txt";
const FALLBACK_NAME: &str = "pd2-launcher-startup-error.txt";
const DIALOG_TITLE: &str = "PD2 Launcher";

static WINDOW_OPENED: AtomicBool = AtomicBool::new(false);
static REPORTED: AtomicBool = AtomicBool::new(false);

/// Report a panic that happens before the window opened, after the default
/// hook printed it. Later panics keep the default behavior only.
pub(crate) fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default(info);
        if !WINDOW_OPENED.load(Ordering::SeqCst) {
            report_system(&format!("The launcher stopped while starting.\n\n{info}"));
        }
    }));
}

/// The window exists: from now on the user can see what happens.
pub(crate) fn window_opened() {
    WINDOW_OPENED.store(true, Ordering::SeqCst);
}

/// Report with the real environment and desktop, once per process.
pub(crate) fn report_system(details: &str) {
    if REPORTED.swap(true, Ordering::SeqCst) {
        return;
    }
    report(details, |name| std::env::var_os(name), notify_desktop);
}

/// Write `details` and tell the user where they are. Returns the report path
/// when it could be written.
pub(crate) fn report(
    details: &str,
    environment: impl Fn(&str) -> Option<OsString>,
    notify: impl FnOnce(&str) -> bool,
) -> Option<PathBuf> {
    let path = write_report(details, &environment);
    let message = startup_message(details, path.as_deref());
    eprintln!("{message}");
    notify(&message);
    path
}

/// Names a cause only when the failure shows it: GPUI draws with Vulkan
/// and falls back to OpenGL, and a surface neither could create means no
/// graphics driver could draw the window. Anything else is not guessed at.
pub(crate) fn startup_message(details: &str, path: Option<&Path>) -> String {
    let no_graphics = [
        "Failed to create surface",
        "No GPU adapters found",
        "Failed to create wgpu device",
    ]
    .iter()
    .any(|marker| details.contains(marker));
    let lead = if no_graphics {
        "PD2 Launcher couldn't open its window: no graphics driver could draw it (neither Vulkan nor OpenGL worked)."
    } else {
        "PD2 Launcher couldn't open its window."
    };
    match path {
        Some(path) => format!("{lead} Details were saved to {}.", path.display()),
        None => format!("{lead} Details could not be saved."),
    }
}

fn write_report(details: &str, environment: &dyn Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let contents = format!("PD2 Launcher {}\n\n{details}\n", env!("CARGO_PKG_VERSION"));
    report_directories(environment)
        .into_iter()
        .find_map(|(directory, name)| write_private(&directory, name, &contents).ok())
}

/// Where a report may go, in order: the Launcher's `logs/`, then the
/// private runtime directory, then the temporary directory.
fn report_directories(
    environment: &dyn Fn(&str) -> Option<OsString>,
) -> Vec<(PathBuf, &'static str)> {
    let mut directories = Vec::new();
    if let Ok(paths) = LauncherPaths::discover_from(environment)
        && let Some(home) = paths.home()
    {
        directories.push((home.join("logs"), REPORT_NAME));
    }
    if let Some(runtime) = environment("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
    {
        directories.push((runtime, FALLBACK_NAME));
    }
    directories.push((std::env::temp_dir(), FALLBACK_NAME));
    directories
}

/// Create `directory` owner-only (one component at a time, never through a
/// symbolic link), then write `name` in it without following a link.
fn write_private(directory: &Path, name: &str, contents: &str) -> io::Result<PathBuf> {
    crate::safe_fs::Directory::ensure_absolute(directory)?;
    let metadata = fs::symlink_metadata(directory)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "report directory is not a directory",
        ));
    }
    let path = directory.join(name);
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(&path)?;
    file.write_all(contents.as_bytes())?;
    Ok(path)
}

/// Show `message` with the first desktop tool that exists. Each is started
/// directly with explicit arguments, never through a shell.
fn notify_desktop(message: &str) -> bool {
    let markup_safe = message
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let attempts: [(&str, Vec<&str>); 3] = [
        (
            "zenity",
            vec!["--error", "--title", DIALOG_TITLE, "--text", &markup_safe],
        ),
        ("kdialog", vec!["--title", DIALOG_TITLE, "--error", message]),
        (
            "notify-send",
            vec!["--app-name", DIALOG_TITLE, DIALOG_TITLE, message],
        ),
    ];
    for (program, arguments) in attempts {
        let status = Command::new(program)
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        match status {
            // zenity and kdialog return 1 when the dialog is closed.
            Ok(status) if status.success() || status.code() == Some(1) => return true,
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    fn environment(pairs: Vec<(&'static str, PathBuf)>) -> impl Fn(&str) -> Option<OsString> {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.clone().into_os_string())
        }
    }

    #[test]
    fn a_window_failure_is_saved_in_the_launcher_logs_and_announced() {
        let fixture = tempfile::tempdir().unwrap();
        let home = fixture.path().join("home");
        fs::create_dir(&home).unwrap();
        let shown = RefCell::new(None);
        let path = report(
            "Vulkan: no suitable device (injected)",
            environment(vec![("HOME", home.clone())]),
            |message| {
                *shown.borrow_mut() = Some(message.to_owned());
                true
            },
        )
        .expect("report written");
        assert_eq!(path, home.join(".pd2launcher/logs/startup-error.txt"));
        let contents = fs::read_to_string(&path).unwrap();
        assert!(contents.contains("no suitable device (injected)"));
        let message = shown.into_inner().expect("notified");
        assert_eq!(
            message,
            format!(
                "PD2 Launcher couldn't open its window. Details were saved to {}.",
                path.display()
            )
        );
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn the_launcher_home_override_is_respected() {
        let fixture = tempfile::tempdir().unwrap();
        let custom = fixture.path().join("custom-home");
        let path = report(
            "injected",
            environment(vec![
                ("HOME", fixture.path().to_owned()),
                ("PD2_LAUNCHER_HOME", custom.clone()),
            ]),
            |_| true,
        )
        .unwrap();
        assert_eq!(path, custom.join("logs/startup-error.txt"));
    }

    #[test]
    fn an_unusable_home_falls_back_to_the_runtime_directory() {
        let fixture = tempfile::tempdir().unwrap();
        let not_a_directory = fixture.path().join("home-file");
        fs::write(&not_a_directory, b"").unwrap();
        let runtime = fixture.path().join("runtime");
        fs::create_dir(&runtime).unwrap();
        let path = report(
            "injected",
            environment(vec![
                ("HOME", not_a_directory),
                ("XDG_RUNTIME_DIR", runtime.clone()),
            ]),
            |_| false,
        )
        .unwrap();
        assert_eq!(path, runtime.join(FALLBACK_NAME));
    }

    #[test]
    fn the_message_says_when_nothing_could_be_saved() {
        assert!(startup_message("injected", None).ends_with("Details could not be saved."));
    }

    /// Only a failure to create the window's surface is called a graphics
    /// problem; a panic such as a missing font is not.
    #[test]
    fn the_message_names_a_graphics_cause_only_when_the_failure_shows_one() {
        let path = Path::new("/r/startup-error.txt");
        assert_eq!(
            startup_message(
                "The window could not be opened.\n\nFailed to create surface: Failed to create surface for any enabled backend: {}",
                Some(path),
            ),
            "PD2 Launcher couldn't open its window: no graphics driver could draw it (neither Vulkan nor OpenGL worked). Details were saved to /r/startup-error.txt."
        );
        for other in [
            "The launcher stopped while starting.\n\nfailed to resolve font '.SystemUIFont' or any of the fallbacks",
            "No desktop display was found: WAYLAND_DISPLAY and DISPLAY are both unset.",
        ] {
            assert_eq!(
                startup_message(other, Some(path)),
                "PD2 Launcher couldn't open its window. Details were saved to /r/startup-error.txt."
            );
        }
    }
}
