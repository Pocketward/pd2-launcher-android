//! Choosing and changing the managed root's location.

use std::{
    ffi::OsStr,
    io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    location::{self, INSTALL_DIRECTORY, Pointer},
    persistence::{LauncherPaths, open_root_layout},
    safe_fs::{Directory, ProbeFaults, RootCapability},
};

/// The relocation intent, in the Launcher's state directory.
pub(crate) const INTENT_FILE: &str = "relocation.json";
/// The root a move to another filesystem left behind, in the state
/// directory, until the user removes it.
pub(crate) const PREVIOUS_FILE: &str = "previous-root.json";
const INTENT_SCHEMA: u32 = 1;
const INTENT_LIMIT: u64 = 8 * 1024;

/// Why a location could not be chosen or changed. Each maps to a stable
/// code the presentation explains.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RelocationError {
    /// The picked parent is not an absolute, reachable directory without
    /// symbolic links in its path.
    ParentInvalid,
    /// `<parent>/pd2-launcher` exists and holds something.
    TargetNotEmpty,
    /// The new root lacks a capability the Launcher needs.
    Capability(RootCapability),
    /// Another Launcher has a root open or is changing it.
    Busy,
    /// The new location is the current one.
    SameLocation,
    /// A previous installation must be removed before another move.
    PreviousPending,
    /// The previous installation is gone, changed, or in use.
    PreviousUnavailable,
    /// The previous installation's characters could not be kept first, so
    /// it was not removed.
    SavesNotKept,
    Io,
}

impl RelocationError {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::ParentInvalid => "location-parent-invalid",
            Self::TargetNotEmpty => "location-target-not-empty",
            Self::Capability(RootCapability::Executable) => "location-noexec",
            Self::Capability(RootCapability::FreeSpace) => "location-free-space",
            Self::Capability(RootCapability::Writable) => "location-not-writable",
            Self::Capability(RootCapability::AtomicRename) => "location-rename-unsupported",
            Self::Capability(RootCapability::Locking) => "location-locking-unsupported",
            Self::Capability(RootCapability::SymbolicLinks) => "location-symlinks-unsupported",
            Self::Capability(RootCapability::CaseSensitive) => "location-case-insensitive",
            Self::Busy => "install-location-busy",
            Self::SameLocation => "location-unchanged",
            Self::PreviousPending => "location-previous-pending",
            Self::PreviousUnavailable => "location-previous-unavailable",
            Self::SavesNotKept => crate::save_keeper::SAVES_NOT_KEPT,
            Self::Io => "location-change-failed",
        }
    }
}

/// A durable record that a same-filesystem move is in progress, written
/// before the rename and removed after the pointer names the new root.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    schema: u32,
    from: PathBuf,
    to: PathBuf,
    root_id: String,
}

/// Crash-injection seam for a same-filesystem move.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MoveStep {
    IntentWritten,
    Exchanged,
    PointerWritten,
    SourceCleared,
}

pub(crate) trait MoveFaults {
    fn after(&self, step: MoveStep) -> io::Result<()>;
}

pub(crate) struct NoMoveFaults;

impl MoveFaults for NoMoveFaults {
    fn after(&self, _step: MoveStep) -> io::Result<()> {
        Ok(())
    }
}

/// Whether `parent` is on the same filesystem as the managed root at
/// `root_path`, so the root can be renamed there.
pub(crate) fn same_filesystem(root_path: &Path, parent: &Path) -> Result<bool, RelocationError> {
    let root = Directory::open(root_path).map_err(|_| RelocationError::Io)?;
    let parent = Directory::open(parent).map_err(|_| RelocationError::ParentInvalid)?;
    Ok(root.device_id().map_err(|_| RelocationError::Io)?
        == parent
            .device_id()
            .map_err(|_| RelocationError::ParentInvalid)?
        && root.filesystem_id().map_err(|_| RelocationError::Io)?
            == parent
                .filesystem_id()
                .map_err(|_| RelocationError::ParentInvalid)?)
}

/// Move the managed root at `root_path` to `<parent>/pd2-launcher` on the
/// same filesystem. The caller holds the root-selection lock
/// exclusively and the root's installation lease. The destination is
/// created empty and probed first, then swapped with the root in one
/// `RENAME_EXCHANGE` behind the relocation intent; the pointer is written
/// next, and only then is the empty directory left at the old path removed
/// and the intent cleared. [`recover`] completes or undoes any cut.
pub(crate) fn move_same_filesystem(
    paths: &LauncherPaths,
    root_path: &Path,
    parent: &Path,
    minimum_free: u64,
    probe_faults: &dyn ProbeFaults,
    faults: &dyn MoveFaults,
) -> Result<PathBuf, RelocationError> {
    let source_parent_path = root_path.parent().ok_or(RelocationError::Io)?;
    let source_name = root_path.file_name().ok_or(RelocationError::Io)?;
    let source_parent = Directory::open(source_parent_path).map_err(|_| RelocationError::Io)?;
    let root = source_parent
        .open_directory(Path::new(source_name))
        .map_err(|_| RelocationError::Io)?;
    let root_id = match location::read_marker(&root).map_err(|_| RelocationError::Io)? {
        Some(root_id) => root_id,
        None => {
            let root_id = location::new_root_id().map_err(|_| RelocationError::Io)?;
            location::write_marker(&root, &root_id).map_err(|_| RelocationError::Io)?;
            root_id
        }
    };
    if !parent.is_absolute() || parent.starts_with(root_path) {
        return Err(RelocationError::ParentInvalid);
    }
    let target_parent = Directory::open(parent).map_err(|_| RelocationError::ParentInvalid)?;
    let (target, created) = target_parent
        .create_or_open_empty_directory(OsStr::new(INSTALL_DIRECTORY))
        .map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                RelocationError::TargetNotEmpty
            } else {
                RelocationError::ParentInvalid
            }
        })?;
    let discard_target = |target_parent: &Directory| {
        if created {
            let _ = target_parent.remove_empty_directory(OsStr::new(INSTALL_DIRECTORY));
        }
    };
    if let Err(capability) = target.probe_root_capabilities(minimum_free, probe_faults) {
        discard_target(&target_parent);
        return Err(RelocationError::Capability(capability));
    }
    let to = parent.join(INSTALL_DIRECTORY);
    let intent = Intent {
        schema: INTENT_SCHEMA,
        from: root_path.to_owned(),
        to: to.clone(),
        root_id: root_id.clone(),
    };
    let write_intent = serde_json::to_vec(&intent)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "intent encode failed"))
        .and_then(|bytes| {
            crate::persistence::atomic_write(&paths.state_root().join(INTENT_FILE), &bytes)
        });
    if write_intent.is_err() {
        discard_target(&target_parent);
        return Err(RelocationError::Io);
    }
    let step = |step| faults.after(step).map_err(|_| RelocationError::Io);
    step(MoveStep::IntentWritten)?;
    if source_parent
        .exchange_directories(
            source_name,
            &root,
            &target_parent,
            OsStr::new(INSTALL_DIRECTORY),
            &target,
        )
        .is_err()
    {
        // Nothing moved: undo the probe folder and the intent now rather
        // than leaving them to the next start's recovery.
        discard_target(&target_parent);
        let _ = clear_intent(paths);
        return Err(RelocationError::Io);
    }
    step(MoveStep::Exchanged)?;
    location::write_pointer(paths.config_root(), &Pointer::new(to.clone(), root_id))
        .map_err(|_| RelocationError::Io)?;
    paths.set_managed_root(Some(to.clone()));
    step(MoveStep::PointerWritten)?;
    source_parent
        .remove_empty_directory(source_name)
        .map_err(|_| RelocationError::Io)?;
    step(MoveStep::SourceCleared)?;
    clear_intent(paths).map_err(|_| RelocationError::Io)?;
    Ok(to)
}

fn clear_intent(paths: &LauncherPaths) -> io::Result<()> {
    let state = Directory::open(paths.state_root())?;
    match state.open_file(Path::new(INTENT_FILE)) {
        Ok(file) => state.remove_open_file_bounded(OsStr::new(INTENT_FILE), &file),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Complete or undo a same-filesystem move a crash interrupted, from what
/// exists: the root (its marker) at the destination gets the pointer; a
/// root still at the source keeps the pointer it had. Only an empty
/// directory the move itself left behind is removed. An intent whose root
/// is at neither place is kept, and the location then resolves as usual
/// (which may be unavailable).
pub(crate) fn recover(paths: &LauncherPaths) -> io::Result<()> {
    let bytes = match crate::persistence::read_bounded_regular(
        &paths.state_root().join(INTENT_FILE),
        INTENT_LIMIT,
    ) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let intent: Intent = serde_json::from_slice(&bytes)
        .ok()
        .filter(|intent: &Intent| {
            intent.schema == INTENT_SCHEMA && intent.from.is_absolute() && intent.to.is_absolute()
        })
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "relocation intent was invalid")
        })?;
    let holds_root = |path: &Path| {
        Directory::open(path)
            .ok()
            .and_then(|root| location::read_marker(&root).ok().flatten())
            .is_some_and(|root_id| root_id == intent.root_id)
    };
    let remove_if_empty = |path: &Path| {
        if let (Some(parent), Some(name)) = (path.parent(), path.file_name())
            && let Ok(parent) = Directory::open(parent)
            && parent
                .open_directory(Path::new(name))
                .and_then(|directory| directory.names())
                .is_ok_and(|names| names.is_empty())
        {
            let _ = parent.remove_empty_directory(name);
        }
    };
    if holds_root(&intent.to) {
        location::write_pointer(
            paths.config_root(),
            &Pointer::new(intent.to.clone(), intent.root_id.clone()),
        )?;
        remove_if_empty(&intent.from);
    } else if holds_root(&intent.from) {
        remove_if_empty(&intent.to);
    } else {
        return Ok(());
    }
    clear_intent(paths)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct PreviousRoot {
    schema: u32,
    path: PathBuf,
    root_id: String,
}

fn read_previous(paths: &LauncherPaths) -> Option<PreviousRoot> {
    crate::persistence::read_bounded_regular(&paths.state_root().join(PREVIOUS_FILE), INTENT_LIMIT)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<PreviousRoot>(&bytes).ok())
        .filter(|previous| previous.schema == INTENT_SCHEMA && previous.path.is_absolute())
}

fn clear_previous(paths: &LauncherPaths) -> io::Result<()> {
    let state = Directory::open(paths.state_root())?;
    match state.open_file(Path::new(PREVIOUS_FILE)) {
        Ok(file) => state.remove_open_file_bounded(OsStr::new(PREVIOUS_FILE), &file),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// The installation a move to another filesystem left behind, if any. A
/// record naming the current root (a move interrupted before its pointer)
/// is not a previous installation.
pub(crate) fn previous_root(paths: &LauncherPaths) -> Option<PathBuf> {
    read_previous(paths)
        .map(|previous| previous.path)
        .filter(|path| *path != paths.managed_root())
}

/// Move to another filesystem: nothing is copied but verified
/// downloads, and the old root is not touched. A new root is created and
/// probed, its cache is seeded from the old one (each pinned download
/// re-verified), the old root is recorded as the previous installation,
/// and the pointer is switched last. The next install runs there from the
/// start. The caller holds the root-selection lock exclusively and the old
/// root's installation lease.
pub(crate) fn move_to_new_filesystem(
    paths: &LauncherPaths,
    root_path: &Path,
    parent: &Path,
    minimum_free: u64,
    probe_faults: &dyn ProbeFaults,
    seed: impl FnOnce(&Directory, &Directory) -> io::Result<()>,
) -> Result<PathBuf, RelocationError> {
    if previous_root(paths).is_some() {
        return Err(RelocationError::PreviousPending);
    }
    if parent.starts_with(root_path) {
        return Err(RelocationError::ParentInvalid);
    }
    let old_root = Directory::open(root_path).map_err(|_| RelocationError::Io)?;
    let old_id = match location::read_marker(&old_root).map_err(|_| RelocationError::Io)? {
        Some(root_id) => root_id,
        None => {
            let root_id = location::new_root_id().map_err(|_| RelocationError::Io)?;
            location::write_marker(&old_root, &root_id).map_err(|_| RelocationError::Io)?;
            root_id
        }
    };
    let new_root = create_root(parent, minimum_free, probe_faults)?;
    if seed(&old_root, &new_root.directory).is_err() {
        new_root.discard();
        return Err(RelocationError::Io);
    }
    let previous = serde_json::to_vec(&PreviousRoot {
        schema: INTENT_SCHEMA,
        path: root_path.to_owned(),
        root_id: old_id,
    })
    .map_err(|_| RelocationError::Io)?;
    if crate::persistence::atomic_write(&paths.state_root().join(PREVIOUS_FILE), &previous).is_err()
        || adopt(paths, &new_root).is_err()
    {
        let _ = clear_previous(paths);
        new_root.discard();
        return Err(RelocationError::Io);
    }
    Ok(new_root.path)
}

/// Remove the previous installation, which the user confirmed. It must
/// still carry its recorded marker, must not be the current root, and its
/// installation lease must be free (no other Launcher uses it). `rescue`
/// gets the old root and its path first.
pub(crate) fn remove_previous(
    paths: &LauncherPaths,
    rescue: impl FnOnce(&Directory, &Path) -> io::Result<()>,
) -> Result<(), RelocationError> {
    let previous = read_previous(paths).ok_or(RelocationError::PreviousUnavailable)?;
    if previous.path == paths.managed_root() {
        return Err(RelocationError::PreviousUnavailable);
    }
    let (parent_path, name) = previous
        .path
        .parent()
        .zip(previous.path.file_name())
        .ok_or(RelocationError::PreviousUnavailable)?;
    let parent = match Directory::open(parent_path) {
        Ok(parent) => parent,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // Already gone: only the record is left.
            return clear_previous(paths).map_err(|_| RelocationError::Io);
        }
        Err(_) => return Err(RelocationError::PreviousUnavailable),
    };
    let root = match parent.open_directory(Path::new(name)) {
        Ok(root) => root,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return clear_previous(paths).map_err(|_| RelocationError::Io);
        }
        Err(_) => return Err(RelocationError::PreviousUnavailable),
    };
    if location::read_marker(&root).ok().flatten().as_deref() != Some(previous.root_id.as_str()) {
        return Err(RelocationError::PreviousUnavailable);
    }
    let lock = root
        .open_file(Path::new(".pd2launcher.lock"))
        .map_err(|_| RelocationError::PreviousUnavailable)?;
    std::fs::File::try_lock(&lock).map_err(|_| RelocationError::Busy)?;
    // The player's characters come out first; when they can't, nothing is
    // removed.
    if rescue(&root, &previous.path).is_err() {
        let _ = std::fs::File::unlock(&lock);
        return Err(RelocationError::SavesNotKept);
    }
    let removed = parent.remove_owned_tree(name);
    let _ = std::fs::File::unlock(&lock);
    removed.map_err(|_| RelocationError::Io)?;
    clear_previous(paths).map_err(|_| RelocationError::Io)
}

/// A new managed root, created and proven but not yet pointed at.
pub(crate) struct NewRoot {
    pub(crate) path: PathBuf,
    pub(crate) directory: Directory,
    pub(crate) root_id: String,
    parent: Directory,
    created: bool,
}

impl NewRoot {
    /// Undo [`create_root`] after a later failure: a root created here is
    /// removed while it still holds only the Launcher's skeleton.
    pub(crate) fn discard(self) {
        if self.created && holds_installation(&self.directory).is_ok_and(|held| !held) {
            let _ = self.parent.remove_owned_tree(OsStr::new(INSTALL_DIRECTORY));
        }
    }
}

/// Create `<parent>/pd2-launcher` and prove it can hold a managed root. An
/// existing directory is accepted only when it is empty, or when it is a
/// Launcher root (its own marker) that holds no installation yet, as a
/// choice interrupted before its pointer was written leaves it. Other
/// content is never adopted.
pub(crate) fn create_root(
    parent: &Path,
    minimum_free: u64,
    faults: &dyn ProbeFaults,
) -> Result<NewRoot, RelocationError> {
    if !parent.is_absolute() {
        return Err(RelocationError::ParentInvalid);
    }
    let parent_directory = Directory::open(parent).map_err(|_| RelocationError::ParentInvalid)?;
    let (directory, created, root_id) =
        match parent_directory.create_or_open_empty_directory(OsStr::new(INSTALL_DIRECTORY)) {
            Ok((directory, created)) => (directory, created, None),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let directory = parent_directory
                    .open_directory(Path::new(INSTALL_DIRECTORY))
                    .map_err(|_| RelocationError::TargetNotEmpty)?;
                match location::read_marker(&directory) {
                    Ok(Some(root_id)) if holds_installation(&directory).is_ok_and(|held| !held) => {
                        (directory, false, Some(root_id))
                    }
                    _ => return Err(RelocationError::TargetNotEmpty),
                }
            }
            Err(_) => return Err(RelocationError::ParentInvalid),
        };
    let path = parent.join(INSTALL_DIRECTORY);
    let root_id = match root_id {
        Some(root_id) => root_id,
        None => location::new_root_id().map_err(|_| RelocationError::Io)?,
    };
    let new_root = NewRoot {
        path,
        directory,
        root_id,
        parent: parent_directory,
        created,
    };
    if let Err(capability) = new_root
        .directory
        .probe_root_capabilities(minimum_free, faults)
    {
        new_root.discard();
        return Err(RelocationError::Capability(capability));
    }
    if location::write_marker(&new_root.directory, &new_root.root_id).is_err()
        || open_root_layout(&new_root.directory).is_err()
    {
        new_root.discard();
        return Err(RelocationError::Io);
    }
    Ok(new_root)
}

/// Whether a managed root holds anything besides the Launcher's skeleton:
/// a prefix, a runtime, or downloaded artifacts.
pub(crate) fn holds_installation(root: &Directory) -> io::Result<bool> {
    for name in ["prefix", "runner"] {
        match root.open_beneath(Path::new(name), rustix::fs::OFlags::PATH) {
            Ok(_) => return Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    match root.open_directory(Path::new("cache/artifacts")) {
        Ok(artifacts) => Ok(!artifacts.names()?.is_empty()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Point this Launcher (and the pointer file) at `new_root`. The caller
/// holds the root-selection lock exclusively.
pub(crate) fn adopt(paths: &LauncherPaths, new_root: &NewRoot) -> io::Result<()> {
    location::write_pointer(
        paths.config_root(),
        &Pointer::new(new_root.path.clone(), new_root.root_id.clone()),
    )?;
    paths.set_managed_root(Some(new_root.path.clone()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::MetadataExt};

    use super::*;
    use crate::safe_fs::NoProbeFaults;

    struct Fail(RootCapability);

    impl ProbeFaults for Fail {
        fn fail(&self, capability: RootCapability) -> bool {
            capability == self.0
        }
    }

    fn paths(fixture: &tempfile::TempDir) -> LauncherPaths {
        LauncherPaths::from_home(fixture.path().join(".pd2launcher"))
    }

    #[test]
    fn a_fresh_root_is_created_inside_the_picked_parent_and_adopted() {
        let fixture = tempfile::tempdir().unwrap();
        let paths = paths(&fixture);
        drop(paths.open_managed_layout().unwrap());
        let parent = fixture.path().join("Games");
        fs::create_dir(&parent).unwrap();

        let new_root = create_root(&parent, 0, &NoProbeFaults).expect("fresh root");
        assert_eq!(new_root.path, parent.join("pd2-launcher"));
        adopt(&paths, &new_root).unwrap();
        assert_eq!(paths.managed_root(), parent.join("pd2-launcher"));

        let reopened = LauncherPaths::from_home(fixture.path().join(".pd2launcher"));
        drop(
            reopened
                .open_managed_layout()
                .expect("resolves the chosen root"),
        );
        assert_eq!(reopened.managed_root(), parent.join("pd2-launcher"));
    }

    #[test]
    fn existing_content_is_never_adopted_and_a_failed_probe_leaves_nothing() {
        let fixture = tempfile::tempdir().unwrap();
        let parent = fixture.path().join("Games");
        fs::create_dir_all(parent.join("pd2-launcher/prefix")).unwrap();
        assert_eq!(
            create_root(&parent, 0, &NoProbeFaults).err(),
            Some(RelocationError::TargetNotEmpty)
        );
        assert!(parent.join("pd2-launcher/prefix").is_dir());

        // An empty folder the user made is fine.
        fs::remove_dir(parent.join("pd2-launcher/prefix")).unwrap();
        assert!(create_root(&parent, 0, &NoProbeFaults).is_ok());
        // A root this Launcher created but never pointed at is reused.
        assert!(create_root(&parent, 0, &NoProbeFaults).is_ok());

        let other = fixture.path().join("Card");
        fs::create_dir(&other).unwrap();
        for capability in [RootCapability::Writable, RootCapability::CaseSensitive] {
            assert_eq!(
                create_root(&other, 0, &Fail(capability)).err(),
                Some(RelocationError::Capability(capability))
            );
            assert!(!other.join("pd2-launcher").exists());
        }
        assert_eq!(
            create_root(Path::new("relative"), 0, &NoProbeFaults).err(),
            Some(RelocationError::ParentInvalid)
        );
        assert_eq!(
            create_root(&fixture.path().join("missing"), 0, &NoProbeFaults).err(),
            Some(RelocationError::ParentInvalid)
        );
        std::os::unix::fs::symlink(&other, fixture.path().join("link")).unwrap();
        assert_eq!(
            create_root(&fixture.path().join("link"), 0, &NoProbeFaults).err(),
            Some(RelocationError::ParentInvalid)
        );
    }

    struct CrashAfter(MoveStep);

    impl MoveFaults for CrashAfter {
        fn after(&self, step: MoveStep) -> io::Result<()> {
            if step == self.0 {
                Err(io::Error::new(io::ErrorKind::Interrupted, "injected crash"))
            } else {
                Ok(())
            }
        }
    }

    /// A default root holding an installation, and a parent on the same
    /// filesystem to move it to.
    fn installed_default_root(fixture: &tempfile::TempDir) -> (LauncherPaths, PathBuf) {
        let paths = paths(fixture);
        drop(paths.open_managed_layout().unwrap());
        let root = paths.managed_root();
        fs::create_dir_all(root.join("prefix/drive_c/Program Files (x86)/Diablo II")).unwrap();
        fs::write(
            root.join("prefix/drive_c/Program Files (x86)/Diablo II/Game.exe"),
            b"game",
        )
        .unwrap();
        fs::create_dir_all(root.join("runner/wine/bin")).unwrap();
        let parent = fixture.path().join("Games");
        fs::create_dir(&parent).unwrap();
        (paths, parent)
    }

    fn resolved(fixture: &tempfile::TempDir) -> PathBuf {
        let paths = paths(fixture);
        drop(paths.open_managed_layout().expect("the location resolves"));
        paths.managed_root()
    }

    fn assert_one_installation(fixture: &tempfile::TempDir, expected: &Path) {
        let root = resolved(fixture);
        assert_eq!(root, expected);
        assert_eq!(
            fs::read(root.join("prefix/drive_c/Program Files (x86)/Diablo II/Game.exe")).unwrap(),
            b"game"
        );
        assert!(
            !fixture
                .path()
                .join(".pd2launcher/config")
                .join(INTENT_FILE)
                .exists()
        );
        let other = if expected == fixture.path().join(".pd2launcher/game") {
            fixture.path().join("Games/pd2-launcher")
        } else {
            fixture.path().join(".pd2launcher/game")
        };
        assert!(
            !other.join("prefix").exists(),
            "exactly one root holds the game"
        );
    }

    #[test]
    fn a_same_filesystem_move_renames_the_root_and_repoints() {
        let fixture = tempfile::tempdir().unwrap();
        let (paths, parent) = installed_default_root(&fixture);
        let from = paths.managed_root();
        let inode = fs::metadata(from.join("prefix")).unwrap().ino();
        assert!(same_filesystem(&from, &parent).unwrap());
        let to = move_same_filesystem(&paths, &from, &parent, 0, &NoProbeFaults, &NoMoveFaults)
            .expect("move");
        assert_eq!(to, parent.join("pd2-launcher"));
        assert_eq!(paths.managed_root(), to);
        assert_eq!(
            fs::metadata(to.join("prefix")).unwrap().ino(),
            inode,
            "renamed, not copied"
        );
        assert!(!from.exists());
        assert_one_installation(&fixture, &to);
    }

    #[test]
    fn every_crash_cut_of_a_move_recovers_to_exactly_one_resolved_root() {
        for (cut, moved) in [
            (MoveStep::IntentWritten, false),
            (MoveStep::Exchanged, true),
            (MoveStep::PointerWritten, true),
            (MoveStep::SourceCleared, true),
        ] {
            let fixture = tempfile::tempdir().unwrap();
            let (paths, parent) = installed_default_root(&fixture);
            let from = paths.managed_root();
            assert_eq!(
                move_same_filesystem(&paths, &from, &parent, 0, &NoProbeFaults, &CrashAfter(cut))
                    .err(),
                Some(RelocationError::Io),
                "{cut:?}"
            );
            // A new process recovers at startup.
            let expected = if moved {
                parent.join("pd2-launcher")
            } else {
                from.clone()
            };
            assert_one_installation(&fixture, &expected);
            if !moved {
                assert!(
                    !parent.join("pd2-launcher").exists(),
                    "{cut:?}: probe folder removed"
                );
            }
        }
    }

    #[test]
    fn a_move_refuses_a_non_empty_destination_and_keeps_the_root() {
        let fixture = tempfile::tempdir().unwrap();
        let (paths, parent) = installed_default_root(&fixture);
        fs::create_dir_all(parent.join("pd2-launcher/Save")).unwrap();
        let from = paths.managed_root();
        assert_eq!(
            move_same_filesystem(&paths, &from, &parent, 0, &NoProbeFaults, &NoMoveFaults).err(),
            Some(RelocationError::TargetNotEmpty)
        );
        assert_one_installation(&fixture, &from);
        assert!(parent.join("pd2-launcher/Save").is_dir());
    }

    #[test]
    fn a_move_to_another_filesystem_starts_fresh_and_keeps_the_old_root_until_removed() {
        let fixture = tempfile::tempdir().unwrap();
        let (paths, parent) = installed_default_root(&fixture);
        let from = paths.managed_root();
        let seeded = std::cell::Cell::new(false);
        let to = move_to_new_filesystem(&paths, &from, &parent, 0, &NoProbeFaults, |old, new| {
            assert!(old.open_directory(Path::new("prefix")).is_ok());
            assert!(new.open_file(Path::new(location::ROOT_MARKER)).is_ok());
            seeded.set(true);
            Ok(())
        })
        .expect("fresh root elsewhere");
        assert!(seeded.get());
        assert_eq!(to, parent.join("pd2-launcher"));
        assert_eq!(paths.managed_root(), to);
        assert!(
            !to.join("prefix").exists(),
            "nothing but downloads is carried over"
        );
        // The old installation is untouched and recorded.
        assert!(
            from.join("prefix/drive_c/Program Files (x86)/Diablo II/Game.exe")
                .is_file()
        );
        assert_eq!(previous_root(&paths), Some(from.clone()));
        assert_eq!(resolved(&fixture), to);
        // Another move waits until the old one is removed.
        let elsewhere = fixture.path().join("Elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        assert_eq!(
            move_to_new_filesystem(&paths, &to, &elsewhere, 0, &NoProbeFaults, |_, _| Ok(())).err(),
            Some(RelocationError::PreviousPending)
        );

        // In use by another Launcher: refused.
        let lock = fs::File::open(from.join(".pd2launcher.lock")).unwrap();
        fs::File::try_lock(&lock).unwrap();
        assert_eq!(
            remove_previous(&paths, |_, _| Ok(())).err(),
            Some(RelocationError::Busy)
        );
        fs::File::unlock(&lock).unwrap();
        // The characters come out first; when they can't, nothing goes.
        assert_eq!(
            remove_previous(&paths, |_, _| Err(io::Error::other("no room"))).err(),
            Some(RelocationError::SavesNotKept)
        );
        assert!(from.join("prefix").is_dir());
        let rescued = std::cell::Cell::new(false);
        remove_previous(&paths, |old, old_path| {
            assert!(
                old.open_directory(Path::new("prefix")).is_ok(),
                "the old root"
            );
            assert_eq!(old_path, from, "and its path");
            rescued.set(true);
            Ok(())
        })
        .expect("confirmed removal");
        assert!(rescued.get());
        assert!(!from.exists());
        assert_eq!(previous_root(&paths), None);
        assert_eq!(resolved(&fixture), to);
    }

    #[test]
    fn a_previous_root_is_removed_only_while_it_still_carries_its_marker() {
        let fixture = tempfile::tempdir().unwrap();
        let (paths, parent) = installed_default_root(&fixture);
        let from = paths.managed_root();
        move_to_new_filesystem(&paths, &from, &parent, 0, &NoProbeFaults, |_, _| Ok(())).unwrap();
        // Something else now lives there.
        location::write_marker(
            &Directory::open(&from).unwrap(),
            &location::new_root_id().unwrap(),
        )
        .unwrap();
        assert_eq!(
            remove_previous(&paths, |_, _| Ok(())).err(),
            Some(RelocationError::PreviousUnavailable)
        );
        assert!(from.join("prefix").is_dir());

        // A failed seed leaves no new root and no record behind.
        let fixture = tempfile::tempdir().unwrap();
        let (paths, parent) = installed_default_root(&fixture);
        let from = paths.managed_root();
        assert_eq!(
            move_to_new_filesystem(&paths, &from, &parent, 0, &NoProbeFaults, |_, _| {
                Err(io::Error::other("seed failed"))
            })
            .err(),
            Some(RelocationError::Io)
        );
        assert!(!parent.join("pd2-launcher").exists());
        assert_eq!(previous_root(&paths), None);
        assert_eq!(resolved(&fixture), from);
    }

    #[test]
    fn a_location_inside_the_current_root_is_refused_without_leftovers() {
        let fixture = tempfile::tempdir().unwrap();
        let (paths, _) = installed_default_root(&fixture);
        let from = paths.managed_root();
        let inside = from.join("prefix");
        assert_eq!(
            move_same_filesystem(&paths, &from, &inside, 0, &NoProbeFaults, &NoMoveFaults).err(),
            Some(RelocationError::ParentInvalid)
        );
        assert_eq!(
            move_to_new_filesystem(&paths, &from, &inside, 0, &NoProbeFaults, |_, _| Ok(())).err(),
            Some(RelocationError::ParentInvalid)
        );
        assert!(!inside.join("pd2-launcher").exists());
        assert!(
            !fixture
                .path()
                .join(".pd2launcher/config")
                .join(INTENT_FILE)
                .exists()
        );
        assert_one_installation(&fixture, &from);
    }

    #[test]
    fn only_a_prefix_runtime_or_download_counts_as_an_installation() {
        let fixture = tempfile::tempdir().unwrap();
        let root = Directory::open(fixture.path()).unwrap();
        open_root_layout(&root).unwrap();
        assert!(!holds_installation(&root).unwrap());
        fs::create_dir_all(fixture.path().join("cache/artifacts")).unwrap();
        assert!(!holds_installation(&root).unwrap());
        fs::write(
            fixture.path().join("cache/artifacts/artifact-00.partial"),
            b"x",
        )
        .unwrap();
        assert!(holds_installation(&root).unwrap());
        fs::remove_file(fixture.path().join("cache/artifacts/artifact-00.partial")).unwrap();
        fs::create_dir(fixture.path().join("runner")).unwrap();
        assert!(holds_installation(&root).unwrap());
    }
}
