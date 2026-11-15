//! The built-in FEX: FEX's own build of FEX for Ubuntu 22.04,
//! in the CPU level this device supports, and FEX's Ubuntu 24.04 RootFS,
//! kept in the managed root and used only by the Launcher's Wine.
//!
//! Everything lives in `<managed root>/fex`: the two FEX programs in
//! `bin/`, the unpacked RootFS in `rootfs/`, and FEX's own configuration,
//! data, and cache folders. FEX gets its own data folder and server socket
//! name, so a FEXServer the device already runs (which serves the device's
//! own RootFS to every client that reaches it) never answers it.

use std::{
    ffi::OsString,
    fs::{self, File},
    io::{self, BufReader, Read, Write},
    os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _},
    path::{Component, Path, PathBuf},
};

use sha2::{Digest as _, Sha256};

/// The folder in the managed root.
pub(crate) const DIRECTORY: &str = "fex";
const RECORD: &str = "installed.json";
const RECORD_SCHEMA: u32 = 1;
const ROOTFS_NAME: &str = "Ubuntu_24_04";
const STAGING: &str = "fex.staging";
const RETIRED: &str = "fex.retired";

/// Every file the built-in FEX downloads, named in one place: FEX's release,
/// and for each file its name on the project's mirror release, its size and
/// SHA-256, and its upstream sources. A file is downloaded from the mirror
/// first, then from each upstream source in order; every source must give
/// the same bytes. The public repository's mirror workflow
/// (`.github/workflows/mirror-fex.yml`) fills the mirror release from the
/// upstream sources with the same checks.
const PINS_JSON: &str = include_str!("builtin_fex_pins.json");

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Pins {
    schema: u32,
    fex_version: &'static str,
    mirror: Mirror,
    files: Vec<PinnedFile>,
}

/// The GitHub release that holds a copy of every pinned file, unchanged.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Mirror {
    repository: &'static str,
    tag: &'static str,
}

/// One pinned file of the built-in FEX.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PinnedFile {
    id: &'static str,
    /// Its name on the mirror release.
    name: &'static str,
    size: u64,
    sha256: &'static str,
    /// Where FEX publishes it, in the order they are tried after the mirror.
    upstream: Vec<&'static str>,
    #[serde(skip)]
    mirror_url: String,
}

impl PinnedFile {
    pub(crate) fn size(&self) -> u64 {
        self.size
    }

    pub(crate) fn sha256(&self) -> &'static str {
        self.sha256
    }

    /// The copy on the project's mirror release, tried first.
    pub(crate) fn mirror_url(&'static self) -> &'static str {
        &self.mirror_url
    }

    /// The upstream sources, tried in order after the mirror.
    pub(crate) fn upstream(&'static self) -> &'static [&'static str] {
        &self.upstream
    }
}

fn pins() -> &'static Pins {
    static PINS: std::sync::OnceLock<Pins> = std::sync::OnceLock::new();
    PINS.get_or_init(|| {
        let mut pins: Pins =
            serde_json::from_str(PINS_JSON).expect("the built-in FEX's pins are valid JSON");
        assert_eq!(pins.schema, 1, "the built-in FEX's pins have schema 1");
        for file in &mut pins.files {
            file.mirror_url = format!(
                "https://github.com/{}/releases/download/{}/{}",
                pins.mirror.repository, pins.mirror.tag, file.name
            );
        }
        pins
    })
}

fn pinned(id: &str) -> &'static PinnedFile {
    pins()
        .files
        .iter()
        .find(|file| file.id == id)
        .expect("every built-in FEX file is pinned")
}

/// FEX's release in every package.
pub(crate) fn fex_version() -> &'static str {
    pins().fex_version
}

/// FEX's Ubuntu 24.04 RootFS, from FEX's own server.
pub(crate) fn rootfs() -> &'static PinnedFile {
    pinned("rootfs")
}

pub(crate) const ROOTFS_CACHE_NAME: &str = "fex-rootfs-ubuntu-24.04-2026-08-11.verified";
/// The unpacked RootFS (1.89 GB), with room to spare for the programs and
/// FEX's caches.
const UNPACKED_BYTES: u64 = 2_100_000_000;

/// FEX's three builds of one release, each compiled for a CPU level
/// (`-march=armv8.0-a`, `armv8.2-a`, `armv8.4-a`).
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum FexVariant {
    Armv8_0,
    Armv8_2,
    Armv8_4,
}

/// The features a CPU level adds that a compiler may use for it, as Linux
/// names them in `/proc/cpuinfo`.
const ARMV8_2_FEATURES: &[&str] = &["atomics", "asimdrdm", "crc32", "dcpop"];
const ARMV8_4_FEATURES: &[&str] = &[
    "atomics", "asimdrdm", "crc32", "dcpop", "lrcpc", "jscvt", "fcma", "ilrcpc", "flagm", "uscat",
    "asimddp", "dit",
];

impl FexVariant {
    /// This CPU level's package.
    pub(crate) fn package(self) -> &'static PinnedFile {
        pinned(match self {
            Self::Armv8_0 => "fex-armv8.0",
            Self::Armv8_2 => "fex-armv8.2",
            Self::Armv8_4 => "fex-armv8.4",
        })
    }

    pub(crate) fn package_size(self) -> u64 {
        self.package().size()
    }

    pub(crate) fn package_sha256(self) -> &'static str {
        self.package().sha256()
    }

    pub(crate) const fn cache_name(self) -> &'static str {
        match self {
            Self::Armv8_0 => "fex-emu-armv8.0_2609.1.verified",
            Self::Armv8_2 => "fex-emu-armv8.2_2609.1.verified",
            Self::Armv8_4 => "fex-emu-armv8.4_2609.1.verified",
        }
    }

    #[cfg(test)]
    pub(crate) const ALL: [Self; 3] = [Self::Armv8_0, Self::Armv8_2, Self::Armv8_4];

    /// The highest CPU level whose features `features` (a `/proc/cpuinfo`
    /// `Features` line) all has.
    pub(crate) fn for_features(features: &str) -> Self {
        let present = features
            .split_once(':')
            .map_or(features, |(_, list)| list)
            .split_whitespace()
            .collect::<Vec<_>>();
        let has = |wanted: &[&str]| wanted.iter().all(|feature| present.contains(feature));
        if has(ARMV8_4_FEATURES) {
            Self::Armv8_4
        } else if has(ARMV8_2_FEATURES) {
            Self::Armv8_2
        } else {
            Self::Armv8_0
        }
    }

    /// This device's CPU level; the baseline build when it can't be read.
    pub(crate) fn current() -> Self {
        fs::read_to_string("/proc/cpuinfo")
            .ok()
            .and_then(|cpuinfo| {
                cpuinfo
                    .lines()
                    .find(|line| line.starts_with("Features"))
                    .map(Self::for_features)
            })
            .unwrap_or(Self::Armv8_0)
    }
}

/// Whether the built-in FEX is the one in use on this ARM64 build: see
/// [`chosen`].
pub(crate) fn in_use(
    settings: Option<&crate::persistence::LauncherSettings>,
    managed_root: &Path,
) -> bool {
    cfg!(target_arch = "aarch64") && chosen(settings, managed_root)
}

/// The player's choice, or, before they chose, whether `managed_root`
/// holds no installation yet (the step 1 default; an installation set up
/// with the device's FEX keeps it). The Download step saves the default as
/// the choice, since the root does not look fresh after it.
pub(crate) fn chosen(
    settings: Option<&crate::persistence::LauncherSettings>,
    managed_root: &Path,
) -> bool {
    use crate::persistence::FexSource;
    match settings {
        Some(settings) if settings.fex_source_chosen() => {
            settings.fex_source() == FexSource::Builtin
        }
        _ => fresh(managed_root),
    }
}

/// No prefix, runtime, or downloads in `managed_root` yet.
fn fresh(managed_root: &Path) -> bool {
    match crate::safe_fs::Directory::open(managed_root) {
        Ok(root) => crate::relocation::holds_installation(&root).is_ok_and(|holds| !holds),
        Err(error) => error.kind() == io::ErrorKind::NotFound,
    }
}

/// Where each part of the built-in FEX lives below a managed root.
pub(crate) struct Layout {
    directory: PathBuf,
}

impl Layout {
    pub(crate) fn new(managed_root: &Path) -> Self {
        Self {
            directory: managed_root.join(DIRECTORY),
        }
    }

    pub(crate) fn interpreter(&self) -> PathBuf {
        self.directory.join("bin/FEX")
    }

    fn rootfs(&self) -> PathBuf {
        self.directory.join("rootfs").join(ROOTFS_NAME)
    }

    /// The environment every guest gets from the built-in FEX.
    pub(crate) fn environment(&self) -> Vec<(OsString, OsString)> {
        let folder = |name: &str| {
            let mut path = self.directory.join(name).into_os_string();
            path.push("/");
            path
        };
        vec![
            (OsString::from("FEX_ROOTFS"), self.rootfs().into_os_string()),
            (OsString::from("FEX_APP_CONFIG_LOCATION"), folder("config")),
            (OsString::from("FEX_APP_DATA_LOCATION"), folder("data")),
            (OsString::from("FEX_APP_CACHE_LOCATION"), folder("cache")),
            (
                OsString::from("FEX_SERVERSOCKETPATH"),
                OsString::from(server_socket_name(&self.directory)),
            ),
        ]
    }
}

/// An abstract socket name of this installation's own FEXServer: never the
/// default `<uid>.FEXServer.Socket` a device FEX uses.
fn server_socket_name(directory: &Path) -> String {
    let digest = Sha256::digest(directory.as_os_str().as_encoded_bytes());
    let short = digest[..6]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!(
        "pd2launcher-{}-{short}.FEXServer.Socket",
        rustix::process::getuid().as_raw()
    )
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema: u32,
    fex_version: String,
    variant: FexVariant,
    package_sha256: String,
    rootfs_sha256: String,
}

impl Record {
    fn current(variant: FexVariant) -> Self {
        Self {
            schema: RECORD_SCHEMA,
            fex_version: fex_version().to_owned(),
            variant,
            package_sha256: variant.package_sha256().to_owned(),
            rootfs_sha256: rootfs().sha256().to_owned(),
        }
    }
}

/// Whether the built-in FEX this release pins for `variant` is installed
/// in `managed_root`, complete.
pub(crate) fn installed_as(managed_root: &Path, variant: FexVariant) -> bool {
    let layout = Layout::new(managed_root);
    let Ok(record) = fs::read(layout.directory.join(RECORD)) else {
        return false;
    };
    serde_json::from_slice::<Record>(&record).is_ok_and(|record| record == Record::current(variant))
        && [layout.interpreter(), layout.directory.join("bin/FEXServer")]
            .iter()
            .all(|program| {
                fs::metadata(program).is_ok_and(|metadata| {
                    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                })
            })
        && layout.rootfs().join("usr").is_dir()
}

/// [`installed_as`] for this device's CPU level.
pub(crate) fn installed(managed_root: &Path) -> bool {
    installed_as(managed_root, FexVariant::current())
}

/// Install the built-in FEX from its verified package and RootFS into
/// `managed_root`, replacing an installed one. It is assembled beside the
/// final folder and renamed into place, and its record is written last, so
/// a stop part way leaves no installation that counts as complete.
pub(crate) fn install(
    managed_root: &Path,
    variant: FexVariant,
    package: &Path,
    rootfs: &Path,
) -> io::Result<()> {
    install_with_space(managed_root, variant, package, rootfs, UNPACKED_BYTES)
}

fn install_with_space(
    managed_root: &Path,
    variant: FexVariant,
    package: &Path,
    rootfs: &Path,
    needed: u64,
) -> io::Result<()> {
    ensure_space(managed_root, needed)?;
    let staging = managed_root.join(STAGING);
    remove_if_present(&staging)?;
    fs::create_dir(&staging)?;
    let bin = staging.join("bin");
    fs::create_dir(&bin)?;
    extract_programs(package, &bin)?;
    let rootfs_parent = staging.join("rootfs");
    fs::create_dir(&rootfs_parent)?;
    unpack_squashfs(rootfs, &rootfs_parent.join(ROOTFS_NAME))?;
    for folder in ["config", "data", "cache"] {
        fs::create_dir(staging.join(folder))?;
    }
    let record = serde_json::to_vec_pretty(&Record::current(variant))
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "record encode failed"))?;
    let mut file = File::create(staging.join(RECORD))?;
    file.write_all(&record)?;
    file.sync_all()?;
    let directory = managed_root.join(DIRECTORY);
    let retired = managed_root.join(RETIRED);
    remove_if_present(&retired)?;
    if fs::symlink_metadata(&directory).is_ok() {
        fs::rename(&directory, &retired)?;
    }
    fs::rename(&staging, &directory)?;
    File::open(managed_root)?.sync_all()?;
    remove_if_present(&retired)
}

fn ensure_space(managed_root: &Path, needed: u64) -> io::Result<()> {
    let stat = rustix::fs::statvfs(managed_root).map_err(io::Error::from)?;
    let available = stat.f_bavail.saturating_mul(stat.f_frsize);
    if available < needed {
        return Err(io::Error::new(
            io::ErrorKind::StorageFull,
            "not enough free space for the built-in FEX",
        ));
    }
    Ok(())
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => remove_tree(path),
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// `remove_dir_all`, after making read-only folders writable (a RootFS has
/// some), so their contents can go.
fn remove_tree(path: &Path) -> io::Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            fs::set_permissions(entry.path(), fs::Permissions::from_mode(0o700))?;
            remove_tree(&entry.path())?;
        } else {
            fs::remove_file(entry.path())?;
        }
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    fs::remove_dir(path)
}

/// The two programs (`usr/bin/FEX`, `usr/bin/FEXServer`) from a `.deb`: an
/// `ar` archive whose `data.tar.zst` holds the package's files.
fn extract_programs(package: &Path, bin: &Path) -> io::Result<()> {
    const WANTED: [&str; 2] = ["usr/bin/FEX", "usr/bin/FEXServer"];
    let data = deb_member(package, "data.tar.zst")?;
    let decoder = zstd::stream::read::Decoder::new(data.as_slice())?;
    let mut archive = tar::Archive::new(decoder);
    let mut found = 0;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let name = path.strip_prefix(".").unwrap_or(&path);
        let Some(wanted) = WANTED.iter().find(|wanted| name == Path::new(wanted)) else {
            continue;
        };
        if !entry.header().entry_type().is_file() {
            return Err(invalid("a FEX program in the package is not a file"));
        }
        let target = bin.join(Path::new(wanted).file_name().expect("a program name"));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o755)
            .open(&target)?;
        io::copy(&mut entry, &mut file)?;
        file.sync_all()?;
        found += 1;
    }
    if found != WANTED.len() {
        return Err(invalid("the FEX package lacks FEX or FEXServer"));
    }
    Ok(())
}

/// The bytes of one member of an `ar` archive.
fn deb_member(package: &Path, name: &str) -> io::Result<Vec<u8>> {
    let mut file = BufReader::new(File::open(package)?);
    let mut magic = [0_u8; 8];
    file.read_exact(&mut magic)?;
    if &magic != b"!<arch>\n" {
        return Err(invalid("the FEX package is not a .deb"));
    }
    loop {
        let mut header = [0_u8; 60];
        if let Err(error) = file.read_exact(&mut header) {
            return Err(if error.kind() == io::ErrorKind::UnexpectedEof {
                invalid("the FEX package has no data.tar.zst")
            } else {
                error
            });
        }
        let member = String::from_utf8_lossy(&header[..16]);
        let member = member.trim_end().trim_end_matches('/');
        let size = String::from_utf8_lossy(&header[48..58])
            .trim()
            .parse::<u64>()
            .map_err(|_| invalid("the FEX package has a bad member size"))?;
        if member == name {
            let mut bytes = Vec::new();
            file.by_ref().take(size).read_to_end(&mut bytes)?;
            if bytes.len() as u64 != size {
                return Err(invalid("the FEX package ends early"));
            }
            return Ok(bytes);
        }
        // Members are padded to an even length.
        io::copy(&mut file.by_ref().take(size + size % 2), &mut io::sink())?;
    }
}

/// Unpack a SquashFS image into `target`, which must not exist: folders,
/// files, and symbolic links (kept as they are; FEX resolves them inside
/// the RootFS). Device nodes, pipes, and sockets are skipped, and set-user
/// and set-group bits dropped.
fn unpack_squashfs(image: &Path, target: &Path) -> io::Result<()> {
    let reader = BufReader::new(File::open(image)?);
    let filesystem = backhand::FilesystemReader::from_reader(reader)
        .map_err(|error| invalid_owned(format!("the RootFS cannot be read: {error}")))?;
    fs::create_dir(target)?;
    let mut folders = Vec::new();
    for node in filesystem.files() {
        let relative = safe_relative(&node.fullpath)?;
        let path = target.join(&relative);
        let mode = u32::from(node.header.permissions) & 0o1777;
        match &node.inner {
            backhand::InnerNode::Dir(_) => {
                if relative.as_os_str().is_empty() {
                    folders.push((path, mode));
                    continue;
                }
                fs::create_dir(&path)?;
                folders.push((path, mode));
            }
            backhand::InnerNode::File(file) => {
                let mut output = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&path)?;
                let mut input = filesystem.file(file).reader_checked().map_err(|error| {
                    invalid_owned(format!("a RootFS file cannot be read: {error}"))
                })?;
                io::copy(&mut input, &mut output)?;
                output.set_permissions(fs::Permissions::from_mode(mode))?;
            }
            backhand::InnerNode::Symlink(link) => {
                std::os::unix::fs::symlink(&link.link, &path)?;
            }
            _ => {}
        }
    }
    // Folders last, deepest first, so a read-only folder is filled first.
    for (path, mode) in folders.into_iter().rev() {
        fs::set_permissions(&path, fs::Permissions::from_mode(mode | 0o700))?;
    }
    Ok(())
}

/// A SquashFS path made relative, with nothing that could leave the target.
fn safe_relative(path: &Path) -> io::Result<PathBuf> {
    let mut relative = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(part) => relative.push(part),
            Component::ParentDir | Component::Prefix(_) => {
                return Err(invalid("the RootFS names a path outside itself"));
            }
        }
    }
    Ok(relative)
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn invalid_owned(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Snapdragon 8 Gen 2 (Cortex-X3/A715/A510).
    const SNAPDRAGON_8_GEN_2: &str = "Features\t: fp asimd evtstrm aes pmull sha1 sha2 crc32 atomics fphp asimdhp cpuid asimdrdm jscvt fcma lrcpc dcpop sha3 sm3 sm4 asimddp sha512 asimdfhm dit uscat ilrcpc flagm ssbs sb paca pacg dcpodp flagm2 frint i8mm bf16 dgh bti";
    /// A Cortex-A55 (ARMv8.2).
    const CORTEX_A55: &str = "Features\t: fp asimd evtstrm aes pmull sha1 sha2 crc32 atomics fphp asimdhp cpuid asimdrdm lrcpc dcpop asimddp";
    /// A Cortex-A53 (ARMv8.0).
    const CORTEX_A53: &str = "Features\t: fp asimd evtstrm aes pmull sha1 sha2 crc32 cpuid";

    #[test]
    fn the_cpu_level_follows_the_features_present() {
        assert_eq!(
            FexVariant::for_features(SNAPDRAGON_8_GEN_2),
            FexVariant::Armv8_4
        );
        assert_eq!(FexVariant::for_features(CORTEX_A55), FexVariant::Armv8_2);
        assert_eq!(FexVariant::for_features(CORTEX_A53), FexVariant::Armv8_0);
        assert_eq!(FexVariant::for_features(""), FexVariant::Armv8_0);
    }

    #[test]
    fn every_variant_is_pinned_to_its_own_package() {
        assert_eq!(pins().schema, 1);
        assert_eq!(fex_version(), "2609.1");
        // The packages the built-in FEX was proven with, byte for byte.
        let tested = [
            (
                FexVariant::Armv8_0,
                "armv8.0",
                3_407_810,
                "2c4e97b245bf32374cdafc9c6908727e1948319dc959e941d2bdd21bf23a68cc",
            ),
            (
                FexVariant::Armv8_2,
                "armv8.2",
                3_411_742,
                "7418a68b713e1828d71019ec7c29b676fbdfc1825873fd68c7b072432f5f65bc",
            ),
            (
                FexVariant::Armv8_4,
                "armv8.4",
                3_415_064,
                "b401af7c1b92c304abb640d0b3eaea81ca900d8597079979915c4edea850474d",
            ),
        ];
        assert_eq!(tested.map(|(variant, ..)| variant), FexVariant::ALL);
        for (variant, level, size, sha256) in tested {
            let package = variant.package();
            assert_eq!((package.size(), package.sha256()), (size, sha256));
            let upstream = format!("fex-emu-{level}_2609.1-1~j_arm64.deb");
            assert_eq!(
                package.upstream(),
                [
                    format!("https://launchpad.net/~fex-emu/+archive/ubuntu/fex/+files/{upstream}"),
                    format!(
                        "https://ppa.launchpadcontent.net/fex-emu/fex/ubuntu/pool/main/f/fex-emu-{level}/{upstream}"
                    ),
                ],
                "Launchpad's file link first: it outlives the pool by some days"
            );
            assert_eq!(
                package.mirror_url(),
                format!(
                    "https://github.com/Pocketward/pd2-launcher/releases/download/fex-2609.1/fex-emu-{level}_2609.1-1.j_arm64.deb"
                )
            );
            assert!(variant.cache_name().ends_with(".verified"));
            assert!(variant.cache_name().contains(fex_version()));
        }
        let rootfs = rootfs();
        assert_eq!(
            (rootfs.size(), rootfs.sha256()),
            (
                525_615_104,
                "2854b06d3ff1b8f6e526135bfb6dd5b7b30ab3ab73e79ae933a3d9fed959a178"
            )
        );
        assert_eq!(
            rootfs.upstream(),
            ["https://rootfs.fex-emu.gg/Ubuntu_24_04/2026-08-11/Ubuntu_24_04.sqsh"]
        );
        assert_eq!(
            rootfs.mirror_url(),
            "https://github.com/Pocketward/pd2-launcher/releases/download/fex-2609.1/Ubuntu_24_04-2026-08-11.sqsh"
        );
        assert!(ROOTFS_CACHE_NAME.contains("2026-08-11"));
    }

    #[test]
    fn the_pins_suit_the_mirror_release() {
        let pins = pins();
        assert_eq!(pins.mirror.tag, format!("fex-{}", pins.fex_version));
        let mut ids = pins.files.iter().map(|file| file.id).collect::<Vec<_>>();
        ids.sort_unstable();
        assert_eq!(ids, ["fex-armv8.0", "fex-armv8.2", "fex-armv8.4", "rootfs"]);
        let mut names = pins.files.iter().map(|file| file.name).collect::<Vec<_>>();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), pins.files.len(), "one name per file");
        for file in &pins.files {
            // A name GitHub keeps as it is (it renames other characters).
            assert!(
                file.name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte)),
                "{}",
                file.name
            );
            assert!(file.size > 0);
            assert!(
                file.sha256.len() == 64
                    && file
                        .sha256
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            );
            assert!(!file.upstream.is_empty());
            assert!(file.upstream.iter().all(|url| url.starts_with("https://")));
        }
    }

    #[test]
    fn guests_get_their_own_folders_and_server() {
        let layout = Layout::new(Path::new("/home/player/.pd2launcher/game"));
        let environment = layout.environment();
        let get = |name: &str| {
            environment
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.to_string_lossy().into_owned())
        };
        assert_eq!(
            get("FEX_ROOTFS").as_deref(),
            Some("/home/player/.pd2launcher/game/fex/rootfs/Ubuntu_24_04")
        );
        assert_eq!(
            get("FEX_APP_DATA_LOCATION").as_deref(),
            Some("/home/player/.pd2launcher/game/fex/data/")
        );
        let socket = get("FEX_SERVERSOCKETPATH").unwrap();
        assert!(socket.starts_with("pd2launcher-") && socket.ends_with(".FEXServer.Socket"));
        assert!(
            !socket.starts_with('/'),
            "an abstract socket name, not a path"
        );
        assert_ne!(
            socket,
            server_socket_name(Path::new("/elsewhere/fex")),
            "each installation its own server"
        );
        assert_eq!(
            layout.interpreter(),
            Path::new("/home/player/.pd2launcher/game/fex/bin/FEX")
        );
    }

    /// A `.deb` with a `data.tar.zst` holding `files`.
    fn deb(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut tar = tar::Builder::new(Vec::new());
        for (name, contents) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            tar.append_data(&mut header, name, *contents).unwrap();
        }
        let data = zstd::stream::encode_all(tar.into_inner().unwrap().as_slice(), 3).unwrap();
        let mut deb = b"!<arch>\n".to_vec();
        for (name, contents) in [
            ("debian-binary", b"2.0\n".to_vec()),
            ("control.tar.zst", b"x".to_vec()),
            ("data.tar.zst", data),
        ] {
            deb.extend_from_slice(
                format!(
                    "{name:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
                    0,
                    0,
                    0,
                    100_644,
                    contents.len()
                )
                .as_bytes(),
            );
            deb.extend_from_slice(&contents);
            if contents.len() % 2 == 1 {
                deb.push(b'\n');
            }
        }
        deb
    }

    #[test]
    fn the_programs_come_out_of_the_package_and_nothing_else() {
        let fixture = tempfile::tempdir().unwrap();
        let package = fixture.path().join("fex.deb");
        fs::write(
            &package,
            deb(&[
                ("./usr/bin/FEX", b"fex"),
                ("./usr/bin/FEXServer", b"server"),
                ("./usr/bin/FEXConfig", b"config"),
            ]),
        )
        .unwrap();
        let bin = fixture.path().join("bin");
        fs::create_dir(&bin).unwrap();
        extract_programs(&package, &bin).unwrap();
        assert_eq!(fs::read(bin.join("FEX")).unwrap(), b"fex");
        assert_eq!(fs::read(bin.join("FEXServer")).unwrap(), b"server");
        assert!(!bin.join("FEXConfig").exists());
        let mode = fs::metadata(bin.join("FEX")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);

        let incomplete = fixture.path().join("incomplete.deb");
        fs::write(&incomplete, deb(&[("./usr/bin/FEX", b"fex")])).unwrap();
        let other = fixture.path().join("other");
        fs::create_dir(&other).unwrap();
        assert!(extract_programs(&incomplete, &other).is_err());
    }

    /// A small SquashFS image: a folder, a file, an absolute link, and a
    /// set-user file.
    fn squashfs() -> Vec<u8> {
        use backhand::{FilesystemWriter, NodeHeader};
        let header = |permissions| NodeHeader::new(permissions, 0, 0, 0);
        let mut writer = FilesystemWriter::default();
        writer.set_compressor(
            backhand::FilesystemCompressor::new(backhand::compression::Compressor::Zstd, None)
                .unwrap(),
        );
        writer.push_dir("usr", header(0o755)).unwrap();
        writer.push_dir("usr/lib", header(0o555)).unwrap();
        writer
            .push_file(
                io::Cursor::new(b"pulse".to_vec()),
                "usr/lib/libpulse.so.0",
                header(0o644),
            )
            .unwrap();
        writer
            .push_file(io::Cursor::new(b"su".to_vec()), "usr/su", header(0o4755))
            .unwrap();
        writer
            .push_symlink("/usr/lib/libpulse.so.0", "libpulse", header(0o777))
            .unwrap();
        let mut image = io::Cursor::new(Vec::new());
        writer.write(&mut image).unwrap();
        image.into_inner()
    }

    #[test]
    fn a_squashfs_image_unpacks_with_its_links_and_without_privileges() {
        let fixture = tempfile::tempdir().unwrap();
        let image = fixture.path().join("rootfs.sqsh");
        fs::write(&image, squashfs()).unwrap();
        let target = fixture.path().join("rootfs");
        unpack_squashfs(&image, &target).unwrap();
        assert_eq!(
            fs::read(target.join("usr/lib/libpulse.so.0")).unwrap(),
            b"pulse"
        );
        assert_eq!(
            fs::read_link(target.join("libpulse")).unwrap(),
            Path::new("/usr/lib/libpulse.so.0"),
            "links stay as the RootFS has them"
        );
        let su = fs::metadata(target.join("usr/su"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(su & 0o7777, 0o755, "no set-user bit");
        remove_tree(&target).unwrap();
        assert!(!target.exists());
    }

    #[test]
    fn an_installation_counts_only_when_complete_and_current() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        let package = root.join("fex.deb");
        fs::write(
            &package,
            deb(&[
                ("./usr/bin/FEX", b"fex"),
                ("./usr/bin/FEXServer", b"server"),
            ]),
        )
        .unwrap();
        let image = root.join("rootfs.sqsh");
        fs::write(&image, squashfs()).unwrap();
        let variant = FexVariant::Armv8_4;
        assert!(!installed_as(root, variant));
        assert_eq!(
            install_with_space(root, variant, &package, &image, u64::MAX)
                .unwrap_err()
                .kind(),
            io::ErrorKind::StorageFull
        );
        install_with_space(root, variant, &package, &image, 0).unwrap();
        assert!(installed_as(root, variant));
        assert!(
            !installed_as(root, FexVariant::Armv8_0),
            "another CPU level"
        );
        assert!(!root.join(STAGING).exists() && !root.join(RETIRED).exists());
        // Installing again replaces it.
        install_with_space(root, variant, &package, &image, 0).unwrap();
        assert!(installed_as(root, variant));
        // A missing program is not an installation.
        fs::remove_file(root.join("fex/bin/FEXServer")).unwrap();
        assert!(!installed_as(root, variant));
    }

    /// The real image: `PD2_TEST_FEX_ROOTFS=<Ubuntu_24_04.sqsh> cargo test
    /// -- --ignored real_rootfs`.
    #[test]
    #[ignore = "needs the downloaded RootFS image"]
    fn the_real_rootfs_unpacks() {
        let image = PathBuf::from(std::env::var_os("PD2_TEST_FEX_ROOTFS").unwrap());
        let fixture = tempfile::tempdir().unwrap();
        let target = fixture.path().join("rootfs");
        unpack_squashfs(&image, &target).unwrap();
        assert!(target.join("usr/lib/i386-linux-gnu/libpulse.so.0").exists());
        assert!(
            target
                .join("usr/lib/i386-linux-gnu/dri/msm_dri.so")
                .exists()
        );
        remove_tree(&target).unwrap();
    }

    #[test]
    fn the_built_in_fex_is_the_default_only_for_a_fresh_installation() {
        use crate::persistence::{FexSource, LauncherSettings};
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("game");
        assert!(chosen(None, &root), "no folder yet");
        fs::create_dir(&root).unwrap();
        assert!(chosen(None, &root), "an empty folder");
        let unchosen = LauncherSettings::default();
        assert!(chosen(Some(&unchosen), &root));
        fs::create_dir(root.join("prefix")).unwrap();
        assert!(
            !chosen(Some(&unchosen), &root),
            "set up with the device's FEX"
        );
        let builtin = LauncherSettings::default().with_fex_source(FexSource::Builtin);
        assert!(chosen(Some(&builtin), &root), "the player's choice wins");
        let device = LauncherSettings::default().with_fex_source(FexSource::Device);
        fs::remove_dir(root.join("prefix")).unwrap();
        assert!(!chosen(Some(&device), &root));
    }

    #[test]
    fn paths_that_leave_the_rootfs_are_refused() {
        assert!(safe_relative(Path::new("/usr/../../etc")).is_err());
        assert_eq!(
            safe_relative(Path::new("/usr/lib")).unwrap(),
            Path::new("usr/lib")
        );
    }
}
