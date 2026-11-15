//! The Launcher's own entry in the desktop's app menu (freedesktop.org
//! Desktop Entry and Icon Theme specifications).
//!
//! On start the interactive Launcher writes `io.github.kapdon.pd2-launcher.desktop`
//! into `$XDG_DATA_HOME/applications` (`~/.local/share/applications`), with
//! `Exec=` naming the executable itself. Every desktop that follows the XDG
//! specifications reads that folder (GNOME, KDE, XFCE, Sway's fuzzel, wofi,
//! rofi), so the location is the same on every distribution. It is skipped
//! where it would be wrong: a package already installed a system entry, the
//! Launcher runs from a temporary location, or the player turned it off.
//! Only files the Launcher wrote (marked [`MANAGED_KEY`]) are ever replaced
//! or removed; a same-named entry of the player's own is left alone.

use std::{
    env,
    ffi::OsString,
    fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const ID: &str = "io.github.kapdon.pd2-launcher";
const TEMPLATE: &str = include_str!("../packaging/io.github.kapdon.pd2-launcher.desktop");
/// The launcher's icon; the title bar draws it too.
pub(crate) const ICON: &[u8] = include_bytes!("../packaging/io.github.kapdon.pd2-launcher.svg");
/// The key that marks an entry the Launcher wrote.
const MANAGED_KEY: &str = "X-PD2-Launcher-Managed";

/// Where the entry goes and what it names, read from the environment once.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Environment {
    /// `$XDG_DATA_HOME`, or `~/.local/share`.
    pub(crate) data_home: Option<PathBuf>,
    /// `$XDG_DATA_DIRS`, or `/usr/local/share:/usr/share`.
    pub(crate) data_dirs: Vec<PathBuf>,
    /// The program the entry starts: this executable.
    pub(crate) executable: Option<PathBuf>,
    /// Folders a program only runs from for a while (`/tmp`, `$TMPDIR`, …).
    pub(crate) temporary: Vec<PathBuf>,
}

impl Environment {
    pub(crate) fn current() -> Self {
        let var = |name: &str| env::var_os(name).filter(|value| !value.is_empty());
        let data_home = var("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| var("HOME").map(|home| PathBuf::from(home).join(".local/share")));
        let data_dirs = var("XDG_DATA_DIRS")
            .map(|dirs| {
                env::split_paths(&dirs)
                    .filter(|dir| dir.is_absolute())
                    .collect::<Vec<_>>()
            })
            .filter(|dirs| !dirs.is_empty())
            .unwrap_or_else(|| vec!["/usr/local/share".into(), "/usr/share".into()]);
        // Not `$APPIMAGE`: no release is an AppImage, so that variable can
        // only come from another program's AppImage that started this one.
        let executable = fs::canonicalize("/proc/self/exe").ok();
        let mut temporary = vec![
            PathBuf::from("/tmp"),
            PathBuf::from("/var/tmp"),
            PathBuf::from("/run"),
            PathBuf::from("/dev/shm"),
        ];
        temporary.extend(var("TMPDIR").map(PathBuf::from));
        Self {
            data_home,
            data_dirs,
            executable,
            temporary,
        }
    }

    fn entry(&self) -> Option<PathBuf> {
        Some(self.applications()?.join(format!("{ID}.desktop")))
    }

    fn applications(&self) -> Option<PathBuf> {
        self.data_home
            .as_ref()
            .map(|home| home.join("applications"))
    }

    fn icon(&self) -> Option<PathBuf> {
        self.data_home
            .as_ref()
            .map(|home| home.join(format!("icons/hicolor/scalable/apps/{ID}.svg")))
    }
}

/// What [`sync`] did.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Outcome {
    /// The entry was written or brought up to date.
    Written,
    /// The entry was already right.
    Current,
    /// Turned off: the Launcher's own entry (if any) was removed.
    Removed,
    Skipped(Skip),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Skip {
    /// Neither `$XDG_DATA_HOME` nor `$HOME` is set.
    NoDataHome,
    /// Where the Launcher runs from is unknown.
    NoExecutable,
    /// A package installed a system entry with the same id.
    SystemEntry(PathBuf),
    /// The Launcher runs from a temporary location.
    Temporary(PathBuf),
    /// An entry with the same name that the Launcher did not write.
    PlayersEntry(PathBuf),
}

impl Skip {
    pub(crate) fn describe(&self) -> String {
        match self {
            Self::NoDataHome => "no home folder to add it to".to_owned(),
            Self::NoExecutable => "where the launcher runs from is unknown".to_owned(),
            Self::SystemEntry(path) => format!("a package already added {}", path.display()),
            Self::Temporary(path) => format!("it runs from a temporary place ({})", path.display()),
            Self::PlayersEntry(path) => format!("{} is not the launcher's own", path.display()),
        }
    }
}

/// Write, refresh, or (when `enabled` is off) remove the Launcher's entry.
pub(crate) fn sync(environment: &Environment, enabled: bool) -> io::Result<Outcome> {
    let (Some(entry), Some(icon)) = (environment.entry(), environment.icon()) else {
        return Ok(Outcome::Skipped(Skip::NoDataHome));
    };
    if !enabled {
        let removed = remove_managed(&entry)?;
        if removed {
            remove_if_ours(&icon)?;
            refresh(&entry);
        }
        return Ok(Outcome::Removed);
    }
    if let Some(system) = environment
        .data_dirs
        .iter()
        .map(|dir| dir.join(format!("applications/{ID}.desktop")))
        .find(|path| path.is_file())
    {
        // A package's entry wins; one the Launcher wrote before would only
        // show the Launcher twice.
        remove_managed(&entry)?;
        return Ok(Outcome::Skipped(Skip::SystemEntry(system)));
    }
    let Some(executable) = environment.executable.as_deref() else {
        return Ok(Outcome::Skipped(Skip::NoExecutable));
    };
    if is_temporary(executable, &environment.temporary) {
        return Ok(Outcome::Skipped(Skip::Temporary(executable.to_owned())));
    }
    let content = desktop_entry(executable);
    match fs::read_to_string(&entry) {
        Ok(existing) if !is_managed(&existing) => {
            return Ok(Outcome::Skipped(Skip::PlayersEntry(entry)));
        }
        // A new icon in an update counts too: the entry alone would not say so.
        Ok(existing) if existing == content && fs::read(&icon).ok().as_deref() == Some(ICON) => {
            return Ok(Outcome::Current);
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    if fs::read(&icon).ok().as_deref() != Some(ICON) {
        write_replacing(&icon, ICON)?;
    }
    write_replacing(&entry, content.as_bytes())?;
    refresh(&entry);
    Ok(Outcome::Written)
}

/// The packaged entry with `Exec=` naming `executable`, marked as the
/// Launcher's own.
fn desktop_entry(executable: &Path) -> String {
    let mut content = String::new();
    for line in TEMPLATE.lines() {
        if line.starts_with("Exec=") {
            content.push_str("Exec=");
            content.push_str(&exec_argument(&executable.to_string_lossy()));
        } else {
            content.push_str(line);
        }
        content.push('\n');
    }
    content.push_str(MANAGED_KEY);
    content.push_str("=true\n");
    content
}

/// `value` as one `Exec=` argument: quoted where the Desktop Entry
/// specification reserves a character, with `%` doubled, and then escaped
/// as a string value (a backslash is written twice).
fn exec_argument(value: &str) -> String {
    const RESERVED: &[char] = &[
        ' ', '\t', '\n', '"', '\'', '\\', '>', '<', '~', '|', '&', ';', '$', '*', '?', '#', '(',
        ')', '`',
    ];
    let value = value.replace('%', "%%");
    let quoted = if value.contains(RESERVED) {
        let mut quoted = String::from("\"");
        for character in value.chars() {
            if matches!(character, '"' | '`' | '$' | '\\') {
                quoted.push('\\');
            }
            quoted.push(character);
        }
        quoted.push('"');
        quoted
    } else {
        value
    };
    quoted.replace('\\', "\\\\")
}

fn is_managed(content: &str) -> bool {
    content
        .lines()
        .any(|line| line.trim() == format!("{MANAGED_KEY}=true"))
}

/// Whether `executable` lies in a temporary location, including an
/// AppImage's mount (`…/.mount_*`).
fn is_temporary(executable: &Path, temporary: &[PathBuf]) -> bool {
    temporary.iter().any(|dir| executable.starts_with(dir))
        || executable
            .components()
            .any(|part| part.as_os_str().to_string_lossy().starts_with(".mount_"))
}

/// Remove `entry` when the Launcher wrote it; whether it did.
fn remove_managed(entry: &Path) -> io::Result<bool> {
    match fs::read_to_string(entry) {
        Ok(content) if is_managed(&content) => {
            fs::remove_file(entry)?;
            Ok(true)
        }
        Ok(_) => Ok(false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Remove the icon only when it is the one the Launcher wrote.
fn remove_if_ours(icon: &Path) -> io::Result<()> {
    if fs::read(icon).ok().as_deref() == Some(ICON) {
        fs::remove_file(icon)?;
    }
    Ok(())
}

/// Replace `path` with `bytes` through a temporary file and a rename, so a
/// menu never reads half an entry.
fn write_replacing(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no parent folder"))?;
    fs::create_dir_all(parent)?;
    let mut name = OsString::from(".");
    name.push(path.file_name().unwrap_or_default());
    name.push(format!(".pd2launcher-{}", std::process::id()));
    let temporary = parent.join(name);
    let written = (|| {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    written
}

/// Let menus that cache entries see the change; nothing depends on it.
fn refresh(entry: &Path) {
    if let Some(applications) = entry.parent() {
        let _ = Command::new("update-desktop-database")
            .arg(applications)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn environment(root: &Path, executable: &str) -> Environment {
        Environment {
            data_home: Some(root.join("home/.local/share")),
            data_dirs: vec![root.join("usr/share")],
            executable: Some(PathBuf::from(executable)),
            temporary: vec![PathBuf::from("/tmp"), root.join("scratch")],
        }
    }

    fn entry(root: &Path) -> PathBuf {
        root.join(format!("home/.local/share/applications/{ID}.desktop"))
    }

    #[test]
    fn exec_names_the_real_program_quoted_where_needed() {
        assert_eq!(
            exec_argument("/opt/pd2/pd2-launcher"),
            "/opt/pd2/pd2-launcher"
        );
        assert_eq!(
            exec_argument("/home/a b/PD2 Launcher.AppImage"),
            "\"/home/a b/PD2 Launcher.AppImage\""
        );
        assert_eq!(exec_argument("/x/100%"), "/x/100%%");
        // `$` is escaped inside the quotes, then the backslash as a string.
        assert_eq!(exec_argument("/x/$y"), "\"/x/\\\\$y\"");
    }

    #[test]
    fn an_appimage_mount_is_a_temporary_place() {
        let fixture = tempfile::tempdir().unwrap();
        let mounted = environment(fixture.path(), "/opt/x/.mount_PD2abc/usr/bin/pd2-launcher");
        assert_eq!(
            sync(&mounted, true).unwrap(),
            Outcome::Skipped(Skip::Temporary(PathBuf::from(
                "/opt/x/.mount_PD2abc/usr/bin/pd2-launcher"
            )))
        );
    }

    #[test]
    fn the_entry_is_written_refreshed_and_left_alone_when_current() {
        let fixture = tempfile::tempdir().unwrap();
        let first = environment(fixture.path(), "/opt/pd2/pd2-launcher");
        assert_eq!(sync(&first, true).unwrap(), Outcome::Written);
        let written = fs::read_to_string(entry(fixture.path())).unwrap();
        assert!(written.contains("Exec=/opt/pd2/pd2-launcher\n"));
        assert!(written.contains("Name=PD2 Launcher\n"));
        assert!(written.contains("X-PD2-Launcher-Managed=true\n"));
        assert!(
            fixture
                .path()
                .join(format!(
                    "home/.local/share/icons/hicolor/scalable/apps/{ID}.svg"
                ))
                .is_file()
        );
        assert_eq!(sync(&first, true).unwrap(), Outcome::Current);
        // An outdated icon beside an unchanged entry is replaced.
        let icon = fixture.path().join(format!(
            "home/.local/share/icons/hicolor/scalable/apps/{ID}.svg"
        ));
        fs::write(&icon, "<svg/>").unwrap();
        assert_eq!(sync(&first, true).unwrap(), Outcome::Written);
        assert_eq!(fs::read(&icon).unwrap(), ICON);
        assert_eq!(sync(&first, true).unwrap(), Outcome::Current);
        // The Launcher moved: the entry follows.
        let moved = environment(fixture.path(), "/home/p/Apps/PD2 Launcher.AppImage");
        assert_eq!(sync(&moved, true).unwrap(), Outcome::Written);
        assert!(
            fs::read_to_string(entry(fixture.path()))
                .unwrap()
                .contains("Exec=\"/home/p/Apps/PD2 Launcher.AppImage\"\n")
        );
    }

    #[test]
    fn a_system_entry_or_a_temporary_place_skips_it() {
        let fixture = tempfile::tempdir().unwrap();
        let system = fixture
            .path()
            .join(format!("usr/share/applications/{ID}.desktop"));
        let first = environment(fixture.path(), "/opt/pd2/pd2-launcher");
        assert_eq!(sync(&first, true).unwrap(), Outcome::Written);
        fs::create_dir_all(system.parent().unwrap()).unwrap();
        fs::write(&system, TEMPLATE).unwrap();
        assert_eq!(
            sync(&first, true).unwrap(),
            Outcome::Skipped(Skip::SystemEntry(system.clone()))
        );
        // The Launcher's own entry gives way to the package's.
        assert!(!entry(fixture.path()).exists());
        fs::remove_file(&system).unwrap();
        let temporary = environment(fixture.path(), "/tmp/pd2-launcher");
        assert!(matches!(
            sync(&temporary, true).unwrap(),
            Outcome::Skipped(Skip::Temporary(_))
        ));
        assert!(!entry(fixture.path()).exists());
    }

    #[test]
    fn the_players_own_entry_is_never_replaced_or_removed() {
        let fixture = tempfile::tempdir().unwrap();
        let path = entry(fixture.path());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "[Desktop Entry]\nName=Mine\nExec=/mine\n").unwrap();
        let launcher = environment(fixture.path(), "/opt/pd2/pd2-launcher");
        assert_eq!(
            sync(&launcher, true).unwrap(),
            Outcome::Skipped(Skip::PlayersEntry(path.clone()))
        );
        assert_eq!(sync(&launcher, false).unwrap(), Outcome::Removed);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "[Desktop Entry]\nName=Mine\nExec=/mine\n"
        );
    }

    #[test]
    fn turning_it_off_removes_only_what_the_launcher_wrote() {
        let fixture = tempfile::tempdir().unwrap();
        let launcher = environment(fixture.path(), "/opt/pd2/pd2-launcher");
        assert_eq!(sync(&launcher, true).unwrap(), Outcome::Written);
        let icon = fixture.path().join(format!(
            "home/.local/share/icons/hicolor/scalable/apps/{ID}.svg"
        ));
        assert_eq!(sync(&launcher, false).unwrap(), Outcome::Removed);
        assert!(!entry(fixture.path()).exists());
        assert!(!icon.exists());
        // Nothing to remove is not an error.
        assert_eq!(sync(&launcher, false).unwrap(), Outcome::Removed);
    }
}
