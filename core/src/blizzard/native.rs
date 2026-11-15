//! The native install: one product's install script run into the game
//! folder, without Blizzard's installer or Wine. Tome members are streamed
//! into the game folder; the members, the key and the owner name go into
//! the game archive in one addition ([`super::mpq::Archive::add`]).
//!
//! A plan must have the shape its product's script has (the Tome is pinned
//! by its SHA-256, so a different plan means a different reader), and every
//! archive and member it names must be there; anything else fails closed.

use std::{
    io::{self, Write},
    path::Path,
};

use rustix::fs::OFlags;
use zeroize::Zeroizing;

use super::{
    keystore,
    mpq::{Archive, MpqError, NewMember},
    script::Operation,
};
use crate::safe_fs::Directory;

/// Where the install script is in each Tome.
pub(crate) const SCRIPT_MEMBER: &str = r"InstallCD\InstallerFileList\InstallerFileList.xml";

/// The shape of one product's plan.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Expected {
    pub(crate) extracts: usize,
    pub(crate) adds: usize,
    pub(crate) deletes: usize,
    /// The archive that takes the members, the key and the owner name.
    pub(crate) container: &'static str,
    pub(crate) product: u32,
    pub(crate) key_member: &'static str,
}

/// Diablo II: 13 files, 25 members into `d2data.mpq`.
pub(crate) const BASE_GAME: Expected = Expected {
    extracts: 13,
    adds: 25,
    deletes: 0,
    container: "d2data.mpq",
    product: 24,
    key_member: keystore::BASE_KEY_MEMBER,
};

/// Lord of Destruction: 46 old files deleted, 11 files (its own programs
/// in place of Diablo II's), 21 members into `d2char.mpq`.
pub(crate) const EXPANSION: Expected = Expected {
    extracts: 11,
    adds: 21,
    deletes: 46,
    container: "d2char.mpq",
    product: 25,
    key_member: keystore::EXPANSION_KEY_MEMBER,
};

#[derive(Debug)]
pub(crate) enum NativeError {
    /// The plan does not have its product's shape.
    Plan(&'static str),
    Mpq(MpqError),
    /// Writing the game folder failed.
    Store(io::ErrorKind),
    Cancelled,
}

impl std::fmt::Display for NativeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Plan(what) => write!(f, "the install is not as expected: {what}"),
            Self::Mpq(error) => write!(f, "an archive: {error:?}"),
            Self::Store(kind) => write!(f, "the game folder: {kind}"),
            Self::Cancelled => f.write_str("stopped"),
        }
    }
}

impl From<MpqError> for NativeError {
    fn from(error: MpqError) -> Self {
        Self::Mpq(error)
    }
}

impl From<io::Error> for NativeError {
    fn from(error: io::Error) -> Self {
        Self::Store(error.kind())
    }
}

/// Check that `plan` has `expected`'s shape.
pub(crate) fn check_plan(plan: &[Operation], expected: &Expected) -> Result<(), NativeError> {
    let count =
        |test: fn(&Operation) -> bool| plan.iter().filter(|operation| test(operation)).count();
    if count(|operation| matches!(operation, Operation::Extract { .. })) != expected.extracts
        || count(|operation| matches!(operation, Operation::Add { .. })) != expected.adds
        || count(|operation| matches!(operation, Operation::Delete { .. })) != expected.deletes
    {
        return Err(NativeError::Plan("operation counts"));
    }
    let mut keys = 0;
    let mut owners = 0;
    for operation in plan {
        match operation {
            Operation::Add { container, .. } if container != expected.container => {
                return Err(NativeError::Plan("another archive"));
            }
            Operation::Key {
                container,
                into,
                product,
            } => {
                if container != expected.container
                    || into != expected.key_member
                    || *product != expected.product
                {
                    return Err(NativeError::Plan("the key member"));
                }
                keys += 1;
            }
            Operation::Owner { container, into } => {
                if container != expected.container || into != keystore::OWNER_MEMBER {
                    return Err(NativeError::Plan("the owner member"));
                }
                owners += 1;
            }
            _ => {}
        }
    }
    if keys != 1 || owners != 1 {
        return Err(NativeError::Plan("one key and one owner"));
    }
    Ok(())
}

/// Run `plan` into `game` with `key` (normalized, already checked):
/// deletes and files in order, then the archive's members, key and owner
/// name in one addition. `progress` gets the bytes written of the total.
///
/// What can fail without a change comes first: the sizes, the archive's
/// members read, the key and the owner wrapped, and, when the archive is in
/// the game folder already, its addition planned. So a refused or full
/// archive changes nothing.
pub(crate) fn run(
    tome: &Archive,
    plan: &[Operation],
    game: &Directory,
    key: &[u8],
    cancelled: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<(), NativeError> {
    let mut total = 0;
    for operation in plan {
        match operation {
            Operation::Extract { from, size, .. } => {
                let length = tome.size(from)?;
                if size.is_some_and(|size| size != length) {
                    return Err(NativeError::Plan("a file of another size"));
                }
                total += length;
            }
            Operation::Add { from, .. } => total += tome.size(from)?,
            Operation::Delete { .. } | Operation::Key { .. } | Operation::Owner { .. } => {}
        }
    }
    // Wiped when dropped, on every path: the wrapped key is key material.
    let (container, members) = archive_members(tome, plan, key)?;
    let new: Vec<NewMember<'_>> = members
        .iter()
        .map(|(name, data)| NewMember { name, data })
        .collect();
    let made_by_plan = plan
        .iter()
        .any(|operation| matches!(operation, Operation::Extract { to, .. } if to == container));
    if !made_by_plan {
        open_archive(game, container)?.check_add(&new)?;
    }
    let mut done = 0;
    for operation in plan {
        if cancelled() {
            return Err(NativeError::Cancelled);
        }
        match operation {
            Operation::Delete { name } => remove_if_present(game, name)?,
            Operation::Extract { from, to, .. } => {
                remove_if_present(game, to)?;
                let file = game.create_file(std::ffi::OsStr::new(to))?;
                let mut writer = Checked {
                    file: &file,
                    cancelled,
                };
                let base = done;
                let length = tome
                    .copy_member(from, &mut writer, |copied| progress(base + copied, total))
                    .map_err(|error| {
                        if cancelled() {
                            NativeError::Cancelled
                        } else {
                            NativeError::Mpq(error)
                        }
                    })?;
                file.sync_all()?;
                done += length;
            }
            Operation::Add { .. } | Operation::Key { .. } | Operation::Owner { .. } => {}
        }
    }
    if cancelled() {
        return Err(NativeError::Cancelled);
    }
    open_archive(game, container)?.add(&new)?;
    progress(total, total);
    Ok(())
}

/// A member for the archive, wiped when dropped.
type ArchiveMember = (String, Zeroizing<Vec<u8>>);

/// The archive the plan adds to, and its members: the Tome's, the wrapped
/// key and the wrapped owner name.
fn archive_members<'p>(
    tome: &Archive,
    plan: &'p [Operation],
    key: &[u8],
) -> Result<(&'p str, Vec<ArchiveMember>), NativeError> {
    let mut members = Vec::new();
    let mut container = None;
    for operation in plan {
        match operation {
            Operation::Add {
                container: into,
                from,
                to,
                size,
            } => {
                let data = Zeroizing::new(tome.member(from)?);
                if size.is_some_and(|size| size != data.len() as u64) {
                    return Err(NativeError::Plan("a member of another size"));
                }
                container = Some(into);
                members.push((to.clone(), data));
            }
            Operation::Key {
                container: into,
                into: name,
                ..
            } => {
                container = Some(into);
                members.push((name.clone(), Zeroizing::new(keystore::wrap(key))));
            }
            Operation::Owner {
                container: into,
                into: name,
            } => {
                container = Some(into);
                members.push((
                    name.clone(),
                    Zeroizing::new(keystore::wrap(keystore::OWNER.as_bytes())),
                ));
            }
            Operation::Delete { .. } | Operation::Extract { .. } => {}
        }
    }
    let container = container.ok_or(NativeError::Plan("no archive"))?;
    Ok((container, members))
}

/// The game archive `name`, open to add to; it must be a regular file
/// beneath the game folder.
fn open_archive(game: &Directory, name: &str) -> Result<Archive, NativeError> {
    game.open_file(Path::new(name))?;
    let file = game.open_beneath(Path::new(name), OFlags::RDWR)?;
    Ok(Archive::open(file)?)
}

fn remove_if_present(game: &Directory, name: &str) -> Result<(), NativeError> {
    match game.open_file(Path::new(name)) {
        Ok(file) => Ok(game.remove_open_file_bounded(std::ffi::OsStr::new(name), &file)?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// A file writer that stops when the user stops.
struct Checked<'a> {
    file: &'a std::fs::File,
    cancelled: &'a dyn Fn() -> bool,
}

impl Write for Checked<'_> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if (self.cancelled)() {
            // Not `Interrupted`: `write_all` would try that again forever.
            return Err(io::Error::other("stopped"));
        }
        let mut file = self.file;
        file.write(data)
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut file = self.file;
        file.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blizzard::mpq::tests::build;

    fn expected() -> Expected {
        Expected {
            extracts: 2,
            adds: 1,
            deletes: 1,
            container: "data.mpq",
            product: 24,
            key_member: keystore::BASE_KEY_MEMBER,
        }
    }

    fn sample_plan() -> Vec<Operation> {
        vec![
            Operation::Delete {
                name: "old.dll".to_owned(),
            },
            Operation::Extract {
                from: r"Common\data.mpq".to_owned(),
                to: "data.mpq".to_owned(),
                size: None,
            },
            Operation::Extract {
                from: r"PC\Game.exe".to_owned(),
                to: "Game.exe".to_owned(),
                size: Some(500),
            },
            Operation::Add {
                container: "data.mpq".to_owned(),
                from: r"PC-100\a.dll".to_owned(),
                to: "a.dll".to_owned(),
                size: Some(300),
            },
            Operation::Key {
                container: "data.mpq".to_owned(),
                into: keystore::BASE_KEY_MEMBER.to_owned(),
                product: 24,
            },
            Operation::Owner {
                container: "data.mpq".to_owned(),
                into: keystore::OWNER_MEMBER.to_owned(),
            },
        ]
    }

    #[test]
    fn a_plan_must_have_its_products_shape() {
        assert!(check_plan(&sample_plan(), &expected()).is_ok());
        let mut other = sample_plan();
        other.pop();
        assert!(check_plan(&other, &expected()).is_err());
        let mut other = sample_plan();
        other[4] = Operation::Key {
            container: "data.mpq".to_owned(),
            into: keystore::BASE_KEY_MEMBER.to_owned(),
            product: 25,
        };
        assert!(check_plan(&other, &expected()).is_err());
        assert!(
            check_plan(
                &sample_plan(),
                &Expected {
                    extracts: 3,
                    ..expected()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn a_plan_runs_into_the_game_folder() {
        let game_archive = build(&[("x.txt", b"kept", false)], 8, 3);
        let program: Vec<u8> = (0..500).map(|index| index as u8).collect();
        let member: Vec<u8> = (0..300).map(|index| (index * 3) as u8).collect();
        let tome_bytes = build(
            &[
                (r"Common\data.mpq", &game_archive, false),
                (r"PC\Game.exe", &program, true),
                (r"PC-100\a.dll", &member, true),
            ],
            8,
            3,
        );
        let mut tome_file = tempfile::NamedTempFile::new().unwrap();
        tome_file.write_all(&tome_bytes).unwrap();
        let tome = Archive::open(tome_file.reopen().unwrap()).unwrap();
        let folder = tempfile::tempdir().unwrap();
        std::fs::write(folder.path().join("old.dll"), b"old").unwrap();
        let game = Directory::open(folder.path()).unwrap();
        let mut seen = Vec::new();
        run(
            &tome,
            &sample_plan(),
            &game,
            b"FAKE-NOT-A-KEY",
            &|| false,
            &mut |done, total| seen.push((done, total)),
        )
        .unwrap();
        assert!(!folder.path().join("old.dll").exists());
        assert_eq!(
            std::fs::read(folder.path().join("Game.exe")).unwrap(),
            program
        );
        let archive =
            Archive::open(std::fs::File::open(folder.path().join("data.mpq")).unwrap()).unwrap();
        assert_eq!(archive.member("x.txt").unwrap(), b"kept");
        assert_eq!(archive.member("a.dll").unwrap(), member);
        assert_eq!(
            archive.member(keystore::BASE_KEY_MEMBER).unwrap(),
            keystore::wrap(b"FAKE-NOT-A-KEY")
        );
        assert_eq!(
            archive.member(keystore::OWNER_MEMBER).unwrap(),
            keystore::wrap(b"Player")
        );
        let total = game_archive.len() as u64 + 500 + 300;
        assert_eq!(seen.last(), Some(&(total, total)));
    }

    /// The plans of the pinned Tomes have their products' shapes:
    /// `PD2_DIRECT_DOWNLOAD_PAYLOADS=<cache/artifacts> cargo test --lib native -- --ignored`.
    #[test]
    #[ignore = "needs the pinned payloads"]
    fn the_pinned_scripts_have_the_expected_plans() {
        let payloads = std::path::PathBuf::from(
            std::env::var_os("PD2_DIRECT_DOWNLOAD_PAYLOADS").expect("a payload folder"),
        );
        for (folder, expected) in [("d2-payload", BASE_GAME), ("lod-payload", EXPANSION)] {
            let tome = Archive::open(
                std::fs::File::open(payloads.join(folder).join("Installer Tome.mpq")).unwrap(),
            )
            .unwrap();
            let script = String::from_utf8(tome.member(SCRIPT_MEMBER).unwrap()).unwrap();
            let plan = crate::blizzard::script::plan(&script).unwrap();
            for operation in &plan {
                if let Operation::Extract { from, .. } | Operation::Add { from, .. } = operation {
                    assert!(tome.size(from).is_ok(), "{from}");
                }
            }
            check_plan(&plan, &expected).unwrap();
        }
    }

    #[test]
    fn a_missing_archive_or_member_and_a_stop_fail() {
        let tome_bytes = build(&[(r"PC\Game.exe", b"exe", false)], 8, 3);
        let mut tome_file = tempfile::NamedTempFile::new().unwrap();
        tome_file.write_all(&tome_bytes).unwrap();
        let tome = Archive::open(tome_file.reopen().unwrap()).unwrap();
        let folder = tempfile::tempdir().unwrap();
        let game = Directory::open(folder.path()).unwrap();
        assert!(matches!(
            run(
                &tome,
                &sample_plan(),
                &game,
                b"x",
                &|| false,
                &mut |_, _| {}
            ),
            Err(NativeError::Mpq(MpqError::Missing))
        ));
        assert!(matches!(
            run(
                &tome,
                &sample_plan()[2..],
                &game,
                b"x",
                &|| true,
                &mut |_, _| {}
            ),
            Err(NativeError::Plan("a file of another size"))
        ));
        let only_key = &sample_plan()[4..];
        assert!(matches!(
            run(&tome, only_key, &game, b"x", &|| false, &mut |_, _| {}),
            Err(NativeError::Store(io::ErrorKind::NotFound))
        ));
    }

    /// A Tome with one big stored member, and a game folder that holds the
    /// archive already (as for Lord of Destruction).
    fn big_member_install(
        archive_slots: usize,
    ) -> (
        tempfile::NamedTempFile,
        Archive,
        tempfile::TempDir,
        Vec<u8>,
        Vec<Operation>,
    ) {
        let big: Vec<u8> = (0..3 * 1024 * 1024 + 17)
            .map(|index| (index % 251) as u8)
            .collect();
        let tome_bytes = build(&[(r"PC\big.bin", &big, false)], 8, 3);
        let mut tome_file = tempfile::NamedTempFile::new().unwrap();
        tome_file.write_all(&tome_bytes).unwrap();
        let tome = Archive::open(tome_file.reopen().unwrap()).unwrap();
        let folder = tempfile::tempdir().unwrap();
        let archive = build(
            &[("x.txt", b"kept", false), ("y.txt", b"kept", false)],
            archive_slots,
            3,
        );
        std::fs::write(folder.path().join("data.mpq"), &archive).unwrap();
        std::fs::write(folder.path().join("old.dll"), b"old").unwrap();
        let plan = vec![
            Operation::Delete {
                name: "old.dll".to_owned(),
            },
            Operation::Extract {
                from: r"PC\big.bin".to_owned(),
                to: "big.bin".to_owned(),
                size: None,
            },
            Operation::Key {
                container: "data.mpq".to_owned(),
                into: keystore::BASE_KEY_MEMBER.to_owned(),
                product: 24,
            },
            Operation::Owner {
                container: "data.mpq".to_owned(),
                into: keystore::OWNER_MEMBER.to_owned(),
            },
        ];
        (tome_file, tome, folder, archive, plan)
    }

    #[test]
    fn a_stop_during_a_file_ends_the_copy() {
        let (_tome_file, tome, folder, archive, plan) = big_member_install(8);
        let game = Directory::open(folder.path()).unwrap();
        // The stop comes after the first write of the 3 MiB file: the
        // copy must end, not try again.
        let calls = std::cell::Cell::new(0);
        let cancelled = || {
            calls.set(calls.get() + 1);
            calls.get() > 3
        };
        assert!(matches!(
            run(&tome, &plan, &game, b"FAKE", &cancelled, &mut |_, _| {}),
            Err(NativeError::Cancelled)
        ));
        let written = std::fs::metadata(folder.path().join("big.bin"))
            .unwrap()
            .len();
        assert!(written > 0 && written < 3 * 1024 * 1024);
        assert_eq!(
            std::fs::read(folder.path().join("data.mpq")).unwrap(),
            archive
        );
    }

    #[test]
    fn a_full_archive_fails_before_any_change() {
        // Two members in two slots: no room for the key and the owner.
        let (_tome_file, tome, folder, archive, plan) = big_member_install(2);
        let game = Directory::open(folder.path()).unwrap();
        assert!(matches!(
            run(&tome, &plan, &game, b"FAKE", &|| false, &mut |_, _| {}),
            Err(NativeError::Mpq(MpqError::Full))
        ));
        assert!(folder.path().join("old.dll").exists());
        assert!(!folder.path().join("big.bin").exists());
        assert_eq!(
            std::fs::read(folder.path().join("data.mpq")).unwrap(),
            archive
        );
    }
}
