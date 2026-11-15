//! Where the Diablo II game folder is, and Diablo II installations the user
//! already has, of any version.
//!
//! The game normally lives in the Managed prefix at
//! `prefix/drive_c/Program Files (x86)/Diablo II`. An existing installation
//! can instead be copied there, or used where it is. A folder
//! used in place is recorded by `prefix/.pd2launcher-game-folder.json` with
//! its path and identity (filesystem id and inode); it is opened by that path with
//! no symbolic link in any component and must still be the same directory,
//! or it is unavailable and nothing runs until it is back. On Android only the
//! inode must match: the filesystem id of f2fs comes from the device number,
//! which a reboot or a system update can change, and the path already names
//! the volume (`/storage/<volume UUID>/` for a card). Because the
//! record lives in the Managed prefix, Start over forgets it together with
//! the prefix and never touches the folder itself.
//!
//! On the desktop Wine reaches a folder used in place through its default
//! `Z:` drive (the host root), so the Launcher creates no link of its own;
//! the Android host links the `G:` drive to it (`in_place_drive`). The
//! Launcher itself only ever opens it by descriptor, below the proven
//! directory.
//!
//! On the Android host the prefix is disposable (any app version makes it
//! again from its template), so the Managed game folder is outside it:
//! `Diablo II` in the managed root ([`ANDROID_GAME_RELATIVE`]), which Wine
//! reaches as `C:\Program Files (x86)\Diablo II` through a relative link in
//! the prefix ([`link_game`]). An import is moved there too, so no game
//! folder is used in place there, and the records of an import are in the
//! managed root, not in the prefix ([`records_relative`]).

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

use rustix::fs::OFlags;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    operation::{OperationControl, WorkPhase, WorkProgress},
    safe_fs::Directory,
};

/// The Managed game folder below the managed root (on the desktop), and the
/// place of the link to it on the Android host ([`link_game`]).
pub(crate) const GAME_RELATIVE: &str = "prefix/drive_c/Program Files (x86)/Diablo II";
/// The Managed game folder below the managed root on the Android host,
/// outside the prefix.
pub(crate) const ANDROID_GAME_RELATIVE: &str = "Diablo II";
/// The target of the prefix's link to [`ANDROID_GAME_RELATIVE`], relative to
/// the link's folder (`prefix/drive_c/Program Files (x86)`), so the link
/// stays right when the app's data folder moves.
pub(crate) const GAME_LINK_TARGET: &str = "../../../Diablo II";
/// The folder of the prefix's game folder link.
const GAME_LINK_FOLDER: &str = "prefix/drive_c/Program Files (x86)";
/// The folder of the prefix state that the Android app keeps (the player's
/// registry values; for the source Blizzard's installers, what those
/// installers wrote outside the game folder), in the managed root. Start
/// over removes it with the game.
pub(crate) const ANDROID_PREFIX_STATE: &str = "prefix-state";

/// The Managed game folder below the managed root on the Android host
/// (`android`) or the desktop.
pub(crate) const fn managed_relative(android: bool) -> &'static str {
    if android {
        ANDROID_GAME_RELATIVE
    } else {
        GAME_RELATIVE
    }
}

/// The folder below the managed root that holds the records of an import
/// ([`IMPORT_RECORD_NAME`], the copied client's mark): the prefix on the
/// desktop, the managed root itself on the Android host (`android`), whose
/// prefix is disposable.
pub(crate) const fn records_relative(android: bool) -> &'static str {
    if android { "" } else { "prefix" }
}

/// The records' folder ([`records_relative`]) of `root`, opened.
pub(crate) fn records_directory(root: &Directory, android: bool) -> io::Result<Directory> {
    match records_relative(android) {
        "" => root.try_clone(),
        relative => root.open_directory(Path::new(relative)),
    }
}

/// Make the prefix's `C:\Program Files (x86)\Diablo II` a link to the
/// Managed game folder of the Android host ([`GAME_LINK_TARGET`]). True when
/// the link was made or replaced, false when it was already right. A link
/// elsewhere is replaced; anything else in its place (a real folder: the
/// game of an older layout, which the app moves out first) is left as it is
/// and refused, never removed.
pub(crate) fn link_game(root: &Directory) -> io::Result<bool> {
    let folder = root.ensure_directory(Path::new(GAME_LINK_FOLDER))?;
    let descriptor = folder.try_clone_file()?;
    match rustix::fs::readlinkat(&descriptor, "Diablo II", Vec::new()) {
        Ok(current) if current.as_bytes() == GAME_LINK_TARGET.as_bytes() => return Ok(false),
        Ok(_) => rustix::fs::unlinkat(&descriptor, "Diablo II", rustix::fs::AtFlags::empty())?,
        Err(rustix::io::Errno::NOENT) => {}
        Err(rustix::io::Errno::INVAL) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "the prefix holds a game folder of its own, not the link",
            ));
        }
        Err(error) => return Err(error.into()),
    }
    rustix::fs::symlinkat(GAME_LINK_TARGET, &descriptor, "Diablo II")?;
    descriptor.sync_all()?;
    Ok(true)
}

/// On the Android host (`android`): the game folder's link ([`link_game`])
/// and the Managed game folder itself, empty when it is new, so a Windows
/// program that installs there (Blizzard's installers, the PD2 installer)
/// finds a folder and not a link that leads nowhere.
pub(crate) fn prepare_game_location(root: &Directory, android: bool) -> io::Result<()> {
    if !android {
        return Ok(());
    }
    root.ensure_directory(Path::new(ANDROID_GAME_RELATIVE))?;
    if link_game(root)? {
        eprintln!("pd2-launcher: the prefix's game folder links to {ANDROID_GAME_RELATIVE}");
    }
    Ok(())
}

/// The `PWD` for a Wine program that starts in `working_directory` below the
/// managed root `root` on the Android host (`android`): the same folder
/// through the prefix's game folder link, when it is in the Managed game
/// folder. Wine takes its start folder from `PWD` when `PWD` names the same
/// folder as the working folder, and it names a path by its text: so the
/// program starts in `C:\Program Files (x86)\Diablo II\...`. Without it Wine
/// names the folder by its real path, below the read-only root drive `Z:`
/// (no free space: the game refuses a new character). None elsewhere.
pub(crate) fn wine_pwd(root: &Path, working_directory: &Path, android: bool) -> Option<PathBuf> {
    if !android {
        return None;
    }
    let below = working_directory
        .strip_prefix(root.join(ANDROID_GAME_RELATIVE))
        .ok()?;
    let through = root.join(GAME_RELATIVE);
    Some(if below.as_os_str().is_empty() {
        through
    } else {
        through.join(below)
    })
}
/// The same folder as Windows programs in the Managed prefix see it.
pub(crate) const MANAGED_WINDOWS_PATH: &str = r"C:\Program Files (x86)\Diablo II";
/// The record of a game folder used in place, in the Managed prefix.
pub(crate) const POINTER_NAME: &str = ".pd2launcher-game-folder.json";
/// The record that the game was imported, in the records' folder
/// ([`records_relative`]: the Managed prefix, or the managed root on the
/// Android host). The Android app reads that it is there (its storage words
/// and its notice about the player's folder), and the Android host's audit
/// takes an imported game's short list of files for it. Its form is the one
/// earlier releases wrote.
pub(crate) const IMPORT_RECORD_NAME: &str = ".pd2launcher-imported-game.json";
const SCHEMA: u32 = 1;
const RECORD_LIMIT: u64 = 8 * 1024;
/// A game folder with more entries than this is not a Diablo II folder.
const MAX_TREE_ENTRIES: usize = 65_536;
const MAX_TREE_DEPTH: usize = 16;

/// Files a complete Diablo II installation needs, by canonical name. They
/// must be there; their version is never checked.
const BASE_FILES: [&str; 9] = [
    "Game.exe",
    "Patch_D2.mpq",
    "Diablo II.exe",
    "d2char.mpq",
    "d2data.mpq",
    "d2music.mpq",
    "d2sfx.mpq",
    "d2speech.mpq",
    "d2video.mpq",
];
const EXPANSION_FILES: [&str; 4] = ["d2exp.mpq", "d2xmusic.mpq", "d2xtalk.mpq", "d2xvideo.mpq"];
/// What an import on the Android host checks instead, by canonical name (the
/// owner's rule of 2026-10-26: a short check of the important files, no
/// version check): Diablo II's and Lord of Destruction's programs and main
/// archives, and Project Diablo 2's programs, each a file that is not empty. The import
/// then records this Project Diablo 2 as it is; the desktop keeps
/// `BASE_FILES` and `EXPANSION_FILES` and installs Project Diablo 2 itself.
/// The audit of an imported game on the Android host proves Diablo II and
/// Lord of Destruction by the same files.
pub(crate) const IMPORT_BASE_FILES: [&str; 5] = [
    "Game.exe",
    "Diablo II.exe",
    "d2data.mpq",
    "d2char.mpq",
    "d2sfx.mpq",
];
pub(crate) const IMPORT_EXPANSION_FILES: [&str; 1] = ["d2exp.mpq"];
const IMPORT_PROJECT_DIABLO_FILES: [&str; 3] = [
    "ProjectD2/Game.exe",
    "ProjectD2/ProjectDiablo.dll",
    "ProjectD2/PD2Launcher.exe",
];
/// Copied only on the desktop, where Install Project Diablo 2 adopts it;
/// the Android app copies folders itself.
const PROJECT_D2: &str = "projectd2";

/// Where the game folder is.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum GameFolder {
    Managed,
    External(ExternalGameFolder),
}

/// A game folder used where it is, and the identity it had when chosen.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ExternalGameFolder {
    path: PathBuf,
    /// `f_fsid`: stable across reboots on desktop file systems (btrfs, ext4),
    /// unlike `st_dev` on btrfs. Not compared on Android (see the module).
    filesystem: u64,
    inode: u64,
}

impl ExternalGameFolder {
    /// The folder at `path` with the identity it has now.
    #[cfg(test)]
    pub(crate) fn at(path: &Path) -> Self {
        let file = std::fs::File::open(path).unwrap();
        Self {
            path: path.to_owned(),
            filesystem: crate::safe_fs::filesystem_id(&file).unwrap(),
            inode: file.metadata().unwrap().ino(),
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Open the folder by its path (no symbolic link in any component) and
    /// prove it is still the directory that was chosen.
    pub(crate) fn open(&self) -> io::Result<Directory> {
        let directory = Directory::open(&self.path)?;
        let file = directory.try_clone_file()?;
        if !crate::safe_fs::same_recorded_file(
            self.filesystem,
            self.inode,
            crate::safe_fs::filesystem_id(&file)?,
            file.metadata()?.ino(),
        ) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the game folder is not the one that was chosen",
            ));
        }
        Ok(directory)
    }
}

/// The recorded game folder could not be established.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum GameFolderError {
    /// The record exists but is not one the Launcher wrote.
    RecordInvalid,
    /// The recorded folder is gone, moved, replaced, or unreadable.
    Unavailable(PathBuf),
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PointerRecord {
    schema: u32,
    path: String,
    filesystem: u64,
    inode: u64,
}

impl GameFolder {
    /// The game folder this managed root uses. A missing record is the
    /// Managed folder; an unreadable or foreign record fails closed.
    pub(crate) fn read(root: &Directory) -> Result<Self, GameFolderError> {
        let prefix = match root.open_directory(Path::new("prefix")) {
            Ok(prefix) => prefix,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::Managed),
            Err(_) => return Err(GameFolderError::RecordInvalid),
        };
        let bytes = match read_bounded(&prefix, POINTER_NAME) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return Ok(Self::Managed),
            Err(()) => return Err(GameFolderError::RecordInvalid),
        };
        let record = serde_json::from_slice::<PointerRecord>(&bytes)
            .ok()
            .filter(|record| record.schema == SCHEMA)
            .ok_or(GameFolderError::RecordInvalid)?;
        let path = PathBuf::from(record.path);
        if !path.is_absolute() || windows_path_for(&path).is_none() {
            return Err(GameFolderError::RecordInvalid);
        }
        Ok(Self::External(ExternalGameFolder {
            path,
            filesystem: record.filesystem,
            inode: record.inode,
        }))
    }

    /// Read the record and prove an external folder is reachable and still
    /// the same one.
    pub(crate) fn resolve(root: &Directory) -> Result<Self, GameFolderError> {
        let folder = Self::read(root)?;
        if let Self::External(external) = &folder
            && external.open().is_err()
        {
            return Err(GameFolderError::Unavailable(external.path.clone()));
        }
        Ok(folder)
    }

    #[cfg(test)]
    pub(crate) fn is_external(&self) -> bool {
        matches!(self, Self::External(_))
    }

    /// Open the game folder. The Managed one is opened beneath the root.
    pub(crate) fn open(&self, root: &Directory) -> io::Result<Directory> {
        self.open_on(root, crate::android_host())
    }

    /// [`Self::open`] on the Android host (`android`) or the desktop.
    pub(crate) fn open_on(&self, root: &Directory, android: bool) -> io::Result<Directory> {
        match self {
            Self::Managed => root.open_directory(Path::new(managed_relative(android))),
            Self::External(external) => external.open(),
        }
    }

    /// The folder as Windows programs in the Managed prefix see it: one
    /// used in place on the Android host through the `G:` drive
    /// (`in_place_drive`).
    pub(crate) fn windows_path(&self) -> String {
        match self {
            Self::Managed => MANAGED_WINDOWS_PATH.to_owned(),
            Self::External(external) => {
                crate::in_place_drive::windows_path(&external.path, crate::android_host())
                    .expect("a recorded path was validated")
            }
        }
    }

    /// The folder's host path, for working directories and diagnostics.
    pub(crate) fn host_path(&self, managed_root: &Path) -> PathBuf {
        self.host_path_on(managed_root, crate::android_host())
    }

    /// [`Self::host_path`] on the Android host (`android`) or the desktop.
    pub(crate) fn host_path_on(&self, managed_root: &Path, android: bool) -> PathBuf {
        match self {
            Self::Managed => managed_root.join(managed_relative(android)),
            Self::External(external) => external.path.clone(),
        }
    }
}

/// `Z:\…` for an absolute host path whose name Windows programs can use.
/// Printable ASCII only: Wine stores other characters in its registry files
/// as escapes, which the Auditor does not decode, and Windows code pages
/// differ on them.
pub(crate) fn windows_path_for(path: &Path) -> Option<String> {
    let text = path.to_str()?;
    if !text
        .bytes()
        .all(|byte| byte.is_ascii_graphic() || byte == b' ')
    {
        return None;
    }
    let relative = text.strip_prefix('/')?;
    if relative.is_empty()
        || relative.split('/').any(|component| {
            component.is_empty()
                || matches!(component, "." | "..")
                || component.chars().any(|character| {
                    character.is_control()
                        || matches!(character, '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
                })
        })
    {
        return None;
    }
    Some(format!(r"Z:\{}", relative.replace('/', "\\")))
}

/// Record `external` as the game folder. The caller holds the installation
/// lease and has proven the folder.
pub(crate) fn write_pointer(prefix: &Directory, external: &ExternalGameFolder) -> io::Result<()> {
    let record = PointerRecord {
        schema: SCHEMA,
        path: external
            .path
            .to_str()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path is not UTF-8"))?
            .to_owned(),
        filesystem: external.filesystem,
        inode: external.inode,
    };
    let bytes = serde_json::to_vec(&record)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "record encode failed"))?;
    prefix.atomic_write(OsStr::new(POINTER_NAME), &bytes)
}

/// How an existing installation became the game.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ImportMode {
    /// Copied into the Managed game folder; the original is not used.
    Copy,
    /// Used where it is; Project Diablo 2 is added to that folder.
    InPlace,
}

#[derive(Serialize)]
struct ImportRecord<'a> {
    schema: u32,
    mode: ImportMode,
    /// The game folder as Windows programs see it.
    install_path: &'a str,
}

/// Write the import record into the records' folder `records`
/// ([`records_directory`]), once the game is in place. The caller holds the
/// installation lease.
pub(crate) fn write_import_record(
    records: &Directory,
    mode: ImportMode,
    install_path: &str,
) -> io::Result<()> {
    let bytes = serde_json::to_vec(&ImportRecord {
        schema: SCHEMA,
        mode,
        install_path,
    })
    .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "record encode failed"))?;
    records.atomic_write(OsStr::new(IMPORT_RECORD_NAME), &bytes)
}

fn read_bounded(directory: &Directory, name: &str) -> Result<Option<Vec<u8>>, ()> {
    let file = match directory.open_file(Path::new(name)) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(()),
    };
    let mut bytes = Vec::new();
    file.take(RECORD_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    if bytes.len() as u64 > RECORD_LIMIT {
        return Err(());
    }
    Ok(Some(bytes))
}

/// Why a folder is not a Diablo II installation the Launcher can use.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExistingInstallProblem {
    /// The folder doesn't exist or isn't a folder.
    NotFound,
    /// The folder or one of its files can't be read.
    Unreadable,
    /// No Diablo II here.
    NotDiablo,
    /// Lord of Destruction isn't installed here.
    NoExpansion,
    /// A file the game needs is missing.
    MissingFile(&'static str),
    /// A symbolic link (or another special file) is part of the folder.
    Links,
    /// The folder is the launcher's own, or contains it.
    InsideLauncher,
    /// The folder's path can't be used by Windows programs.
    UnsupportedPath,
}

/// A validated Diablo II + Lord of Destruction folder, of any version.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ExistingInstall {
    folder: ExternalGameFolder,
    /// Bytes of everything but ProjectD2.
    copy_bytes: u64,
    /// Bytes of its ProjectD2, which a copy on the desktop brings along.
    project_d2_bytes: u64,
    /// It holds a Project Diablo 2 to adopt: a ProjectD2 with the client's
    /// launch files (`Game.exe`, `ProjectDiablo.dll`) present and not empty.
    project_diablo: bool,
    /// No Project Diablo 2 folder, or one named exactly `ProjectD2`. The
    /// Launcher reads and writes that name; Windows programs would use a
    /// differently cased one, and the two would disagree in place.
    project_d2_canonical: bool,
}

impl ExistingInstall {
    pub(crate) fn path(&self) -> &Path {
        &self.folder.path
    }

    /// Bytes a copy writes: on the desktop its ProjectD2 too, which the
    /// install then adopts; the Android app copies a folder itself and
    /// leaves the engine's copy as it was.
    pub(crate) fn copy_bytes(&self) -> u64 {
        if copies_project_d2() {
            self.copy_bytes.saturating_add(self.project_d2_bytes)
        } else {
            self.copy_bytes
        }
    }

    /// Whether it holds a Project Diablo 2 the install adopts.
    pub(crate) fn project_diablo(&self) -> bool {
        self.project_diablo
    }

    pub(crate) fn folder(&self) -> &ExternalGameFolder {
        &self.folder
    }

    /// Validate again, for the folder recorded at the choice, right before
    /// it is imported: it must still be the same folder and still usable.
    pub(crate) fn revalidate(&self, managed_root: &Path) -> Result<Self, ExistingInstallProblem> {
        let fresh = validate_existing(&self.folder.path, managed_root)?;
        // `validate_existing` checks as the host does (on Android the
        // import's short list).
        if fresh.folder != self.folder {
            return Err(ExistingInstallProblem::NotFound);
        }
        Ok(fresh)
    }
}

/// Check that `path` is the folder of a complete Diablo II + Lord of
/// Destruction installation, of any version: the player chose it, so it is
/// used as it is. Every file the game needs (`BASE_FILES` and
/// `EXPANSION_FILES`) is a regular file in it, and no symbolic link or
/// special file is part of it. Names are matched without regard to case, as
/// Windows does. The folder is only read: it is never patched. On the
/// Android host the files checked are the import's short list instead
/// (`IMPORT_BASE_FILES`, `IMPORT_EXPANSION_FILES` and Project Diablo 2's).
pub(crate) fn validate_existing(
    path: &Path,
    managed_root: &Path,
) -> Result<ExistingInstall, ExistingInstallProblem> {
    validate_existing_on(path, managed_root, crate::android_host())
}

/// [`validate_existing`] for the Android host's import (`android`) or the
/// desktop's.
fn validate_existing_on(
    path: &Path,
    managed_root: &Path,
    android: bool,
) -> Result<ExistingInstall, ExistingInstallProblem> {
    if !path.is_absolute() {
        return Err(ExistingInstallProblem::NotFound);
    }
    if path.starts_with(managed_root) || managed_root.starts_with(path) {
        return Err(ExistingInstallProblem::InsideLauncher);
    }
    let directory = Directory::open(path).map_err(open_problem)?;
    let (filesystem, inode) = directory
        .try_clone_file()
        .and_then(|file| {
            Ok((
                crate::safe_fs::filesystem_id(&file)?,
                file.metadata()?.ino(),
            ))
        })
        .map_err(|_| ExistingInstallProblem::Unreadable)?;
    let names = case_insensitive_names(&directory)?;
    if android {
        // The app checked the picked folder with the same list before its
        // copy, and named every missing file; this is the check that the
        // copy is complete.
        if let Some(missing) = missing_import_files(&directory, &names)?.first() {
            return Err(ExistingInstallProblem::MissingFile(missing));
        }
    } else {
        let has = |name: &str| names.contains_key(&name.to_ascii_lowercase());
        if !has("game.exe") && !has("d2data.mpq") {
            return Err(ExistingInstallProblem::NotDiablo);
        }
        // No version check: what `Game.exe` and `Patch_D2.mpq` hold is the
        // player's choice. Project Diablo 2 runs its own game files from
        // `ProjectD2`. A missing `Game.exe` is named before a missing Lord
        // of Destruction, as the list below would not.
        if open_named(&directory, &names, "Game.exe")?.is_none() {
            return Err(ExistingInstallProblem::MissingFile("Game.exe"));
        }
        if open_named(&directory, &names, "d2exp.mpq")?.is_none() {
            return Err(ExistingInstallProblem::NoExpansion);
        }
        for name in BASE_FILES.into_iter().chain(EXPANSION_FILES) {
            if open_named(&directory, &names, name)?.is_none() {
                return Err(ExistingInstallProblem::MissingFile(name));
            }
        }
    }
    // Every part must be a real file or folder: a copy never follows a
    // link, and a folder used in place is audited without following one.
    let mut visited = 0;
    let mut project_d2_bytes = 0;
    let copy_bytes = tree_bytes(&directory, 0, &mut visited, true, &mut project_d2_bytes)?;
    let project_d2_canonical = names
        .get(PROJECT_D2)
        .is_none_or(|name| name == OsStr::new("ProjectD2"));
    let project_diablo = match names.get(PROJECT_D2) {
        Some(name) => {
            let project = directory
                .open_directory(Path::new(name))
                .map_err(open_problem)?;
            let inside = case_insensitive_names(&project)?;
            ["Game.exe", "ProjectDiablo.dll"].into_iter().all(|file| {
                open_named(&project, &inside, file)
                    .ok()
                    .flatten()
                    .and_then(|file| file.metadata().ok())
                    .is_some_and(|metadata| metadata.len() > 0)
            })
        }
        None => false,
    };
    let folder = ExternalGameFolder {
        path: path.to_owned(),
        filesystem,
        inode,
    };
    Ok(ExistingInstall {
        folder,
        copy_bytes,
        project_d2_bytes,
        project_diablo,
        project_d2_canonical,
    })
}

/// The files of the import's list (`IMPORT_BASE_FILES`,
/// `IMPORT_EXPANSION_FILES`, then Project Diablo 2's) that `directory`
/// (with its `names`) does not have as a file that is not empty, in that
/// order.
fn missing_import_files(
    directory: &Directory,
    names: &BTreeMap<String, OsString>,
) -> Result<Vec<&'static str>, ExistingInstallProblem> {
    let project = match names.get(PROJECT_D2) {
        Some(name) => {
            let project = directory
                .open_directory(Path::new(name))
                .map_err(open_problem)?;
            let inside = case_insensitive_names(&project)?;
            Some((project, inside))
        }
        None => None,
    };
    let mut missing = Vec::new();
    for file in IMPORT_BASE_FILES
        .into_iter()
        .chain(IMPORT_EXPANSION_FILES)
        .chain(IMPORT_PROJECT_DIABLO_FILES)
    {
        let opened = match file.split_once('/') {
            Some((_, inner)) => match &project {
                Some((project, inside)) => open_named(project, inside, inner)?,
                None => None,
            },
            None => open_named(directory, names, file)?,
        };
        let present = opened
            .and_then(|file| file.metadata().ok())
            .is_some_and(|metadata| metadata.is_file() && metadata.len() > 0);
        if !present {
            missing.push(file);
        }
    }
    Ok(missing)
}

/// Whether the folder can be used where it is: Windows programs can name
/// it, and its Project Diablo 2 folder (if any) is named as the Launcher
/// names it.
pub(crate) fn usable_in_place(install: &ExistingInstall) -> bool {
    install.project_d2_canonical && windows_path_for(install.path()).is_some()
}

fn open_problem(error: io::Error) -> ExistingInstallProblem {
    match error.raw_os_error() {
        Some(code) if code == rustix::io::Errno::LOOP.raw_os_error() => {
            ExistingInstallProblem::Links
        }
        Some(code)
            if code == rustix::io::Errno::NOENT.raw_os_error()
                || code == rustix::io::Errno::NOTDIR.raw_os_error() =>
        {
            ExistingInstallProblem::NotFound
        }
        _ if error.kind() == io::ErrorKind::InvalidData => ExistingInstallProblem::NotFound,
        _ => ExistingInstallProblem::Unreadable,
    }
}

/// Lower-case name → the name on disk. Two names that differ only in case
/// are ambiguous for Windows programs and are refused.
fn case_insensitive_names(
    directory: &Directory,
) -> Result<BTreeMap<String, OsString>, ExistingInstallProblem> {
    let mut names = BTreeMap::new();
    for name in directory
        .names()
        .map_err(|_| ExistingInstallProblem::Unreadable)?
    {
        let Some(text) = name.to_str() else {
            continue;
        };
        if names
            .insert(text.to_ascii_lowercase(), name.clone())
            .is_some()
        {
            return Err(ExistingInstallProblem::Unreadable);
        }
    }
    Ok(names)
}

fn open_named(
    directory: &Directory,
    names: &BTreeMap<String, OsString>,
    canonical: &str,
) -> Result<Option<File>, ExistingInstallProblem> {
    let Some(actual) = names.get(&canonical.to_ascii_lowercase()) else {
        return Ok(None);
    };
    let file = directory
        .open_beneath(Path::new(actual), OFlags::RDONLY)
        .map_err(|error| match open_problem(error) {
            ExistingInstallProblem::NotFound => ExistingInstallProblem::Unreadable,
            problem => problem,
        })?;
    let metadata = file
        .metadata()
        .map_err(|_| ExistingInstallProblem::Unreadable)?;
    if !metadata.is_file() {
        return Err(ExistingInstallProblem::Links);
    }
    Ok(Some(file))
}

fn sha256_hex(file: &mut File) -> io::Result<String> {
    file.seek(SeekFrom::Start(0))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 256 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// Total size of the regular files below `directory`, refusing any link or
/// special file. At the top level ProjectD2 is walked (links there are
/// refused too) and counted apart, in `project`.
fn tree_bytes(
    directory: &Directory,
    depth: usize,
    visited: &mut usize,
    top: bool,
    project: &mut u64,
) -> Result<u64, ExistingInstallProblem> {
    if depth > MAX_TREE_DEPTH {
        return Err(ExistingInstallProblem::Unreadable);
    }
    let mut total = 0_u64;
    for name in directory
        .names()
        .map_err(|_| ExistingInstallProblem::Unreadable)?
    {
        *visited += 1;
        if *visited > MAX_TREE_ENTRIES {
            return Err(ExistingInstallProblem::Unreadable);
        }
        let node = directory
            .open_beneath(Path::new(&name), OFlags::PATH)
            .map_err(open_problem)?;
        let metadata = node
            .metadata()
            .map_err(|_| ExistingInstallProblem::Unreadable)?;
        if metadata.file_type().is_symlink() {
            return Err(ExistingInstallProblem::Links);
        }
        let skipped = top && is_project_d2(&name);
        if metadata.is_dir() {
            let child = directory
                .open_directory(Path::new(&name))
                .map_err(open_problem)?;
            let bytes = tree_bytes(&child, depth + 1, visited, false, project)?;
            if skipped {
                *project = project.saturating_add(bytes);
            } else {
                total = total.saturating_add(bytes);
            }
        } else if metadata.is_file() {
            if skipped {
                *project = project.saturating_add(metadata.len());
            } else {
                total = total.saturating_add(metadata.len());
            }
        } else {
            return Err(ExistingInstallProblem::Links);
        }
    }
    Ok(total)
}

/// Whether a copied import brings the folder's ProjectD2 along: on the
/// desktop, where the install then adopts it; never on the Android host.
pub(crate) fn copies_project_d2() -> bool {
    !crate::android_host()
}

fn is_project_d2(name: &OsStr) -> bool {
    name.to_str()
        .is_some_and(|name| name.eq_ignore_ascii_case(PROJECT_D2))
}

/// The canonical name of a file the Auditor checks, for a name that differs
/// from it only in case (`patch_d2.mpq` → `Patch_D2.mpq`), of the Project
/// Diablo 2 folder (`ProjectD2`, the name the Launcher uses), and on the
/// desktop of the characters folder (`Save`, the name the Launcher keeps
/// them under).
fn canonical_name(name: &OsStr) -> OsString {
    if is_project_d2(name) {
        return OsString::from("ProjectD2");
    }
    if !crate::android_host()
        && name
            .to_str()
            .is_some_and(|name| name.eq_ignore_ascii_case(crate::save_keeper::SAVE))
    {
        return OsString::from(crate::save_keeper::SAVE);
    }
    name.to_str()
        .and_then(|text| {
            BASE_FILES
                .into_iter()
                .chain(EXPANSION_FILES)
                .find(|canonical| canonical.eq_ignore_ascii_case(text))
        })
        .map_or_else(|| name.to_owned(), OsString::from)
}

/// Copy the game folder into the empty Managed game folder `destination`:
/// every file and folder (ProjectD2 only with `project_d2`, under that
/// name), reflinked where the filesystem allows it, each verified by
/// SHA-256 against its source after the copy. On the desktop each file
/// keeps its source's times, so a character copied again is not mistaken
/// for one saved since.
/// No link is ever followed; the original is only read. A stop lands
/// between this process's own file operations, under the base-game stage
/// marker, so it is a proven-safe boundary that Start over resolves.
pub(crate) fn copy_game_folder(
    source: &Directory,
    destination: &Directory,
    total_bytes: u64,
    project_d2: bool,
    control: &OperationControl,
) -> io::Result<()> {
    let mut copied = 0_u64;
    copy_tree(
        source,
        destination,
        0,
        top_level(project_d2),
        total_bytes,
        &mut copied,
        control,
    )
    .inspect_err(|error| {
        if error.kind() == io::ErrorKind::Interrupted && control.is_cancelled() {
            control.mark_cancelled_safe();
        }
    })
}

/// Where `copy_tree` is: the game folder itself (with or without its
/// ProjectD2), or below it.
#[derive(Clone, Copy, Eq, PartialEq)]
enum Level {
    Top { project_d2: bool },
    Below,
}

const fn top_level(project_d2: bool) -> Level {
    Level::Top { project_d2 }
}

fn copy_tree(
    source: &Directory,
    destination: &Directory,
    depth: usize,
    level: Level,
    total: u64,
    copied: &mut u64,
    control: &OperationControl,
) -> io::Result<()> {
    let top = matches!(level, Level::Top { .. });
    if depth > MAX_TREE_DEPTH {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the game folder is too deep",
        ));
    }
    let mut names = source.names()?;
    names.sort();
    // At the top a name takes its canonical form unless another entry has
    // it already (`save` beside `Save`): that one is copied under its own
    // name, so nothing is merged or lost (the keeper merges a second
    // `Save` at the next keep).
    let mut taken = names
        .iter()
        .filter(|name| top && canonical_name(name) == **name)
        .cloned()
        .collect::<BTreeSet<_>>();
    for name in names {
        if control.is_cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "copy stopped"));
        }
        if level == (Level::Top { project_d2: false }) && is_project_d2(&name) {
            continue;
        }
        let node = source.open_beneath(Path::new(&name), OFlags::PATH)?;
        let metadata = node.metadata()?;
        let target = match top.then(|| canonical_name(&name)) {
            Some(canonical) if canonical == name || taken.insert(canonical.clone()) => canonical,
            _ => name.clone(),
        };
        if metadata.is_dir() {
            let child = source.open_directory(Path::new(&name))?;
            let created = destination.ensure_directory(Path::new(&target))?;
            copy_tree(
                &child,
                &created,
                depth + 1,
                Level::Below,
                total,
                copied,
                control,
            )?;
        } else if metadata.is_file() {
            let mut input = source.open_beneath(Path::new(&name), OFlags::RDONLY)?;
            // Its times before the copy reads it.
            let times = input.metadata()?;
            if !times.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "a game file changed while it was copied",
                ));
            }
            let mut output = destination.create_file(&target)?;
            copy_file(&mut input, &mut output, total, copied, control)?;
            if sha256_hex(&mut input)? != sha256_hex(&mut output)? {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "a copied game file did not verify",
                ));
            }
            if !crate::android_host() {
                output.set_times(
                    std::fs::FileTimes::new()
                        .set_accessed(times.accessed()?)
                        .set_modified(times.modified()?),
                )?;
                output.sync_all()?;
            }
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the game folder contains a link or special file",
            ));
        }
    }
    destination.sync()
}

fn copy_file(
    input: &mut File,
    output: &mut File,
    total: u64,
    copied: &mut u64,
    control: &OperationControl,
) -> io::Result<()> {
    let size = input.metadata()?.len();
    // A reflink shares the blocks on the same filesystem; anything else is
    // a plain copy.
    if rustix::fs::ioctl_ficlone(&*output, &*input).is_err() {
        output.set_len(0)?;
        output.seek(SeekFrom::Start(0))?;
        input.seek(SeekFrom::Start(0))?;
        let mut buffer = vec![0_u8; 1024 * 1024];
        loop {
            if control.is_cancelled() {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "copy stopped"));
            }
            let read = input.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            output.write_all(&buffer[..read])?;
            report_copy(
                control,
                (*copied).saturating_add(output.stream_position()?),
                total,
            );
        }
    }
    output.sync_all()?;
    if output.metadata()?.len() != size {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "a copied game file has the wrong size",
        ));
    }
    *copied = copied.saturating_add(size);
    report_copy(control, *copied, total);
    Ok(())
}

fn report_copy(control: &OperationControl, copied: u64, total: u64) {
    control.report(
        WorkProgress::for_phase(WorkPhase::ImportGame).with_file_bytes(copied, Some(total)),
    );
}

#[cfg(test)]
pub(crate) mod tests {
    use std::{
        fs,
        os::unix::fs::{MetadataExt, symlink},
    };

    use super::*;

    /// A complete Diablo II folder whose `Game.exe` and `Patch_D2.mpq` are
    /// `game` and `archive`.
    pub(crate) fn write_game_folder(path: &Path, game: &[u8], archive: &[u8]) {
        fs::create_dir_all(path.join("Save")).unwrap();
        for name in BASE_FILES.into_iter().chain(EXPANSION_FILES) {
            fs::write(path.join(name), name.as_bytes()).unwrap();
        }
        fs::write(path.join("Game.exe"), game).unwrap();
        fs::write(path.join("Patch_D2.mpq"), archive).unwrap();
        fs::write(path.join("Save/hero.d2s"), b"hero").unwrap();
        fs::create_dir_all(path.join("ProjectD2")).unwrap();
        fs::write(path.join("ProjectD2/Game.exe"), b"pd2").unwrap();
    }

    /// A `Game.exe` whose version resource names `version`, as Blizzard's
    /// files do ("1, 0, 13, 60" is 1.13c).
    fn version_resource(version: &str) -> Vec<u8> {
        let mut bytes = b"MZ fixture ".to_vec();
        for text in ["FileVersion\0", "\0", version, "\0"] {
            bytes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
        }
        bytes
    }

    fn validate(path: &Path) -> Result<ExistingInstall, ExistingInstallProblem> {
        validate_existing(path, Path::new("/nonexistent/pd2-launcher"))
    }

    #[test]
    fn a_folder_is_named_for_what_keeps_it_from_being_usable() {
        let fixture = tempfile::tempdir().unwrap();
        let root = Path::new("/nonexistent/pd2-launcher");
        let folder = fixture.path().join("Diablo II");

        // Not there, not a folder, not Diablo II.
        assert_eq!(
            validate_existing(&folder, root),
            Err(ExistingInstallProblem::NotFound)
        );
        fs::write(fixture.path().join("file"), b"x").unwrap();
        assert_eq!(
            validate_existing(&fixture.path().join("file"), root),
            Err(ExistingInstallProblem::NotFound)
        );
        fs::create_dir(&folder).unwrap();
        assert_eq!(
            validate_existing(&folder, root),
            Err(ExistingInstallProblem::NotDiablo)
        );

        // Inside, or containing, the launcher's own folder.
        write_game_folder(&folder, b"game", b"archive");
        assert_eq!(
            validate_existing(&folder, fixture.path()),
            Err(ExistingInstallProblem::InsideLauncher)
        );
        assert_eq!(
            validate_existing(fixture.path(), &folder.join("pd2-launcher")),
            Err(ExistingInstallProblem::InsideLauncher)
        );
    }

    #[test]
    fn the_rest_of_the_folder_must_be_complete_and_link_free() {
        let fixture = tempfile::tempdir().unwrap();
        let folder = fixture.path().join("Diablo II");
        let (game, archive) = (b"game-exe".as_slice(), b"patch-archive".as_slice());
        write_game_folder(&folder, game, archive);

        let valid = validate(&folder).expect("a complete folder");
        assert_eq!(valid.path(), folder);
        // The desktop's copy brings ProjectD2 along (`pd2`, 3 bytes); it
        // holds no Project Diablo 2 to adopt without its launch files.
        assert!(valid.copy_bytes() > 0);
        assert!(!valid.project_diablo());
        assert_eq!(
            valid.copy_bytes() - 3,
            BASE_FILES
                .into_iter()
                .chain(EXPANSION_FILES)
                .filter(|name| !matches!(*name, "Game.exe" | "Patch_D2.mpq"))
                .map(|name| name.len() as u64)
                .sum::<u64>()
                + game.len() as u64
                + archive.len() as u64
                + 4
        );

        // A ProjectD2 with the client's launch files is one to adopt.
        fs::write(folder.join("ProjectD2/ProjectDiablo.dll"), b"dll").unwrap();
        let found = validate(&folder).expect("with PD2");
        assert!(found.project_diablo());
        assert_eq!(found.copy_bytes(), valid.copy_bytes() + 3);
        fs::write(folder.join("ProjectD2/ProjectDiablo.dll"), b"").unwrap();
        assert!(
            !validate(&folder).unwrap().project_diablo(),
            "an empty launch file is what a stopped copy leaves"
        );
        fs::remove_file(folder.join("ProjectD2/ProjectDiablo.dll")).unwrap();

        // Names match without regard to case, as Windows does.
        fs::rename(folder.join("Patch_D2.mpq"), folder.join("patch_d2.mpq")).unwrap();
        assert!(validate(&folder).is_ok());

        // No Lord of Destruction; a missing file.
        fs::remove_file(folder.join("d2exp.mpq")).unwrap();
        assert_eq!(validate(&folder), Err(ExistingInstallProblem::NoExpansion));
        fs::write(folder.join("d2exp.mpq"), b"lod").unwrap();
        fs::remove_file(folder.join("d2music.mpq")).unwrap();
        assert_eq!(
            validate(&folder),
            Err(ExistingInstallProblem::MissingFile("d2music.mpq"))
        );
        fs::write(folder.join("d2music.mpq"), b"music").unwrap();

        // A link anywhere in it, even one pointing inside, or a linked
        // required file.
        let outside = fixture.path().join("outside");
        fs::create_dir(&outside).unwrap();
        symlink(&outside, folder.join("Save/elsewhere")).unwrap();
        assert_eq!(validate(&folder), Err(ExistingInstallProblem::Links));
        fs::remove_file(folder.join("Save/elsewhere")).unwrap();
        fs::rename(folder.join("d2xtalk.mpq"), outside.join("d2xtalk.mpq")).unwrap();
        symlink(outside.join("d2xtalk.mpq"), folder.join("d2xtalk.mpq")).unwrap();
        assert_eq!(validate(&folder), Err(ExistingInstallProblem::Links));
        fs::remove_file(folder.join("d2xtalk.mpq")).unwrap();
        fs::write(folder.join("d2xtalk.mpq"), b"talk").unwrap();

        // The folder reached through a link.
        symlink(&folder, fixture.path().join("linked")).unwrap();
        assert_eq!(
            validate(&fixture.path().join("linked")),
            Err(ExistingInstallProblem::Links)
        );
    }

    #[test]
    fn a_folder_of_any_diablo_ii_version_is_used_as_it_is() {
        let fixture = tempfile::tempdir().unwrap();
        let folder = fixture.path().join("Diablo II");
        // 1.13c, 1.14b and 1.14d (the version resources Blizzard's files
        // have), others, a `Game.exe` that is no Blizzard file, and a
        // `Patch_D2.mpq` that does not match its `Game.exe`: the player
        // chose the folder, so its version is not checked.
        for (version, archive) in [
            ("1, 0, 13, 60", b"1.13c archive".as_slice()),
            ("1.14.0.64", b"1.14a archive"),
            ("1.14.1.68", b"any language's archive"),
            ("1, 14, 3, 71", b"not 1.14d's archive"),
            ("2.0.0", b"archive"),
        ] {
            write_game_folder(&folder, &version_resource(version), archive);
            fs::remove_dir_all(folder.join("ProjectD2")).unwrap();
            // Without a ProjectD2: Install Project Diablo 2 installs it.
            let found = validate(&folder).expect(version);
            assert!(!found.project_diablo(), "{version}");
            assert!(usable_in_place(&found), "{version}");
            // With an older or newer Project Diablo 2: it is adopted, and
            // its update brings it up to date.
            fs::create_dir(folder.join("ProjectD2")).unwrap();
            fs::write(folder.join("ProjectD2/Game.exe"), b"pd2 1.13c").unwrap();
            fs::write(folder.join("ProjectD2/ProjectDiablo.dll"), b"older pd2").unwrap();
            let found = validate(&folder).expect(version);
            assert!(found.project_diablo(), "{version}");
            // Validation only reads: nothing is patched.
            assert_eq!(
                fs::read(folder.join("Game.exe")).unwrap(),
                version_resource(version)
            );
            assert_eq!(fs::read(folder.join("Patch_D2.mpq")).unwrap(), archive);
        }
        // What stays required is the files of Diablo II and Lord of
        // Destruction.
        fs::remove_file(folder.join("Patch_D2.mpq")).unwrap();
        assert_eq!(
            validate(&folder),
            Err(ExistingInstallProblem::MissingFile("Patch_D2.mpq"))
        );
        fs::write(folder.join("Patch_D2.mpq"), b"archive").unwrap();
        fs::remove_file(folder.join("Game.exe")).unwrap();
        assert_eq!(
            validate(&folder),
            Err(ExistingInstallProblem::MissingFile("Game.exe"))
        );
    }

    #[test]
    fn an_unreadable_folder_is_reported_as_such() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = tempfile::tempdir().unwrap();
        let folder = fixture.path().join("Diablo II");
        write_game_folder(&folder, b"game", b"archive");
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read_dir(&folder).is_ok() {
            // Permissions don't apply here (running as root).
            fs::set_permissions(&folder, fs::Permissions::from_mode(0o700)).unwrap();
            return;
        }
        let result = validate_existing(&folder, Path::new("/nonexistent/x"));
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(result, Err(ExistingInstallProblem::Unreadable));
    }

    #[test]
    fn a_copy_is_verified_brings_project_d2_only_when_asked_and_uses_canonical_names() {
        let fixture = tempfile::tempdir().unwrap();
        let source_path = fixture.path().join("Diablo II");
        write_game_folder(&source_path, b"game", b"archive");
        fs::rename(
            source_path.join("Patch_D2.mpq"),
            source_path.join("patch_d2.mpq"),
        )
        .unwrap();
        let destination_path = fixture.path().join("copy");
        fs::create_dir(&destination_path).unwrap();
        let source = Directory::open(&source_path).unwrap();
        let destination = Directory::open(&destination_path).unwrap();
        let control = OperationControl::default();
        // A character's file keeps its time; a `save` folder takes the
        // name the Launcher keeps characters under.
        let saved = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000);
        File::options()
            .write(true)
            .open(source_path.join("Save/hero.d2s"))
            .unwrap()
            .set_modified(saved)
            .unwrap();
        fs::rename(source_path.join("Save"), source_path.join("save")).unwrap();
        copy_game_folder(&source, &destination, 100, false, &control).expect("copied");
        fs::rename(source_path.join("save"), source_path.join("Save")).unwrap();
        assert_eq!(
            fs::read(destination_path.join("Patch_D2.mpq")).unwrap(),
            b"archive"
        );
        assert_eq!(
            fs::read(destination_path.join("Save/hero.d2s")).unwrap(),
            b"hero"
        );
        assert!(!destination_path.join("save").exists());
        assert_eq!(
            fs::metadata(destination_path.join("Save/hero.d2s"))
                .unwrap()
                .modified()
                .unwrap(),
            saved
        );
        assert!(!destination_path.join("ProjectD2").exists());
        // The desktop's copy brings ProjectD2 along, under that name, for the
        // install to adopt.
        assert!(copies_project_d2(), "the tests run as the desktop");
        fs::rename(source_path.join("ProjectD2"), source_path.join("projectd2")).unwrap();
        let with_project = fixture.path().join("with-project");
        fs::create_dir(&with_project).unwrap();
        copy_game_folder(
            &source,
            &Directory::open(&with_project).unwrap(),
            100,
            true,
            &OperationControl::default(),
        )
        .expect("copied");
        assert_eq!(
            fs::read(with_project.join("ProjectD2/Game.exe")).unwrap(),
            b"pd2"
        );
        assert!(!with_project.join("projectd2").exists());
        fs::rename(source_path.join("projectd2"), source_path.join("ProjectD2")).unwrap();
        // The original is untouched.
        assert!(source_path.join("patch_d2.mpq").is_file());
        assert!(source_path.join("ProjectD2/Game.exe").is_file());

        // A stopped copy reports it and leaves the rest for Start over.
        let stopped = fixture.path().join("stopped");
        fs::create_dir(&stopped).unwrap();
        let control = OperationControl::default();
        control.request_cancel();
        assert!(
            copy_game_folder(
                &source,
                &Directory::open(&stopped).unwrap(),
                1,
                true,
                &control
            )
            .is_err()
        );
        assert_eq!(
            control.cancellation_safety(),
            crate::operation::CancellationSafety::ProvenSafe
        );

        // A link found during the copy fails it.
        symlink("/etc", source_path.join("Save/link")).unwrap();
        let again = fixture.path().join("again");
        fs::create_dir(&again).unwrap();
        assert!(
            copy_game_folder(
                &source,
                &Directory::open(&again).unwrap(),
                1,
                true,
                &OperationControl::default()
            )
            .is_err()
        );
    }

    #[test]
    fn a_second_spelling_of_a_name_is_copied_under_its_own_name() {
        let fixture = tempfile::tempdir().unwrap();
        let source_path = fixture.path().join("Diablo II");
        write_game_folder(&source_path, b"game", b"archive");
        // `Save` and `save` both: the second keeps its name, nothing merged.
        fs::create_dir(source_path.join("save")).unwrap();
        fs::write(source_path.join("save/hero.d2s"), b"other hero").unwrap();
        fs::write(source_path.join("game.exe"), b"other game").unwrap();
        let destination_path = fixture.path().join("copy");
        fs::create_dir(&destination_path).unwrap();
        copy_game_folder(
            &Directory::open(&source_path).unwrap(),
            &Directory::open(&destination_path).unwrap(),
            100,
            false,
            &OperationControl::default(),
        )
        .expect("copied");
        let read = |name: &str| fs::read(destination_path.join(name)).unwrap();
        assert_eq!(read("Save/hero.d2s"), b"hero");
        assert_eq!(read("save/hero.d2s"), b"other hero");
        assert_eq!(read("Game.exe"), b"game");
        assert_eq!(read("game.exe"), b"other game");
    }

    #[test]
    fn a_differently_cased_project_diablo_folder_can_only_be_copied() {
        let fixture = tempfile::tempdir().unwrap();
        let game = fixture.path().join("Diablo II");
        write_game_folder(&game, b"game", b"archive");
        let valid = validate(&game).unwrap();
        assert!(usable_in_place(&valid));
        fs::rename(game.join("ProjectD2"), game.join("projectd2")).unwrap();
        let valid = validate(&game).unwrap();
        assert!(!usable_in_place(&valid));
        fs::remove_dir_all(game.join("projectd2")).unwrap();
        let valid = validate(&game).unwrap();
        assert!(usable_in_place(&valid));
    }

    #[test]
    fn a_folder_used_in_place_is_recorded_by_identity_and_fails_closed() {
        let fixture = tempfile::tempdir().unwrap();
        let root_path = fixture.path().join("root");
        fs::create_dir_all(root_path.join("prefix")).unwrap();
        let root = Directory::open(&root_path).unwrap();
        assert_eq!(GameFolder::read(&root), Ok(GameFolder::Managed));

        let game_path = fixture.path().join("My Games/Diablo II");
        fs::create_dir_all(&game_path).unwrap();
        let file = fs::File::open(&game_path).unwrap();
        let external = ExternalGameFolder {
            path: game_path.clone(),
            filesystem: crate::safe_fs::filesystem_id(&file).unwrap(),
            inode: file.metadata().unwrap().ino(),
        };
        write_pointer(
            &root.open_directory(Path::new("prefix")).unwrap(),
            &external,
        )
        .unwrap();
        let folder = GameFolder::resolve(&root).expect("available");
        assert!(folder.is_external());
        assert_eq!(folder.host_path(&root_path), game_path);
        assert!(folder.windows_path().starts_with(r"Z:\"));
        assert!(folder.windows_path().ends_with(r"\My Games\Diablo II"));

        // Replaced by another folder with the same name: unavailable.
        fs::rename(&game_path, fixture.path().join("moved")).unwrap();
        fs::create_dir_all(&game_path).unwrap();
        assert_eq!(
            GameFolder::resolve(&root),
            Err(GameFolderError::Unavailable(game_path.clone()))
        );
        // Gone: unavailable.
        fs::remove_dir(&game_path).unwrap();
        assert_eq!(
            GameFolder::resolve(&root),
            Err(GameFolderError::Unavailable(game_path.clone()))
        );
        // Back where it was: available again.
        fs::rename(fixture.path().join("moved"), &game_path).unwrap();
        assert!(GameFolder::resolve(&root).is_ok());
        // Reached through a link: unavailable.
        fs::rename(&game_path, fixture.path().join("real")).unwrap();
        symlink(fixture.path().join("real"), &game_path).unwrap();
        assert!(matches!(
            GameFolder::resolve(&root),
            Err(GameFolderError::Unavailable(_))
        ));

        // A record the launcher didn't write fails closed.
        fs::write(root_path.join("prefix").join(POINTER_NAME), b"{}").unwrap();
        assert_eq!(GameFolder::read(&root), Err(GameFolderError::RecordInvalid));
        fs::write(
            root_path.join("prefix").join(POINTER_NAME),
            br#"{"schema":1,"path":"relative","filesystem":1,"inode":2}"#,
        )
        .unwrap();
        assert_eq!(GameFolder::read(&root), Err(GameFolderError::RecordInvalid));
    }

    /// On the Android host the import checks only its short list (Diablo
    /// II's and Lord of Destruction's programs and main archives, Project
    /// Diablo 2's programs), each a file that is not empty, with no version
    /// check; every missing one is named, in the list's order.
    #[test]
    fn on_android_the_import_checks_its_short_list_and_names_every_missing_file() {
        let fixture = tempfile::tempdir().unwrap();
        let folder = fixture.path().join("Diablo II");
        fs::create_dir_all(folder.join("projectd2")).unwrap();
        for name in [
            "game.exe",
            "DIABLO II.EXE",
            "d2data.mpq",
            "d2char.mpq",
            "d2sfx.mpq",
            "d2exp.mpq",
        ] {
            fs::write(folder.join(name), b"x").unwrap();
        }
        for name in ["GAME.EXE", "projectdiablo.dll", "pd2launcher.exe"] {
            fs::write(folder.join("projectd2").join(name), b"x").unwrap();
        }
        let root = Path::new("/nonexistent/pd2-launcher");
        // No Patch_D2.mpq, no music, speech or video archives: not checked.
        assert!(validate_existing_on(&folder, root, true).is_ok());
        assert!(matches!(
            validate_existing_on(&folder, root, false),
            Err(ExistingInstallProblem::MissingFile(_))
        ));

        fs::remove_file(folder.join("d2char.mpq")).unwrap();
        fs::write(folder.join("projectd2/pd2launcher.exe"), b"").unwrap();
        fs::remove_file(folder.join("projectd2/projectdiablo.dll")).unwrap();
        let directory = Directory::open(&folder).unwrap();
        let names = case_insensitive_names(&directory).unwrap();
        assert_eq!(
            missing_import_files(&directory, &names).unwrap(),
            [
                "d2char.mpq",
                "ProjectD2/ProjectDiablo.dll",
                "ProjectD2/PD2Launcher.exe"
            ]
        );
        assert!(matches!(
            validate_existing_on(&folder, root, true),
            Err(ExistingInstallProblem::MissingFile("d2char.mpq"))
        ));
        fs::remove_dir_all(folder.join("projectd2")).unwrap();
        let names = case_insensitive_names(&directory).unwrap();
        assert_eq!(
            missing_import_files(&directory, &names).unwrap(),
            [
                "d2char.mpq",
                "ProjectD2/Game.exe",
                "ProjectD2/ProjectDiablo.dll",
                "ProjectD2/PD2Launcher.exe"
            ]
        );
    }

    #[test]
    fn the_import_record_has_the_form_earlier_releases_wrote() {
        let fixture = tempfile::tempdir().unwrap();
        let prefix = Directory::open(fixture.path()).unwrap();
        write_import_record(&prefix, ImportMode::InPlace, r"Z:\games\Diablo II").unwrap();
        assert_eq!(
            fs::read_to_string(fixture.path().join(IMPORT_RECORD_NAME)).unwrap(),
            r#"{"schema":1,"mode":"in-place","install_path":"Z:\\games\\Diablo II"}"#
        );
    }

    /// A managed root with a prefix folder to link in, as the Android host
    /// lays it out.
    fn android_root() -> (tempfile::TempDir, Directory) {
        let fixture = tempfile::tempdir().unwrap();
        fs::create_dir_all(fixture.path().join("prefix/drive_c/Program Files (x86)")).unwrap();
        let root = Directory::open(fixture.path()).unwrap();
        (fixture, root)
    }

    #[test]
    fn the_android_game_folder_is_outside_the_prefix_and_the_records_are_in_the_root() {
        assert_eq!(managed_relative(true), "Diablo II");
        assert_eq!(managed_relative(false), GAME_RELATIVE);
        assert_eq!(records_relative(true), "");
        assert_eq!(records_relative(false), "prefix");
        let fixture = tempfile::tempdir().unwrap();
        assert_eq!(
            GameFolder::Managed.host_path_on(fixture.path(), true),
            fixture.path().join("Diablo II")
        );
        assert_eq!(
            GameFolder::Managed.host_path_on(fixture.path(), false),
            fixture.path().join(GAME_RELATIVE)
        );
        // Wine sees it at the same place on both hosts.
        assert_eq!(
            GameFolder::Managed.windows_path(),
            r"C:\Program Files (x86)\Diablo II"
        );
    }

    #[test]
    fn the_game_folder_link_is_relative_and_replaces_only_a_link() {
        let (fixture, root) = android_root();
        let link = fixture.path().join(GAME_RELATIVE);
        assert!(link_game(&root).unwrap());
        assert_eq!(fs::read_link(&link).unwrap(), Path::new(GAME_LINK_TARGET));
        // Again: nothing changes.
        assert!(!link_game(&root).unwrap());
        // It leads to the game folder outside the prefix.
        fs::create_dir_all(fixture.path().join("Diablo II/ProjectD2")).unwrap();
        assert!(link.join("ProjectD2").is_dir());
        // A link elsewhere is replaced, and only the link goes.
        let elsewhere = fixture.path().join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        fs::write(elsewhere.join("keep"), b"keep").unwrap();
        fs::remove_file(&link).unwrap();
        symlink(&elsewhere, &link).unwrap();
        assert!(link_game(&root).unwrap());
        assert_eq!(fs::read_link(&link).unwrap(), Path::new(GAME_LINK_TARGET));
        assert_eq!(fs::read(elsewhere.join("keep")).unwrap(), b"keep");
    }

    #[test]
    fn a_game_folder_inside_the_prefix_is_refused_and_kept() {
        let (fixture, root) = android_root();
        // The layout before the disposable prefix: the game in the prefix.
        let game = fixture.path().join(GAME_RELATIVE);
        fs::create_dir_all(game.join("Save")).unwrap();
        fs::write(game.join("Save/hero.d2s"), b"hero").unwrap();
        assert!(link_game(&root).is_err());
        assert!(prepare_game_location(&root, true).is_err());
        assert!(fs::symlink_metadata(&game).unwrap().is_dir());
        assert_eq!(fs::read(game.join("Save/hero.d2s")).unwrap(), b"hero");
    }

    #[test]
    fn the_game_location_and_its_link_are_made_only_on_android() {
        let (fixture, root) = android_root();
        prepare_game_location(&root, false).unwrap();
        assert!(!fixture.path().join("Diablo II").exists());
        assert!(fs::symlink_metadata(fixture.path().join(GAME_RELATIVE)).is_err());
        prepare_game_location(&root, true).unwrap();
        assert!(fixture.path().join("Diablo II").is_dir());
        assert_eq!(
            fs::read_link(fixture.path().join(GAME_RELATIVE)).unwrap(),
            Path::new(GAME_LINK_TARGET)
        );
        // A game there stays as it is.
        fs::write(fixture.path().join("Diablo II/Game.exe"), b"game").unwrap();
        prepare_game_location(&root, true).unwrap();
        assert_eq!(
            fs::read(fixture.path().join("Diablo II/Game.exe")).unwrap(),
            b"game"
        );
    }

    #[test]
    fn wine_starts_in_the_game_folder_through_the_link() {
        let (fixture, root) = android_root();
        let managed = fixture.path();
        prepare_game_location(&root, true).unwrap();
        let project = managed.join("Diablo II/ProjectD2");
        fs::create_dir_all(&project).unwrap();
        let pwd = wine_pwd(managed, &project, true).unwrap();
        // The path through the prefix's C: drive, whose text Wine names
        // C:\Program Files (x86)\Diablo II\ProjectD2.
        assert_eq!(
            pwd,
            managed.join("prefix/drive_c/Program Files (x86)/Diablo II/ProjectD2")
        );
        // Wine takes PWD only when it names the working folder itself.
        let (by_pwd, real) = (fs::metadata(&pwd).unwrap(), fs::metadata(&project).unwrap());
        assert_eq!((by_pwd.dev(), by_pwd.ino()), (real.dev(), real.ino()));
        assert_eq!(
            wine_pwd(managed, &managed.join("Diablo II"), true).unwrap(),
            managed.join(GAME_RELATIVE)
        );
        // Outside the game folder, and on the desktop: no PWD.
        assert_eq!(wine_pwd(managed, managed, true), None);
        assert_eq!(wine_pwd(managed, &managed.join("cache"), true), None);
        assert_eq!(wine_pwd(managed, &project, false), None);
    }

    #[test]
    fn windows_paths_use_the_z_drive_and_refuse_unusable_names() {
        assert_eq!(
            windows_path_for(Path::new("/home/deck/Games/Diablo II")).as_deref(),
            Some(r"Z:\home\deck\Games\Diablo II")
        );
        assert_eq!(windows_path_for(Path::new("relative")), None);
        assert_eq!(windows_path_for(Path::new("/")), None);
        assert_eq!(windows_path_for(Path::new("/games/a:b")), None);
        assert_eq!(windows_path_for(Path::new("/games/a\\b")), None);
        assert_eq!(windows_path_for(Path::new("/home/zoë/Diablo II")), None);
    }
}
