//! Linux descriptor-relative mutation helpers.
//!
//! Every mutator first retains the validated directory object and then uses
//! `*at` operations relative to that descriptor. Renaming an ancestor path can
//! therefore redirect later pathname lookups, but cannot redirect a Launcher
//! mutation outside the directory object that was actually verified.

use std::{
    ffi::{OsStr, OsString},
    fs::{self, File},
    io::{self, Seek, Write},
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Component, Path, PathBuf},
    sync::OnceLock,
};

use rustix::fs::{
    AtFlags, Dir, FileType, Mode, OFlags, RenameFlags, ResolveFlags, mkdirat, openat, openat2,
    readlinkat, renameat, renameat_with, statat, symlinkat, unlinkat,
};
#[cfg(test)]
use rustix::rand::{GetRandomFlags, getrandom};
use serde::{Deserialize, Serialize};

pub(crate) const RETAINED_DIRECTORY: &str = ".pd2launcher-retained";

/// Set once `openat2` was refused as unimplemented, so later lookups go
/// straight to the component-by-component fallback.
static OPENAT2_UNAVAILABLE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Set on the Android host: f2fs makes `f_fsid` from the device number,
/// which a reboot or a system update can change, so a recorded filesystem id
/// is not compared there.
static FILESYSTEM_ID_UNSTABLE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Whether a file recorded as (`recorded_filesystem`, `recorded_inode`) is
/// the file seen now: the same inode, and the same filesystem where its id
/// is stable. On the Android host only the inode is compared; every record
/// is in the app's own data or names its volume in its path.
pub(crate) fn same_recorded_file(
    recorded_filesystem: u64,
    recorded_inode: u64,
    filesystem: u64,
    inode: u64,
) -> bool {
    same_file_identity(
        (recorded_filesystem, recorded_inode),
        (filesystem, inode),
        FILESYSTEM_ID_UNSTABLE.load(std::sync::atomic::Ordering::Relaxed),
    )
}

fn same_file_identity(recorded: (u64, u64), now: (u64, u64), filesystem_unstable: bool) -> bool {
    recorded.1 == now.1 && (filesystem_unstable || recorded.0 == now.0)
}

/// The directories above the Launcher home that may be opened search-only
/// (`O_PATH`): `/` at once, the others when a read-only open of them is
/// refused. On the Android host an app may search `/`, `/data` and
/// `/data/data` but not list them. Unset elsewhere, so every directory open
/// stays as it is.
static SEARCH_ONLY_ANCESTORS: OnceLock<Vec<PathBuf>> = OnceLock::new();

#[cfg(test)]
thread_local! {
    /// `openat2` calls this thread made, the probe's included.
    static OPENAT2_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The Android host, set by the Launcher engine before its first lookup:
/// Android's app seccomp policy kills the process for `openat2` instead of
/// refusing it (so neither the call nor its probe may run), and the
/// ancestors of the Launcher's `home` are opened search-only where they
/// cannot be read.
pub(crate) fn use_android_host(home: Option<&Path>) {
    OPENAT2_UNAVAILABLE.store(true, std::sync::atomic::Ordering::Relaxed);
    FILESYSTEM_ID_UNSTABLE.store(true, std::sync::atomic::Ordering::Relaxed);
    if let Some(home) = home {
        let _ = SEARCH_ONLY_ANCESTORS.set(home_ancestors(home));
    }
}

/// Every directory above `home`, `/` included; never `home` itself.
fn home_ancestors(home: &Path) -> Vec<PathBuf> {
    home.ancestors().skip(1).map(Path::to_owned).collect()
}

fn search_only_ancestors() -> &'static [PathBuf] {
    SEARCH_ONLY_ANCESTORS.get().map_or(&[], Vec::as_slice)
}

/// Open `relative` below `directory` without following any symbolic link,
/// as `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS)`
/// does. `relative` must consist of normal components only.
///
/// Where `openat2` is unavailable (kernels before 5.6, and sandboxes that
/// refuse it, such as Android's app seccomp policy under proot in DroidDeck),
/// the path is walked one component at a time with `openat(O_NOFOLLOW)`,
/// each intermediate directory opened `O_PATH | O_DIRECTORY` from the one
/// before, so no lookup restarts from a path that could change. A symbolic
/// link before the last component fails with `ELOOP`, as with `openat2`.
pub(crate) fn openat_beneath(
    directory: impl rustix::fd::AsFd,
    relative: &Path,
    flags: OFlags,
) -> rustix::io::Result<rustix::fd::OwnedFd> {
    use std::sync::atomic::Ordering;
    let flags = flags | OFlags::CLOEXEC;
    // Tests can run every lookup through the fallback.
    #[cfg(test)]
    if std::env::var_os("PD2_TEST_OPENAT2_UNAVAILABLE").is_some() {
        return openat_beneath_by_component(directory, relative, flags);
    }
    if !OPENAT2_UNAVAILABLE.load(Ordering::Relaxed) {
        #[cfg(test)]
        OPENAT2_CALLS.with(|calls| calls.set(calls.get() + 1));
        match openat2(
            &directory,
            relative,
            flags,
            Mode::empty(),
            ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS,
        ) {
            Err(rustix::io::Errno::NOSYS) => OPENAT2_UNAVAILABLE.store(true, Ordering::Relaxed),
            // Some seccomp filters refuse unknown calls with EPERM instead:
            // a refused probe that needs no permission tells them apart
            // from a real permission error.
            Err(rustix::io::Errno::PERM) if openat2_filtered() => {
                OPENAT2_UNAVAILABLE.store(true, Ordering::Relaxed);
            }
            result => return result,
        }
    }
    openat_beneath_by_component(directory, relative, flags)
}

/// Whether `openat2` itself is refused: opening the current directory
/// beneath itself needs no permission, so EPERM there comes from a filter.
fn openat2_filtered() -> bool {
    #[cfg(test)]
    OPENAT2_CALLS.with(|calls| calls.set(calls.get() + 1));
    matches!(
        openat2(
            rustix::fs::CWD,
            ".",
            OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
            ResolveFlags::BENEATH,
        ),
        Err(rustix::io::Errno::PERM)
    )
}

fn openat_beneath_by_component(
    directory: impl rustix::fd::AsFd,
    relative: &Path,
    flags: OFlags,
) -> rustix::io::Result<rustix::fd::OwnedFd> {
    let mut names = Vec::new();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(rustix::io::Errno::XDEV);
        };
        names.push(name);
    }
    let Some((last, parents)) = names.split_last() else {
        return Err(rustix::io::Errno::INVAL);
    };
    // A symbolic link opened with `O_NOFOLLOW | O_DIRECTORY` reports
    // `ENOTDIR`; openat2 reports `ELOOP`.
    let refuse_link = |parent: rustix::fd::BorrowedFd<'_>, name: &OsStr, error| match statat(
        parent,
        name,
        AtFlags::SYMLINK_NOFOLLOW,
    ) {
        Ok(stat) if FileType::from_raw_mode(stat.st_mode) == FileType::Symlink => {
            rustix::io::Errno::LOOP
        }
        _ => error,
    };
    let mut current: Option<rustix::fd::OwnedFd> = None;
    for name in parents {
        let parent = current
            .as_ref()
            .map_or_else(|| directory.as_fd(), rustix::fd::AsFd::as_fd);
        let next = openat(
            parent,
            *name,
            OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|error| match error {
            rustix::io::Errno::NOTDIR => refuse_link(parent, name, error),
            error => error,
        })?;
        current = Some(next);
    }
    let parent = current
        .as_ref()
        .map_or_else(|| directory.as_fd(), rustix::fd::AsFd::as_fd);
    // A link as the last component fails with `ELOOP`, except that
    // `O_PATH | O_NOFOLLOW` opens the link itself, as openat2 does.
    let opened = openat(parent, *last, flags | OFlags::NOFOLLOW, Mode::empty()).map_err(
        |error| match error {
            rustix::io::Errno::NOTDIR => refuse_link(parent, last, error),
            error => error,
        },
    )?;
    if !flags.contains(OFlags::NOFOLLOW)
        && FileType::from_raw_mode(rustix::fs::fstat(&opened)?.st_mode) == FileType::Symlink
    {
        return Err(rustix::io::Errno::LOOP);
    }
    Ok(opened)
}
#[cfg(test)]
const NAME_ATTEMPTS: usize = 16;
const MAX_DIRECTORY_ENTRIES: usize = 8192;
/// Depth bound for removing a Launcher-owned tree.
const MAX_OWNED_TREE_DEPTH: usize = 64;

pub(crate) struct Directory(File);

/// Whether a directory holds an unfinished atomic write: a temporary file
/// left by a writer that stopped before its rename. The rename either
/// happened or not, so there is never anything to finish, only residue to
/// remove.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AtomicIntentSnapshot {
    Clear,
    RecoverableResidue,
}

impl Directory {
    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        let relative = absolute_relative(path)?;
        let root = Self::root(search_only_ancestors())?;
        if relative.as_os_str().is_empty() {
            return Ok(root);
        }
        root.open_directory(relative)
    }

    /// Creates an absolute Launcher-owned directory one retained component at
    /// a time. No lookup is restarted from a mutable absolute path.
    pub(crate) fn ensure_absolute(path: &Path) -> io::Result<Self> {
        Self::ensure_absolute_with(path, search_only_ancestors())
    }

    /// [`Self::ensure_absolute`] where only the directories in `search_only`
    /// (the home's ancestors) may be retained search-only when they cannot
    /// be read.
    fn ensure_absolute_with(path: &Path, search_only: &[PathBuf]) -> io::Result<Self> {
        let relative = absolute_relative(path)?;
        let root = Self::root(search_only)?;
        if relative.as_os_str().is_empty() {
            return Ok(root);
        }
        root.ensure_components(Path::new("/"), relative, search_only)
    }

    fn root(search_only: &[PathBuf]) -> io::Result<Self> {
        // A search-only `/` (the Android host's) is opened `O_PATH` at once:
        // SELinux refuses a read-only open of it there, and logs a denial
        // for each.
        let flags = if search_only
            .iter()
            .any(|ancestor| ancestor == Path::new("/"))
        {
            OFlags::PATH
        } else {
            OFlags::RDONLY
        };
        let directory = File::from(
            rustix::fs::open(
                "/",
                flags | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(io::Error::from)?,
        );
        validate_directory(&directory)?;
        Ok(Self(directory))
    }

    pub(crate) fn open_directory(&self, relative: &Path) -> io::Result<Self> {
        validate_relative(relative)?;
        let directory = File::from(
            openat_beneath(&self.0, relative, OFlags::RDONLY | OFlags::DIRECTORY)
                .map_err(io::Error::from)?,
        );
        validate_directory(&directory)?;
        Ok(Self(directory))
    }

    /// The retained descriptor, for descriptor-relative calls this type does
    /// not wrap (`statat`, `readlinkat`, `symlinkat`, `renameat`).
    pub(crate) fn as_file(&self) -> &File {
        &self.0
    }

    pub(crate) fn try_clone(&self) -> io::Result<Self> {
        self.0.try_clone().map(Self)
    }

    pub(crate) fn try_clone_file(&self) -> io::Result<File> {
        self.0.try_clone()
    }

    /// Create or retain every normal component below this directory. Every
    /// lookup remains relative to the preceding retained descriptor.
    pub(crate) fn ensure_directory(&self, relative: &Path) -> io::Result<Self> {
        self.ensure_components(Path::new(""), relative, &[])
    }

    /// [`Self::ensure_directory`] for this directory at `location`: a
    /// component whose path is in `search_only` and cannot be read is
    /// retained search-only (`O_PATH`), which is enough to walk on. Every
    /// other component fails closed.
    fn ensure_components(
        &self,
        location: &Path,
        relative: &Path,
        search_only: &[PathBuf],
    ) -> io::Result<Self> {
        validate_relative(relative)?;
        let mut current = self.try_clone()?;
        let mut location = location.to_owned();
        for component in relative.components() {
            let Component::Normal(name) = component else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "descriptor-relative path was invalid",
                ));
            };
            match mkdirat(&current.0, name, Mode::RUSR | Mode::WUSR | Mode::XUSR) {
                Ok(()) => current.0.sync_all()?,
                Err(error) if error == rustix::io::Errno::EXIST => {}
                Err(error) => return Err(error.into()),
            }
            location.push(name);
            current = match current.open_directory(Path::new(name)) {
                Err(error)
                    if error.raw_os_error() == Some(rustix::io::Errno::ACCESS.raw_os_error())
                        && search_only.contains(&location) =>
                {
                    let directory = File::from(
                        openat_beneath(
                            &current.0,
                            Path::new(name),
                            OFlags::PATH | OFlags::DIRECTORY,
                        )
                        .map_err(io::Error::from)?,
                    );
                    validate_directory(&directory)?;
                    Self(directory)
                }
                result => result?,
            };
        }
        Ok(current)
    }

    /// Create the directory `name`, or retain an existing one only while it
    /// is completely empty. Returns whether it was created here.
    pub(crate) fn create_or_open_empty_directory(&self, name: &OsStr) -> io::Result<(Self, bool)> {
        validate_name(name)?;
        let created = match mkdirat(&self.0, name, Mode::RUSR | Mode::WUSR | Mode::XUSR) {
            Ok(()) => {
                self.0.sync_all()?;
                true
            }
            Err(error) if error == rustix::io::Errno::EXIST => false,
            Err(error) => return Err(error.into()),
        };
        let directory = self.open_directory(Path::new(name))?;
        if !created && !directory.names_including_retained()?.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "an existing directory was not empty",
            ));
        }
        Ok((directory, created))
    }

    /// Remove the empty directory `name` this Launcher just created.
    pub(crate) fn remove_empty_directory(&self, name: &OsStr) -> io::Result<()> {
        validate_name(name)?;
        unlinkat(&self.0, name, AtFlags::REMOVEDIR).map_err(io::Error::from)?;
        self.0.sync_all()
    }

    pub(crate) fn open_file(&self, relative: &Path) -> io::Result<File> {
        let file = self.open_beneath(relative, OFlags::RDONLY)?;
        validate_regular(&file)?;
        Ok(file)
    }

    /// Opens a node below this retained directory without imposing a file-type
    /// policy. Callers that need directories, `O_PATH`, or executable-specific
    /// validation retain the same root capability and apply their own type
    /// contract to the returned descriptor.
    pub(crate) fn open_beneath(&self, relative: &Path, mut flags: OFlags) -> io::Result<File> {
        validate_relative(relative)?;
        flags |= OFlags::NOFOLLOW | OFlags::CLOEXEC;
        Ok(File::from(
            openat_beneath(&self.0, relative, flags).map_err(io::Error::from)?,
        ))
    }

    pub(crate) fn open_or_create_file(&self, name: &OsStr) -> io::Result<File> {
        validate_name(name)?;
        let file = match rustix::fs::openat(
            &self.0,
            name,
            OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(file) => File::from(file),
            Err(error) if error == rustix::io::Errno::NOENT => File::from(
                rustix::fs::openat(
                    &self.0,
                    name,
                    OFlags::CREATE
                        | OFlags::EXCL
                        | OFlags::RDWR
                        | OFlags::NOFOLLOW
                        | OFlags::CLOEXEC,
                    Mode::RUSR | Mode::WUSR,
                )
                .map_err(io::Error::from)?,
            ),
            Err(error) => return Err(error.into()),
        };
        validate_regular(&file)?;
        Ok(file)
    }

    pub(crate) fn create_file(&self, name: &OsStr) -> io::Result<File> {
        validate_name(name)?;
        let file = File::from(
            rustix::fs::openat(
                &self.0,
                name,
                OFlags::CREATE | OFlags::EXCL | OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            )
            .map_err(io::Error::from)?,
        );
        validate_regular(&file)?;
        Ok(file)
    }

    /// Create a flat, same-directory symbolic link or prove that an existing
    /// unique link has exactly the expected target. Both names are restricted
    /// to one normal path component, so the link cannot escape its retained
    /// parent directory.
    pub(crate) fn create_or_validate_flat_symlink(
        &self,
        name: &OsStr,
        target: &OsStr,
    ) -> io::Result<()> {
        validate_name(target)?;
        self.create_or_validate_symlink(name, Path::new(target))
    }

    /// Create a link whose relative `target` may name other directories. The
    /// caller proves lexically that the target stays inside its tree; this
    /// only rejects absolute targets and never follows the link itself.
    pub(crate) fn create_or_validate_relative_symlink(
        &self,
        name: &OsStr,
        target: &Path,
    ) -> io::Result<()> {
        if target.as_os_str().is_empty()
            || target
                .components()
                .any(|component| matches!(component, Component::RootDir | Component::Prefix(_)))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "symbolic link target was not relative",
            ));
        }
        self.create_or_validate_symlink(name, target)
    }

    fn create_or_validate_symlink(&self, name: &OsStr, target: &Path) -> io::Result<()> {
        validate_name(name)?;
        let target = target.as_os_str();
        match symlinkat(target, &self.0, name) {
            Ok(()) => self.0.sync_all()?,
            Err(error) if error == rustix::io::Errno::EXIST => {}
            Err(error) => return Err(error.into()),
        }
        let metadata = statat(&self.0, name, AtFlags::SYMLINK_NOFOLLOW).map_err(io::Error::from)?;
        if FileType::from_raw_mode(metadata.st_mode) != FileType::Symlink || metadata.st_nlink != 1
        {
            return Err(identity_error("symbolic link shape was invalid"));
        }
        let observed = readlinkat(&self.0, name, Vec::new()).map_err(io::Error::from)?;
        if observed.as_bytes() != target.as_bytes() {
            return Err(identity_error("symbolic link target was invalid"));
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn rename_open_file_to(
        &self,
        source: &OsStr,
        opened: &File,
        destination: &Self,
        target: &OsStr,
    ) -> io::Result<()> {
        self.rename_open_file_to_inner(source, opened, destination, target, || Ok(()))
    }

    #[cfg(test)]
    fn rename_open_file_to_inner(
        &self,
        source: &OsStr,
        opened: &File,
        destination: &Self,
        target: &OsStr,
        before_link: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        self.require_named_file(source, opened)?;
        validate_name(target)?;
        let prepared = destination.prepare_copy(opened)?;
        before_link()?;
        destination.link_prepared(&prepared, target)?;
        // Detach the source only after the exact retained bytes are linked at
        // the destination. If its mutable name was substituted, preserve both
        // objects and report ambiguity instead of moving attacker bytes.
        self.retain_open_file(source, opened, "promoted")?;
        destination.0.sync_all()
    }

    #[cfg(test)]
    pub(crate) fn rename_open_directory_to(
        &self,
        source: &OsStr,
        opened: &Self,
        destination: &Self,
        target: &OsStr,
    ) -> io::Result<()> {
        self.rename_open_directory_to_inner(source, opened, destination, target, || Ok(()))
    }

    #[cfg(test)]
    fn rename_open_directory_to_inner(
        &self,
        source: &OsStr,
        opened: &Self,
        destination: &Self,
        target: &OsStr,
        before_materialize: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        self.require_named_directory(source, opened)?;
        validate_name(target)?;
        before_materialize()?;
        mkdirat(&destination.0, target, Mode::RUSR | Mode::WUSR | Mode::XUSR)
            .map_err(io::Error::from)?;
        let materialized = destination.open_directory(Path::new(target))?;
        copy_directory(opened, &materialized)?;
        destination.require_named_directory(target, &materialized)?;
        self.retain_open_directory(source, opened, "promoted")?;
        destination.0.sync_all()
    }

    #[cfg(test)]
    pub(crate) fn remove_open_file(&self, source: &OsStr, opened: &File) -> io::Result<()> {
        self.retain_open_file(source, opened, "discarded")?;
        self.0.sync_all()
    }

    /// Remove a launcher-owned file the caller holds open, and only that file.
    pub(crate) fn remove_open_file_bounded(&self, source: &OsStr, opened: &File) -> io::Result<()> {
        self.remove_open_file_bounded_recoverable(source, opened, &mut |_| Ok(()))
    }

    fn remove_open_file_bounded_recoverable(
        &self,
        source: &OsStr,
        opened: &File,
        hook: &mut impl FnMut(AtomicMutation) -> io::Result<()>,
    ) -> io::Result<()> {
        validate_name(source)?;
        self.require_named_file(source, opened)?;
        unlinkat(&self.0, source, AtFlags::empty())?;
        hook(AtomicMutation::Removed)?;
        self.0.sync_all()?;
        hook(AtomicMutation::DirectorySynced)
    }

    pub(crate) fn atomic_write(&self, name: &OsStr, bytes: &[u8]) -> io::Result<()> {
        self.atomic_write_recoverable(name, bytes, &mut |_| Ok(()))
    }

    pub(crate) fn atomic_write_from_open(&self, name: &OsStr, source: &File) -> io::Result<()> {
        self.atomic_write_from_open_recoverable(name, source, &mut |_| Ok(()))
    }

    /// Remove what an interrupted atomic write left in this directory.
    pub(crate) fn recover_atomic(&self) -> io::Result<()> {
        self.remove_atomic_residue(|_| true)
    }

    /// Nothing to forget: a write leaves no record behind. `opened` must still
    /// be the file named `name`.
    pub(crate) fn forget_atomic_target(&self, name: &OsStr, opened: &File) -> io::Result<()> {
        self.require_named_file(name, opened)
    }

    /// Whether `opened` is the file currently named `name`, as an atomic
    /// write leaves it.
    pub(crate) fn atomic_publication_matches(
        &self,
        name: &OsStr,
        opened: &File,
    ) -> io::Result<bool> {
        validate_name(name)?;
        match self.open_beneath(Path::new(name), OFlags::PATH) {
            Ok(current) => same_identity(&current, opened),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Remove what interrupted atomic writes of `names` left here.
    pub(crate) fn recover_atomic_for_names(
        &self,
        names: &[(&OsStr, AtomicIntentKind)],
        _hook: &mut impl FnMut(AtomicMutation) -> io::Result<()>,
    ) -> io::Result<()> {
        let tags = names
            .iter()
            .map(|(name, _)| target_tag(name))
            .collect::<Vec<_>>();
        self.remove_atomic_residue(|tag| tags.iter().any(|wanted| wanted == tag))
    }

    /// Whether an interrupted atomic write of one of `names` left residue.
    pub(crate) fn has_pending_atomic_for_names(
        &self,
        names: &[(&OsStr, AtomicIntentKind)],
    ) -> io::Result<bool> {
        let tags = names
            .iter()
            .map(|(name, _)| target_tag(name))
            .collect::<Vec<_>>();
        Ok(self
            .atomic_residue()?
            .iter()
            .any(|(_, tag)| tags.contains(tag)))
    }

    pub(crate) fn pending_atomic_intent(
        &self,
        _byte_limit: u64,
    ) -> io::Result<AtomicIntentSnapshot> {
        Ok(if self.atomic_residue()?.is_empty() {
            AtomicIntentSnapshot::Clear
        } else {
            AtomicIntentSnapshot::RecoverableResidue
        })
    }

    /// Replace (or create) `name` with `bytes`: a temporary file beside it
    /// is written and flushed, then renamed over it, so `name` is always the
    /// old file or the new one, on any filesystem with POSIX rename.
    pub(crate) fn atomic_write_recoverable(
        &self,
        name: &OsStr,
        bytes: &[u8],
        hook: &mut impl FnMut(AtomicMutation) -> io::Result<()>,
    ) -> io::Result<()> {
        self.publish_atomically(name, None, |file| file.write_all(bytes), hook)
    }

    /// [`Self::atomic_write_recoverable`] with the bytes and permissions of
    /// the open `source`.
    pub(crate) fn atomic_write_from_open_recoverable(
        &self,
        name: &OsStr,
        source: &File,
        hook: &mut impl FnMut(AtomicMutation) -> io::Result<()>,
    ) -> io::Result<()> {
        validate_regular(source)?;
        let mut reader = source.try_clone()?;
        reader.rewind()?;
        let permissions = source.metadata()?.permissions();
        self.publish_atomically(
            name,
            Some(permissions),
            |file| io::copy(&mut reader, file).map(drop),
            hook,
        )
    }

    /// A copy of the open `source` published as `name` like
    /// [`Self::atomic_write_from_open`], whose temporary file also takes the
    /// times `accessed` and `modified` before it is flushed and renamed, so
    /// `name` never carries the time of the copy, even after a stop. The
    /// source is only read, so it may have other names (hard links); the
    /// file written is always a new one.
    pub(crate) fn atomic_copy_with_times(
        &self,
        name: &OsStr,
        source: &File,
        accessed: std::time::SystemTime,
        modified: std::time::SystemTime,
    ) -> io::Result<()> {
        let metadata = source.metadata()?;
        if !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "opened object was not a regular file",
            ));
        }
        let mut reader = source.try_clone()?;
        reader.rewind()?;
        self.publish_atomically(
            name,
            Some(metadata.permissions()),
            |file| {
                io::copy(&mut reader, file)?;
                file.set_times(
                    fs::FileTimes::new()
                        .set_accessed(accessed)
                        .set_modified(modified),
                )
            },
            &mut |_| Ok(()),
        )
    }

    fn publish_atomically(
        &self,
        name: &OsStr,
        permissions: Option<fs::Permissions>,
        fill: impl FnOnce(&mut File) -> io::Result<()>,
        hook: &mut impl FnMut(AtomicMutation) -> io::Result<()>,
    ) -> io::Result<()> {
        validate_name(name)?;
        // What an earlier, interrupted write of this same file left. Other
        // targets' temporary files may belong to writes in progress.
        let tag = target_tag(name);
        self.remove_atomic_residue(|residue| residue == tag)?;
        let temporary = atomic_temporary_name(name)?;
        let mut file = File::from(
            openat(
                &self.0,
                &temporary,
                OFlags::CREATE | OFlags::EXCL | OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            )
            .map_err(io::Error::from)?,
        );
        let written = (|| {
            fill(&mut file)?;
            if let Some(permissions) = permissions {
                file.set_permissions(permissions)?;
            }
            file.sync_all()
        })();
        if let Err(error) = written {
            let _ = unlinkat(&self.0, &temporary, AtFlags::empty());
            return Err(error);
        }
        // From here a stop is a crash: the residue is removed by recovery.
        hook(AtomicMutation::Prepared)?;
        if let Err(error) = renameat(&self.0, &temporary, &self.0, name) {
            let _ = unlinkat(&self.0, &temporary, AtFlags::empty());
            return Err(error.into());
        }
        hook(AtomicMutation::Published)?;
        self.0.sync_all()?;
        hook(AtomicMutation::DirectorySynced)
    }

    /// Temporary files of interrupted atomic writes, with their target tags.
    fn atomic_residue(&self) -> io::Result<Vec<(OsString, String)>> {
        Ok(self
            .names_inner(false)?
            .into_iter()
            .filter_map(|name| {
                let tag = atomic_temporary_tag(&name)?;
                Some((name, tag))
            })
            .collect())
    }

    fn remove_atomic_residue(&self, wanted: impl Fn(&str) -> bool) -> io::Result<()> {
        let mut removed = false;
        for (name, tag) in self.atomic_residue()? {
            if wanted(&tag) {
                match unlinkat(&self.0, &name, AtFlags::empty()) {
                    Ok(()) | Err(rustix::io::Errno::NOENT) => removed = true,
                    Err(error) => return Err(error.into()),
                }
            }
        }
        if removed {
            self.0.sync_all()?;
        }
        Ok(())
    }

    #[cfg(test)]
    fn atomic_write_inner(
        &self,
        name: &OsStr,
        bytes: &[u8],
        before_promote: impl FnOnce(&Self, &OsStr) -> io::Result<()>,
    ) -> io::Result<()> {
        validate_name(name)?;
        let expected_target = match self.open_file(Path::new(name)) {
            Ok(file) => Some(file),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        let temporary = unique("partial")?;
        let mut file = File::from(
            rustix::fs::openat(
                &self.0,
                &temporary,
                OFlags::CREATE | OFlags::EXCL | OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            )
            .map_err(io::Error::from)?,
        );
        let result = (|| {
            file.write_all(bytes)?;
            file.sync_all()?;
            validate_regular(&file)?;
            before_promote(self, &temporary)?;
            self.replace_with_open_file(&temporary, &file, name, expected_target.as_ref())
        })();
        if result.is_err() {
            let _ = self.remove_open_file(&temporary, &file);
        }
        result
    }

    #[cfg(test)]
    fn retained_directory(&self) -> io::Result<Self> {
        self.ensure_directory(Path::new(RETAINED_DIRECTORY))
    }

    #[cfg(test)]
    fn retain_open_file(&self, source: &OsStr, opened: &File, suffix: &str) -> io::Result<()> {
        self.retain_open_file_inner(
            source,
            opened,
            suffix,
            |_, _| Ok(()),
            |_, _, verified| {
                verified.set_len(0)?;
                verified.sync_all()
            },
        )
    }

    #[cfg(test)]
    fn retain_open_file_inner(
        &self,
        source: &OsStr,
        opened: &File,
        suffix: &str,
        mut before_rename: impl FnMut(&Self, &OsStr) -> io::Result<()>,
        after_verified: impl FnOnce(&Self, &OsStr, &File) -> io::Result<()>,
    ) -> io::Result<()> {
        self.require_named_file(source, opened)?;
        let retained = self.retained_directory()?;
        for _ in 0..NAME_ATTEMPTS {
            let candidate = unique(suffix)?;
            before_rename(&retained, &candidate)?;
            match renameat_with(
                &self.0,
                source,
                &retained.0,
                &candidate,
                RenameFlags::NOREPLACE,
            ) {
                Ok(()) => {
                    let renamed = retained.open_file(Path::new(&candidate))?;
                    if !same_identity(opened, &renamed)? {
                        return Err(identity_error("retained file was substituted"));
                    }
                    let writable = open_rw(&retained, &candidate)?;
                    if !same_identity(&renamed, &writable)? {
                        return Err(identity_error(
                            "retained cleanup descriptor was substituted",
                        ));
                    }
                    after_verified(&retained, &candidate, &writable)?;
                    retained.0.sync_all()?;
                    return Ok(());
                }
                Err(error) if error == rustix::io::Errno::EXIST => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not reserve a collision-free retained file name",
        ))
    }

    #[cfg(test)]
    fn retain_open_directory(&self, source: &OsStr, opened: &Self, suffix: &str) -> io::Result<()> {
        self.retain_open_directory_inner(source, opened, suffix, |_, _| Ok(()), |_, _| Ok(()))
    }

    #[cfg(test)]
    fn retain_open_directory_inner(
        &self,
        source: &OsStr,
        opened: &Self,
        suffix: &str,
        mut before_rename: impl FnMut(&Self, &OsStr) -> io::Result<()>,
        after_verified: impl FnOnce(&Self, &OsStr) -> io::Result<()>,
    ) -> io::Result<()> {
        self.require_named_directory(source, opened)?;
        let retained = self.retained_directory()?;
        for _ in 0..NAME_ATTEMPTS {
            let candidate = unique(suffix)?;
            before_rename(&retained, &candidate)?;
            match renameat_with(
                &self.0,
                source,
                &retained.0,
                &candidate,
                RenameFlags::NOREPLACE,
            ) {
                Ok(()) => {
                    let renamed = retained.open_directory(Path::new(&candidate))?;
                    if !same_identity(&opened.0, &renamed.0)? {
                        return Err(identity_error("retained directory was substituted"));
                    }
                    after_verified(&retained, &candidate)?;
                    retained.0.sync_all()?;
                    return Ok(());
                }
                Err(error) if error == rustix::io::Errno::EXIST => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not reserve a collision-free retained directory name",
        ))
    }

    #[cfg(test)]
    fn replace_with_open_file(
        &self,
        source: &OsStr,
        opened: &File,
        target: &OsStr,
        expected_target: Option<&File>,
    ) -> io::Result<()> {
        self.require_named_file(source, opened)?;
        let prepared = self.prepare_copy(opened)?;
        if let Some(file) = expected_target {
            self.retain_open_file(target, file, "replaced")?;
        }
        self.link_prepared(&prepared, target)?;
        self.retain_open_file(source, opened, "committed")?;
        self.0.sync_all()
    }

    fn require_named_file(&self, name: &OsStr, opened: &File) -> io::Result<()> {
        validate_name(name)?;
        validate_regular(opened)?;
        let named = self.open_file(Path::new(name))?;
        if same_identity(opened, &named)? {
            Ok(())
        } else {
            Err(identity_error(
                "file name no longer identified retained object",
            ))
        }
    }

    /// Proves that a caller-retained regular file is still the exact object
    /// published at `name`. This is used by restartable materializers after
    /// writing through the retained descriptor; a name substitution therefore
    /// becomes an indeterminate failure rather than redirecting the write.
    pub(crate) fn require_open_file(&self, name: &OsStr, opened: &File) -> io::Result<()> {
        self.require_named_file(name, opened)
    }

    /// Reclaim bytes from the exact Launcher-owned file without unlinking a
    /// mutable name. The writable reopen is compared with the caller's proven
    /// descriptor before truncation and rebound again afterwards.
    pub(crate) fn truncate_open_file(&self, name: &OsStr, opened: &File) -> io::Result<()> {
        self.require_named_file(name, opened)?;
        let writable = open_rw(self, name)?;
        if !same_identity(opened, &writable)? {
            return Err(identity_error("truncate target identity was substituted"));
        }
        writable.set_len(0)?;
        writable.sync_all()?;
        self.require_named_file(name, &writable)
    }

    #[cfg(test)]
    fn require_named_directory(&self, name: &OsStr, opened: &Self) -> io::Result<()> {
        validate_name(name)?;
        let named = self.open_directory(Path::new(name))?;
        if same_identity(&opened.0, &named.0)? {
            Ok(())
        } else {
            Err(identity_error(
                "directory name no longer identified retained object",
            ))
        }
    }

    #[cfg(test)]
    fn prepare_copy(&self, opened: &File) -> io::Result<File> {
        validate_regular(opened)?;
        let mut source = opened.try_clone()?;
        source.rewind()?;
        let mut prepared = File::from(
            openat(
                &self.0,
                ".",
                OFlags::TMPFILE | OFlags::RDWR | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            )
            .map_err(io::Error::from)?,
        );
        io::copy(&mut source, &mut prepared)?;
        prepared.set_permissions(opened.metadata()?.permissions())?;
        prepared.sync_all()?;
        let metadata = prepared.metadata()?;
        if !metadata.is_file() || metadata.nlink() != 0 {
            return Err(identity_error("anonymous promotion file was not retained"));
        }
        Ok(prepared)
    }

    #[cfg(test)]
    fn link_prepared(&self, prepared: &File, target: &OsStr) -> io::Result<()> {
        validate_name(target)?;
        rustix::fs::linkat(prepared, "", &self.0, target, AtFlags::EMPTY_PATH)
            .map_err(io::Error::from)?;
        let linked = self.open_file(Path::new(target))?;
        if !same_identity(prepared, &linked)? {
            return Err(identity_error("linked promotion target was substituted"));
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn remove_tree(&self, name: &OsStr) -> io::Result<()> {
        validate_name(name)?;
        match self.open_directory(Path::new(name)) {
            Ok(directory) => self.remove_open_directory(name, &directory),
            Err(directory_error) => match self.open_file(Path::new(name)) {
                Ok(file) => self.remove_open_file(name, &file),
                Err(file_error)
                    if directory_error.kind() == io::ErrorKind::NotFound
                        && file_error.kind() == io::ErrorKind::NotFound =>
                {
                    Ok(())
                }
                Err(_) => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "prune target was not a unique retained file or directory",
                )),
            },
        }
    }

    #[cfg(test)]
    pub(crate) fn remove_open_directory(&self, name: &OsStr, opened: &Self) -> io::Result<()> {
        // Linux cannot unlink a directory by retained FD. Detach the exact
        // public entry into the collision-resistant retained namespace and do
        // not perform a later mutable-name unlink.
        opened.validate_contents()?;
        self.retain_open_directory(name, opened, "pruned")?;
        self.0.sync_all()
    }

    #[cfg(test)]
    fn validate_contents(&self) -> io::Result<()> {
        for name in self.names()? {
            match self.open_directory(Path::new(&name)) {
                Ok(directory) => directory.validate_contents()?,
                Err(directory_error) => {
                    if self.open_file(Path::new(&name)).is_err() {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "tree entry was not a safe unique file or directory: {directory_error}"
                            ),
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    /// Rename the retained directory `source` to `target` below
    /// `destination` without replacing any existing name, and prove the
    /// renamed directory is still the one the caller retained.
    pub(crate) fn rename_directory_noreplace(
        &self,
        source: &OsStr,
        opened: &Self,
        destination: &Self,
        target: &OsStr,
    ) -> io::Result<()> {
        validate_name(source)?;
        validate_name(target)?;
        let named = self.open_directory(Path::new(source))?;
        if !same_identity(&opened.0, &named.0)? {
            return Err(identity_error("rename source was substituted"));
        }
        renameat_with(
            &self.0,
            source,
            &destination.0,
            target,
            RenameFlags::NOREPLACE,
        )
        .map_err(io::Error::from)?;
        let renamed = destination.open_directory(Path::new(target))?;
        if !same_identity(&opened.0, &renamed.0)? {
            return Err(identity_error("renamed directory was substituted"));
        }
        destination.0.sync_all()?;
        self.0.sync_all()
    }

    /// Atomically swap the retained directory `source` below this directory
    /// with the retained directory `target` below `destination`
    /// (`RENAME_EXCHANGE`), then prove each name now holds the other's
    /// object. Both must be on one filesystem.
    pub(crate) fn exchange_directories(
        &self,
        source: &OsStr,
        opened_source: &Self,
        destination: &Self,
        target: &OsStr,
        opened_target: &Self,
    ) -> io::Result<()> {
        validate_name(source)?;
        validate_name(target)?;
        if !same_identity(&opened_source.0, &self.open_directory(Path::new(source))?.0)?
            || !same_identity(
                &opened_target.0,
                &destination.open_directory(Path::new(target))?.0,
            )?
        {
            return Err(identity_error("exchange source was substituted"));
        }
        renameat_with(
            &self.0,
            source,
            &destination.0,
            target,
            RenameFlags::EXCHANGE,
        )
        .map_err(io::Error::from)?;
        if !same_identity(
            &opened_source.0,
            &destination.open_directory(Path::new(target))?.0,
        )? || !same_identity(&opened_target.0, &self.open_directory(Path::new(source))?.0)?
        {
            return Err(identity_error("exchanged directory was substituted"));
        }
        destination.0.sync_all()?;
        self.0.sync_all()
    }

    /// The device this directory lives on; a rename across devices fails.
    pub(crate) fn device_id(&self) -> io::Result<u64> {
        Ok(self.0.metadata()?.dev())
    }

    /// Remove the Launcher-owned directory tree `name` below this directory.
    /// No symbolic link is ever followed: links are unlinked as names, each
    /// subdirectory is reopened beneath its retained parent, and depth is
    /// bounded. A substituted node fails the removal instead of redirecting it.
    pub(crate) fn remove_owned_tree(&self, name: &OsStr) -> io::Result<()> {
        validate_name(name)?;
        self.open_directory(Path::new(name))?
            .remove_owned_contents(0)?;
        unlinkat(&self.0, name, AtFlags::REMOVEDIR).map_err(io::Error::from)?;
        self.0.sync_all()
    }

    fn remove_owned_contents(&self, depth: usize) -> io::Result<()> {
        if depth > MAX_OWNED_TREE_DEPTH {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "owned tree exceeded the depth bound",
            ));
        }
        // Wine maps a read-only attribute onto a directory's mode. Entries
        // cannot be unlinked from a directory the owner cannot write, so the
        // retained descriptor (never a path) regains owner write first.
        let mode = self.0.metadata()?.mode();
        if mode & 0o300 != 0o300 {
            rustix::fs::fchmod(&self.0, Mode::from_raw_mode((mode | 0o700) & 0o7777))
                .map_err(io::Error::from)?;
        }
        for name in self.names_inner(false)? {
            let metadata =
                statat(&self.0, &name, AtFlags::SYMLINK_NOFOLLOW).map_err(io::Error::from)?;
            if FileType::from_raw_mode(metadata.st_mode) == FileType::Directory {
                self.open_directory(Path::new(&name))?
                    .remove_owned_contents(depth + 1)?;
                unlinkat(&self.0, &name, AtFlags::REMOVEDIR).map_err(io::Error::from)?;
            } else {
                unlinkat(&self.0, &name, AtFlags::empty()).map_err(io::Error::from)?;
            }
        }
        Ok(())
    }

    pub(crate) fn names(&self) -> io::Result<Vec<OsString>> {
        self.names_inner(true)
    }

    fn names_including_retained(&self) -> io::Result<Vec<OsString>> {
        self.names_inner(false)
    }

    fn names_inner(&self, skip_retained: bool) -> io::Result<Vec<OsString>> {
        let mut stream = Dir::read_from(&self.0).map_err(io::Error::from)?;
        let mut names = Vec::new();
        while let Some(entry) = stream.read() {
            let entry = entry.map_err(io::Error::from)?;
            let bytes = entry.file_name().to_bytes();
            if !matches!(bytes, b"." | b"..")
                && (!skip_retained || !is_launcher_scratch(OsStr::from_bytes(bytes)))
            {
                names.push(OsStr::from_bytes(bytes).to_owned());
                if names.len() > MAX_DIRECTORY_ENTRIES {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "managed directory entry limit exceeded",
                    ));
                }
            }
        }
        Ok(names)
    }

    pub(crate) fn sync(&self) -> io::Result<()> {
        self.0.sync_all()
    }

    /// Reboot-stable filesystem id that anchors legacy identity migration.
    pub(crate) fn filesystem_id(&self) -> io::Result<u64> {
        filesystem_id(&self.0)
    }
}

/// A facility a managed root must provide. Only what the
/// Launcher and Wine actually use is required: the filesystem type, its
/// identity, and who owns the directory are not checked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RootCapability {
    /// Not mounted `noexec`: Wine and the game execute from the root.
    Executable,
    FreeSpace,
    /// The Launcher can create and write files in it.
    Writable,
    /// `rename` replaces a file in one step (atomic writes rely on it).
    AtomicRename,
    /// `flock`.
    Locking,
    /// `symlinkat` (Wine's `dosdevices`).
    SymbolicLinks,
    CaseSensitive,
}

/// Test seam: fail the probe at one capability after its real check.
pub(crate) trait ProbeFaults {
    fn fail(&self, capability: RootCapability) -> bool;
}

pub(crate) struct NoProbeFaults;

impl ProbeFaults for NoProbeFaults {
    fn fail(&self, _capability: RootCapability) -> bool {
        false
    }
}

/// Minimum free space for a new managed root.
pub(crate) const ROOT_MINIMUM_FREE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const PROBE_NAMES: [&str; 6] = [
    ".pd2launcher-probe-a",
    ".pd2launcher-probe-b",
    ".pd2launcher-probe-c",
    ".pd2launcher-probe-link",
    ".pd2launcher-probe-case",
    ".pd2launcher-PROBE-CASE",
];

impl Directory {
    /// Prove that this (new, Launcher-created) root provides every facility
    /// the Launcher relies on. Every probe entry is removed again, whatever
    /// the outcome.
    pub(crate) fn probe_root_capabilities(
        &self,
        minimum_free: u64,
        faults: &dyn ProbeFaults,
    ) -> Result<(), RootCapability> {
        let result = self.probe_root_capabilities_inner(minimum_free, faults);
        for name in PROBE_NAMES {
            let _ = unlinkat(&self.0, name, AtFlags::empty());
        }
        let _ = self.0.sync_all();
        result
    }

    fn probe_root_capabilities_inner(
        &self,
        minimum_free: u64,
        faults: &dyn ProbeFaults,
    ) -> Result<(), RootCapability> {
        use RootCapability as Cap;
        let check = |capability: Cap, ok: bool| {
            if ok && !faults.fail(capability) {
                Ok(())
            } else {
                Err(capability)
            }
        };
        let statvfs = rustix::fs::fstatvfs(&self.0).map_err(|_| Cap::Writable)?;
        check(
            Cap::Executable,
            !statvfs
                .f_flag
                .contains(rustix::fs::StatVfsMountFlags::NOEXEC),
        )?;
        check(
            Cap::FreeSpace,
            statvfs.f_bavail.saturating_mul(statvfs.f_frsize) >= minimum_free,
        )?;

        let mut created = self
            .create_file(OsStr::new(PROBE_NAMES[0]))
            .map_err(|_| Cap::Writable)?;
        check(Cap::Writable, created.write_all(b"probe").is_ok())?;
        let second = self.create_file(OsStr::new(PROBE_NAMES[1])).is_ok();
        check(
            Cap::AtomicRename,
            second && renameat(&self.0, PROBE_NAMES[1], &self.0, PROBE_NAMES[0]).is_ok(),
        )?;

        let locked = File::try_lock(&created).is_ok();
        let _ = File::unlock(&created);
        check(Cap::Locking, locked)?;

        check(
            Cap::SymbolicLinks,
            symlinkat("target", &self.0, PROBE_NAMES[3]).is_ok(),
        )?;

        let lower = self.create_file(OsStr::new(PROBE_NAMES[4])).is_ok();
        let upper_absent = matches!(
            statat(&self.0, PROBE_NAMES[5], AtFlags::SYMLINK_NOFOLLOW),
            Err(rustix::io::Errno::NOENT)
        );
        check(Cap::CaseSensitive, lower && upper_absent)
    }
}

/// The steps of an atomic write or removal, for crash-point tests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AtomicMutation {
    /// The temporary file holds the new bytes and is on disk.
    Prepared,
    /// The temporary file was renamed over the target.
    Published,
    /// The target was unlinked.
    Removed,
    /// The directory entry change is on disk.
    DirectorySynced,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AtomicIntentKind {
    Publication,
    Removal,
}

/// Durable identity of a Launcher-owned file.
///
/// Persisted identities bind the filesystem id reported by `fstatfs` with the
/// inode number. `st_dev` is not persisted: btrfs subvolumes, NFS, and other
/// anonymous-device mounts receive a fresh device number at every mount, so a
/// record written before a reboot would otherwise disown every live slot.
/// `f_fsid` is derived from the filesystem UUID (and, on btrfs, the subvolume
/// root) and therefore survives remounts while still distinguishing a file
/// substituted from another filesystem. Birth time is deliberately not part
/// of the identity: `statx` reports it only on some filesystems, kernels, and
/// sandboxes, so persisting it would add an environment-dependent
/// component that could disown live slots. Inode reuse still requires
/// removing the owned file, which the recorded length and digest checks detect.
///
/// `LegacyDevice` is only ever produced by decoding a schema-2 record and it
/// never matches a live file. Such values must first be rebound through
/// [`FileIdentity::migrate_legacy`], and the variant serializes exactly as
/// schema 2 did so checksums over legacy payloads remain verifiable.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub(crate) enum FileIdentity {
    Filesystem { filesystem: u64, inode: u64 },
    LegacyDevice { device: u64, inode: u64 },
}

/// Current identities compare by [`same_recorded_file`]: on the Android host
/// the filesystem id is not compared.
impl PartialEq for FileIdentity {
    fn eq(&self, other: &Self) -> bool {
        match (*self, *other) {
            (
                Self::Filesystem { filesystem, inode },
                Self::Filesystem {
                    filesystem: other_filesystem,
                    inode: other_inode,
                },
            ) => same_recorded_file(filesystem, inode, other_filesystem, other_inode),
            (
                Self::LegacyDevice { device, inode },
                Self::LegacyDevice {
                    device: other_device,
                    inode: other_inode,
                },
            ) => device == other_device && inode == other_inode,
            _ => false,
        }
    }
}

impl Eq for FileIdentity {}

impl FileIdentity {
    pub(crate) fn of(file: &File) -> io::Result<Self> {
        Ok(Self::Filesystem {
            filesystem: filesystem_id(file)?,
            inode: file.metadata()?.ino(),
        })
    }

    pub(crate) fn matches(self, file: &File) -> io::Result<bool> {
        Ok(self == Self::of(file)?)
    }

    const fn is_legacy(self) -> bool {
        matches!(self, Self::LegacyDevice { .. })
    }

    /// Rebinds schema-2 `{device, inode}` identities onto `filesystem`, the
    /// filesystem id of the directory that owns the persisted record.
    ///
    /// The recorded device number is meaningless after a remount, so it is
    /// never compared with the live `st_dev`. It is still required to be one
    /// value across the whole record: every identity in one record names a
    /// file of one namespace, and a record mixing devices was not written by
    /// the Launcher. The rebound identity is then held to the strict current
    /// rule, so a live file is accepted only with the same inode on the same
    /// filesystem as the owning namespace. A different inode, or a file
    /// mounted in from another filesystem, still fails closed, except for a
    /// copied idle namespace (`copied_namespace_record`). The caller has
    /// already verified the record checksum over its legacy encoding. A
    /// record mixing legacy and current identities was never written by any
    /// Launcher schema and is rejected as well.
    ///
    /// Returns whether any identity was rebound.
    pub(crate) fn migrate_legacy<'a>(
        identities: impl IntoIterator<Item = &'a mut Self>,
        filesystem: u64,
    ) -> io::Result<bool> {
        let identities = identities.into_iter().collect::<Vec<_>>();
        let legacy = identities
            .iter()
            .filter(|identity| identity.is_legacy())
            .count();
        if legacy == 0 {
            return Ok(false);
        }
        if legacy != identities.len() {
            return Err(identity_error(
                "identity record mixed legacy and current forms",
            ));
        }
        let mut recorded_device = None;
        for identity in identities {
            if let Self::LegacyDevice { device, inode } = *identity {
                if *recorded_device.get_or_insert(device) != device {
                    return Err(identity_error(
                        "legacy identity record mixed device numbers",
                    ));
                }
                *identity = Self::Filesystem { filesystem, inode };
            }
        }
        Ok(true)
    }

    #[cfg(test)]
    pub(crate) const fn for_test(filesystem: u64, inode: u64) -> Self {
        Self::Filesystem { filesystem, inode }
    }
}

/// File system id (`f_fsid`) of an open descriptor. Stable across reboots on
/// most file systems (btrfs, ext4: from the UUID), but f2fs, Android's data
/// partition, makes it from the device number, which a reboot or a system
/// update can change.
pub(crate) fn filesystem_id(file: &File) -> io::Result<u64> {
    Ok(rustix::fs::fstatvfs(file)?.f_fsid)
}

fn open_rw(directory: &Directory, name: &OsStr) -> io::Result<File> {
    let file = directory.open_beneath(Path::new(name), OFlags::RDWR)?;
    validate_regular(&file)?;
    Ok(file)
}

fn validate_relative(path: &Path) -> io::Result<()> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "descriptor-relative path was invalid",
        ));
    }
    Ok(())
}

fn absolute_relative(path: &Path) -> io::Result<&Path> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "managed directory path was not absolute",
        ));
    }
    let relative = path.strip_prefix("/").map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "managed directory path could not be anchored at root",
        )
    })?;
    if !relative.as_os_str().is_empty() {
        validate_relative(relative)?;
    }
    Ok(relative)
}

fn validate_name(name: &OsStr) -> io::Result<()> {
    let path = Path::new(name);
    validate_relative(path)?;
    if path.components().count() != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "descriptor-relative name was not flat",
        ));
    }
    Ok(())
}

fn validate_directory(directory: &File) -> io::Result<()> {
    let metadata = directory.metadata()?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "opened object was not a directory",
        ));
    }
    Ok(())
}

fn validate_regular(file: &File) -> io::Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "opened object was not a unique regular file",
        ));
    }
    Ok(())
}

fn same_identity(left: &File, right: &File) -> io::Result<bool> {
    let left = left.metadata()?;
    let right = right.metadata()?;
    Ok(left.dev() == right.dev() && left.ino() == right.ino())
}

fn identity_error(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// Prefix of the temporary file an atomic write renames into place.
const ATOMIC_TEMPORARY_PREFIX: &str = ".pd2launcher-write-";

/// The 32-hex-digit tag of `target` in its temporary file's name: the first
/// 16 bytes of its name's SHA-256, a fixed length whatever the name's.
fn target_tag(target: &OsStr) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(target.as_bytes())[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// `.pd2launcher-write-<target tag>-<random>`: residue is attributed to its
/// target without a record.
fn atomic_temporary_name(target: &OsStr) -> io::Result<OsString> {
    let mut random = [0_u8; 8];
    if rustix::rand::getrandom(&mut random, rustix::rand::GetRandomFlags::empty())
        .map_err(io::Error::from)?
        != random.len()
    {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "too few random bytes",
        ));
    }
    let token = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(OsString::from(format!(
        "{ATOMIC_TEMPORARY_PREFIX}{}-{token}",
        target_tag(target)
    )))
}

/// The target tag of an atomic write's temporary file, or `None` for any
/// other name.
fn atomic_temporary_tag(name: &OsStr) -> Option<String> {
    let rest = name.to_str()?.strip_prefix(ATOMIC_TEMPORARY_PREFIX)?;
    let (tag, token) = rest.split_once('-')?;
    let hex = |part: &str, length| {
        part.len() == length && part.bytes().all(|byte| byte.is_ascii_hexdigit())
    };
    (hex(tag, 32) && hex(token, 16)).then(|| tag.to_owned())
}

/// Whether `name` is the Launcher's own scratch in a directory it owns: an
/// atomic write's temporary file, or the retained folder earlier versions
/// kept.
pub(crate) fn is_launcher_scratch(name: &OsStr) -> bool {
    name == OsStr::new(RETAINED_DIRECTORY) || atomic_temporary_tag(name).is_some()
}

#[cfg(test)]
fn unique(suffix: &str) -> io::Result<OsString> {
    let mut random = [0_u8; 16];
    let mut filled = 0;
    while filled < random.len() {
        let read =
            getrandom(&mut random[filled..], GetRandomFlags::empty()).map_err(io::Error::from)?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "kernel returned no random bytes for retained object name",
            ));
        }
        filled += read;
    }
    let mut encoded = String::with_capacity(random.len() * 2);
    for byte in random {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    Ok(OsString::from(format!(".pd2launcher-{encoded}.{suffix}")))
}

#[cfg(test)]
fn copy_directory(source: &Directory, destination: &Directory) -> io::Result<()> {
    for name in source.names()? {
        match source.open_directory(Path::new(&name)) {
            Ok(source_child) => {
                mkdirat(&destination.0, &name, Mode::RUSR | Mode::WUSR | Mode::XUSR)
                    .map_err(io::Error::from)?;
                let destination_child = destination.open_directory(Path::new(&name))?;
                copy_directory(&source_child, &destination_child)?;
                destination.require_named_directory(&name, &destination_child)?;
            }
            Err(directory_error) => match source.open_file(Path::new(&name)) {
                Ok(source_file) => {
                    let prepared = destination.prepare_copy(&source_file)?;
                    destination.link_prepared(&prepared, &name)?;
                }
                Err(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "directory entry was not a safe file or directory: {directory_error}"
                        ),
                    ));
                }
            },
        }
    }
    destination.sync()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_working_openat2_is_not_mistaken_for_a_filtered_one() {
        // Where openat2 works, an EPERM is a real permission error and is
        // returned as one, never turned into the fallback.
        if openat2(
            rustix::fs::CWD,
            ".",
            OFlags::PATH | OFlags::DIRECTORY,
            Mode::empty(),
            ResolveFlags::BENEATH,
        )
        .is_ok()
        {
            assert!(!openat2_filtered());
        }
    }

    #[test]
    fn an_atomic_write_is_the_old_file_or_the_new_one_at_every_cut() {
        use std::os::unix::fs::PermissionsExt;
        for cut in [
            AtomicMutation::Prepared,
            AtomicMutation::Published,
            AtomicMutation::DirectorySynced,
        ] {
            let fixture = tempfile::tempdir().unwrap();
            let directory = Directory::open(fixture.path()).unwrap();
            directory
                .atomic_write(OsStr::new("state.json"), b"old")
                .unwrap();
            let stopped = directory.atomic_write_recoverable(
                OsStr::new("state.json"),
                b"new",
                &mut |event| {
                    if event == cut {
                        Err(io::Error::other("crash"))
                    } else {
                        Ok(())
                    }
                },
            );
            assert!(stopped.is_err(), "{cut:?} was not reached");
            let bytes = fs::read(fixture.path().join("state.json")).unwrap();
            assert_eq!(
                bytes,
                if cut == AtomicMutation::Prepared {
                    b"old".as_slice()
                } else {
                    b"new".as_slice()
                },
                "{cut:?}"
            );
            // A crash before the rename leaves residue, attributed to its
            // target and hidden from listings; recovery removes it.
            let residue = cut == AtomicMutation::Prepared;
            let state = [(OsStr::new("state.json"), AtomicIntentKind::Publication)];
            let other = [(OsStr::new("other.json"), AtomicIntentKind::Publication)];
            assert_eq!(
                directory.has_pending_atomic_for_names(&state).unwrap(),
                residue
            );
            assert!(!directory.has_pending_atomic_for_names(&other).unwrap());
            assert_eq!(
                directory.pending_atomic_intent(0).unwrap(),
                if residue {
                    AtomicIntentSnapshot::RecoverableResidue
                } else {
                    AtomicIntentSnapshot::Clear
                }
            );
            assert_eq!(directory.names().unwrap(), [OsString::from("state.json")]);
            directory.recover_atomic().unwrap();
            assert_eq!(fs::read_dir(fixture.path()).unwrap().count(), 1);
            assert_eq!(
                directory.pending_atomic_intent(0).unwrap(),
                AtomicIntentSnapshot::Clear
            );
            let mode = fs::metadata(fixture.path().join("state.json"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0, "atomic writes are private");
        }
    }

    #[test]
    fn a_publication_matches_only_the_file_currently_named() {
        let fixture = tempfile::tempdir().unwrap();
        let directory = Directory::open(fixture.path()).unwrap();
        directory.atomic_write(OsStr::new("a"), b"one").unwrap();
        let first = directory.open_file(Path::new("a")).unwrap();
        assert!(
            directory
                .atomic_publication_matches(OsStr::new("a"), &first)
                .unwrap()
        );
        directory.atomic_write(OsStr::new("a"), b"two").unwrap();
        assert!(
            !directory
                .atomic_publication_matches(OsStr::new("a"), &first)
                .unwrap()
        );
        assert!(
            !directory
                .atomic_publication_matches(OsStr::new("missing"), &first)
                .unwrap()
        );
    }

    #[test]
    fn temporary_names_carry_their_target_and_nothing_else_looks_like_one() {
        let name = atomic_temporary_name(OsStr::new("journal.json")).unwrap();
        assert_eq!(
            atomic_temporary_tag(&name),
            Some(target_tag(OsStr::new("journal.json")))
        );
        // Any name a filesystem allows can be written atomically.
        let long = OsString::from("n".repeat(255));
        assert!(atomic_temporary_name(&long).unwrap().len() <= 255);
        assert!(is_launcher_scratch(&name));
        assert!(is_launcher_scratch(OsStr::new(RETAINED_DIRECTORY)));
        for other in [
            "journal.json",
            ".pd2launcher-write-",
            ".pd2launcher-write-zz-0123456789abcdef",
            ".pd2launcher-write-0123456789abcdef0123456789abcdef-short",
        ] {
            assert!(!is_launcher_scratch(OsStr::new(other)), "{other}");
        }
    }

    #[test]
    fn the_openat2_fallback_resolves_beneath_without_following_links() {
        let fixture = tempfile::tempdir().unwrap();
        let base = fixture.path();
        std::fs::create_dir_all(base.join("a/b")).unwrap();
        std::fs::write(base.join("a/b/file"), b"x").unwrap();
        std::os::unix::fs::symlink(base.join("a/b"), base.join("a/link-dir")).unwrap();
        std::os::unix::fs::symlink(base.join("a/b/file"), base.join("a/b/link-file")).unwrap();
        let root = File::open(base).unwrap();
        let open = |path: &str, flags| {
            openat_beneath_by_component(&root, Path::new(path), flags | OFlags::NOFOLLOW)
        };
        let mut text = String::new();
        File::from(open("a/b/file", OFlags::RDONLY).unwrap())
            .read_to_string(&mut text)
            .unwrap();
        assert_eq!(text, "x");
        assert!(open("a/b", OFlags::RDONLY | OFlags::DIRECTORY).is_ok());
        assert!(open("a/b/file", OFlags::PATH).is_ok());
        for linked in ["a/link-dir/file", "a/b/link-file", "a/link-dir"] {
            assert_eq!(
                open(linked, OFlags::RDONLY).err(),
                Some(rustix::io::Errno::LOOP),
                "{linked}"
            );
        }
        assert_eq!(
            open("a/link-dir/file", OFlags::PATH).err(),
            Some(rustix::io::Errno::LOOP)
        );
        // `O_PATH | O_NOFOLLOW` opens a last-component link as the link;
        // without `O_NOFOLLOW` it is refused.
        assert!(open("a/b/link-file", OFlags::PATH).is_ok());
        let follow = |path: &str, flags| openat_beneath_by_component(&root, Path::new(path), flags);
        assert_eq!(
            follow("a/b/link-file", OFlags::PATH).err(),
            Some(rustix::io::Errno::LOOP)
        );
        assert_eq!(
            follow("a/link-dir", OFlags::RDONLY | OFlags::DIRECTORY).err(),
            Some(rustix::io::Errno::LOOP)
        );
        assert_eq!(
            open("a/missing/file", OFlags::RDONLY).err(),
            Some(rustix::io::Errno::NOENT)
        );
        assert_eq!(
            open("a/b/file/below", OFlags::RDONLY).err(),
            Some(rustix::io::Errno::NOTDIR)
        );
        assert_eq!(
            open("../escape", OFlags::RDONLY).err(),
            Some(rustix::io::Errno::XDEV)
        );
        // openat2 itself agrees wherever it is available.
        for (path, flags) in [
            ("a/b/file", OFlags::RDONLY),
            ("a/link-dir/file", OFlags::RDONLY),
            ("a/b/link-file", OFlags::PATH),
            ("a/missing/file", OFlags::RDONLY),
            ("a/b/file/below", OFlags::RDONLY),
        ] {
            if let Ok(native) = std::panic::catch_unwind(|| {
                openat2(
                    &root,
                    path,
                    flags | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                    ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS,
                )
                .err()
            }) && native != Some(rustix::io::Errno::NOSYS)
            {
                assert_eq!(open(path, flags).err(), native, "{path}");
            }
        }
    }

    fn copy_tree(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_tree(&entry.path(), &target);
            } else {
                fs::copy(entry.path(), &target).unwrap();
            }
        }
    }

    #[test]
    fn a_copied_idle_namespace_is_rebound_and_keeps_its_unchanged_publication() {
        let fixture = tempfile::tempdir().unwrap();
        let original = fixture.path().join("original");
        fs::create_dir(&original).unwrap();
        Directory::open(&original)
            .unwrap()
            .atomic_write(OsStr::new("record.json"), b"one")
            .unwrap();
        // Copied, every file has a new identity (here a new inode).
        let copy = fixture.path().join("copy");
        copy_tree(&original, &copy);
        let directory = Directory::open(&copy).unwrap();
        let record = directory.open_file(Path::new("record.json")).unwrap();
        assert!(
            directory
                .atomic_publication_matches(OsStr::new("record.json"), &record)
                .unwrap()
        );
        directory
            .atomic_write(OsStr::new("record.json"), b"two")
            .unwrap();
        assert_eq!(fs::read(copy.join("record.json")).unwrap(), b"two");
        // The original is untouched and still works on its own.
        Directory::open(&original)
            .unwrap()
            .atomic_write(OsStr::new("record.json"), b"three")
            .unwrap();
    }

    use std::{cell::Cell, fs, io::Read};
    use tempfile::TempDir;

    struct FailAt(RootCapability);

    impl ProbeFaults for FailAt {
        fn fail(&self, capability: RootCapability) -> bool {
            capability == self.0
        }
    }

    #[test]
    fn a_capable_root_passes_the_probe_and_any_failed_capability_is_named_and_cleaned_up() {
        let fixture = tempfile::tempdir().unwrap();
        let root = Directory::open(fixture.path()).unwrap();
        root.probe_root_capabilities(0, &NoProbeFaults)
            .expect("the test filesystem provides every capability");
        assert_eq!(fs::read_dir(fixture.path()).unwrap().count(), 0);
        for capability in [
            RootCapability::Executable,
            RootCapability::FreeSpace,
            RootCapability::Writable,
            RootCapability::AtomicRename,
            RootCapability::Locking,
            RootCapability::SymbolicLinks,
            RootCapability::CaseSensitive,
        ] {
            assert_eq!(
                root.probe_root_capabilities(0, &FailAt(capability)),
                Err(capability)
            );
            assert_eq!(
                fs::read_dir(fixture.path()).unwrap().count(),
                0,
                "{capability:?} left a probe entry"
            );
        }
        assert_eq!(
            root.probe_root_capabilities(u64::MAX, &NoProbeFaults),
            Err(RootCapability::FreeSpace)
        );
    }

    #[test]
    fn retained_parent_descriptor_prevents_ancestor_swap_from_redirecting_writes() {
        let fixture = TempDir::new().unwrap();
        let managed = fixture.path().join("managed");
        let outside = fixture.path().join("outside");
        fs::create_dir(&managed).unwrap();
        fs::create_dir(&outside).unwrap();
        let retained = Directory::open(&managed).unwrap();
        fs::rename(&managed, fixture.path().join("managed-away")).unwrap();
        std::os::unix::fs::symlink(&outside, &managed).unwrap();

        retained
            .atomic_write(OsStr::new("state.json"), b"safe")
            .unwrap();

        assert_eq!(
            fs::read(fixture.path().join("managed-away/state.json")).unwrap(),
            b"safe"
        );
        assert!(fs::read_dir(outside).unwrap().next().is_none());
    }

    #[test]
    fn every_atomic_replacement_cut_recovers_without_publishing_empty_state() {
        for surface in [
            "settings.json",
            "diagnostics.json",
            "journal.json",
            "jobs.json",
        ] {
            let observed = atomic_mutations_for_existing(surface);
            assert!(!observed.is_empty());
            for fail_at in 0..observed.len() {
                let fixture = TempDir::new().unwrap();
                let state = fixture.path().join(surface);
                let directory = Directory::open(fixture.path()).unwrap();
                directory.atomic_write(OsStr::new(surface), b"old").unwrap();
                let mut mutation = 0_usize;
                let error = directory.atomic_write_recoverable(
                    OsStr::new(surface),
                    b"replacement",
                    &mut |_| {
                        if state.exists() {
                            assert!(!fs::read(&state).unwrap().is_empty());
                        }
                        let current = mutation;
                        mutation += 1;
                        if current == fail_at {
                            Err(io::Error::other("injected process stop"))
                        } else {
                            Ok(())
                        }
                    },
                );
                assert!(
                    error.is_err(),
                    "cut {fail_at} was not reached for {surface}"
                );
                drop(directory);

                let reconstructed = Directory::open(fixture.path()).unwrap();
                reconstructed
                    .atomic_write(OsStr::new(surface), b"replacement")
                    .unwrap();
                let metadata = fs::metadata(&state).unwrap();
                assert_eq!(fs::read(&state).unwrap(), b"replacement");
                assert_eq!(metadata.nlink(), 1);
            }
        }
    }

    #[test]
    fn every_first_publication_cut_recovers_from_the_durable_prepared_slot() {
        for surface in [
            "settings.json",
            "diagnostics.json",
            "journal.json",
            "jobs.json",
        ] {
            let observed = atomic_mutations_for_missing(surface);
            assert!(!observed.is_empty());
            for fail_at in 0..observed.len() {
                let fixture = TempDir::new().unwrap();
                let state = fixture.path().join(surface);
                let directory = Directory::open(fixture.path()).unwrap();
                directory
                    .atomic_write(OsStr::new("bootstrap"), b"owned")
                    .unwrap();
                let mut mutation = 0_usize;
                let error = directory.atomic_write_recoverable(
                    OsStr::new(surface),
                    b"first publication",
                    &mut |_| {
                        if state.exists() {
                            assert!(!fs::read(&state).unwrap().is_empty());
                        }
                        let current = mutation;
                        mutation += 1;
                        if current == fail_at {
                            Err(io::Error::other("injected process stop"))
                        } else {
                            Ok(())
                        }
                    },
                );
                assert!(
                    error.is_err(),
                    "cut {fail_at} was not reached for {surface}"
                );
                drop(directory);

                let reconstructed = Directory::open(fixture.path()).unwrap();
                reconstructed
                    .atomic_write(OsStr::new(surface), b"first publication")
                    .unwrap();
                let metadata = fs::metadata(&state).unwrap();
                assert_eq!(fs::read(&state).unwrap(), b"first publication");
                assert_eq!(metadata.nlink(), 1);
            }
        }
    }

    fn atomic_mutations_for_existing(surface: &str) -> Vec<AtomicMutation> {
        let fixture = TempDir::new().unwrap();
        let directory = Directory::open(fixture.path()).unwrap();
        directory.atomic_write(OsStr::new(surface), b"old").unwrap();
        let mut observed = Vec::new();
        directory
            .atomic_write_recoverable(OsStr::new(surface), b"replacement", &mut |mutation| {
                observed.push(mutation);
                Ok(())
            })
            .unwrap();
        observed
    }

    fn atomic_mutations_for_missing(surface: &str) -> Vec<AtomicMutation> {
        let fixture = TempDir::new().unwrap();
        let directory = Directory::open(fixture.path()).unwrap();
        directory
            .atomic_write(OsStr::new("bootstrap"), b"owned")
            .unwrap();
        let mut observed = Vec::new();
        directory
            .atomic_write_recoverable(OsStr::new(surface), b"first publication", &mut |mutation| {
                observed.push(mutation);
                Ok(())
            })
            .unwrap();
        observed
    }

    fn open_publication_mutations() -> Vec<AtomicMutation> {
        let fixture = TempDir::new().unwrap();
        fs::write(fixture.path().join("backup-slot"), []).unwrap();
        fs::write(fixture.path().join("source"), b"previous bytes").unwrap();
        let directory = Directory::open(fixture.path()).unwrap();
        let source = directory.open_file(Path::new("source")).unwrap();
        let mut observed = Vec::new();
        directory
            .atomic_write_from_open_recoverable(
                OsStr::new("backup-slot"),
                &source,
                &mut |mutation| {
                    observed.push(mutation);
                    Ok(())
                },
            )
            .unwrap();
        observed
    }

    #[test]
    fn every_open_backup_publication_cut_proves_or_recovers_exact_identity() {
        let observed = open_publication_mutations();
        for fail_at in 0..observed.len() {
            let fixture = TempDir::new().unwrap();
            fs::write(fixture.path().join("backup-slot"), []).unwrap();
            fs::write(fixture.path().join("source"), b"previous bytes").unwrap();
            let directory = Directory::open(fixture.path()).unwrap();
            let source = directory.open_file(Path::new("source")).unwrap();
            let mut mutation = 0_usize;
            let stopped = directory.atomic_write_from_open_recoverable(
                OsStr::new("backup-slot"),
                &source,
                &mut |_| {
                    let current = mutation;
                    mutation += 1;
                    if current == fail_at {
                        Err(io::Error::other("injected backup publication stop"))
                    } else {
                        Ok(())
                    }
                },
            );
            assert!(stopped.is_err(), "backup cut {fail_at} was not reached");

            if fs::read(fixture.path().join("backup-slot")).unwrap() == b"previous bytes" {
                let published = directory.open_file(Path::new("backup-slot")).unwrap();
                assert!(
                    directory
                        .atomic_publication_matches(OsStr::new("backup-slot"), &published)
                        .unwrap(),
                    "published backup cut {fail_at} lacked durable identity proof"
                );
            }
            directory
                .atomic_write_from_open(OsStr::new("backup-slot"), &source)
                .unwrap();
            assert_eq!(
                fs::read(fixture.path().join("backup-slot")).unwrap(),
                b"previous bytes"
            );
        }
    }

    #[test]
    fn absolute_open_and_creation_reject_symlink_ancestors() {
        let fixture = TempDir::new().unwrap();
        let outside = fixture.path().join("outside");
        fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, fixture.path().join("redirect")).unwrap();

        assert!(Directory::open(&fixture.path().join("redirect")).is_err());
        assert!(
            Directory::ensure_absolute(&fixture.path().join("redirect/managed/runtime")).is_err()
        );
        assert!(fs::read_dir(outside).unwrap().next().is_none());
    }

    #[test]
    fn a_copy_with_times_keeps_the_sources_times_and_reads_a_file_with_other_names() {
        use std::time::{Duration, UNIX_EPOCH};
        let fixture = TempDir::new().unwrap();
        fs::create_dir(fixture.path().join("into")).unwrap();
        fs::write(fixture.path().join("Hero.d2s"), b"hero").unwrap();
        // A second name: the source is only read, so it may have one.
        fs::hard_link(
            fixture.path().join("Hero.d2s"),
            fixture.path().join("Hero.link"),
        )
        .unwrap();
        let from = Directory::open(fixture.path()).unwrap();
        let into = Directory::open(&fixture.path().join("into")).unwrap();
        let source = from
            .open_beneath(Path::new("Hero.d2s"), OFlags::RDONLY)
            .unwrap();
        assert!(
            into.atomic_write_from_open(OsStr::new("Hero.d2s"), &source)
                .is_err(),
            "the plain copy still wants a unique file"
        );
        let accessed = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let modified = UNIX_EPOCH + Duration::from_secs(1_600_000_000);
        into.atomic_copy_with_times(OsStr::new("Hero.d2s"), &source, accessed, modified)
            .unwrap();
        let copy = fs::metadata(fixture.path().join("into/Hero.d2s")).unwrap();
        assert_eq!(
            fs::read(fixture.path().join("into/Hero.d2s")).unwrap(),
            b"hero"
        );
        assert_eq!(copy.modified().unwrap(), modified);
        assert_eq!(copy.accessed().unwrap(), accessed);
        assert_eq!(copy.nlink(), 1, "the copy is a file of its own");
        assert_eq!(
            fs::read_dir(fixture.path().join("into")).unwrap().count(),
            1,
            "no temporary file is left"
        );
    }

    #[test]
    fn retained_file_collision_never_replaces_unknown_object() {
        let fixture = TempDir::new().unwrap();
        fs::write(fixture.path().join("payload"), b"verified").unwrap();
        let directory = Directory::open(fixture.path()).unwrap();
        let opened = directory.open_file(Path::new("payload")).unwrap();
        let collision = Cell::new(true);

        directory
            .retain_open_file_inner(
                OsStr::new("payload"),
                &opened,
                "collision",
                |retained, candidate| {
                    if collision.replace(false) {
                        retained.create_file(candidate)?.write_all(b"unknown")?;
                    }
                    Ok(())
                },
                |_, _, _| Ok(()),
            )
            .unwrap();

        let retained = directory.retained_directory().unwrap();
        let mut contents = retained
            .names()
            .unwrap()
            .into_iter()
            .map(|name| fs::read(fixture.path().join(RETAINED_DIRECTORY).join(name)).unwrap())
            .collect::<Vec<_>>();
        contents.sort();
        assert_eq!(contents, vec![b"unknown".to_vec(), b"verified".to_vec()]);
    }

    #[test]
    fn retained_directory_collision_never_replaces_unknown_object() {
        let fixture = TempDir::new().unwrap();
        fs::create_dir(fixture.path().join("tree")).unwrap();
        fs::write(fixture.path().join("tree/verified"), b"verified").unwrap();
        let directory = Directory::open(fixture.path()).unwrap();
        let opened = directory.open_directory(Path::new("tree")).unwrap();
        let collision = Cell::new(true);

        directory
            .retain_open_directory_inner(
                OsStr::new("tree"),
                &opened,
                "collision",
                |retained, candidate| {
                    if collision.replace(false) {
                        let unknown = retained.ensure_directory(Path::new(candidate))?;
                        unknown.create_file(OsStr::new("unknown"))?;
                    }
                    Ok(())
                },
                |_, _| Ok(()),
            )
            .unwrap();

        let retained_path = fixture.path().join(RETAINED_DIRECTORY);
        let entries = fs::read_dir(retained_path)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(entries.len(), 2);
        assert!(
            entries
                .iter()
                .any(|entry| entry.path().join("unknown").exists())
        );
        assert!(
            entries
                .iter()
                .any(|entry| entry.path().join("verified").exists())
        );
    }

    #[test]
    fn post_verification_file_substitution_is_preserved_not_deleted() {
        let fixture = TempDir::new().unwrap();
        fs::write(fixture.path().join("payload"), b"verified").unwrap();
        let directory = Directory::open(fixture.path()).unwrap();
        let opened = directory.open_file(Path::new("payload")).unwrap();
        let mut attacker_name = OsString::new();

        directory
            .retain_open_file_inner(
                OsStr::new("payload"),
                &opened,
                "post-check",
                |_, _| Ok(()),
                |retained, candidate, verified| {
                    attacker_name = candidate.to_owned();
                    renameat_with(
                        &retained.0,
                        candidate,
                        &retained.0,
                        "verified-away",
                        RenameFlags::NOREPLACE,
                    )
                    .map_err(io::Error::from)?;
                    retained.create_file(candidate)?.write_all(b"attacker")?;
                    verified.set_len(0)?;
                    verified.sync_all()
                },
            )
            .unwrap();

        let retained_path = fixture.path().join(RETAINED_DIRECTORY);
        assert_eq!(fs::read(retained_path.join("verified-away")).unwrap(), b"");
        assert_eq!(
            fs::read(retained_path.join(attacker_name)).unwrap(),
            b"attacker"
        );
    }

    #[test]
    fn post_verification_directory_substitution_is_preserved_not_deleted() {
        let fixture = TempDir::new().unwrap();
        fs::create_dir(fixture.path().join("tree")).unwrap();
        fs::write(fixture.path().join("tree/verified"), b"verified").unwrap();
        let directory = Directory::open(fixture.path()).unwrap();
        let opened = directory.open_directory(Path::new("tree")).unwrap();
        let mut attacker_name = OsString::new();

        directory
            .retain_open_directory_inner(
                OsStr::new("tree"),
                &opened,
                "post-check",
                |_, _| Ok(()),
                |retained, candidate| {
                    attacker_name = candidate.to_owned();
                    renameat_with(
                        &retained.0,
                        candidate,
                        &retained.0,
                        "verified-away",
                        RenameFlags::NOREPLACE,
                    )
                    .map_err(io::Error::from)?;
                    let attacker = retained.ensure_directory(Path::new(candidate))?;
                    attacker.create_file(OsStr::new("attacker"))?;
                    Ok(())
                },
            )
            .unwrap();

        let retained_path = fixture.path().join(RETAINED_DIRECTORY);
        assert!(retained_path.join("verified-away/verified").exists());
        assert!(retained_path.join(attacker_name).join("attacker").exists());
    }

    #[test]
    fn post_validation_source_swap_never_promotes_attacker_bytes() {
        let fixture = TempDir::new().unwrap();
        let staging = fixture.path().join("staging");
        let managed = fixture.path().join("managed");
        fs::create_dir(&staging).unwrap();
        fs::create_dir(&managed).unwrap();
        fs::write(staging.join("Game.exe"), b"verified").unwrap();
        let source = Directory::open(&staging).unwrap();
        let destination = Directory::open(&managed).unwrap();
        let opened = source.open_file(Path::new("Game.exe")).unwrap();

        assert!(
            source
                .rename_open_file_to_inner(
                    OsStr::new("Game.exe"),
                    &opened,
                    &destination,
                    OsStr::new("Game.exe"),
                    || {
                        fs::rename(staging.join("Game.exe"), staging.join("verified-away"))?;
                        fs::write(staging.join("Game.exe"), b"attacker")
                    },
                )
                .is_err()
        );
        assert_eq!(fs::read(managed.join("Game.exe")).unwrap(), b"verified");
        assert_eq!(fs::read(staging.join("Game.exe")).unwrap(), b"attacker");
    }

    #[test]
    fn post_validation_directory_swap_never_materializes_attacker_tree() {
        let fixture = TempDir::new().unwrap();
        let staging = fixture.path().join("staging");
        let managed = fixture.path().join("managed");
        fs::create_dir(&staging).unwrap();
        fs::create_dir(&managed).unwrap();
        fs::create_dir(staging.join("runtime")).unwrap();
        fs::write(staging.join("runtime/verified"), b"verified").unwrap();
        let source = Directory::open(&staging).unwrap();
        let destination = Directory::open(&managed).unwrap();
        let opened = source.open_directory(Path::new("runtime")).unwrap();

        assert!(
            source
                .rename_open_directory_to_inner(
                    OsStr::new("runtime"),
                    &opened,
                    &destination,
                    OsStr::new("runtime"),
                    || {
                        fs::rename(staging.join("runtime"), staging.join("verified-away"))?;
                        fs::create_dir(staging.join("runtime"))?;
                        fs::write(staging.join("runtime/attacker"), b"attacker")
                    },
                )
                .is_err()
        );
        assert_eq!(
            fs::read(managed.join("runtime/verified")).unwrap(),
            b"verified"
        );
        assert!(!managed.join("runtime/attacker").exists());
        assert!(staging.join("runtime/attacker").exists());
    }

    #[test]
    fn retained_parent_descriptor_prevents_recursive_prune_from_deleting_outside() {
        let fixture = TempDir::new().unwrap();
        let managed = fixture.path().join("managed");
        let outside = fixture.path().join("outside");
        fs::create_dir_all(managed.join("staging/nested")).unwrap();
        fs::write(managed.join("staging/nested/file"), b"managed").unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep"), b"outside").unwrap();
        let retained = Directory::open(&managed).unwrap();
        fs::rename(&managed, fixture.path().join("managed-away")).unwrap();
        std::os::unix::fs::symlink(&outside, &managed).unwrap();

        retained.remove_tree(OsStr::new("staging")).unwrap();

        assert!(!fixture.path().join("managed-away/staging").exists());
        assert_eq!(fs::read(outside.join("keep")).unwrap(), b"outside");
    }

    #[test]
    fn retained_destination_descriptor_prevents_promotion_parent_swap() {
        let fixture = TempDir::new().unwrap();
        let staging = fixture.path().join("staging");
        let managed = fixture.path().join("managed");
        let outside = fixture.path().join("outside");
        fs::create_dir(&staging).unwrap();
        fs::create_dir(&managed).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(staging.join("Game.exe"), b"verified").unwrap();
        let source = Directory::open(&staging).unwrap();
        let destination = Directory::open(&managed).unwrap();
        let opened = source.open_file(Path::new("Game.exe")).unwrap();
        fs::rename(&managed, fixture.path().join("managed-away")).unwrap();
        std::os::unix::fs::symlink(&outside, &managed).unwrap();

        source
            .rename_open_file_to(
                OsStr::new("Game.exe"),
                &opened,
                &destination,
                OsStr::new("Game.exe"),
            )
            .unwrap();

        assert_eq!(
            fs::read(fixture.path().join("managed-away/Game.exe")).unwrap(),
            b"verified"
        );
        assert!(fs::read_dir(outside).unwrap().next().is_none());
    }

    #[test]
    fn bound_atomic_temp_rejects_name_replacement_without_overwriting_target() {
        let fixture = TempDir::new().unwrap();
        fs::write(fixture.path().join("state.json"), b"old").unwrap();
        let directory = Directory::open(fixture.path()).unwrap();
        let mut replacement_name = OsString::new();

        let error = directory
            .atomic_write_inner(OsStr::new("state.json"), b"new", |_, temporary| {
                replacement_name = temporary.to_owned();
                fs::rename(
                    fixture.path().join(temporary),
                    fixture.path().join("retained-away"),
                )?;
                fs::write(fixture.path().join(temporary), b"attacker")
            })
            .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(fs::read(fixture.path().join("state.json")).unwrap(), b"old");
        assert_eq!(
            fs::read(fixture.path().join("retained-away")).unwrap(),
            b"new"
        );
        assert_eq!(
            fs::read(fixture.path().join(replacement_name)).unwrap(),
            b"attacker"
        );
    }

    #[test]
    fn bound_atomic_target_rejects_replacement_without_deleting_unknown_bytes() {
        let fixture = TempDir::new().unwrap();
        fs::write(fixture.path().join("state.json"), b"old").unwrap();
        let directory = Directory::open(fixture.path()).unwrap();

        let error = directory
            .atomic_write_inner(OsStr::new("state.json"), b"new", |_, _| {
                fs::rename(
                    fixture.path().join("state.json"),
                    fixture.path().join("old-retained"),
                )?;
                fs::write(fixture.path().join("state.json"), b"attacker")
            })
            .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(
            fs::read(fixture.path().join("state.json")).unwrap(),
            b"attacker"
        );
        assert_eq!(
            fs::read(fixture.path().join("old-retained")).unwrap(),
            b"old"
        );
    }

    #[test]
    fn verified_source_name_replacement_is_never_promoted_or_deleted() {
        let fixture = TempDir::new().unwrap();
        let staging = fixture.path().join("staging");
        let managed = fixture.path().join("managed");
        fs::create_dir(&staging).unwrap();
        fs::create_dir(&managed).unwrap();
        fs::write(staging.join("Game.exe"), b"verified").unwrap();
        let source = Directory::open(&staging).unwrap();
        let destination = Directory::open(&managed).unwrap();
        let opened = source.open_file(Path::new("Game.exe")).unwrap();
        fs::rename(staging.join("Game.exe"), staging.join("verified-away")).unwrap();
        fs::write(staging.join("Game.exe"), b"attacker").unwrap();

        assert!(
            source
                .rename_open_file_to(
                    OsStr::new("Game.exe"),
                    &opened,
                    &destination,
                    OsStr::new("Game.exe"),
                )
                .is_err()
        );
        assert_eq!(fs::read(staging.join("Game.exe")).unwrap(), b"attacker");
        assert_eq!(
            fs::read(staging.join("verified-away")).unwrap(),
            b"verified"
        );
        assert!(!managed.join("Game.exe").exists());
    }

    #[test]
    fn retained_directory_name_replacement_is_never_promoted_or_pruned() {
        let fixture = TempDir::new().unwrap();
        let source_path = fixture.path().join("source");
        let destination_path = fixture.path().join("destination");
        fs::create_dir(&source_path).unwrap();
        fs::create_dir(&destination_path).unwrap();
        fs::create_dir(source_path.join("runtime")).unwrap();
        fs::write(source_path.join("runtime/verified"), b"verified").unwrap();
        let source = Directory::open(&source_path).unwrap();
        let destination = Directory::open(&destination_path).unwrap();
        let runtime = source.open_directory(Path::new("runtime")).unwrap();
        fs::rename(
            source_path.join("runtime"),
            source_path.join("retained-away"),
        )
        .unwrap();
        fs::create_dir(source_path.join("runtime")).unwrap();
        fs::write(source_path.join("runtime/attacker"), b"attacker").unwrap();

        assert!(
            source
                .rename_open_directory_to(
                    OsStr::new("runtime"),
                    &runtime,
                    &destination,
                    OsStr::new("runtime"),
                )
                .is_err()
        );
        assert!(
            source
                .remove_open_directory(OsStr::new("runtime"), &runtime)
                .is_err()
        );
        assert_eq!(
            fs::read(source_path.join("retained-away/verified")).unwrap(),
            b"verified"
        );
        assert_eq!(
            fs::read(source_path.join("runtime/attacker")).unwrap(),
            b"attacker"
        );
        assert!(fs::read_dir(destination_path).unwrap().next().is_none());
    }

    #[test]
    fn file_identity_binds_filesystem_and_inode_but_not_device_number() {
        let fixture = TempDir::new().unwrap();
        let first_path = fixture.path().join("first");
        fs::write(&first_path, b"one").unwrap();
        fs::write(fixture.path().join("second"), b"two").unwrap();
        let first = File::open(&first_path).unwrap();
        let second = File::open(fixture.path().join("second")).unwrap();
        let identity = FileIdentity::of(&first).unwrap();
        let metadata = first.metadata().unwrap();

        assert_eq!(
            identity,
            FileIdentity::Filesystem {
                filesystem: filesystem_id(&first).unwrap(),
                inode: metadata.ino(),
            }
        );
        assert!(identity.matches(&File::open(&first_path).unwrap()).unwrap());
        assert!(!identity.matches(&second).unwrap());
        let FileIdentity::Filesystem { filesystem, inode } = identity else {
            unreachable!()
        };
        assert!(
            !FileIdentity::Filesystem {
                filesystem: filesystem.wrapping_add(1),
                inode,
            }
            .matches(&first)
            .unwrap(),
            "the same inode number on another filesystem is a different file"
        );
        // Even the exact same-boot device number never lets a legacy value
        // stand in for a live identity; it must be migrated first.
        assert!(
            !FileIdentity::LegacyDevice {
                device: metadata.dev(),
                inode,
            }
            .matches(&first)
            .unwrap()
        );
        assert_eq!(
            serde_json::to_value(identity).unwrap(),
            serde_json::json!({ "filesystem": filesystem, "inode": inode })
        );
        assert_eq!(
            serde_json::from_str::<FileIdentity>(r#"{"device":53,"inode":7}"#).unwrap(),
            FileIdentity::LegacyDevice {
                device: 53,
                inode: 7
            }
        );
        assert_eq!(
            serde_json::to_string(&FileIdentity::LegacyDevice {
                device: 53,
                inode: 7
            })
            .unwrap(),
            r#"{"device":53,"inode":7}"#,
            "legacy identities re-encode byte-for-byte for checksum verification"
        );
    }

    #[test]
    fn legacy_identity_migration_rebinds_only_a_consistent_record() {
        let legacy = |device, inode| FileIdentity::LegacyDevice { device, inode };
        let mut identities = [legacy(53, 1), legacy(53, 2)];
        assert!(FileIdentity::migrate_legacy(&mut identities, 9).unwrap());
        assert_eq!(
            identities,
            [FileIdentity::for_test(9, 1), FileIdentity::for_test(9, 2)]
        );
        assert!(!FileIdentity::migrate_legacy(&mut identities, 7).unwrap());
        assert_eq!(identities[0], FileIdentity::for_test(9, 1));

        let mut mixed_devices = [legacy(53, 1), legacy(52, 2)];
        assert!(FileIdentity::migrate_legacy(&mut mixed_devices, 9).is_err());
        let mut mixed_forms = [legacy(53, 1), FileIdentity::for_test(9, 2)];
        assert!(FileIdentity::migrate_legacy(&mut mixed_forms, 9).is_err());
    }

    /// A directory that can be searched but not listed, as `/data` is for
    /// an Android app. Readable again when the guard is dropped.
    struct SearchOnly(PathBuf);

    impl SearchOnly {
        fn new(path: PathBuf) -> Self {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o111)).unwrap();
            Self(path)
        }
    }

    impl Drop for SearchOnly {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
        }
    }

    #[test]
    fn an_unreadable_ancestor_of_the_home_is_walked_search_only() {
        let fixture = tempfile::tempdir().unwrap();
        let ancestor = fixture.path().join("data");
        fs::create_dir_all(ancestor.join("app")).unwrap();
        let home = ancestor.join("app/launcher");
        let config = home.join("config");
        let _locked = SearchOnly::new(ancestor.clone());
        // Root reads every directory: nothing to show.
        if rustix::process::geteuid().is_root() {
            return;
        }
        let refused = Directory::ensure_absolute_with(&config, &[]).err().unwrap();
        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
        let config_directory =
            Directory::ensure_absolute_with(&config, &home_ancestors(&home)).unwrap();
        config_directory
            .atomic_write(OsStr::new("settings.json"), b"{}")
            .unwrap();
        assert_eq!(fs::read(config.join("settings.json")).unwrap(), b"{}");
    }

    #[test]
    fn an_unreadable_directory_that_is_not_an_ancestor_of_the_home_still_fails_closed() {
        let fixture = tempfile::tempdir().unwrap();
        let home = fixture.path().join("data/app/launcher");
        fs::create_dir_all(home.join("locked/inside")).unwrap();
        fs::create_dir_all(fixture.path().join("elsewhere/inside")).unwrap();
        let _below_home = SearchOnly::new(home.join("locked"));
        let _beside_home = SearchOnly::new(fixture.path().join("elsewhere"));
        if rustix::process::geteuid().is_root() {
            return;
        }
        let ancestors = home_ancestors(&home);
        assert!(!ancestors.contains(&home), "the home itself is not one");
        for path in [
            home.join("locked/inside"),
            fixture.path().join("elsewhere/inside"),
        ] {
            let refused = Directory::ensure_absolute_with(&path, &ancestors)
                .err()
                .unwrap();
            assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied, "{path:?}");
        }
    }

    #[test]
    fn a_search_only_root_is_opened_o_path_at_once() {
        let search_only = |directory: &Directory| {
            rustix::fs::fcntl_getfl(&directory.0)
                .unwrap()
                .contains(OFlags::PATH)
        };
        // The Android host: "/" is an ancestor of the home. This host could
        // read it, so `O_PATH` shows that no read-only open came first.
        let home = Path::new("/data/user/0/app/files/launcher");
        let root = Directory::root(&home_ancestors(home)).unwrap();
        assert!(search_only(&root));
        let temporary = tempfile::tempdir().unwrap();
        let below = absolute_relative(temporary.path()).unwrap();
        assert!(root.open_directory(below).is_ok());
        // Elsewhere "/" is opened read-only, as before.
        assert!(!search_only(&Directory::root(&[]).unwrap()));
    }

    #[test]
    fn an_unstable_filesystem_id_compares_the_inode_only() {
        assert!(same_file_identity((65054, 7), (65042, 7), true));
        assert!(!same_file_identity((65054, 7), (65042, 7), false));
        assert!(same_file_identity((65054, 7), (65054, 7), false));
        assert!(!same_file_identity((65054, 7), (65054, 8), true));
        assert!(!same_file_identity((65054, 7), (65054, 8), false));
    }

    #[test]
    fn the_android_host_never_calls_openat2() {
        // The switch is for the whole process, so it runs in a child of
        // this test binary and the other tests keep their lookups.
        const CHILD: &str = "PD2_TEST_ANDROID_HOST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "safe_fs::tests::the_android_host_never_calls_openat2",
                    "--test-threads=1",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            let output = String::from_utf8_lossy(&child.stdout);
            assert!(child.status.success(), "{output}");
            assert!(output.contains("1 passed"), "the child ran it: {output}");
            return;
        }
        let fixture = tempfile::tempdir().unwrap();
        let home = fixture.path().join("launcher");
        use_android_host(Some(&home));
        assert_eq!(search_only_ancestors(), home_ancestors(&home));
        let config = Directory::ensure_absolute(&home.join("config")).unwrap();
        config
            .atomic_write(OsStr::new("settings.json"), b"{}")
            .unwrap();
        Directory::open(&home)
            .unwrap()
            .open_file(Path::new("config/settings.json"))
            .unwrap();
        assert_eq!(OPENAT2_CALLS.with(Cell::get), 0);
    }
}
