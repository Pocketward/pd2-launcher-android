//! Characters survive Start over: the desktop keeps the game's `Save`
//! outside what Start over deletes.
//!
//! The game keeps characters in `<InstallPath>\Save` (the Launcher writes no
//! `Save Path`), so the Managed game folder's `Save` becomes a
//! symbolic link to `~/.pd2launcher/userdata/Save`, a folder of the player's
//! own that no install, update, Start over or move of the installation
//! touches; Wine follows the link like a folder. Start over removes the
//! Managed prefix with the owned-tree removal, which unlinks a link as a
//! name and never follows it ([`Directory::remove_owned_tree`]), so it takes
//! only the link. A Diablo II folder used where it is is the player's own
//! and is left alone: Start over never touches it.
//!
//! A `Save` that is still a real folder (the game's, an installer's, a
//! copied Diablo II's, or an older installation's) has its saves moved into
//! the kept folder first, once: each file is published there by a temporary
//! file that takes the source's times before a rename, and only
//! once every one is there is the folder renamed aside, the link made, and
//! the folder removed. A stop at any point leaves every file either still in
//! the game folder or already in the kept folder or a backup; the next keep
//! finishes from where it stopped. `Save` in another case (a copy's `save`)
//! is the game's `Save` to Wine too: its saves are kept the same way and it
//! goes, so only the link is left.
//!
//! A character is the files sharing a name up to the first dot with a
//! `.d2s` (`Hero.d2s`, `Hero.key`, `Hero.ma0`, `Hero.map`). One already kept
//! with the same content is not kept twice; one with other content is never
//! overwritten: the one saved last (by its own `.d2s`) stays, the kept one on
//! a tie, and the other goes to `~/.pd2launcher/userdata/backups/backup-<time>`.
//! Every other saved file is kept on its own by the same rule, dated by its
//! own time: the shared stash, and the files of the folders inside `Save`
//! (PD2's `ProjectD2`, the online characters' hotkeys and automaps), which
//! are kept in the same folders of the kept folder, every spelling of `Save`
//! merged into them. Each other copy goes to a backup laid out as the kept
//! folder is ([`PutAside`] reports them for the player to be told). Hidden
//! files and folders (not the game's), links (never followed, kept as links)
//! and a file or folder the kept folder already holds as another kind are
//! copied into such a backup under `Save`, as the `Save` folder held them.

use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs::File,
    io::{self, Read, Seek},
    os::unix::ffi::OsStrExt,
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use rustix::fs::{AtFlags, FileType, OFlags};

use crate::safe_fs::Directory;

/// Start over or removing an old installation did not run: the characters
/// could not be moved to their own folder first.
pub(crate) const SAVES_NOT_KEPT: &str = "saves-not-kept";
/// The game (or the official launcher, whose Play starts it) did not start:
/// after a keep that failed, the Managed game folder had no `Save` the game
/// would find the characters in, and none could be put back.
pub(crate) const SAVES_UNAVAILABLE: &str = "saves-unavailable";

/// The game's own characters folder, in its game folder.
pub(crate) const SAVE: &str = "Save";
/// Where a `Save` folder whose characters are all kept waits, between its
/// rename and its removal, while the link takes its place.
const MOVED: &str = "Save.pd2launcher-moved";
/// How deep a folder inside a `Save` folder is kept or copied into a
/// backup. A deeper one fails the keep before anything is copied, so
/// nothing is removed.
const MAX_COPY_DEPTH: usize = 16;
/// The most of the prefix's registry read for its `Save Path`.
const REGISTRY_LIMIT: u64 = 16 * 1024 * 1024;
/// The key the game reads its paths from, as Wine's `user.reg` names it.
const DIABLO_KEY: &str = r"[Software\\Blizzard Entertainment\\Diablo II]";

/// The player's own folders below the Launcher's home: the kept characters
/// and the backups of characters that were not kept twice.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct KeptSaves {
    save: PathBuf,
    backups: PathBuf,
}

impl KeptSaves {
    /// `<home>/userdata/Save` and `<home>/userdata/backups`.
    pub(crate) fn below(home: &Path) -> Self {
        Self {
            save: home.join("userdata").join(SAVE),
            backups: home.join("userdata").join("backups"),
        }
    }

    /// Where the characters are kept.
    pub(crate) fn path(&self) -> &Path {
        &self.save
    }
}

/// What a keep put in a backup folder instead of the kept folder, for the
/// player to be told.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct PutAside {
    /// Characters that had another copy that differed: the one saved last
    /// is kept, the other went to a backup.
    pub(crate) characters: usize,
    /// Other saved files (the shared stash, the files of a folder inside
    /// `Save` such as `ProjectD2`) that had another copy that differed: the
    /// one saved last is kept, the other went to a backup.
    pub(crate) files: usize,
    /// Other things a `Save` folder held (hidden files and folders, links,
    /// a file named `Save`, a file or folder the kept folder holds as
    /// another kind), copied to a backup.
    pub(crate) others: usize,
    /// The backup folder written last.
    pub(crate) folder: Option<PathBuf>,
}

impl PutAside {
    pub(crate) fn is_empty(&self) -> bool {
        self.characters == 0 && self.files == 0 && self.others == 0
    }

    /// Both keeps' together, in the later one's folder.
    pub(crate) fn add(&mut self, later: Self) {
        self.characters += later.characters;
        self.files += later.files;
        self.others += later.others;
        if later.folder.is_some() {
            self.folder = later.folder;
        }
    }
}

/// What a keep took from `Save` folders into the kept folder.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Saves {
    /// Characters: the files sharing a name with a `.d2s`.
    pub(crate) characters: usize,
    /// Other saved files, each on its own: the shared stash, and the files
    /// of the folders inside `Save`.
    pub(crate) files: usize,
}

impl Saves {
    pub(crate) fn is_empty(self) -> bool {
        self.characters == 0 && self.files == 0
    }

    fn add(&mut self, other: Self) {
        self.characters += other.characters;
        self.files += other.files;
    }
}

/// What a keep found and did.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Kept {
    /// `Save` already links to the kept folder.
    Linked,
    /// `Save` now links to the kept folder; `saves` came from `Save`
    /// folders (none when there was none), and `put_aside` went to a backup.
    Moved { saves: Saves, put_aside: PutAside },
    /// There is no game folder (yet): nothing to keep.
    NoGame,
}

impl Kept {
    /// What this keep put in a backup.
    pub(crate) fn put_aside(&self) -> PutAside {
        match self {
            Self::Moved { put_aside, .. } => put_aside.clone(),
            Self::Linked | Self::NoGame => PutAside::default(),
        }
    }
}

/// Keep the characters of the Managed game folder below `root` (the Managed
/// root): its `Save` links to the kept folder once they are all there.
pub(crate) fn keep_managed(
    root: &Directory,
    kept: &KeptSaves,
    now: SystemTime,
) -> io::Result<Kept> {
    match root.open_directory(Path::new(crate::game_folder::GAME_RELATIVE)) {
        Ok(game) => keep(&game, kept, now),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Kept::NoGame),
        Err(error) => Err(error),
    }
}

/// Move the characters of the Managed game folder below `root` into the
/// kept folder without linking anything: for an installation about to be
/// removed (the one a move to another drive left behind). Every `Save`
/// folder counts, in any case. Returns what came out, and what was put
/// aside.
pub(crate) fn rescue_managed(
    root: &Directory,
    kept: &KeptSaves,
    now: SystemTime,
) -> io::Result<(Saves, PutAside)> {
    let game = match root.open_directory(Path::new(crate::game_folder::GAME_RELATIVE)) {
        Ok(game) => game,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok((Saves::default(), PutAside::default()));
        }
        Err(error) => return Err(error),
    };
    let mut keeper = Keeper::new(kept, now)?;
    let mut saves = Saves::default();
    let mut names = vec![OsString::from(MOVED)];
    names.extend(spellings(&game)?);
    for name in names {
        if entry(&game, &name)? == Some(FileType::Directory) {
            saves.add(keeper.move_into(&game.open_directory(Path::new(&name))?, false)?);
        }
    }
    Ok((saves, keeper.put_aside))
}

/// Keep the characters of the game folder `game`: make its `Save` a link to
/// the kept folder, moving the characters of every `Save` folder (in any
/// case) there first. Succeeds only when no `Save` but links is left.
pub(crate) fn keep(game: &Directory, kept: &KeptSaves, now: SystemTime) -> io::Result<Kept> {
    let mut keeper = Keeper::new(kept, now)?;
    let mut saves = Saves::default();
    let mut linked = false;
    // An earlier keep stopped between renaming a folder aside and removing
    // it: its characters were kept before the rename, and keeping them again
    // changes nothing; then it goes.
    if entry(game, OsStr::new(MOVED))? == Some(FileType::Directory) {
        saves.add(keeper.move_into(&game.open_directory(Path::new(MOVED))?, false)?);
        game.remove_owned_tree(OsStr::new(MOVED))?;
    }
    // `Save` in another case (a copy's `save`) is the game's `Save` to Wine
    // too: its characters are kept the same way, then it goes, so only the
    // link is left for the game and for Start over.
    for name in spellings(game)?
        .into_iter()
        .filter(|name| name.as_os_str() != SAVE)
    {
        match entry(game, &name)? {
            Some(FileType::Directory) => {
                saves.add(keeper.move_into(&game.open_directory(Path::new(&name))?, false)?);
                keeper.target.sync()?;
                rustix::fs::renameat(fd(game), name.as_os_str(), fd(game), MOVED)?;
                game.sync()?;
                // A removal that fails is finished by the next keep.
                let _ = game.remove_owned_tree(OsStr::new(MOVED));
            }
            // A link holds nothing itself.
            Some(FileType::Symlink) | None => {}
            Some(_) => keeper.back_up_file(game, &name)?,
        }
    }
    match entry(game, OsStr::new(SAVE))? {
        None => link(game, kept)?,
        Some(FileType::Symlink) => {
            linked = links_to(game, kept)?;
            if !linked {
                // A link elsewhere holds nothing itself; what it points at
                // is left as it is.
                rustix::fs::unlinkat(fd(game), SAVE, AtFlags::empty())?;
                link(game, kept)?;
            }
        }
        Some(FileType::Directory) => {
            saves.add(keeper.move_into(&game.open_directory(Path::new(SAVE))?, false)?);
            keeper.target.sync()?;
            // Every save is kept: the folder steps aside for the link.
            rustix::fs::renameat(fd(game), SAVE, fd(game), MOVED)?;
            if let Err(error) = link(game, kept) {
                // Put back, so the game still finds its characters; should
                // that fail too, `restore_managed` puts it back before Play.
                let _ = rustix::fs::renameat(fd(game), MOVED, fd(game), SAVE);
                return Err(error);
            }
            game.sync()?;
            // What is left in it is kept too; a removal that fails is
            // finished by the next keep.
            let _ = game.remove_owned_tree(OsStr::new(MOVED));
        }
        // A file named Save: kept as a backup, then the link.
        Some(_) => {
            keeper.back_up_file(game, OsStr::new(SAVE))?;
            link(game, kept)?;
        }
    }
    game.sync()?;
    // Start over removes whatever is left: only links may be.
    for name in spellings(game)? {
        if entry(game, &name)? != Some(FileType::Symlink) {
            return Err(io::Error::other(
                "a Save folder is still in the game folder",
            ));
        }
    }
    let put_aside = keeper.put_aside;
    Ok(if linked && saves.is_empty() && put_aside.is_empty() {
        Kept::Linked
    } else {
        Kept::Moved { saves, put_aside }
    })
}

/// After a keep that failed, before the game starts: the Managed game folder
/// below `root` has a `Save` the game finds its characters in. A folder a
/// stopped keep had stepped aside is put back, and only when that fails is
/// there an error: the game would not see those characters. With neither,
/// the link is made again, even when the kept folder can't be made or opened
/// here (a link of the player's own, to a synced folder say): making a link
/// never opens its target, and Wine follows a chain of links. Should even
/// that fail, the game makes its own `Save`, as before this keep. A game
/// folder that can't be read is the game's own start to report.
pub(crate) fn restore_managed(root: &Directory, kept: &KeptSaves) -> io::Result<()> {
    let Ok(game) = root.open_directory(Path::new(crate::game_folder::GAME_RELATIVE)) else {
        return Ok(());
    };
    if !spellings(&game).is_ok_and(|names| names.is_empty()) {
        return Ok(());
    }
    if entry(&game, OsStr::new(MOVED)).ok().flatten() == Some(FileType::Directory) {
        rustix::fs::renameat(fd(&game), MOVED, fd(&game), SAVE)?;
        return game.sync();
    }
    let _ = Directory::ensure_absolute(&kept.save);
    if link(&game, kept).is_ok() {
        let _ = game.sync();
    }
    Ok(())
}

/// Copy into the kept folder the characters of a `Save Path` that the
/// registry of the prefix below `root` (the managed root at `root_path`)
/// names inside that root, other than the Managed game folder's `Save`,
/// before what holds them is removed (Start over, removing an old
/// installation). The Launcher writes no `Save Path` and the game writes
/// `<InstallPath>\Save` when there is none, which [`keep`]
/// covers; a player or another program can set one elsewhere. Only
/// characters and the shared stash (the files beside a `.d2s` or a
/// `.stash`) are copied, and no folder in it, since such a folder need not
/// be the game's alone; nothing is linked. Returns what came out, and what
/// was put aside.
pub(crate) fn keep_save_path(
    root: &Directory,
    root_path: &Path,
    kept: &KeptSaves,
    now: SystemTime,
) -> io::Result<(Saves, PutAside)> {
    let below = registry_save_path(root)?
        .and_then(|path| below_root(&path, root_path))
        .filter(|components| !is_managed_save(components));
    let Some(components) = below else {
        return Ok((Saves::default(), PutAside::default()));
    };
    let Some(folder) = open_as_wine_finds(root, &components)? else {
        return Ok((Saves::default(), PutAside::default()));
    };
    let mut keeper = Keeper::new(kept, now)?;
    let saves = keeper.move_into(&folder, true)?;
    Ok((saves, keeper.put_aside))
}

fn fd(directory: &Directory) -> &File {
    directory.as_file()
}

/// The type of `name` in `directory`, without following a link.
fn entry(directory: &Directory, name: &OsStr) -> io::Result<Option<FileType>> {
    match rustix::fs::statat(fd(directory), name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => Ok(Some(FileType::from_raw_mode(stat.st_mode))),
        Err(rustix::io::Errno::NOENT) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// The names in `game` that are `Save` in any case: Wine finds each as the
/// game's `Save`.
fn spellings(game: &Directory) -> io::Result<Vec<OsString>> {
    Ok(game
        .names()?
        .into_iter()
        .filter(|name| name.as_bytes().eq_ignore_ascii_case(SAVE.as_bytes()))
        .collect())
}

fn links_to(game: &Directory, kept: &KeptSaves) -> io::Result<bool> {
    let found = rustix::fs::readlinkat(fd(game), SAVE, Vec::new())?;
    Ok(found.as_bytes() == kept.save.as_os_str().as_bytes())
}

fn link(game: &Directory, kept: &KeptSaves) -> io::Result<()> {
    rustix::fs::symlinkat(kept.save.as_path(), fd(game), SAVE)?;
    Ok(())
}

/// One file of a `Save` folder's top: its name, content and times.
struct SaveFile {
    name: OsString,
    content: Vec<u8>,
    accessed: SystemTime,
    modified: SystemTime,
}

/// The regular files at the top of `directory`, by character name (up to
/// the first dot); hidden files are no one's. `saves_only`: only names with
/// a `.d2s` or a `.stash` file.
fn characters(
    directory: &Directory,
    saves_only: bool,
) -> io::Result<BTreeMap<Vec<u8>, Vec<SaveFile>>> {
    let mut names: BTreeMap<Vec<u8>, Vec<OsString>> = BTreeMap::new();
    for name in directory.names()? {
        let bytes = name.as_bytes();
        if hidden(&name) || entry(directory, &name)? != Some(FileType::RegularFile) {
            continue;
        }
        let character = bytes
            .split(|byte| *byte == b'.')
            .next()
            .unwrap_or(bytes)
            .to_vec();
        names.entry(character).or_default().push(name);
    }
    let mut found = BTreeMap::new();
    for (character, names) in names {
        if saves_only
            && !names
                .iter()
                .any(|name| has_extension(name, b".d2s") || has_extension(name, b".stash"))
        {
            continue;
        }
        let mut files = Vec::new();
        for name in names {
            let mut file = directory.open_beneath(Path::new(&name), OFlags::RDONLY)?;
            // The times before the read, which may change the access time.
            let metadata = file.metadata()?;
            let (accessed, modified) = (metadata.accessed()?, metadata.modified()?);
            let mut content = Vec::new();
            file.read_to_end(&mut content)?;
            files.push(SaveFile {
                name,
                content,
                accessed,
                modified,
            });
        }
        found.insert(character, files);
    }
    Ok(found)
}

fn has_extension(name: &OsStr, extension: &[u8]) -> bool {
    let name = name.as_bytes();
    name.len() > extension.len()
        && name[name.len() - extension.len()..].eq_ignore_ascii_case(extension)
}

/// A name starting with a dot: not the game's.
fn hidden(name: &OsStr) -> bool {
    name.as_bytes().starts_with(b".")
}

/// Whether `files` are a character's: one of them is a `.d2s`.
fn is_character(files: &[SaveFile]) -> bool {
    files.iter().any(|file| has_extension(&file.name, b".d2s"))
}

/// When the character `character` was saved last: by its own
/// `<character>.d2s`, which the game writes at every save (a copy with one
/// before a copy without; never `Hero.bak.d2s`), or for a copy without one
/// by the latest of its files.
fn saved_at(character: &[u8], files: &[SaveFile]) -> (bool, SystemTime) {
    let own = |file: &&SaveFile| {
        let name = file.name.as_bytes();
        name.len() == character.len() + 4
            && name.starts_with(character)
            && has_extension(&file.name, b".d2s")
    };
    match files.iter().find(own) {
        Some(file) => (true, file.modified),
        None => (
            false,
            files
                .iter()
                .map(|file| file.modified)
                .max()
                .unwrap_or(UNIX_EPOCH),
        ),
    }
}

/// Every file of `incoming` is already kept with the same content (a
/// folder whose removal stopped part way holds only some of them).
fn already_kept(incoming: &[SaveFile], kept: &[SaveFile]) -> bool {
    incoming.iter().all(|file| {
        kept.iter()
            .any(|other| other.name == file.name && other.content == file.content)
    })
}

/// Whether the open files `a` and `b` hold the same bytes.
fn same_content(a: &File, b: &File) -> io::Result<bool> {
    if a.metadata()?.len() != b.metadata()?.len() {
        return Ok(false);
    }
    let (mut a, mut b) = (a.try_clone()?, b.try_clone()?);
    a.rewind()?;
    b.rewind()?;
    let (mut left, mut right) = (vec![0_u8; 64 * 1024], vec![0_u8; 64 * 1024]);
    loop {
        let read = a.read(&mut left)?;
        if read == 0 {
            return Ok(b.read(&mut right)? == 0);
        }
        match b.read_exact(&mut right[..read]) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(false),
            Err(error) => return Err(error),
        }
        if left[..read] != right[..read] {
            return Ok(false);
        }
    }
}

fn too_deep() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "a folder in Save is too deep to keep",
    )
}

/// No folder in `directory` (at `depth` below a `Save` folder's top) is
/// deeper than [`MAX_COPY_DEPTH`]; links are not followed.
fn check_depth(directory: &Directory, depth: usize) -> io::Result<()> {
    for name in directory.names()? {
        if entry(directory, &name)? == Some(FileType::Directory) {
            if depth >= MAX_COPY_DEPTH {
                return Err(too_deep());
            }
            check_depth(&directory.open_directory(Path::new(&name))?, depth + 1)?;
        }
    }
    Ok(())
}

/// Whether `relative` names anything below `directory`, no link followed;
/// a path below a file or a link counts as taken.
fn holds(directory: &Directory, relative: &Path) -> bool {
    let Some(name) = relative.file_name() else {
        return true;
    };
    let parent = match relative
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        None => directory.try_clone(),
        Some(parent) => directory.open_directory(parent),
    };
    match parent {
        Ok(parent) => entry(&parent, name).ok().flatten().is_some(),
        Err(error) => error.kind() != io::ErrorKind::NotFound,
    }
}

/// The folder `folder` below `directory`, made when missing; `directory`
/// itself for none.
fn below(directory: Directory, folder: &Path) -> io::Result<Directory> {
    if folder.as_os_str().is_empty() {
        Ok(directory)
    } else {
        directory.ensure_directory(folder)
    }
}

/// One keep: the kept folder, the time its backups are named by, and what
/// it put aside.
struct Keeper<'a> {
    kept: &'a KeptSaves,
    target: Directory,
    now: SystemTime,
    put_aside: PutAside,
    /// The `Save` of the backup folder the rest of the `Save` folder being
    /// kept goes to, once made.
    rest: Option<Directory>,
    /// The backup folders this keep made for the rest of a `Save` folder,
    /// which no other copy goes to: a folder inside `Save` named `Save`
    /// would put one at a path the rest may take later.
    rest_folders: Vec<String>,
}

impl<'a> Keeper<'a> {
    fn new(kept: &'a KeptSaves, now: SystemTime) -> io::Result<Self> {
        Ok(Self {
            kept,
            target: Directory::ensure_absolute(&kept.save)?,
            now,
            put_aside: PutAside::default(),
            rest: None,
            rest_folders: Vec::new(),
        })
    }

    /// Copy the saves of the folder `source` into the kept folder, and the
    /// rest of it into a backup. The source is only read. `saves_only`:
    /// only the characters and the shared stash at its top, nothing else
    /// ([`keep_save_path`]).
    fn move_into(&mut self, source: &Directory, saves_only: bool) -> io::Result<Saves> {
        if !saves_only {
            // A folder too deep fails the keep before anything is copied.
            check_depth(source, 0)?;
        }
        self.rest = None;
        let target = self.target.try_clone()?;
        let mut saves = Saves::default();
        let incoming = characters(source, saves_only)?;
        let mut existing = characters(&target, false)?;
        for (character, files) in incoming {
            let kept_files = existing.remove(&character).unwrap_or_default();
            if is_character(&files) || is_character(&kept_files) {
                saves.characters += 1;
                self.keep_character(source, &character, &files, &kept_files)?;
            } else {
                // Files of no character (the shared stash): each on its own.
                for file in &files {
                    saves.files += 1;
                    self.keep_file(source, &file.name, &target, Path::new(""))?;
                }
            }
        }
        if !saves_only {
            let mut names = source.names()?;
            names.sort();
            for name in names {
                match entry(source, &name)? {
                    // A character or another saved file, kept above.
                    Some(FileType::RegularFile) if !hidden(&name) => {}
                    Some(FileType::Directory) if !hidden(&name) => {
                        saves.files +=
                            self.keep_folder(source, &name, &target, Path::new(&name), 0)?;
                    }
                    _ => self.put_aside_entry(source, &name, Path::new(""), 0)?,
                }
            }
        }
        target.sync()?;
        Ok(saves)
    }

    /// Keep the character `character` (`files`, read from `source`) in the
    /// kept folder, which holds `kept_files` of it: the same files once,
    /// else the copy saved last as a whole, the kept one on a tie; the other
    /// goes to a backup.
    fn keep_character(
        &mut self,
        source: &Directory,
        character: &[u8],
        files: &[SaveFile],
        kept_files: &[SaveFile],
    ) -> io::Result<()> {
        if kept_files.is_empty() {
            return publish_all(source, files, &self.target);
        }
        if already_kept(files, kept_files) {
            return Ok(());
        }
        let names = files
            .iter()
            .chain(kept_files)
            .map(|file| PathBuf::from(&file.name))
            .collect::<Vec<_>>();
        let backup = self.backup_folder(&names)?;
        self.put_aside.characters += 1;
        if saved_at(character, kept_files) >= saved_at(character, files) {
            // The kept one was saved last (or at the same time): the
            // incoming one is backed up.
            publish_all(source, files, &backup)?;
            return backup.sync();
        }
        publish_all(&self.target, kept_files, &backup)?;
        backup.sync()?;
        for file in kept_files {
            if !files.iter().any(|incoming| incoming.name == file.name) {
                rustix::fs::unlinkat(fd(&self.target), file.name.as_os_str(), AtFlags::empty())?;
            }
        }
        publish_all(source, files, &self.target)
    }

    /// Keep the saved file `name` of `from` in `into`, the kept folder's
    /// `folder`: the same content once, else the copy saved last (by its
    /// own time), the kept one on a tie; the other goes to a backup, at the
    /// same place there.
    fn keep_file(
        &mut self,
        from: &Directory,
        name: &OsStr,
        into: &Directory,
        folder: &Path,
    ) -> io::Result<()> {
        let incoming = from.open_beneath(Path::new(name), OFlags::RDONLY)?;
        // The times before the read, which may change the access time.
        let metadata = incoming.metadata()?;
        let (accessed, modified) = (metadata.accessed()?, metadata.modified()?);
        match entry(into, name)? {
            None => into.atomic_copy_with_times(name, &incoming, accessed, modified),
            Some(FileType::RegularFile) => {
                let kept = into.open_beneath(Path::new(name), OFlags::RDONLY)?;
                let kept_modified = kept.metadata()?.modified()?;
                if same_content(&incoming, &kept)? {
                    return Ok(());
                }
                let backup = below(self.backup_folder(&[folder.join(name)])?, folder)?;
                self.put_aside.files += 1;
                if kept_modified >= modified {
                    backup.atomic_copy_with_times(name, &incoming, accessed, modified)?;
                    return backup.sync();
                }
                copy_file(into, name, &backup)?;
                backup.sync()?;
                into.atomic_copy_with_times(name, &incoming, accessed, modified)
            }
            // The kept folder holds a folder or a link by that name: it
            // stays, and this file goes to a backup.
            Some(_) => self.put_aside_entry(from, name, folder, 0),
        }
    }

    /// Keep the folder `name` of `from` as the kept folder's `folder`
    /// (whose parent there is `into`): each saved file in it by
    /// [`Self::keep_file`], each folder the same way, and the rest to a
    /// backup. `depth`: how far below the `Save` folder's top it is.
    /// Returns how many saved files it held.
    fn keep_folder(
        &mut self,
        from: &Directory,
        name: &OsStr,
        into: &Directory,
        folder: &Path,
        depth: usize,
    ) -> io::Result<usize> {
        if depth >= MAX_COPY_DEPTH {
            return Err(too_deep());
        }
        let target = match entry(into, name)? {
            None | Some(FileType::Directory) => into.ensure_directory(Path::new(name))?,
            // The kept folder holds a file or a link by that name: it
            // stays, and this folder goes to a backup whole.
            Some(_) => {
                let parent = folder.parent().unwrap_or(Path::new(""));
                self.put_aside_entry(from, name, parent, depth)?;
                return Ok(0);
            }
        };
        let source = from.open_directory(Path::new(name))?;
        let mut files = 0;
        let mut names = source.names()?;
        names.sort();
        for child in names {
            match entry(&source, &child)? {
                Some(FileType::RegularFile) if !hidden(&child) => {
                    files += 1;
                    self.keep_file(&source, &child, &target, folder)?;
                }
                Some(FileType::Directory) if !hidden(&child) => {
                    let inner = folder.join(&child);
                    files += self.keep_folder(&source, &child, &target, &inner, depth + 1)?;
                }
                _ => self.put_aside_entry(&source, &child, folder, depth + 1)?,
            }
        }
        target.sync()?;
        Ok(files)
    }

    /// Copy what `from` holds as `name` (a file, a folder with what it
    /// holds, or a link as a link, never followed) into this keep's backup
    /// of the rest of the `Save` folder, at `folder` there; a special file
    /// holds nothing and is left out. `depth`: how far below the `Save`
    /// folder's top `name` is.
    fn put_aside_entry(
        &mut self,
        from: &Directory,
        name: &OsStr,
        folder: &Path,
        depth: usize,
    ) -> io::Result<()> {
        if !matches!(
            entry(from, name)?,
            Some(FileType::RegularFile | FileType::Directory | FileType::Symlink)
        ) {
            return Ok(());
        }
        let rest = match &self.rest {
            Some(rest) => rest.try_clone()?,
            None => {
                let backup = self.backup_folder(&[])?;
                if let Some(folder) = self.put_aside.folder.as_deref().and_then(Path::file_name) {
                    self.rest_folders
                        .push(folder.to_string_lossy().into_owned());
                }
                let rest = backup.ensure_directory(Path::new(SAVE))?;
                self.rest = Some(rest.try_clone()?);
                rest
            }
        };
        let into = below(rest, folder)?;
        copy_entry(from, name, &into, depth)?;
        into.sync()?;
        self.put_aside.others += 1;
        Ok(())
    }

    /// A file where the game wants its `Save` folder: kept in a backup, then
    /// removed for the link.
    fn back_up_file(&mut self, game: &Directory, name: &OsStr) -> io::Result<()> {
        let backup = self.backup_folder(&[PathBuf::from(name)])?;
        copy_file(game, name, &backup)?;
        backup.sync()?;
        self.put_aside.others += 1;
        rustix::fs::unlinkat(fd(game), name, AtFlags::empty())?;
        Ok(())
    }

    /// `backup-<seconds>`, or the first `backup-<seconds>-<n>` that holds
    /// none of the paths `names` and is no rest of this keep's (for none,
    /// the first that does not exist yet).
    fn backup_folder(&mut self, names: &[PathBuf]) -> io::Result<Directory> {
        let seconds = self
            .now
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        let backups = Directory::ensure_absolute(&self.kept.backups)?;
        for n in 0..1000 {
            let folder = if n == 0 {
                format!("backup-{seconds}")
            } else {
                format!("backup-{seconds}-{n}")
            };
            let free = match entry(&backups, OsStr::new(&folder))? {
                None => true,
                Some(FileType::Directory)
                    if !names.is_empty() && !self.rest_folders.contains(&folder) =>
                {
                    let existing = backups.open_directory(Path::new(&folder))?;
                    names.iter().all(|name| !holds(&existing, name))
                }
                _ => false,
            };
            if free {
                let directory = backups.ensure_directory(Path::new(&folder))?;
                self.put_aside.folder = Some(self.kept.backups.join(folder));
                return Ok(directory);
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "no free backup folder",
        ))
    }
}

/// Publish `files` (read from `from`) into `into`, each by a temporary file
/// that takes the file's times and a rename.
fn publish_all(from: &Directory, files: &[SaveFile], into: &Directory) -> io::Result<()> {
    for file in files {
        let source = from.open_beneath(Path::new(&file.name), OFlags::RDONLY)?;
        into.atomic_copy_with_times(&file.name, &source, file.accessed, file.modified)?;
    }
    Ok(())
}

fn copy_file(from: &Directory, name: &OsStr, into: &Directory) -> io::Result<()> {
    let source = from.open_beneath(Path::new(name), OFlags::RDONLY)?;
    let metadata = source.metadata()?;
    into.atomic_copy_with_times(name, &source, metadata.accessed()?, metadata.modified()?)
}

/// Copy a regular file, a folder with what it holds, or a link as a link:
/// no link is followed, and special files are left out. A folder too deep
/// fails the copy.
fn copy_entry(from: &Directory, name: &OsStr, into: &Directory, depth: usize) -> io::Result<()> {
    match entry(from, name)? {
        Some(FileType::RegularFile) => copy_file(from, name, into),
        Some(FileType::Symlink) => {
            let target = rustix::fs::readlinkat(fd(from), name, Vec::new())?;
            rustix::fs::symlinkat(target.as_c_str(), fd(into), name)?;
            Ok(())
        }
        Some(FileType::Directory) if depth >= MAX_COPY_DEPTH => Err(too_deep()),
        Some(FileType::Directory) => {
            let source = from.open_directory(Path::new(name))?;
            let target = into.ensure_directory(Path::new(name))?;
            for child in source.names()? {
                copy_entry(&source, &child, &target, depth + 1)?;
            }
            target.sync()
        }
        _ => Ok(()),
    }
}

/// The `Save Path` the game reads from the prefix's registry below `root`,
/// when one is set. A registry that is missing, or is a link, sets none.
fn registry_save_path(root: &Directory) -> io::Result<Option<String>> {
    let file = match root.open_beneath(
        Path::new("prefix/user.reg"),
        OFlags::RDONLY | OFlags::NONBLOCK,
    ) {
        Ok(file) => file,
        Err(error)
            if error.kind() == io::ErrorKind::NotFound
                || error.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error()) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    if !file.metadata()?.is_file() {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    file.take(REGISTRY_LIMIT).read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let mut in_key = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_key = line
                .split_once(']')
                .is_some_and(|(key, _)| DIABLO_KEY.eq_ignore_ascii_case(&format!("{key}]")));
        } else if in_key
            && let Some(value) = line
                .get(..11)
                .filter(|name| name.eq_ignore_ascii_case("\"Save Path\""))
                .and_then(|_| line[11..].strip_prefix('='))
        {
            return Ok(registry_string(value));
        }
    }
    Ok(None)
}

/// A string value as Wine's registry files write it, `"…"` or
/// `str(2):"…"`, with its `\\`, `\"` and `\x…` escapes undone; `None` for
/// any other kind of value.
fn registry_string(value: &str) -> Option<String> {
    let value = value.strip_prefix("str(2):").unwrap_or(value);
    let mut chars = value.strip_prefix('"')?.chars().peekable();
    let mut text = String::new();
    loop {
        match chars.next()? {
            '"' => return Some(text),
            '\\' => match chars.next()? {
                'x' => {
                    let mut code = 0;
                    for _ in 0..4 {
                        match chars.peek().and_then(|digit| digit.to_digit(16)) {
                            Some(digit) => {
                                code = code * 16 + digit;
                                chars.next();
                            }
                            None => break,
                        }
                    }
                    text.push(char::from_u32(code)?);
                }
                'n' => text.push('\n'),
                'r' => text.push('\r'),
                't' => text.push('\t'),
                '0' => text.push('\0'),
                other => text.push(other),
            },
            other => text.push(other),
        }
    }
}

/// Where the Windows path `windows` is below the managed root at
/// `root_path`, as path components: `C:` is the prefix's `drive_c`, `Z:`
/// the host's root. `None` for anywhere else (another drive, a share,
/// outside the root, a path that climbs).
fn below_root(windows: &str, root_path: &Path) -> Option<Vec<String>> {
    let mut parts = windows
        .split(['\\', '/'])
        .filter(|part| !part.is_empty() && *part != ".");
    let drive = parts.next()?;
    let rest = parts.map(str::to_owned).collect::<Vec<_>>();
    if rest.iter().any(|part| part == ".." || part.contains('\0')) {
        return None;
    }
    if drive.eq_ignore_ascii_case("C:") {
        return Some(
            ["prefix", "drive_c"]
                .into_iter()
                .map(str::to_owned)
                .chain(rest)
                .collect(),
        );
    }
    if !drive.eq_ignore_ascii_case("Z:") {
        return None;
    }
    let root = root_path
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name.to_str()),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    (rest.len() > root.len()
        && rest
            .iter()
            .zip(&root)
            .all(|(part, name)| part.eq_ignore_ascii_case(name)))
    .then(|| rest[root.len()..].to_vec())
}

/// Whether `components` name the Managed game folder's `Save`, which
/// [`keep`] covers.
fn is_managed_save(components: &[String]) -> bool {
    let managed = crate::game_folder::GAME_RELATIVE
        .split('/')
        .chain([SAVE])
        .collect::<Vec<_>>();
    components.len() == managed.len()
        && components
            .iter()
            .zip(managed)
            .all(|(part, name)| part.eq_ignore_ascii_case(name))
}

/// Open the folder `components` below `root` as Wine finds it: each name as
/// it is, else in another case, and never through a link (what a link
/// leads to is not removed through it). `None` when there is no such
/// folder.
fn open_as_wine_finds(root: &Directory, components: &[String]) -> io::Result<Option<Directory>> {
    let mut current = root.try_clone()?;
    for component in components {
        let name = match entry(&current, OsStr::new(component))? {
            Some(_) => OsString::from(component),
            None => match current
                .names()?
                .into_iter()
                .find(|name| name.as_bytes().eq_ignore_ascii_case(component.as_bytes()))
            {
                Some(name) => name,
                None => return Ok(None),
            },
        };
        if entry(&current, &name)? != Some(FileType::Directory) {
            return Ok(None);
        }
        current = current.open_directory(Path::new(&name))?;
    }
    Ok(Some(current))
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::unix::fs::{MetadataExt, PermissionsExt},
        time::Duration,
    };

    use tempfile::TempDir;

    use super::*;

    struct Fixture {
        _home: TempDir,
        game: PathBuf,
        kept: KeptSaves,
    }

    impl Fixture {
        fn new() -> Self {
            let home = TempDir::new().unwrap();
            let game = home
                .path()
                .join("game/prefix/drive_c/Program Files (x86)/Diablo II");
            fs::create_dir_all(&game).unwrap();
            let kept = KeptSaves::below(home.path());
            Self {
                _home: home,
                game,
                kept,
            }
        }

        fn root_path(&self) -> PathBuf {
            self.game.ancestors().nth(4).unwrap().to_path_buf()
        }

        fn root(&self) -> Directory {
            Directory::open(&self.root_path()).unwrap()
        }

        fn save(&self) -> PathBuf {
            self.game.join(SAVE)
        }

        fn write(&self, folder: &Path, name: &str, content: &[u8], age: u64) {
            fs::create_dir_all(folder).unwrap();
            let path = folder.join(name);
            fs::write(&path, content).unwrap();
            let file = File::options().write(true).open(&path).unwrap();
            file.set_modified(SystemTime::now() - Duration::from_secs(age))
                .unwrap();
        }

        fn keep(&self) -> io::Result<Kept> {
            keep_managed(&self.root(), &self.kept, now())
        }

        /// Every backup folder.
        fn backups(&self) -> Vec<PathBuf> {
            fs::read_dir(&self.kept.backups)
                .map(|entries| entries.map(|entry| entry.unwrap().path()).collect())
                .unwrap_or_default()
        }

        fn backed_up(&self, name: &str, content: &[u8]) -> bool {
            self.backups()
                .iter()
                .any(|folder| fs::read(folder.join(name)).is_ok_and(|found| found == content))
        }
    }

    fn now() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_800_000_000)
    }

    fn moved(characters: usize, files: usize) -> Kept {
        Kept::Moved {
            saves: Saves { characters, files },
            put_aside: PutAside::default(),
        }
    }

    fn saves(characters: usize, files: usize) -> Saves {
        Saves { characters, files }
    }

    fn links_to_kept(fixture: &Fixture) -> bool {
        fs::symlink_metadata(fixture.save()).is_ok_and(|metadata| metadata.file_type().is_symlink())
            && fs::read_link(fixture.save()).unwrap() == fixture.kept.path()
    }

    fn modified(path: &Path) -> SystemTime {
        fs::metadata(path).unwrap().modified().unwrap()
    }

    #[test]
    fn a_game_folder_without_save_links_it_to_the_kept_folder() {
        let fixture = Fixture::new();
        assert_eq!(fixture.keep().unwrap(), moved(0, 0));
        assert!(links_to_kept(&fixture));
        // Owner-only, as every folder the launcher makes.
        let mode = fs::metadata(fixture.kept.path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
        // Once linked, keeping again changes nothing.
        assert_eq!(fixture.keep().unwrap(), Kept::Linked);
        // Wine and the game write through the link into the kept folder.
        fs::write(fixture.save().join("Hero.d2s"), b"hero").unwrap();
        assert_eq!(
            fs::read(fixture.kept.path().join("Hero.d2s")).unwrap(),
            b"hero"
        );
    }

    #[test]
    fn no_game_folder_keeps_nothing() {
        let home = TempDir::new().unwrap();
        fs::create_dir_all(home.path().join("game/prefix")).unwrap();
        let root = Directory::open(&home.path().join("game")).unwrap();
        let kept = KeptSaves::below(home.path());
        assert_eq!(keep_managed(&root, &kept, now()).unwrap(), Kept::NoGame);
        assert!(!kept.path().exists());
    }

    #[test]
    fn a_save_folder_moves_its_characters_once_keeping_their_times() {
        let fixture = Fixture::new();
        for name in ["Hero.d2s", "Hero.key", "Hero.ma0"] {
            fixture.write(&fixture.save(), name, name.as_bytes(), 3600);
        }
        fixture.write(&fixture.save(), "pd2_shared.stash", b"stash", 60);
        let accessed = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        File::options()
            .write(true)
            .open(fixture.save().join("Hero.d2s"))
            .unwrap()
            .set_times(fs::FileTimes::new().set_accessed(accessed))
            .unwrap();
        let before = modified(&fixture.save().join("Hero.d2s"));
        assert_eq!(
            fixture.keep().unwrap(),
            moved(1, 1),
            "Hero, and the shared stash on its own"
        );
        assert!(links_to_kept(&fixture));
        for name in ["Hero.d2s", "Hero.key", "Hero.ma0", "pd2_shared.stash"] {
            assert!(fixture.kept.path().join(name).is_file(), "{name}");
        }
        let kept = fs::metadata(fixture.kept.path().join("Hero.d2s")).unwrap();
        assert_eq!(
            kept.modified().unwrap(),
            before,
            "the time stays, so a later merge picks the newer one"
        );
        assert_eq!(kept.accessed().unwrap(), accessed, "and the access time");
        assert!(!fixture.game.join(MOVED).exists(), "the folder is gone");
        assert!(!fixture.kept.backups.exists(), "nothing needed a backup");
        // The game sees them through the link.
        assert_eq!(
            fs::read(fixture.save().join("Hero.d2s")).unwrap(),
            b"Hero.d2s"
        );
        assert_eq!(fixture.keep().unwrap(), Kept::Linked);
    }

    #[test]
    fn a_character_is_never_kept_twice_nor_overwritten() {
        let fixture = Fixture::new();
        let kept = fixture.kept.path().to_path_buf();
        // Kept already: the same Twin, an older Old, a newer New.
        fixture.write(&kept, "Twin.d2s", b"same", 100);
        fixture.write(&kept, "Old.d2s", b"kept old", 5000);
        fixture.write(&kept, "Old.key", b"kept old key", 5000);
        fixture.write(&kept, "New.d2s", b"kept new", 10);
        // The game folder's: the same Twin, a newer Old, an older New.
        fixture.write(&fixture.save(), "Twin.d2s", b"same", 50);
        fixture.write(&fixture.save(), "Old.d2s", b"game newer", 10);
        fixture.write(&fixture.save(), "New.d2s", b"game older", 5000);
        let Kept::Moved { saves, put_aside } = fixture.keep().unwrap() else {
            panic!("moved");
        };
        assert_eq!(saves.characters, 3);
        assert_eq!(put_aside.characters, 2, "Old and New had another copy");
        assert_eq!(put_aside.files, 0);
        assert_eq!(put_aside.others, 0);
        assert!(put_aside.folder.unwrap().starts_with(&fixture.kept.backups));
        let read = |path: PathBuf| fs::read(path).unwrap();
        assert_eq!(read(kept.join("Twin.d2s")), b"same");
        assert_eq!(read(kept.join("Old.d2s")), b"game newer", "the newer stays");
        assert!(
            !kept.join("Old.key").exists(),
            "the older one goes as a whole"
        );
        assert_eq!(read(kept.join("New.d2s")), b"kept new", "the newer stays");
        // The other one of each is in a backup, never lost.
        assert!(fixture.backed_up("Old.d2s", b"kept old"));
        assert!(fixture.backed_up("Old.key", b"kept old key"));
        assert!(fixture.backed_up("New.d2s", b"game older"));
        assert!(
            !fixture.backed_up("Twin.d2s", b"same"),
            "the same one is not kept twice"
        );
    }

    #[test]
    fn a_character_is_dated_by_its_d2s_and_a_tie_keeps_the_kept_one() {
        let fixture = Fixture::new();
        let kept = fixture.kept.path().to_path_buf();
        // Kept: saved 10 s ago. The game folder's (a re-imported copy): its
        // .d2s from long ago, a companion file touched since.
        fixture.write(&kept, "Hero.d2s", b"kept newer", 10);
        fixture.write(&fixture.save(), "Hero.d2s", b"imported older", 5000);
        fixture.write(&fixture.save(), "Hero.key", b"touched", 1);
        // A copy with a .d2s comes before one without (a map left behind).
        fixture.write(&kept, "Ghost.d2s", b"ghost", 5000);
        fixture.write(&fixture.save(), "Ghost.map", b"map", 1);
        // No .d2s on either side: the latest file decides.
        fixture.write(&kept, "pd2_shared.stash", b"kept stash", 5000);
        fixture.write(&fixture.save(), "pd2_shared.stash", b"game stash", 10);
        // The same time: the kept one stays.
        fixture.write(&kept, "Tie.d2s", b"kept tie", 0);
        fixture.write(&fixture.save(), "Tie.d2s", b"game tie", 0);
        let tie = modified(&kept.join("Tie.d2s"));
        File::options()
            .write(true)
            .open(fixture.save().join("Tie.d2s"))
            .unwrap()
            .set_modified(tie)
            .unwrap();
        let put_aside = fixture.keep().unwrap().put_aside();
        assert_eq!(put_aside.characters, 3, "Hero, Ghost and Tie");
        assert_eq!(put_aside.files, 1, "the shared stash is no character");
        let read = |name: &str| fs::read(kept.join(name)).unwrap();
        assert_eq!(read("Hero.d2s"), b"kept newer", "dated by the .d2s");
        assert!(!kept.join("Hero.key").exists());
        assert!(fixture.backed_up("Hero.d2s", b"imported older"));
        assert!(fixture.backed_up("Hero.key", b"touched"));
        assert_eq!(read("Ghost.d2s"), b"ghost");
        assert!(fixture.backed_up("Ghost.map", b"map"));
        assert_eq!(read("pd2_shared.stash"), b"game stash");
        assert!(fixture.backed_up("pd2_shared.stash", b"kept stash"));
        assert_eq!(read("Tie.d2s"), b"kept tie");
        assert!(fixture.backed_up("Tie.d2s", b"game tie"));
    }

    #[test]
    fn a_folder_whose_removal_stopped_part_way_puts_nothing_aside() {
        let fixture = Fixture::new();
        let kept = fixture.kept.path().to_path_buf();
        fixture.write(&kept, "Hero.d2s", b"hero", 10);
        fixture.write(&kept, "Hero.key", b"key", 10);
        // Only the key is left of the stepped-aside folder.
        fixture.write(&fixture.game.join(MOVED), "Hero.key", b"key", 10);
        fixture.write(&fixture.save(), "Other.d2s", b"other", 0);
        assert_eq!(fixture.keep().unwrap().put_aside(), PutAside::default());
        assert!(!fixture.kept.backups.exists());
        assert_eq!(fs::read(kept.join("Hero.d2s")).unwrap(), b"hero");
    }

    #[test]
    fn the_rest_of_a_save_folder_is_backed_up_and_a_link_in_it_never_followed() {
        let fixture = Fixture::new();
        let outside = fixture.game.parent().unwrap().join("outside");
        fixture.write(&outside, "secret.txt", b"not the player's", 0);
        fixture.write(&fixture.save(), "Hero.d2s", b"hero", 0);
        fixture.write(&fixture.save(), ".hidden", b"hidden", 0);
        fixture.write(&fixture.save().join(".sync"), "state", b"sync", 0);
        fixture.write(&fixture.save().join("old"), "Ghost.d2s", b"ghost", 0);
        std::os::unix::fs::symlink(&outside, fixture.save().join("elsewhere")).unwrap();
        let put_aside = fixture.keep().unwrap().put_aside();
        assert_eq!(put_aside.characters, 0);
        assert_eq!(put_aside.files, 0);
        assert_eq!(put_aside.others, 3, ".hidden, .sync/ and the link");
        assert!(links_to_kept(&fixture));
        assert!(fixture.kept.path().join("Hero.d2s").is_file());
        // A folder is kept with the characters.
        assert_eq!(
            fs::read(fixture.kept.path().join("old/Ghost.d2s")).unwrap(),
            b"ghost"
        );
        let backup = fixture.backups()[0].join(SAVE);
        assert_eq!(put_aside.folder.as_deref(), backup.parent());
        assert_eq!(fs::read(backup.join(".hidden")).unwrap(), b"hidden");
        assert_eq!(fs::read(backup.join(".sync/state")).unwrap(), b"sync");
        assert!(!backup.join("old").exists());
        assert_eq!(
            fs::read_link(backup.join("elsewhere")).unwrap(),
            outside,
            "a link is kept as a link"
        );
        assert!(!fixture.kept.path().join("elsewhere").exists());
        assert_eq!(
            fs::read(outside.join("secret.txt")).unwrap(),
            b"not the player's",
            "what a link pointed at is left as it was"
        );
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 1);
    }

    #[test]
    fn a_folder_too_deep_to_keep_fails_the_keep_and_nothing_is_removed() {
        let fixture = Fixture::new();
        fixture.write(&fixture.save(), "Hero.d2s", b"hero", 0);
        let mut deep = fixture.save();
        for level in 0..=MAX_COPY_DEPTH {
            deep.push(format!("d{level}"));
        }
        fixture.write(&deep, "Deep.d2s", b"deep", 0);
        assert!(fixture.keep().is_err());
        assert!(!links_to_kept(&fixture));
        assert_eq!(fs::read(deep.join("Deep.d2s")).unwrap(), b"deep");
        assert_eq!(fs::read(fixture.save().join("Hero.d2s")).unwrap(), b"hero");
        // Found before anything was copied.
        assert_eq!(fs::read_dir(fixture.kept.path()).unwrap().count(), 0);
        assert!(!fixture.kept.backups.exists());
    }

    #[test]
    fn a_folder_in_save_is_kept_with_the_characters() {
        let fixture = Fixture::new();
        // PD2 keeps the online characters' hotkeys and automaps here.
        let online = fixture.save().join("ProjectD2");
        fixture.write(&fixture.save(), "Hero.d2s", b"hero", 0);
        fixture.write(&online, "Realm.key", b"hotkeys", 3600);
        fixture.write(&online, "Realm.ma0", b"automap", 60);
        fixture.write(&online, "Realm.map", b"map", 60);
        let before = modified(&online.join("Realm.key"));
        assert_eq!(fixture.keep().unwrap(), moved(1, 3));
        assert!(links_to_kept(&fixture));
        let kept = fixture.kept.path().join("ProjectD2");
        for (name, content) in [
            ("Realm.key", &b"hotkeys"[..]),
            ("Realm.ma0", b"automap"),
            ("Realm.map", b"map"),
        ] {
            assert_eq!(fs::read(kept.join(name)).unwrap(), content, "{name}");
        }
        assert_eq!(modified(&kept.join("Realm.key")), before, "its time stays");
        let mode = fs::metadata(&kept).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "owner-only");
        assert!(!fixture.kept.backups.exists(), "nothing needed a backup");
        // The game sees them through the link.
        assert_eq!(
            fs::read(fixture.save().join("ProjectD2/Realm.key")).unwrap(),
            b"hotkeys"
        );
        assert_eq!(fixture.keep().unwrap(), Kept::Linked);
    }

    #[test]
    fn a_folder_in_save_merges_file_by_file_with_the_kept_one() {
        let fixture = Fixture::new();
        let kept = fixture.kept.path().join("ProjectD2");
        // Kept already: a newer A, an older B, the same C, a D of its own, a
        // T saved at the same time.
        fixture.write(&kept, "A.key", b"kept newer", 10);
        fixture.write(&kept, "B.key", b"kept older", 5000);
        fixture.write(&kept, "C.key", b"same", 100);
        fixture.write(&kept, "D.key", b"kept only", 100);
        fixture.write(&kept, "T.map", b"kept tie", 0);
        let online = fixture.save().join("ProjectD2");
        fixture.write(&online, "A.key", b"game older", 5000);
        fixture.write(&online, "B.key", b"game newer", 10);
        fixture.write(&online, "C.key", b"same", 50);
        fixture.write(&online, "T.map", b"game tie", 0);
        File::options()
            .write(true)
            .open(online.join("T.map"))
            .unwrap()
            .set_modified(modified(&kept.join("T.map")))
            .unwrap();
        // Another spelling of Save merges into the same folder.
        fixture.write(&fixture.game.join("save/ProjectD2"), "E.key", b"new", 0);
        let Kept::Moved { saves, put_aside } = fixture.keep().unwrap() else {
            panic!("moved");
        };
        assert_eq!(saves, self::saves(0, 5));
        assert_eq!(put_aside.characters, 0, "none of them is a character");
        assert_eq!(put_aside.files, 3, "A, B and T had another copy");
        assert_eq!(put_aside.others, 0);
        assert!(links_to_kept(&fixture));
        assert!(!fixture.game.join("save").exists());
        let read = |name: &str| fs::read(kept.join(name)).unwrap();
        assert_eq!(read("A.key"), b"kept newer", "the newer stays");
        assert_eq!(read("B.key"), b"game newer", "the newer stays");
        assert_eq!(read("C.key"), b"same");
        assert_eq!(read("D.key"), b"kept only");
        assert_eq!(read("E.key"), b"new");
        assert_eq!(read("T.map"), b"kept tie", "a tie keeps the kept one");
        // Each other copy is in a backup, laid out as the kept folder is.
        assert!(fixture.backed_up("ProjectD2/A.key", b"game older"));
        assert!(fixture.backed_up("ProjectD2/B.key", b"kept older"));
        assert!(fixture.backed_up("ProjectD2/T.map", b"game tie"));
        assert!(
            !fixture.backed_up("ProjectD2/C.key", b"same"),
            "the same one is not kept twice"
        );
        assert_eq!(
            put_aside.folder.as_deref(),
            Some(fixture.backups()[0].as_path())
        );
    }

    #[test]
    fn nested_folders_are_kept_down_to_the_depth_limit() {
        let fixture = Fixture::new();
        let mut deep = fixture.save();
        let mut relative = PathBuf::new();
        for level in 0..MAX_COPY_DEPTH {
            deep.push(format!("d{level}"));
            relative.push(format!("d{level}"));
            fixture.write(&deep, "level.map", level.to_string().as_bytes(), 0);
        }
        fixture.write(&deep, "Deepest.key", b"deepest", 0);
        // Kept already, two levels down: an older copy.
        fixture.write(
            &fixture.kept.path().join("d0/d1"),
            "level.map",
            b"old",
            5000,
        );
        let Kept::Moved { saves, put_aside } = fixture.keep().unwrap() else {
            panic!("moved");
        };
        assert_eq!(saves, self::saves(0, MAX_COPY_DEPTH + 1));
        assert_eq!(put_aside.files, 1);
        assert!(links_to_kept(&fixture));
        assert_eq!(
            fs::read(fixture.kept.path().join(&relative).join("Deepest.key")).unwrap(),
            b"deepest"
        );
        assert_eq!(
            fs::read(fixture.kept.path().join("d0/d1/level.map")).unwrap(),
            b"1"
        );
        assert!(fixture.backed_up("d0/d1/level.map", b"old"));
    }

    #[test]
    fn a_link_in_a_folder_of_save_is_never_followed() {
        let fixture = Fixture::new();
        let outside = fixture.game.parent().unwrap().join("outside");
        fixture.write(&outside, "Stranger.key", b"not the player's", 0);
        let online = fixture.save().join("ProjectD2");
        fixture.write(&online, "Realm.key", b"hotkeys", 0);
        fixture.write(&online, ".hidden", b"hidden", 0);
        std::os::unix::fs::symlink(&outside, online.join("folder")).unwrap();
        std::os::unix::fs::symlink(outside.join("Stranger.key"), online.join("Stranger.key"))
            .unwrap();
        let put_aside = fixture.keep().unwrap().put_aside();
        assert_eq!(put_aside.others, 3, ".hidden and the two links");
        let kept = fixture.kept.path().join("ProjectD2");
        assert_eq!(fs::read(kept.join("Realm.key")).unwrap(), b"hotkeys");
        assert!(fs::symlink_metadata(kept.join("folder")).is_err());
        assert!(fs::symlink_metadata(kept.join("Stranger.key")).is_err());
        let backup = fixture.backups()[0].join(SAVE).join("ProjectD2");
        assert_eq!(fs::read(backup.join(".hidden")).unwrap(), b"hidden");
        assert_eq!(fs::read_link(backup.join("folder")).unwrap(), outside);
        assert_eq!(
            fs::read_link(backup.join("Stranger.key")).unwrap(),
            outside.join("Stranger.key")
        );
        assert_eq!(
            fs::read(outside.join("Stranger.key")).unwrap(),
            b"not the player's",
            "what a link pointed at is left as it was"
        );
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 1);

        // Where the kept folder holds another kind of thing by a name (a
        // file for a folder, a folder or a link for a file), it stays and
        // the incoming one goes to a backup whole.
        let fixture = Fixture::new();
        fixture.write(fixture.kept.path(), "Other", b"a kept file", 0);
        fixture.write(
            &fixture.kept.path().join("ProjectD2/Realm.key"),
            "x",
            b"x",
            0,
        );
        std::os::unix::fs::symlink(&outside, fixture.kept.path().join("Linked")).unwrap();
        fixture.write(&fixture.save().join("Other"), "Inside.key", b"inside", 0);
        fixture.write(
            &fixture.save().join("ProjectD2"),
            "Realm.key",
            b"hotkeys",
            0,
        );
        fixture.write(&fixture.save().join("Linked"), "Through.key", b"through", 0);
        let put_aside = fixture.keep().unwrap().put_aside();
        assert_eq!(put_aside.others, 3);
        assert_eq!(
            fs::read(fixture.kept.path().join("Other")).unwrap(),
            b"a kept file"
        );
        assert!(fixture.kept.path().join("ProjectD2/Realm.key").is_dir());
        assert!(
            !outside.join("Through.key").exists(),
            "a kept link is not followed"
        );
        let backup = fixture.backups()[0].join(SAVE);
        assert_eq!(
            fs::read(backup.join("Other/Inside.key")).unwrap(),
            b"inside"
        );
        assert_eq!(
            fs::read(backup.join("ProjectD2/Realm.key")).unwrap(),
            b"hotkeys"
        );
        assert_eq!(
            fs::read(backup.join("Linked/Through.key")).unwrap(),
            b"through"
        );
    }

    #[test]
    fn a_folder_named_save_in_save_never_shares_a_backup_path_with_the_rest() {
        let fixture = Fixture::new();
        // In name order: a hidden file (the rest starts), a folder named
        // Save whose x/y has an older kept copy, and a folder x whose y the
        // kept folder holds as a folder (so it goes to the rest, at Save/x/y).
        fixture.write(&fixture.save(), ".hidden", b"hidden", 0);
        fixture.write(
            &fixture.kept.path().join("Save/x"),
            "y",
            b"kept older",
            5000,
        );
        fixture.write(&fixture.save().join("Save/x"), "y", b"game newer", 10);
        fixture.write(&fixture.kept.path().join("x/y"), "inside", b"kept", 0);
        fixture.write(&fixture.save().join("x"), "y", b"conflict", 0);
        let put_aside = fixture.keep().unwrap().put_aside();
        assert_eq!(put_aside.files, 1);
        assert_eq!(put_aside.others, 2);
        assert_eq!(
            fs::read(fixture.kept.path().join("Save/x/y")).unwrap(),
            b"game newer"
        );
        // Both copies are in backups, neither over the other.
        assert!(fixture.backed_up("Save/x/y", b"kept older"));
        assert!(fixture.backed_up("Save/x/y", b"conflict"));
        assert!(fixture.backed_up("Save/.hidden", b"hidden"));
        assert_eq!(fixture.backups().len(), 2);
    }

    #[test]
    fn a_folder_of_save_whose_keep_stopped_part_way_is_finished_with_nothing_aside() {
        let fixture = Fixture::new();
        let kept = fixture.kept.path().join("ProjectD2");
        fixture.write(&kept, "A.key", b"a", 10);
        // A keep stopped after the folder stepped aside: A is kept, B not
        // yet. A file of it has another name (a hard link).
        let aside = fixture.game.join(MOVED).join("ProjectD2");
        fixture.write(&aside, "A.key", b"a", 10);
        fixture.write(&aside, "B.key", b"b", 10);
        let other_name = fixture.game.join("B copy.key");
        fs::hard_link(aside.join("B.key"), &other_name).unwrap();
        assert_eq!(fixture.keep().unwrap(), moved(0, 2));
        assert!(links_to_kept(&fixture));
        assert!(!fixture.game.join(MOVED).exists());
        assert_eq!(fs::read(kept.join("B.key")).unwrap(), b"b");
        assert!(!fixture.kept.backups.exists());
        let copy = fs::metadata(kept.join("B.key")).unwrap();
        assert_eq!(copy.nlink(), 1, "the kept copy is a file of its own");
        assert_eq!(fs::read(&other_name).unwrap(), b"b", "the other name stays");
    }

    #[test]
    fn a_save_file_with_other_names_is_kept() {
        let fixture = Fixture::new();
        fixture.write(&fixture.save(), "Hero.d2s", b"hero", 0);
        fs::hard_link(
            fixture.save().join("Hero.d2s"),
            fixture.game.join("Hero copy.d2s"),
        )
        .unwrap();
        assert_eq!(fixture.keep().unwrap(), moved(1, 0));
        assert_eq!(
            fs::read(fixture.kept.path().join("Hero.d2s")).unwrap(),
            b"hero"
        );
        assert_eq!(
            fs::read(fixture.game.join("Hero copy.d2s")).unwrap(),
            b"hero",
            "its other name is left as it was"
        );
    }

    #[test]
    fn save_in_another_case_is_kept_and_only_the_link_is_left() {
        let fixture = Fixture::new();
        // A copy keeps `save`; Wine finds it as the game's Save.
        fixture.write(&fixture.game.join("save"), "Hero.d2s", b"hero", 0);
        fixture.write(&fixture.game.join("SAVE"), "Other.d2s", b"other", 0);
        fixture.write(&fixture.save(), "Third.d2s", b"third", 0);
        let theirs = fixture.game.parent().unwrap().join("theirs");
        fs::create_dir_all(&theirs).unwrap();
        std::os::unix::fs::symlink(&theirs, fixture.game.join("sAvE")).unwrap();
        assert_eq!(fixture.keep().unwrap(), moved(3, 0));
        assert!(links_to_kept(&fixture));
        for name in ["Hero.d2s", "Other.d2s", "Third.d2s"] {
            assert!(fixture.kept.path().join(name).is_file(), "{name}");
        }
        assert!(!fixture.game.join("save").exists());
        assert!(!fixture.game.join("SAVE").exists());
        assert!(!fixture.game.join(MOVED).exists());
        assert!(
            fs::symlink_metadata(fixture.game.join("sAvE"))
                .unwrap()
                .file_type()
                .is_symlink(),
            "a link holds nothing itself and is left"
        );
        assert_eq!(fixture.keep().unwrap(), Kept::Linked);

        // An old installation's every Save folder is rescued too.
        let fixture = Fixture::new();
        fixture.write(&fixture.game.join("save"), "Hero.d2s", b"hero", 0);
        let (saves, _) = rescue_managed(&fixture.root(), &fixture.kept, now()).unwrap();
        assert_eq!(saves, self::saves(1, 0));
        assert!(fixture.kept.path().join("Hero.d2s").is_file());
    }

    #[test]
    fn a_link_elsewhere_is_replaced_and_its_target_left_alone() {
        let fixture = Fixture::new();
        let theirs = fixture.game.parent().unwrap().join("their saves");
        fixture.write(&theirs, "Mine.d2s", b"mine", 0);
        std::os::unix::fs::symlink(&theirs, fixture.save()).unwrap();
        fixture.keep().unwrap();
        assert!(links_to_kept(&fixture));
        assert_eq!(fs::read(theirs.join("Mine.d2s")).unwrap(), b"mine");
    }

    #[test]
    fn a_keep_that_fails_changes_nothing_and_a_stopped_one_finishes_later() {
        let fixture = Fixture::new();
        fixture.write(&fixture.save(), "Hero.d2s", b"hero", 0);
        // The kept folder can't be written: nothing moves, the game keeps
        // its folder and its characters.
        fs::create_dir_all(fixture.kept.path()).unwrap();
        fs::set_permissions(fixture.kept.path(), fs::Permissions::from_mode(0o500)).unwrap();
        assert!(fixture.keep().is_err());
        assert!(fixture.save().is_dir());
        assert!(
            !fs::symlink_metadata(fixture.save())
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(fixture.save().join("Hero.d2s")).unwrap(), b"hero");
        fs::set_permissions(fixture.kept.path(), fs::Permissions::from_mode(0o700)).unwrap();

        // A keep stopped after the folder stepped aside, before the link:
        // the next keep keeps what it holds, links, and removes it.
        fixture.write(fixture.kept.path(), "Hero.d2s", b"hero", 0);
        fs::rename(fixture.save(), fixture.game.join(MOVED)).unwrap();
        fixture.write(&fixture.game.join(MOVED), "Late.d2s", b"late", 0);
        assert_eq!(fixture.keep().unwrap(), moved(2, 0));
        assert!(links_to_kept(&fixture));
        assert!(!fixture.game.join(MOVED).exists());
        assert_eq!(
            fs::read(fixture.kept.path().join("Late.d2s")).unwrap(),
            b"late"
        );

        // A stop part way through the copies: what was not copied yet is
        // still in the game folder, and the next keep copies it.
        let fixture = Fixture::new();
        fixture.write(&fixture.save(), "A.d2s", b"a", 0);
        fixture.write(&fixture.save(), "B.d2s", b"b", 0);
        fixture.write(fixture.kept.path(), "A.d2s", b"a", 0);
        fixture.keep().unwrap();
        assert_eq!(fs::read(fixture.kept.path().join("B.d2s")).unwrap(), b"b");
        assert!(!fixture.kept.backups.exists());
    }

    #[test]
    fn before_play_a_save_stepped_aside_is_put_back_or_the_link_made_again() {
        let fixture = Fixture::new();
        fixture.write(&fixture.save(), "Hero.d2s", b"hero", 0);
        // A keep that stopped after stepping the folder aside, whose rename
        // back failed too.
        fs::rename(fixture.save(), fixture.game.join(MOVED)).unwrap();
        restore_managed(&fixture.root(), &fixture.kept).unwrap();
        assert_eq!(fs::read(fixture.save().join("Hero.d2s")).unwrap(), b"hero");
        assert!(!fixture.game.join(MOVED).exists());
        // A Save already there is left as it is.
        restore_managed(&fixture.root(), &fixture.kept).unwrap();
        assert!(fixture.save().is_dir());

        // No Save at all: the link is made again.
        fs::remove_dir_all(fixture.save()).unwrap();
        restore_managed(&fixture.root(), &fixture.kept).unwrap();
        assert!(links_to_kept(&fixture));

        // When even the link can't be made, the game makes its own Save, as
        // before any keep.
        fs::remove_file(fixture.save()).unwrap();
        fs::set_permissions(&fixture.game, fs::Permissions::from_mode(0o500)).unwrap();
        restore_managed(&fixture.root(), &fixture.kept).unwrap();
        // Only a folder stepped aside that can't be put back stops the game.
        fs::set_permissions(&fixture.game, fs::Permissions::from_mode(0o700)).unwrap();
        fixture.write(&fixture.game.join(MOVED), "Hero.d2s", b"hero", 0);
        fs::set_permissions(&fixture.game, fs::Permissions::from_mode(0o500)).unwrap();
        assert!(restore_managed(&fixture.root(), &fixture.kept).is_err());
        fs::set_permissions(&fixture.game, fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn a_kept_folder_the_player_made_a_link_never_stops_play_but_stops_start_over() {
        let fixture = Fixture::new();
        // userdata/Save is the player's link to a synced folder.
        let synced = fixture.game.parent().unwrap().join("synced");
        fixture.write(&synced, "Kept.d2s", b"kept", 0);
        fs::create_dir_all(fixture.kept.path().parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&synced, fixture.kept.path()).unwrap();

        // No Save yet: the keep fails safely, and the link is made for the
        // game, which finds the characters through both links.
        assert!(fixture.keep().is_err());
        restore_managed(&fixture.root(), &fixture.kept).unwrap();
        assert!(links_to_kept(&fixture));
        assert_eq!(fs::read(fixture.save().join("Kept.d2s")).unwrap(), b"kept");

        // A Save folder: the keep fails and changes nothing (so Start over
        // does not run); the game keeps its folder.
        fs::remove_file(fixture.save()).unwrap();
        fixture.write(&fixture.save(), "Hero.d2s", b"hero", 0);
        assert!(fixture.keep().is_err());
        restore_managed(&fixture.root(), &fixture.kept).unwrap();
        assert_eq!(fs::read(fixture.save().join("Hero.d2s")).unwrap(), b"hero");
        assert!(!synced.join("Hero.d2s").exists());
        assert_eq!(fs::read(synced.join("Kept.d2s")).unwrap(), b"kept");
    }

    #[test]
    fn a_character_is_dated_by_its_own_d2s_not_another_beside_it() {
        let fixture = Fixture::new();
        let kept = fixture.kept.path().to_path_buf();
        // Kept: Hero.d2s saved long ago, a backup copy beside it touched
        // since. The game folder's Hero.d2s is newer than the kept one.
        fixture.write(&kept, "Hero.d2s", b"kept hero", 5000);
        fixture.write(&kept, "Hero.bak.d2s", b"kept backup", 1);
        fixture.write(&fixture.save(), "Hero.d2s", b"game hero", 100);
        // Only another .d2s and no own one: dated by the latest file, and a
        // copy with its own .d2s comes first.
        fixture.write(&kept, "Ghost.bak.d2s", b"ghost backup", 1);
        fixture.write(&fixture.save(), "Ghost.d2s", b"game ghost", 5000);
        assert_eq!(fixture.keep().unwrap().put_aside().characters, 2);
        assert_eq!(fs::read(kept.join("Hero.d2s")).unwrap(), b"game hero");
        assert!(fixture.backed_up("Hero.d2s", b"kept hero"));
        assert!(fixture.backed_up("Hero.bak.d2s", b"kept backup"));
        assert_eq!(fs::read(kept.join("Ghost.d2s")).unwrap(), b"game ghost");
        assert!(fixture.backed_up("Ghost.bak.d2s", b"ghost backup"));
    }

    #[test]
    fn start_overs_removal_follows_no_link_so_the_kept_characters_stay() {
        let fixture = Fixture::new();
        fixture.write(&fixture.save(), "Hero.d2s", b"hero", 0);
        fixture.keep().unwrap();
        // Start over's removal of the Managed prefix.
        fixture
            .root()
            .remove_owned_tree(OsStr::new("prefix"))
            .unwrap();
        assert!(!fixture.game.exists());
        assert_eq!(
            fs::read(fixture.kept.path().join("Hero.d2s")).unwrap(),
            b"hero"
        );
        // A new installation links to the same characters.
        fs::create_dir_all(&fixture.game).unwrap();
        assert_eq!(fixture.keep().unwrap(), moved(0, 0));
        assert_eq!(fs::read(fixture.save().join("Hero.d2s")).unwrap(), b"hero");
    }

    #[test]
    fn an_installation_about_to_be_removed_has_its_characters_rescued() {
        let fixture = Fixture::new();
        fixture.write(&fixture.save(), "Hero.d2s", b"hero", 0);
        assert_eq!(
            rescue_managed(&fixture.root(), &fixture.kept, now()).unwrap(),
            (saves(1, 0), PutAside::default())
        );
        assert_eq!(
            fs::read(fixture.kept.path().join("Hero.d2s")).unwrap(),
            b"hero"
        );
        // Nothing is linked in a folder about to go.
        assert!(fixture.save().is_dir());
        assert!(
            !fs::symlink_metadata(fixture.save())
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    fn write_registry(fixture: &Fixture, save_path: &str) {
        let registry = format!(
            "WINE REGISTRY Version 2\n\n[Software\\\\Blizzard Entertainment\\\\Diablo II] 1800000000\n#time=1dc0\n\"InstallPath\"=\"C:\\\\Program Files (x86)\\\\Diablo II\"\n\"Save Path\"={save_path}\n\n[Software\\\\Wine] 1800000000\n\"Save Path\"=\"C:\\\\elsewhere\"\n"
        );
        fs::write(fixture.root_path().join("prefix/user.reg"), registry).unwrap();
    }

    #[test]
    fn a_save_path_inside_the_prefix_has_its_characters_kept_before_removal() {
        let fixture = Fixture::new();
        let saved = fixture
            .root_path()
            .join("prefix/drive_c/users/steamuser/Saved Games/Diablo II");
        fixture.write(&saved, "Hero.d2s", b"hero", 0);
        fixture.write(&saved, "Hero.key", b"key", 0);
        fixture.write(&saved, "pd2_shared.stash", b"stash", 0);
        // Not a character: such a folder need not be the game's alone.
        fixture.write(&saved, "notes.txt", b"notes", 0);
        fixture.write(&saved.join("old"), "Old.d2s", b"old", 0);
        // As the registry spells it, in another case.
        write_registry(
            &fixture,
            r#""C:\\USERS\\steamuser\\saved games\\Diablo II\\""#,
        );
        let keep = || keep_save_path(&fixture.root(), &fixture.root_path(), &fixture.kept, now());
        assert_eq!(keep().unwrap(), (saves(1, 1), PutAside::default()));
        for name in ["Hero.d2s", "Hero.key", "pd2_shared.stash"] {
            assert!(fixture.kept.path().join(name).is_file(), "{name}");
        }
        assert!(!fixture.kept.path().join("notes.txt").exists());
        assert!(!fixture.kept.backups.exists());
        assert!(saved.join("Hero.d2s").is_file(), "the source is only read");

        // The same folder by its host path through Z:.
        fs::remove_dir_all(fixture.kept.path()).unwrap();
        let host = format!(
            "\"Z:{}\\\\prefix\\\\drive_c\\\\users\\\\steamuser\\\\Saved Games\\\\Diablo II\"",
            fixture.root_path().to_str().unwrap().replace('/', "\\\\")
        );
        write_registry(&fixture, &host);
        assert_eq!(keep().unwrap().0, saves(1, 1));

        // The Managed game folder's own Save, a folder outside the root,
        // another drive, or another kind of value: nothing here.
        for path in [
            r#""C:\\Program Files (x86)\\Diablo II\\Save\\""#,
            r#""Z:\\home\\someone\\Diablo II\\Save""#,
            r#""D:\\Saves""#,
            "dword:00000001",
        ] {
            write_registry(&fixture, path);
            assert_eq!(
                keep().unwrap(),
                (saves(0, 0), PutAside::default()),
                "{path}"
            );
        }
        // No registry at all.
        fs::remove_file(fixture.root_path().join("prefix/user.reg")).unwrap();
        assert_eq!(keep().unwrap(), (saves(0, 0), PutAside::default()));
    }

    #[test]
    fn registry_strings_are_read_as_wine_writes_them() {
        assert_eq!(
            registry_string(r#""C:\\users\\J\xf6rg\\Saved Games\\""#).as_deref(),
            Some("C:\\users\\Jörg\\Saved Games\\")
        );
        assert_eq!(
            registry_string(r#"str(2):"%USERPROFILE%\\x""#).as_deref(),
            Some("%USERPROFILE%\\x")
        );
        assert_eq!(
            registry_string(r#""a \"quoted\" \x00e9\x0031""#).as_deref(),
            Some("a \"quoted\" é1")
        );
        assert_eq!(registry_string("hex:00,01"), None);
        assert_eq!(registry_string(r#""unterminated"#), None);
        assert_eq!(
            below_root(r"C:\users\x\..\y", Path::new("/r")),
            None,
            "a path that climbs is not resolved"
        );
        assert_eq!(
            below_root(r"z:/R/prefix/drive_c", Path::new("/r")),
            Some(vec!["prefix".to_owned(), "drive_c".to_owned()])
        );
        assert_eq!(below_root(r"\\server\share", Path::new("/r")), None);
    }
}
