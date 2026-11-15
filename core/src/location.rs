//! Where the Managed installation lives.
//!
//! The default root, `~/.pd2launcher/game`, needs no
//! configuration and is Launcher-owned. A root the user chose is named by a
//! separate pointer file in the config root and proven by the ownership
//! marker inside it. Resolution fails closed: a pointer that cannot be read,
//! or a root that is missing or not carrying the pointer's marker, is
//! "unavailable", never a reason to fall back to the default or to create
//! the root again (an unmounted card must not look like an empty place to
//! install).

use std::{
    ffi::OsStr,
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::safe_fs::Directory;

/// Ownership marker inside every managed root.
pub(crate) const ROOT_MARKER: &str = ".pd2launcher-root.json";
/// Pointer to a chosen root, in the Launcher's config directory.
pub(crate) const POINTER_FILE: &str = "managed-root.json";
/// Root-selection lock, in the Launcher's state directory.
pub(crate) const SELECTION_LOCK: &str = "managed-root.lock";
/// The directory the Launcher creates inside the parent the user picked.
pub(crate) const INSTALL_DIRECTORY: &str = "pd2-launcher";
const SCHEMA: u32 = 1;
const RECORD_LIMIT: u64 = 4096;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct MarkerRecord {
    schema: u32,
    root_id: String,
}

/// The persisted choice of a non-default managed root.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Pointer {
    schema: u32,
    path: PathBuf,
    root_id: String,
}

impl Pointer {
    pub(crate) fn new(path: PathBuf, root_id: String) -> Self {
        Self {
            schema: SCHEMA,
            path,
            root_id,
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn root_id(&self) -> &str {
        &self.root_id
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PointerState {
    /// No pointer: the default root is used.
    Absent,
    Present(Pointer),
    /// A pointer exists but is not one the Launcher wrote.
    Invalid,
}

pub(crate) fn read_pointer(config_root: &Path) -> PointerState {
    match crate::persistence::read_bounded_regular(&config_root.join(POINTER_FILE), RECORD_LIMIT) {
        Ok(bytes) => match serde_json::from_slice::<Pointer>(&bytes) {
            Ok(pointer)
                if pointer.schema == SCHEMA
                    && pointer.path.is_absolute()
                    && valid_root_id(&pointer.root_id) =>
            {
                PointerState::Present(pointer)
            }
            _ => PointerState::Invalid,
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => PointerState::Absent,
        Err(_) => PointerState::Invalid,
    }
}

pub(crate) fn write_pointer(config_root: &Path, pointer: &Pointer) -> io::Result<()> {
    let bytes = serde_json::to_vec(pointer)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "pointer encode failed"))?;
    crate::persistence::atomic_write(&config_root.join(POINTER_FILE), &bytes)
}

/// The root id in `root`'s marker: `Ok(None)` when there is none, an error
/// when a marker exists but is not one the Launcher wrote.
pub(crate) fn read_marker(root: &Directory) -> io::Result<Option<String>> {
    let file = match root.open_file(Path::new(ROOT_MARKER)) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take(RECORD_LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > RECORD_LIMIT {
        return Err(invalid("root marker exceeded its bound"));
    }
    match serde_json::from_slice::<MarkerRecord>(&bytes) {
        Ok(record) if record.schema == SCHEMA && valid_root_id(&record.root_id) => {
            Ok(Some(record.root_id))
        }
        _ => Err(invalid("root marker was invalid")),
    }
}

pub(crate) fn write_marker(root: &Directory, root_id: &str) -> io::Result<()> {
    let bytes = serde_json::to_vec(&MarkerRecord {
        schema: SCHEMA,
        root_id: root_id.to_owned(),
    })
    .map_err(|_| invalid("root marker encode failed"))?;
    root.atomic_write(OsStr::new(ROOT_MARKER), &bytes)
}

/// A fresh random root id (128 bits, hex).
pub(crate) fn new_root_id() -> io::Result<String> {
    let mut bytes = [0_u8; 16];
    let mut filled = 0;
    while filled < bytes.len() {
        filled +=
            rustix::rand::getrandom(&mut bytes[filled..], rustix::rand::GetRandomFlags::empty())
                .map_err(io::Error::from)?;
    }
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn valid_root_id(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Open the chosen root named by `pointer` only if it carries the pointer's
/// marker. Nothing is created.
pub(crate) fn open_chosen_root(pointer: &Pointer) -> io::Result<Directory> {
    let root = Directory::open(pointer.path())?;
    match read_marker(&root)? {
        Some(root_id) if root_id == pointer.root_id() => Ok(root),
        _ => Err(invalid("managed root marker did not match the pointer")),
    }
}

/// The root-selection lock: shared while a Launcher has a root open,
/// exclusive while a location is chosen or moved.
pub(crate) struct SelectionLock(File);

impl SelectionLock {
    pub(crate) fn acquire_shared(state_root: &Path) -> io::Result<Self> {
        let lock = Self::open(state_root)?;
        File::try_lock_shared(&lock.0).map_err(|_| busy())?;
        Ok(lock)
    }

    fn open(state_root: &Path) -> io::Result<Self> {
        let file = Directory::ensure_absolute(state_root)?
            .open_or_create_file(OsStr::new(SELECTION_LOCK))?;
        Ok(Self(file))
    }

    /// Convert this Launcher's shared hold into an exclusive one, failing
    /// while any other Launcher has a root open.
    pub(crate) fn upgrade(&self) -> io::Result<ExclusiveSelection<'_>> {
        File::try_lock(&self.0).map_err(|_| busy())?;
        Ok(ExclusiveSelection(self))
    }
}

impl Drop for SelectionLock {
    fn drop(&mut self) {
        let _ = File::unlock(&self.0);
    }
}

/// An exclusive hold that returns to shared when dropped.
pub(crate) struct ExclusiveSelection<'a>(&'a SelectionLock);

impl Drop for ExclusiveSelection<'_> {
    fn drop(&mut self) {
        let _ = File::try_lock_shared(&self.0.0);
    }
}

fn busy() -> io::Error {
    io::Error::new(io::ErrorKind::WouldBlock, "root selection is busy")
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn pointers_and_markers_round_trip_and_anything_else_fails_closed() {
        let fixture = tempfile::tempdir().unwrap();
        let config = fixture.path().join("config");
        fs::create_dir(&config).unwrap();
        assert_eq!(read_pointer(&config), PointerState::Absent);

        let root_path = fixture.path().join("games/pd2-launcher");
        fs::create_dir_all(&root_path).unwrap();
        let root = Directory::open(&root_path).unwrap();
        assert_eq!(read_marker(&root).unwrap(), None);
        let id = new_root_id().unwrap();
        assert_ne!(id, new_root_id().unwrap());
        write_marker(&root, &id).unwrap();
        assert_eq!(read_marker(&root).unwrap(), Some(id.clone()));

        let pointer = Pointer::new(root_path.clone(), id.clone());
        write_pointer(&config, &pointer).unwrap();
        assert_eq!(
            read_pointer(&config),
            PointerState::Present(pointer.clone())
        );
        assert!(open_chosen_root(&pointer).is_ok());

        // Another root id, a missing root, and an empty mount point all fail.
        let other = Pointer::new(root_path.clone(), new_root_id().unwrap());
        assert!(open_chosen_root(&other).is_err());
        let missing = Pointer::new(fixture.path().join("unplugged/pd2-launcher"), id.clone());
        assert!(open_chosen_root(&missing).is_err());
        assert!(!fixture.path().join("unplugged").exists());
        let empty = fixture.path().join("mnt/pd2-launcher");
        fs::create_dir_all(&empty).unwrap();
        assert!(open_chosen_root(&Pointer::new(empty.clone(), id.clone())).is_err());
        assert_eq!(fs::read_dir(&empty).unwrap().count(), 0);

        for bytes in [
            b"not json".to_vec(),
            br#"{"schema":1,"path":"relative/pd2-launcher","root_id":"00000000000000000000000000000000"}"#.to_vec(),
            br#"{"schema":2,"path":"/games/pd2-launcher","root_id":"00000000000000000000000000000000"}"#.to_vec(),
            br#"{"schema":1,"path":"/games/pd2-launcher","root_id":"short"}"#.to_vec(),
            br#"{"schema":1,"path":"/games/pd2-launcher","root_id":"00000000000000000000000000000000","x":1}"#.to_vec(),
        ] {
            fs::write(config.join(POINTER_FILE), bytes).unwrap();
            assert_eq!(read_pointer(&config), PointerState::Invalid);
        }
        fs::write(root_path.join(ROOT_MARKER), b"{}").unwrap();
        assert!(read_marker(&root).is_err());
    }

    #[test]
    fn the_selection_lock_is_shared_while_open_and_exclusive_to_change() {
        let fixture = tempfile::tempdir().unwrap();
        let first = SelectionLock::acquire_shared(fixture.path()).unwrap();
        let second = SelectionLock::acquire_shared(fixture.path()).unwrap();
        assert!(first.upgrade().is_err(), "another Launcher has a root open");
        drop(second);
        let exclusive = first.upgrade().unwrap();
        assert!(SelectionLock::acquire_shared(fixture.path()).is_err());
        drop(exclusive);
        assert!(SelectionLock::acquire_shared(fixture.path()).is_ok());
    }
}
