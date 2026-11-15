//! The Android host's drive for a game folder used in place.
//!
//! Elsewhere a folder used in place is reached through Wine's `Z:` drive,
//! the host root. On Android that root is the read-only system partition,
//! which reports no free space, and before a new character is saved the game
//! (D2Launch) asks `GetDiskFreeSpaceA` about the root of its save path, Fog's
//! `<InstallPath>\Save\`, and refuses the character under 5000 bytes free
//! ("not enough room on your hard disk"). So on the Android host the folder
//! is reached through the `G:` drive instead: a link in the prefix's
//! `dosdevices` to the folder's parent, the app's copy below `files/import` on
//! the data partition. Its Windows path is `G:\<name>`: the game starts
//! from there and the PD2 installer gets it as `/DIR=` (its `InstallPath`).
//! The game writes a missing `InstallPath` as the folder it starts from.
//! Every free-space question about `G:` is answered by that partition. The desktop keeps `Z:`, a root with free space.

use std::{
    io,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
};

use rustix::{fs::AtFlags, io::Errno};

use crate::{
    game_folder::{GameFolder, windows_path_for},
    safe_fs::Directory,
};

/// The drive's name in `dosdevices`.
const DRIVE: &str = "g:";

/// A folder used in place as Windows programs see it: on the Android host
/// (`android`) `G:\<name>` when it has a parent other than the host root,
/// else `Z:\…` ([`windows_path_for`]); none for a path Windows programs
/// cannot use.
pub(crate) fn windows_path(folder: &Path, android: bool) -> Option<String> {
    let host_root = windows_path_for(folder)?;
    match (android, drive_target(folder)) {
        (true, Some(_)) => Some(format!(r"G:\{}", folder.file_name()?.to_str()?)),
        _ => Some(host_root),
    }
}

/// The folder used in place that needs the drive: an external one on the
/// Android host; none for the Managed folder or the desktop.
pub(crate) fn folder(android: bool, game_folder: &GameFolder) -> Option<PathBuf> {
    match game_folder {
        GameFolder::External(external) if android && drive_target(external.path()).is_some() => {
            Some(external.path().to_owned())
        }
        _ => None,
    }
}

/// Make the prefix's `G:` drive a link to `folder`'s parent. True when the
/// link was made or replaced, false when it was already right. A link
/// elsewhere is replaced; anything else in its place is left alone and
/// refused.
pub(crate) fn link(root: &Directory, folder: &Path) -> io::Result<bool> {
    let target = drive_target(folder)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no parent for the drive"))?;
    let devices = root
        .open_directory(Path::new("prefix/dosdevices"))?
        .try_clone_file()?;
    match rustix::fs::readlinkat(&devices, DRIVE, Vec::new()) {
        Ok(current) if current.as_bytes() == target.as_os_str().as_bytes() => return Ok(false),
        Ok(_) => rustix::fs::unlinkat(&devices, DRIVE, AtFlags::empty())?,
        Err(Errno::NOENT) => {}
        Err(error) => return Err(error.into()),
    }
    rustix::fs::symlinkat(target, &devices, DRIVE)?;
    Ok(true)
}

/// The drive's root: the folder's parent, never the host root itself.
fn drive_target(folder: &Path) -> Option<&Path> {
    folder
        .parent()
        .filter(|parent| parent.is_absolute() && *parent != Path::new("/"))
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::symlink};

    use tempfile::TempDir;

    use super::*;

    const COPY: &str = "/data/user/0/com.pd2launcher.app/files/import/a1/Diablo II";

    fn prefix() -> (TempDir, Directory) {
        let fixture = TempDir::new().unwrap();
        fs::create_dir_all(fixture.path().join("prefix/dosdevices")).unwrap();
        symlink("/", fixture.path().join("prefix/dosdevices/z:")).unwrap();
        let root = Directory::open(fixture.path()).unwrap();
        (fixture, root)
    }

    #[test]
    fn on_android_a_folder_used_in_place_is_on_the_g_drive() {
        assert_eq!(
            windows_path(Path::new(COPY), true).as_deref(),
            Some(r"G:\Diablo II")
        );
        // The desktop keeps the host root's drive.
        assert_eq!(
            windows_path(Path::new("/home/deck/Games/Diablo II"), false).as_deref(),
            Some(r"Z:\home\deck\Games\Diablo II")
        );
        // A folder right below the host root has no other drive to be on.
        assert_eq!(
            windows_path(Path::new("/Diablo II"), true).as_deref(),
            Some(r"Z:\Diablo II")
        );
        // Names Windows programs cannot use are refused either way.
        assert_eq!(windows_path(Path::new("/games/a:b"), true), None);
    }

    #[test]
    fn only_a_folder_used_in_place_on_android_needs_the_drive() {
        let folder_used = TempDir::new().unwrap();
        let external = GameFolder::External(crate::game_folder::ExternalGameFolder::at(
            folder_used.path(),
        ));
        assert_eq!(folder(true, &external).as_deref(), Some(folder_used.path()));
        assert_eq!(folder(false, &external), None);
        assert_eq!(folder(true, &GameFolder::Managed), None);
    }

    #[test]
    fn the_drive_links_to_the_folders_parent_and_replaces_a_link_elsewhere() {
        let (fixture, root) = prefix();
        let drive = fixture.path().join("prefix/dosdevices/g:");

        assert!(link(&root, Path::new(COPY)).unwrap());
        assert_eq!(
            fs::read_link(&drive).unwrap(),
            Path::new("/data/user/0/com.pd2launcher.app/files/import/a1")
        );
        // Again: nothing changes.
        assert!(!link(&root, Path::new(COPY)).unwrap());

        let other = "/data/user/0/com.pd2launcher.app/files/import/b2/Diablo II";
        assert!(link(&root, Path::new(other)).unwrap());
        assert_eq!(
            fs::read_link(&drive).unwrap(),
            Path::new("/data/user/0/com.pd2launcher.app/files/import/b2")
        );
        // The host root's drive is untouched.
        assert_eq!(
            fs::read_link(fixture.path().join("prefix/dosdevices/z:")).unwrap(),
            Path::new("/")
        );
    }

    #[test]
    fn something_other_than_a_link_in_the_drives_place_is_left_alone() {
        let (fixture, root) = prefix();
        let drive = fixture.path().join("prefix/dosdevices/g:");
        fs::create_dir(&drive).unwrap();

        assert!(link(&root, Path::new("/games/Diablo II")).is_err());
        assert!(drive.is_dir());
    }

    #[test]
    fn the_host_root_is_never_the_drive() {
        let (fixture, root) = prefix();
        assert!(link(&root, Path::new("/Diablo II")).is_err());
        assert!(!fixture.path().join("prefix/dosdevices/g:").exists());
    }
}
