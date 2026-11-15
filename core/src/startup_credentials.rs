//! Opt-in local product-key loading for repeatable installation testing.
//!
//! The Launcher never writes this file or copies it into package/persistent
//! state. A developer or user may provide both values through the process
//! environment, or, for the unattended CLI only, a `.env.local` in the
//! current launch directory. The interactive Launcher never reads
//! `.env.local`: a file that happens to sit beside a double-clicked
//! executable must not prefill keys or redirect downloads.
//! The same sources may also choose the Blizzard download as installer source
//! (`D2_INSTALLER_SOURCE=download`) and override download URLs (`PD2_URL_*`).
//! The process environment wins over `.env.local` for every name;
//! the installer choice (`D2_INSTALLER_SOURCE` with `D2_INSTALLER_DIR`) is
//! taken as a pair from the first layer that sets either of them.

use std::{
    collections::BTreeMap,
    env, fs,
    io::{self, Read},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

use zeroize::Zeroize;

use crate::sources::{DownloadArtifact, overrides_from_pairs};

const LOCAL_ENV_FILE: &str = ".env.local";
const D2_INSTALLER_SOURCE: &str = "D2_INSTALLER_SOURCE";
const INSTALLER_SOURCE_DOWNLOAD: &str = "download";
const INSTALLER_SOURCE_FOLDER: &str = "folder";
const D2_KEY: &str = "D2_KEY";
const D2_LOD_KEY: &str = "D2_LOD_KEY";
const D2_INSTALLER_DIR: &str = "D2_INSTALLER_DIR";
const MAX_LOCAL_ENV_BYTES: u64 = 16 * 1024;

/// Installer media chosen before the interactive screen or automation starts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum StartupInstallerSource {
    /// A user-selected local folder.
    Directory(PathBuf),
    /// The official downloader wrappers from the built-in Blizzard URLs.
    BlizzardDownload,
    /// A Diablo II the user already has, of any version (unattended only).
    ExistingInstall(PathBuf),
}

/// `D2_INSTALLER_SOURCE` held a value other than `download` or `folder`. It
/// is reported instead of silently falling back to a folder.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct InvalidInstallerSource;

impl InvalidInstallerSource {
    pub(crate) const MESSAGE: &'static str = "D2_INSTALLER_SOURCE must be \"download\" or \"folder\". Choose the installer files manually.";
}

/// The installer choice of one configuration layer: the process environment
/// or one `.env.local`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct InstallerLayer {
    /// The layer sets `D2_INSTALLER_DIR` or `D2_INSTALLER_SOURCE`.
    set: bool,
    directory: Option<PathBuf>,
    source: Option<InstallerSourceValue>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InstallerSourceValue {
    Download,
    Folder,
    Invalid,
}

impl InstallerSourceValue {
    /// `None` for an empty value, which leaves the name unset.
    fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        if value.is_empty() {
            None
        } else if value.eq_ignore_ascii_case(INSTALLER_SOURCE_DOWNLOAD) {
            Some(Self::Download)
        } else if value.eq_ignore_ascii_case(INSTALLER_SOURCE_FOLDER) {
            Some(Self::Folder)
        } else {
            Some(Self::Invalid)
        }
    }
}

impl InstallerLayer {
    fn from_environment() -> Self {
        let directory = env::var_os(D2_INSTALLER_DIR).filter(|value| !value.is_empty());
        let source = env::var(D2_INSTALLER_SOURCE)
            .ok()
            .and_then(|value| InstallerSourceValue::parse(&value))
            .or_else(|| {
                // A non-UTF-8 value is set but cannot name a valid choice.
                env::var_os(D2_INSTALLER_SOURCE)
                    .filter(|value| !value.is_empty() && value.to_str().is_none())
                    .map(|_| InstallerSourceValue::Invalid)
            });
        Self {
            set: directory.is_some() || source.is_some(),
            directory: directory
                .map(PathBuf::from)
                .filter(|path| path.is_absolute()),
            source,
        }
    }

    /// `download` selects the Blizzard download and takes precedence over
    /// `D2_INSTALLER_DIR`; `folder` or no source uses the directory.
    fn resolve(self) -> Result<Option<StartupInstallerSource>, InvalidInstallerSource> {
        match self.source {
            Some(InstallerSourceValue::Invalid) => Err(InvalidInstallerSource),
            Some(InstallerSourceValue::Download) => {
                Ok(Some(StartupInstallerSource::BlizzardDownload))
            }
            Some(InstallerSourceValue::Folder) | None => {
                Ok(self.directory.map(StartupInstallerSource::Directory))
            }
        }
    }
}

/// The installer choice of the first layer (environment, then each
/// `.env.local`) that sets `D2_INSTALLER_DIR` or `D2_INSTALLER_SOURCE`; the
/// two names are never mixed across layers.
fn installer_choice(
    environment: InstallerLayer,
    files: impl IntoIterator<Item = InstallerLayer>,
) -> Result<Option<StartupInstallerSource>, InvalidInstallerSource> {
    std::iter::once(environment)
        .chain(files)
        .find(|layer| layer.set)
        .map_or(Ok(None), InstallerLayer::resolve)
}

/// How the Launcher was started, which decides whether `.env.local` is read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LaunchMode {
    /// The GUI a user opened: the process environment only.
    Interactive,
    /// `--unattended-install`: the process environment, then `.env.local`.
    Unattended,
    /// `--engine` (Android): only what the Android app sends on the Control
    /// channel; neither the process environment nor `.env.local`.
    Engine,
}

/// The `.env.local` files `mode` may read, in precedence order.
fn local_env_files(mode: LaunchMode) -> Vec<PathBuf> {
    match mode {
        LaunchMode::Interactive | LaunchMode::Engine => Vec::new(),
        LaunchMode::Unattended => local_env_candidates(),
    }
}

pub(crate) struct StartupInputs {
    product_keys: Option<StartupProductKeys>,
    installer: Result<Option<StartupInstallerSource>, InvalidInstallerSource>,
}

impl StartupInputs {
    pub(crate) fn load(mode: LaunchMode) -> Self {
        Self::load_from_files(local_env_files(mode))
    }

    fn load_from_files(local_env_files: Vec<PathBuf>) -> Self {
        Self::load_layers(local_env_files, StartupProductKeys::dev_build)
    }

    /// The process environment first, then each `.env.local`, then the keys
    /// a dev build was compiled with: a test machine's own keys win.
    fn load_layers(
        local_env_files: Vec<PathBuf>,
        compiled: impl FnOnce() -> Option<StartupProductKeys>,
    ) -> Self {
        let mut product_keys = StartupProductKeys::from_environment();
        let environment = InstallerLayer::from_environment();
        let mut file_layers = Vec::new();

        if product_keys.is_none() || !environment.set {
            for path in local_env_files {
                let Ok(Some(mut from_file)) = load_local_env_file(&path) else {
                    continue;
                };
                if product_keys.is_none() {
                    product_keys = from_file.product_keys.take();
                }
                file_layers.push(std::mem::take(&mut from_file.installer));
            }
        }

        if product_keys.is_none() {
            product_keys = compiled();
        }

        Self {
            product_keys,
            installer: installer_choice(environment, file_layers),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_tests(
        product_keys: Option<(&str, &str)>,
        installer_directory: Option<PathBuf>,
        installer_download: bool,
    ) -> Self {
        let layer = InstallerLayer {
            set: true,
            directory: installer_directory,
            source: installer_download.then_some(InstallerSourceValue::Download),
        };
        Self {
            product_keys: product_keys.and_then(|(base, expansion)| {
                StartupProductKeys::validated(Some(base.to_owned()), Some(expansion.to_owned()))
            }),
            installer: layer.resolve(),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_invalid_installer_source() -> Self {
        Self {
            product_keys: None,
            installer: Err(InvalidInstallerSource),
        }
    }

    pub(crate) fn into_parts(
        mut self,
    ) -> (
        String,
        String,
        Result<Option<StartupInstallerSource>, InvalidInstallerSource>,
    ) {
        let (base, expansion) = self
            .product_keys
            .take()
            .map(StartupProductKeys::into_parts)
            .unwrap_or_default();
        (base, expansion, self.installer.clone())
    }
}

/// `PD2_URL_*` download-source overrides from the process environment, with
/// names missing there filled from `.env.local` in unattended mode.
pub(crate) fn download_source_environment(mode: LaunchMode) -> BTreeMap<DownloadArtifact, String> {
    if mode == LaunchMode::Engine {
        return BTreeMap::new();
    }
    download_sources_from_files(local_env_files(mode))
}

fn download_sources_from_files(
    local_env_files: Vec<PathBuf>,
) -> BTreeMap<DownloadArtifact, String> {
    let variables: Vec<(String, String)> = DownloadArtifact::ALL
        .into_iter()
        .filter_map(|artifact| {
            env::var(artifact.environment_name())
                .ok()
                .map(|value| (artifact.environment_name().to_owned(), value))
        })
        .collect();
    let mut overrides = overrides_from_pairs(
        variables
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
    );
    if overrides.len() < DownloadArtifact::ALL.len() {
        for path in local_env_files {
            let Ok(Some(from_file)) = load_local_env_file(&path) else {
                continue;
            };
            for (artifact, url) in from_file.download_sources {
                overrides.entry(artifact).or_insert(url);
            }
        }
    }
    overrides
}

pub(crate) struct StartupProductKeys {
    base: String,
    expansion: String,
}

impl StartupProductKeys {
    fn from_environment() -> Option<Self> {
        let environment_base = env::var(D2_KEY).ok();
        let environment_expansion = env::var(D2_LOD_KEY).ok();
        Self::validated(environment_base, environment_expansion)
    }

    /// The keys a dev build was compiled with (src/dev_build.rs).
    fn dev_build() -> Option<Self> {
        let (base, expansion) = crate::dev_build::product_keys()?;
        Self::validated(Some(base.to_owned()), Some(expansion.to_owned()))
    }

    fn validated(base: Option<String>, expansion: Option<String>) -> Option<Self> {
        let (mut base, mut expansion) = match (base, expansion) {
            (Some(base), Some(expansion)) => (base, expansion),
            (mut base, mut expansion) => {
                base.zeroize();
                expansion.zeroize();
                return None;
            }
        };
        if !valid_product_key(&base) || !valid_product_key(&expansion) {
            base.zeroize();
            expansion.zeroize();
            return None;
        }
        Some(Self { base, expansion })
    }

    pub(crate) fn into_parts(mut self) -> (String, String) {
        (
            std::mem::take(&mut self.base),
            std::mem::take(&mut self.expansion),
        )
    }
}

struct LocalEnvInputs {
    product_keys: Option<StartupProductKeys>,
    installer: InstallerLayer,
    download_sources: BTreeMap<DownloadArtifact, String>,
}

fn load_local_env_file(path: &Path) -> io::Result<Option<LocalEnvInputs>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.nlink() != 1
        || metadata.len() > MAX_LOCAL_ENV_BYTES
    {
        return Ok(None);
    }
    let mut file = fs::File::open(path)?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.by_ref()
        .take(MAX_LOCAL_ENV_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_LOCAL_ENV_BYTES {
        bytes.zeroize();
        return Ok(None);
    }
    let parsed = std::str::from_utf8(&bytes).ok().and_then(parse_local_env);
    bytes.zeroize();
    Ok(parsed)
}

impl Drop for StartupProductKeys {
    fn drop(&mut self) {
        self.base.zeroize();
        self.expansion.zeroize();
    }
}

fn local_env_candidates() -> Vec<PathBuf> {
    env::current_dir()
        .map(|directory| vec![directory.join(LOCAL_ENV_FILE)])
        .unwrap_or_default()
}

fn parse_local_env(contents: &str) -> Option<LocalEnvInputs> {
    let mut base = None;
    let mut expansion = None;
    let mut installer = InstallerLayer::default();
    let mut directory_seen = false;
    let mut source_pairs = Vec::new();
    let mut duplicate = false;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        match name.trim() {
            D2_KEY if base.is_none() => base = Some(unquote(value.trim())?.to_owned()),
            D2_LOD_KEY if expansion.is_none() => {
                expansion = Some(unquote(value.trim())?.to_owned());
            }
            D2_INSTALLER_DIR if !directory_seen => {
                directory_seen = true;
                let value = unquote(value.trim())?;
                installer.set |= !value.is_empty();
                let path = PathBuf::from(value);
                if path.is_absolute() {
                    installer.directory = Some(path);
                }
            }
            D2_KEY | D2_LOD_KEY => duplicate = true,
            D2_INSTALLER_DIR => duplicate = true,
            D2_INSTALLER_SOURCE if installer.source.is_none() => {
                installer.source = InstallerSourceValue::parse(unquote(value.trim())?);
                installer.set |= installer.source.is_some();
            }
            D2_INSTALLER_SOURCE => duplicate = true,
            name if name.starts_with("PD2_URL_") => {
                // A malformed URL line is skipped on its own; it never drops
                // the rest of the file (such as the product keys).
                if let Some(value) = unquote(value.trim()) {
                    source_pairs.push((name, value));
                }
            }
            _ => {}
        }
    }
    if duplicate {
        base.zeroize();
        expansion.zeroize();
        return None;
    }
    Some(LocalEnvInputs {
        product_keys: StartupProductKeys::validated(base, expansion),
        installer,
        download_sources: overrides_from_pairs(source_pairs),
    })
}

fn unquote(value: &str) -> Option<&str> {
    if value.len() >= 2 {
        let first = value.as_bytes()[0];
        let last = value.as_bytes()[value.len() - 1];
        if (first == b'\'' && last == b'\'') || (first == b'"' && last == b'"') {
            return value.get(1..value.len() - 1);
        }
    }
    (!value.starts_with(['\'', '"']) && !value.ends_with(['\'', '"'])).then_some(value)
}

fn valid_product_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_env_parser_requires_both_valid_named_keys_and_an_absolute_installer_directory() {
        assert!(
            parse_local_env("D2_KEY=AAAA-BBBB\n")
                .unwrap()
                .product_keys
                .is_none()
        );
        assert!(
            parse_local_env("D2_KEY=not valid\nD2_LOD_KEY=CCCC-DDDD\n")
                .unwrap()
                .product_keys
                .is_none()
        );
        assert!(
            parse_local_env("D2_KEY=AAAA-BBBB\nD2_KEY=other\nD2_LOD_KEY=CCCC-DDDD\n").is_none()
        );

        let mut inputs = parse_local_env(
            "# local only\nexport D2_KEY='AAAA-BBBB'\nD2_LOD_KEY=\"CCCC-DDDD\"\nD2_INSTALLER_DIR='/home/test/Downloads'\n",
        )
        .unwrap();
        assert_eq!(
            inputs.product_keys.take().unwrap().into_parts(),
            ("AAAA-BBBB".to_owned(), "CCCC-DDDD".to_owned())
        );
        assert_eq!(
            inputs.installer.directory,
            Some(PathBuf::from("/home/test/Downloads"))
        );

        let relative = parse_local_env("D2_INSTALLER_DIR=Downloads\n").unwrap();
        assert!(relative.installer.directory.is_none());
    }

    #[test]
    fn local_env_parser_reads_installer_source_and_download_url_overrides() {
        let inputs = parse_local_env(
            "D2_INSTALLER_SOURCE=download\nPD2_URL_WINE='https://mirror.example/wine.tar.xz'\nPD2_URL_PD2_CLIENT_BASE=https://mirror.example/pd2/\nPD2_URL_GECKO=ftp://nope\n",
        )
        .unwrap();
        assert_eq!(
            inputs.installer.source,
            Some(InstallerSourceValue::Download)
        );
        assert_eq!(
            inputs
                .download_sources
                .get(&DownloadArtifact::Wine)
                .map(String::as_str),
            Some("https://mirror.example/wine.tar.xz")
        );
        assert_eq!(
            inputs
                .download_sources
                .get(&DownloadArtifact::Pd2ClientBase)
                .map(String::as_str),
            Some("https://mirror.example/pd2")
        );
        assert!(
            !inputs
                .download_sources
                .contains_key(&DownloadArtifact::Gecko)
        );
        assert_eq!(
            parse_local_env("D2_INSTALLER_SOURCE=folder\n")
                .unwrap()
                .installer
                .source,
            Some(InstallerSourceValue::Folder)
        );
    }

    #[test]
    fn an_unknown_installer_source_is_an_error_not_a_silent_folder() {
        let layer =
            parse_local_env("D2_INSTALLER_SOURCE=dowload\nD2_INSTALLER_DIR=/home/test/Downloads\n")
                .unwrap()
                .installer;
        assert_eq!(layer.clone().resolve(), Err(InvalidInstallerSource));
        let folder = InstallerLayer {
            source: Some(InstallerSourceValue::Folder),
            ..layer
        };
        assert_eq!(
            folder.resolve(),
            Ok(Some(StartupInstallerSource::Directory(PathBuf::from(
                "/home/test/Downloads"
            ))))
        );
    }

    #[test]
    fn a_layer_that_sets_either_installer_name_owns_the_whole_choice() {
        // The process environment chose a folder; `.env.local`'s download
        // choice (and directory) must not be mixed in.
        let environment = InstallerLayer {
            set: true,
            directory: Some(PathBuf::from("/media/env")),
            source: None,
        };
        let file = parse_local_env("D2_INSTALLER_SOURCE=download\nD2_INSTALLER_DIR=/media/file\n")
            .unwrap()
            .installer;
        assert_eq!(
            installer_choice(environment, [file.clone()]),
            Ok(Some(StartupInstallerSource::Directory(PathBuf::from(
                "/media/env"
            ))))
        );
        // An environment source alone also owns the choice: `.env.local`'s
        // directory is not borrowed for it.
        let folder_only = InstallerLayer {
            set: true,
            directory: None,
            source: Some(InstallerSourceValue::Folder),
        };
        assert_eq!(installer_choice(folder_only, [file.clone()]), Ok(None));
        // An environment layer setting nothing leaves `.env.local` in charge.
        assert_eq!(
            installer_choice(InstallerLayer::default(), [InstallerLayer::default(), file]),
            Ok(Some(StartupInstallerSource::BlizzardDownload))
        );
        assert!(!parse_local_env("D2_KEY=AAAA-BBBB\n").unwrap().installer.set);
    }

    #[test]
    fn download_choice_takes_precedence_over_an_installer_directory() {
        let (_, _, source) =
            StartupInputs::for_tests(None, Some(PathBuf::from("/home/test/Downloads")), true)
                .into_parts();
        assert_eq!(source, Ok(Some(StartupInstallerSource::BlizzardDownload)));
        let (_, _, source) =
            StartupInputs::for_tests(None, Some(PathBuf::from("/home/test/Downloads")), false)
                .into_parts();
        assert_eq!(
            source,
            Ok(Some(StartupInstallerSource::Directory(PathBuf::from(
                "/home/test/Downloads"
            ))))
        );
        assert_eq!(
            StartupInputs::for_tests(None, None, false).into_parts().2,
            Ok(None)
        );
    }

    #[test]
    fn a_local_env_file_wins_over_compiled_dev_keys() {
        if std::env::var_os(D2_KEY).is_some() || std::env::var_os(D2_LOD_KEY).is_some() {
            return;
        }
        let compiled = || {
            StartupProductKeys::validated(
                Some("IIIIJJJJKKKKLLLL".to_owned()),
                Some("MMMMNNNNPPPPQQQQ".to_owned()),
            )
        };
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(LOCAL_ENV_FILE);
        std::fs::write(
            &path,
            "D2_KEY=AAAABBBBCCCCDDDD\nD2_LOD_KEY=EEEEFFFFGGGGHHHH\n",
        )
        .unwrap();
        let (base, _, _) = StartupInputs::load_layers(vec![path], compiled).into_parts();
        assert_eq!(base, "AAAABBBBCCCCDDDD");
        // Without a file, the compiled keys are used.
        let (base, _, _) = StartupInputs::load_layers(Vec::new(), compiled).into_parts();
        assert_eq!(base, "IIIIJJJJKKKKLLLL");
    }

    #[test]
    fn only_unattended_mode_reads_a_local_env_file() {
        assert!(local_env_files(LaunchMode::Interactive).is_empty());
        assert!(local_env_files(LaunchMode::Engine).is_empty());
        // The engine takes no download source from its environment either.
        assert!(download_source_environment(LaunchMode::Engine).is_empty());
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(LOCAL_ENV_FILE);
        std::fs::write(
            &path,
            "D2_KEY=AAAABBBBCCCCDDDD\nD2_LOD_KEY=EEEEFFFFGGGGHHHH\nPD2_URL_WINE=https://mirror.example/wine.tar.xz\n",
        )
        .unwrap();
        // The unattended reader takes keys and sources from the file...
        let (base, expansion, _) = StartupInputs::load_from_files(vec![path.clone()]).into_parts();
        if std::env::var_os(D2_KEY).is_none() && std::env::var_os(D2_LOD_KEY).is_none() {
            assert_eq!(
                (base.as_str(), expansion.as_str()),
                ("AAAABBBBCCCCDDDD", "EEEEFFFFGGGGHHHH")
            );
        }
        if std::env::var_os(DownloadArtifact::Wine.environment_name()).is_none() {
            assert_eq!(
                download_sources_from_files(vec![path])
                    .get(&DownloadArtifact::Wine)
                    .map(String::as_str),
                Some("https://mirror.example/wine.tar.xz")
            );
        }
        // ...while the interactive Launcher has no file to read at all.
        let (base, expansion, _) =
            StartupInputs::load_from_files(local_env_files(LaunchMode::Interactive)).into_parts();
        if std::env::var_os(D2_KEY).is_none() {
            assert!(base.is_empty() && expansion.is_empty());
        }
    }

    #[test]
    fn local_env_file_rejects_links_and_oversized_or_malformed_content() {
        let directory = tempfile::tempdir().unwrap();
        let valid = directory.path().join("valid.env");
        fs::write(
            &valid,
            b"D2_KEY=AAAA-BBBB\nD2_LOD_KEY=CCCC-DDDD\nD2_INSTALLER_DIR=/home/test/Downloads\n",
        )
        .unwrap();
        let loaded = load_local_env_file(&valid).unwrap().unwrap();
        assert!(loaded.product_keys.is_some());
        assert_eq!(
            loaded.installer.directory,
            Some(PathBuf::from("/home/test/Downloads"))
        );

        // A malformed PD2_URL_* line is skipped alone; the keys still load.
        let url_line = directory.path().join("url.env");
        fs::write(
            &url_line,
            b"D2_KEY=AAAA-BBBB\nPD2_URL_WINE='https://mirror.example/w.tar.xz\nD2_LOD_KEY=CCCC-DDDD\nPD2_URL_GECKO=https://mirror.example/g.msi\n",
        )
        .unwrap();
        let loaded = load_local_env_file(&url_line).unwrap().unwrap();
        assert!(loaded.product_keys.is_some());
        assert_eq!(
            loaded.download_sources.keys().copied().collect::<Vec<_>>(),
            [DownloadArtifact::Gecko]
        );

        let malformed = directory.path().join("malformed.env");
        fs::write(&malformed, b"D2_KEY=AAAA-BBBB\nD2_LOD_KEY='unterminated\n").unwrap();
        assert!(load_local_env_file(&malformed).unwrap().is_none());

        let oversized = directory.path().join("oversized.env");
        fs::write(&oversized, vec![b'x'; MAX_LOCAL_ENV_BYTES as usize + 1]).unwrap();
        assert!(load_local_env_file(&oversized).unwrap().is_none());

        let linked = directory.path().join("linked.env");
        std::os::unix::fs::symlink(&valid, &linked).unwrap();
        assert!(load_local_env_file(&linked).unwrap().is_none());
    }
}
