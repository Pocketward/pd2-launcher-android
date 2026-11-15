//! Launcher-owned installation workflow.
//!
//! Installation orchestration depends on domain policy and narrow acquisition,
//! execution, transaction, and persistence interfaces. It never renders UI.

use std::{
    borrow::Cow,
    error::Error,
    ffi::OsStr,
    fmt::{self, Write as _},
    fs::File,
    io::{Read, Seek, SeekFrom},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256, Sha512};
use zeroize::Zeroize;

pub(crate) use crate::downloads::DownloadPlan;
use crate::{
    InstallationStatus, InterruptedStage,
    audit::{CheckStatus, HostSupport},
    game_folder::{ExistingInstall, ExistingInstallProblem, GameFolder, ImportMode},
    operation::{OperationControl, ProgressUnit, WorkPhase, WorkProgress},
    safe_fs::Directory,
};

mod adapters;

pub(crate) use adapters::{
    InstallerMedia, ProductionArtifactSource, ProductionInstallDriver, UpdaterClientSynchronizer,
    XINPUT_BRIDGE_PATH, XINPUT_BRIDGE_SHA256, XINPUT_BRIDGE_SIZE, android_gl_probe_copy,
    fex_strict_settings, gl_probe_helper, install_xinput_bridge, recorded_cache_names,
    remove_wine_gecko_and_mono, seed_verified_cache,
};

/// A temporary authorization that deliberately cannot be cloned or serialized.
pub(crate) struct InstallationAuthorization {
    base_key: ProductKey,
    expansion_key: ProductKey,
    vendor_terms_accepted: bool,
    /// Lets tests see when the authorization is dropped.
    #[cfg(test)]
    alive: std::sync::Arc<()>,
}

impl InstallationAuthorization {
    /// Dead once this authorization is dropped.
    #[cfg(test)]
    pub(crate) fn liveness(&self) -> std::sync::Weak<()> {
        std::sync::Arc::downgrade(&self.alive)
    }

    pub(crate) fn new(
        base_key: String,
        expansion_key: String,
        vendor_terms_accepted: bool,
    ) -> Result<Self, AuthorizationError> {
        // Consume both submitted strings into wiping containers before any
        // early return. Otherwise a missing-consent or first-key error would
        // drop the other key as an ordinary, non-wiping `String`.
        let base_key = ProductKey::new(base_key);
        let expansion_key = ProductKey::new(expansion_key);
        if !vendor_terms_accepted {
            return Err(AuthorizationError::ConsentRequired);
        }
        let base_key = base_key.map_err(|_| AuthorizationError::InvalidBaseKey)?;
        let expansion_key = expansion_key.map_err(|_| AuthorizationError::InvalidExpansionKey)?;
        Ok(Self {
            base_key,
            expansion_key,
            vendor_terms_accepted,
            #[cfg(test)]
            alive: std::sync::Arc::new(()),
        })
    }

    fn key_for(&self, product: VendorProduct) -> &ProductKey {
        match product {
            VendorProduct::BaseGame => &self.base_key,
            VendorProduct::Expansion => &self.expansion_key,
        }
    }
}

impl fmt::Debug for InstallationAuthorization {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InstallationAuthorization")
            .field("base_key", &Redacted)
            .field("expansion_key", &Redacted)
            .field("vendor_terms_accepted", &self.vendor_terms_accepted)
            .finish()
    }
}

struct Redacted;

impl fmt::Debug for Redacted {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

struct ProductKey {
    bytes: Box<[u8]>,
}

impl ProductKey {
    fn new(value: String) -> Result<Self, ()> {
        let mut bytes = value.into_bytes();
        if bytes.is_empty()
            || bytes.len() > 64
            || !bytes
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
        {
            bytes.zeroize();
            return Err(());
        }
        Ok(Self {
            bytes: bytes.into_boxed_slice(),
        })
    }

    fn expose(&self) -> &[u8] {
        &self.bytes
    }

    /// The key as the installers get it, from the key check.
    fn from_checked(key: &crate::blizzard::NormalizedKey) -> Self {
        Self {
            bytes: key.as_bytes().to_vec().into_boxed_slice(),
        }
    }
}

impl Drop for ProductKey {
    fn drop(&mut self) {
        self.bytes.zeroize();
    }
}

/// Stable authorization failures that cannot contain submitted key material.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AuthorizationError {
    ConsentRequired,
    InvalidBaseKey,
    InvalidExpansionKey,
}

impl fmt::Display for AuthorizationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConsentRequired => formatter.write_str("vendor-term consent is required"),
            Self::InvalidBaseKey => formatter.write_str("the D2 product key format is invalid"),
            Self::InvalidExpansionKey => {
                formatter.write_str("the LoD product key format is invalid")
            }
        }
    }
}

impl Error for AuthorizationError {}

/// Exact external inputs used by the Managed installation workflow.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ArtifactKind {
    ManagedRuntime,
    Gecko,
    /// Wine Mono, installed silently so Wine never shows its Mono prompt.
    WineMono,
    /// .NET 6 Desktop Runtime (x86), which the official PD2 launcher needs.
    DotnetDesktop,
    BaseGameWrapper,
    ExpansionWrapper,
    ClientBootstrap,
    /// The built-in FEX's package, for this device's CPU level.
    FexPackage,
    /// The built-in FEX's RootFS.
    FexRootFs,
}

impl ArtifactKind {
    pub(crate) const fn download_artifact(self) -> crate::sources::DownloadArtifact {
        use crate::sources::DownloadArtifact;
        match self {
            Self::ManagedRuntime => DownloadArtifact::Wine,
            Self::Gecko => DownloadArtifact::Gecko,
            Self::WineMono => DownloadArtifact::WineMono,
            Self::DotnetDesktop => DownloadArtifact::DotnetDesktop,
            Self::BaseGameWrapper => DownloadArtifact::D2Installer,
            Self::ExpansionWrapper => DownloadArtifact::LodInstaller,
            Self::ClientBootstrap => DownloadArtifact::Pd2Installer,
            Self::FexPackage => DownloadArtifact::Fex,
            Self::FexRootFs => DownloadArtifact::FexRootFs,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RedirectPolicy {
    None,
    GithubReleaseAsset,
    /// A Launchpad PPA's file link (`…/+files/<name>`), which sends the
    /// request on to the Launchpad librarian's copy of the same file name.
    LaunchpadLibrarian,
    /// Custom sources only: any `http`/`https` destination, bounded hops.
    AnyHttp,
}

impl RedirectPolicy {
    /// The policy of a built-in source, from where it is.
    fn for_builtin_source(url: &str) -> Self {
        if url.starts_with("https://github.com/") {
            Self::GithubReleaseAsset
        } else if url.starts_with("https://launchpad.net/") {
            Self::LaunchpadLibrarian
        } else {
            Self::None
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SizePolicy {
    Exact(u64),
    Bounded { min: u64, max: u64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IntegrityPolicy {
    Sha256(&'static str),
    Sha512(&'static str),
    PeExecutable,
    /// A user-supplied source: content is deliberately not
    /// verified. Size caps, staging, and extraction rules still apply.
    Unverified,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ArtifactSpec {
    kind: ArtifactKind,
    request_url: Cow<'static, str>,
    redirects: RedirectPolicy,
    size: SizePolicy,
    integrity: IntegrityPolicy,
    expected_filename: Option<&'static str>,
    /// Other built-in sources of the same bytes, tried in order when
    /// `request_url` does not give them. Each has the size and digest above
    /// and the redirect policy of where it is.
    fallback_urls: &'static [&'static str],
}

impl ArtifactSpec {
    pub(crate) fn request_url(&self) -> &str {
        &self.request_url
    }

    /// Every source of this artifact in the order they are tried, each as a
    /// spec of its own: `request_url` first, then each fallback.
    pub(crate) fn sources(&self) -> impl Iterator<Item = Self> + '_ {
        let only = |request_url: Cow<'static, str>, redirects| Self {
            request_url,
            redirects,
            fallback_urls: &[],
            ..self.clone()
        };
        std::iter::once(only(self.request_url.clone(), self.redirects)).chain(
            self.fallback_urls
                .iter()
                .map(move |url| only(Cow::Borrowed(*url), RedirectPolicy::for_builtin_source(url))),
        )
    }

    /// Whether an artifact requested from `requested_url` and served from
    /// `final_url` came from one of this spec's sources.
    fn approves_source(&self, requested_url: &str, final_url: &str) -> bool {
        self.sources()
            .find(|source| source.request_url() == requested_url)
            .is_some_and(|source| source.approves_final_url(final_url))
    }

    pub(crate) fn kind(&self) -> ArtifactKind {
        self.kind
    }

    /// A user-supplied source: any `http`/`https` URL, redirects allowed, no
    /// digest, size-exactness, PE, or filename verification; only the
    /// generous size cap remains.
    fn custom(kind: ArtifactKind, url: String, maximum_bytes: u64) -> Self {
        Self {
            kind,
            request_url: Cow::Owned(url),
            redirects: RedirectPolicy::AnyHttp,
            size: SizePolicy::Bounded {
                min: 1,
                max: maximum_bytes,
            },
            integrity: IntegrityPolicy::Unverified,
            expected_filename: None,
            fallback_urls: &[],
        }
    }

    pub(crate) fn is_custom(&self) -> bool {
        self.integrity == IntegrityPolicy::Unverified
    }

    pub(crate) fn maximum_bytes(&self) -> u64 {
        match self.size {
            SizePolicy::Exact(size) => size,
            SizePolicy::Bounded { max, .. } => max,
        }
    }

    /// Whether the acquisition adapter may follow or accept this final URL.
    /// Adapters must apply this before streaming a redirected response; the
    /// workflow repeats it after download against retained provenance.
    pub(crate) fn approves_final_url(&self, final_url: &str) -> bool {
        match self.redirects {
            RedirectPolicy::None => final_url == self.request_url,
            RedirectPolicy::AnyHttp => zed_reqwest::Url::parse(final_url).is_ok_and(|url| {
                matches!(url.scheme(), "http" | "https") && url.host_str().is_some()
            }),
            RedirectPolicy::GithubReleaseAsset => {
                final_url == self.request_url
                    || final_url
                        .strip_prefix("https://release-assets.githubusercontent.com/")
                        .is_some_and(|rest| {
                            !rest.is_empty()
                                && !rest.contains('\\')
                                && !rest.contains('#')
                                && !rest.split('/').any(|part| matches!(part, "." | ".."))
                        })
            }
            // `https://launchpadlibrarian.net/<number>/<the requested file name>`.
            RedirectPolicy::LaunchpadLibrarian => {
                final_url == self.request_url
                    || final_url
                        .strip_prefix("https://launchpadlibrarian.net/")
                        .and_then(|rest| rest.split_once('/'))
                        .is_some_and(|(number, name)| {
                            !number.is_empty()
                                && number.bytes().all(|byte| byte.is_ascii_digit())
                                && !name.is_empty()
                                && self
                                    .request_url
                                    .rsplit_once("/+files/")
                                    .is_some_and(|(_, requested)| requested == name)
                        })
            }
        }
    }
}

const WINE_MONO_URL: &str = "https://dl.winehq.org/wine/wine-mono/11.3.0/wine-mono-11.3.0-x86.msi";
const DOTNET_DESKTOP_URL: &str = "https://builds.dotnet.microsoft.com/dotnet/WindowsDesktop/6.0.36/windowsdesktop-runtime-6.0.36-win-x86.exe";

/// Acquisition contracts. The built-in entries are frozen and
/// updating one requires a reviewed Launcher release. A user may replace an
/// entry with a custom, unverified source through
/// [`ArtifactCatalog::with_sources`]; built-in entries are never relaxed.
#[derive(Clone, Debug)]
pub(crate) struct ArtifactCatalog {
    specs: [ArtifactSpec; 9],
}

impl ArtifactCatalog {
    fn new(specs: [ArtifactSpec; 9]) -> Self {
        Self { specs }
    }

    pub(crate) fn pinned() -> Self {
        let runtime = crate::runtime::current();
        let fex = crate::builtin_fex::FexVariant::current();
        Self::new([
            ArtifactSpec {
                kind: ArtifactKind::ManagedRuntime,
                request_url: Cow::Borrowed(runtime.archive_url),
                redirects: RedirectPolicy::GithubReleaseAsset,
                size: SizePolicy::Exact(runtime.archive_size),
                integrity: IntegrityPolicy::Sha512(runtime.archive_sha512),
                expected_filename: None,
                fallback_urls: &[],
            },
            ArtifactSpec {
                kind: ArtifactKind::Gecko,
                request_url: Cow::Borrowed(
                    "https://dl.winehq.org/wine/wine-gecko/2.47.4/wine-gecko-2.47.4-x86.msi",
                ),
                redirects: RedirectPolicy::None,
                size: SizePolicy::Exact(55_187_968),
                integrity: IntegrityPolicy::Sha256(
                    "26cecc47706b091908f7f814bddb074c61beb8063318e9efc5a7f789857793d6",
                ),
                expected_filename: None,
                fallback_urls: &[],
            },
            ArtifactSpec {
                kind: ArtifactKind::WineMono,
                request_url: Cow::Borrowed(WINE_MONO_URL),
                redirects: RedirectPolicy::None,
                size: SizePolicy::Exact(83_348_992),
                integrity: IntegrityPolicy::Sha256(crate::audit::WINE_MONO_MSI_SHA256),
                expected_filename: None,
                fallback_urls: &[],
            },
            ArtifactSpec {
                kind: ArtifactKind::DotnetDesktop,
                request_url: Cow::Borrowed(DOTNET_DESKTOP_URL),
                redirects: RedirectPolicy::None,
                size: SizePolicy::Exact(52_173_224),
                // The SHA-512 Microsoft publishes in its 6.0 releases.json.
                integrity: IntegrityPolicy::Sha512(
                    "a18351aabfe1590e58af79e57ac2414254ba80cb7a1fef19545a6b8418575c735fc1dc164c3c7fed426c4698f099991487fa4f443bab93afd41d1563845fbcf4",
                ),
                expected_filename: None,
                fallback_urls: &[],
            },
            ArtifactSpec {
                kind: ArtifactKind::BaseGameWrapper,
                request_url: Cow::Borrowed(
                    "https://downloader.battle.net/download/getLegacy?os=win&locale=enUS&product=D2DV",
                ),
                redirects: RedirectPolicy::None,
                size: SizePolicy::Bounded {
                    min: 1_000_000,
                    max: 8_000_000,
                },
                integrity: IntegrityPolicy::PeExecutable,
                expected_filename: Some("Downloader_Diablo2_enUS.exe"),
                fallback_urls: &[],
            },
            ArtifactSpec {
                kind: ArtifactKind::ExpansionWrapper,
                request_url: Cow::Borrowed(
                    "https://downloader.battle.net/download/getLegacy?os=win&locale=enUS&product=D2XP",
                ),
                redirects: RedirectPolicy::None,
                size: SizePolicy::Bounded {
                    min: 1_000_000,
                    max: 8_000_000,
                },
                integrity: IntegrityPolicy::PeExecutable,
                expected_filename: Some("Downloader_Diablo2_Lord_of_Destruction_enUS.exe"),
                fallback_urls: &[],
            },
            ArtifactSpec {
                kind: ArtifactKind::ClientBootstrap,
                request_url: Cow::Borrowed(
                    "https://storage.googleapis.com/project-diablo-2-website-public/installer/ProjectD2Installer.exe",
                ),
                redirects: RedirectPolicy::None,
                size: SizePolicy::Exact(37_374_204),
                integrity: IntegrityPolicy::Sha256(
                    "50ef80e41ef626a14a2cd584b915e79774ad0b03d51cd03af719a6fbf8d93677",
                ),
                expected_filename: None,
                fallback_urls: &[],
            },
            Self::builtin_fex_spec(ArtifactKind::FexPackage, fex.package()),
            Self::builtin_fex_spec(ArtifactKind::FexRootFs, crate::builtin_fex::rootfs()),
        ])
    }

    /// A built-in FEX file: from the project's mirror release first, then
    /// from its upstream sources, the same bytes from each.
    fn builtin_fex_spec(
        kind: ArtifactKind,
        file: &'static crate::builtin_fex::PinnedFile,
    ) -> ArtifactSpec {
        ArtifactSpec {
            kind,
            request_url: Cow::Borrowed(file.mirror_url()),
            redirects: RedirectPolicy::for_builtin_source(file.mirror_url()),
            size: SizePolicy::Exact(file.size()),
            integrity: IntegrityPolicy::Sha256(file.sha256()),
            expected_filename: None,
            fallback_urls: file.upstream(),
        }
    }

    /// The built-in catalog with each custom source in `sources` replacing
    /// its built-in entry.
    pub(crate) fn with_sources(sources: &crate::sources::DownloadSources) -> Self {
        let mut catalog = Self::pinned();
        for spec in &mut catalog.specs {
            if let Some(url) = sources.custom_url(spec.kind.download_artifact()) {
                *spec = ArtifactSpec::custom(
                    spec.kind,
                    url.to_owned(),
                    crate::sources::CUSTOM_ARTIFACT_MAX_BYTES,
                );
            }
        }
        catalog
    }

    pub(crate) fn request_url(&self, kind: ArtifactKind) -> &str {
        self.spec(kind).request_url()
    }

    pub(crate) fn spec(&self, kind: ArtifactKind) -> &ArtifactSpec {
        self.specs
            .iter()
            .find(|spec| spec.kind == kind)
            .expect("artifact catalog must contain every workflow input")
    }
}

/// Provenance returned by the approved network adapter after a bounded,
/// partial-file download has closed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AcquiredArtifact {
    path: PathBuf,
    origin: ArtifactOrigin,
    requested_url: String,
    final_url: String,
    suggested_filename: Option<String>,
    retained_receipt_sha256: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ArtifactOrigin {
    ApprovedNetwork,
    UserSelected,
}

impl AcquiredArtifact {
    pub(crate) fn new(
        path: PathBuf,
        requested_url: impl Into<String>,
        final_url: impl Into<String>,
        suggested_filename: Option<String>,
    ) -> Self {
        Self {
            path,
            origin: ArtifactOrigin::ApprovedNetwork,
            requested_url: requested_url.into(),
            final_url: final_url.into(),
            suggested_filename,
            retained_receipt_sha256: None,
        }
    }

    pub(crate) fn user_selected(path: PathBuf, filename: impl Into<String>) -> Self {
        Self {
            path,
            origin: ArtifactOrigin::UserSelected,
            requested_url: String::new(),
            final_url: String::new(),
            suggested_filename: Some(filename.into()),
            retained_receipt_sha256: None,
        }
    }

    fn with_retained_receipt_sha256(mut self, digest: impl Into<String>) -> Self {
        self.retained_receipt_sha256 = Some(digest.into());
        self
    }
}

/// Closed and independently identity-checked artifact safe to pass to a
/// stage-specific adapter. It is still never executable by the verifier.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct VerifiedArtifact {
    kind: ArtifactKind,
    path: PathBuf,
    origin: ArtifactOrigin,
    observed_sha256: String,
    retained_receipt_sha256: Option<String>,
    requested_url: String,
    final_url: String,
    /// The user-supplied request URL when this artifact came from a custom,
    /// unverified source; `None` for built-in, verified sources.
    custom_source: Option<String>,
}

impl VerifiedArtifact {
    pub(crate) fn kind(&self) -> ArtifactKind {
        self.kind
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn custom_source(&self) -> Option<&str> {
        self.custom_source.as_deref()
    }

    pub(crate) fn content_sha256(&self) -> &str {
        &self.observed_sha256
    }

    fn execution_sha256(&self) -> Result<&str, ArtifactRejection> {
        if matches!(
            self.kind,
            ArtifactKind::BaseGameWrapper | ArtifactKind::ExpansionWrapper
        ) {
            self.retained_receipt_sha256
                .as_deref()
                .filter(|digest| *digest == self.observed_sha256)
                .ok_or(ArtifactRejection::DigestMismatch)
        } else {
            Ok(&self.observed_sha256)
        }
    }

    #[cfg(test)]
    pub(crate) fn observed_sha256(&self) -> &str {
        &self.observed_sha256
    }

    #[cfg(test)]
    pub(crate) fn requested_url(&self) -> &str {
        &self.requested_url
    }

    #[cfg(test)]
    pub(crate) fn final_url(&self) -> &str {
        &self.final_url
    }

    #[cfg(test)]
    pub(crate) fn origin(&self) -> ArtifactOrigin {
        self.origin
    }
}

/// External acquisition boundary. Implementations must download into a unique
/// partial below `cache` and retain that exact open file through verification
/// and safe promotion before returning its provenance.
pub(crate) trait ArtifactSource {
    fn acquire_at(
        &self,
        spec: &ArtifactSpec,
        cache: &Path,
        cache_directory: &Directory,
        control: &OperationControl,
    ) -> Result<AcquiredArtifact, AdapterFailure>;

    #[cfg(test)]
    fn acquire(
        &self,
        spec: &ArtifactSpec,
        cache: &Path,
        control: &OperationControl,
    ) -> Result<AcquiredArtifact, AdapterFailure> {
        let directory = Directory::open(cache).map_err(|_| AdapterFailure::here())?;
        self.acquire_at(spec, cache, &directory, control)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ArtifactRejection {
    UnsafePath,
    UnexpectedType,
    SizeMismatch,
    RequestUrlMismatch,
    UnapprovedFinalUrl,
    UnexpectedFilename,
    DigestMismatch,
    InvalidExecutable,
    Unreadable,
}

fn verify_artifact_at(
    spec: &ArtifactSpec,
    cache: &Path,
    cache_directory: &Directory,
    artifact: AcquiredArtifact,
) -> Result<VerifiedArtifact, ArtifactRejection> {
    validate_artifact_provenance(spec, &artifact)?;
    let name = artifact
        .path
        .file_name()
        .ok_or(ArtifactRejection::UnsafePath)?
        .to_owned();
    let direct_cache_entry = artifact.path == cache.join(&name);
    let complete_payload_entry = match spec.kind {
        ArtifactKind::BaseGameWrapper => {
            artifact.path == cache.join("d2-payload").join("Installer.exe")
        }
        ArtifactKind::ExpansionWrapper => {
            artifact.path == cache.join("lod-payload").join("Installer.exe")
        }
        _ => false,
    };
    if !direct_cache_entry && !complete_payload_entry {
        return Err(ArtifactRejection::UnsafePath);
    }
    let relative = artifact
        .path
        .strip_prefix(cache)
        .map_err(|_| ArtifactRejection::UnsafePath)?;
    let mut file = cache_directory
        .open_file(relative)
        .map_err(|_| ArtifactRejection::Unreadable)?;
    let metadata = file.metadata().map_err(|_| ArtifactRejection::Unreadable)?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(ArtifactRejection::UnexpectedType);
    }
    let retained_path = artifact.path.clone();
    verify_open_artifact(spec, artifact, retained_path, &mut file)
}

#[cfg(test)]
fn verify_artifact(
    spec: &ArtifactSpec,
    cache: &Path,
    artifact: AcquiredArtifact,
) -> Result<VerifiedArtifact, ArtifactRejection> {
    let directory = Directory::open(cache).map_err(|_| ArtifactRejection::UnsafePath)?;
    verify_artifact_at(spec, cache, &directory, artifact)
}

fn validate_artifact_provenance(
    spec: &ArtifactSpec,
    artifact: &AcquiredArtifact,
) -> Result<(), ArtifactRejection> {
    match artifact.origin {
        ArtifactOrigin::ApprovedNetwork => {
            if !spec
                .sources()
                .any(|source| source.request_url() == artifact.requested_url)
            {
                return Err(ArtifactRejection::RequestUrlMismatch);
            }
            if !spec.approves_source(&artifact.requested_url, &artifact.final_url) {
                return Err(ArtifactRejection::UnapprovedFinalUrl);
            }
        }
        ArtifactOrigin::UserSelected
            if !matches!(
                spec.kind,
                ArtifactKind::BaseGameWrapper | ArtifactKind::ExpansionWrapper
            ) =>
        {
            return Err(ArtifactRejection::UnapprovedFinalUrl);
        }
        ArtifactOrigin::UserSelected => {}
    }
    if spec
        .expected_filename
        .is_some_and(|expected| artifact.suggested_filename.as_deref() != Some(expected))
    {
        return Err(ArtifactRejection::UnexpectedFilename);
    }
    Ok(())
}

fn verify_open_artifact(
    spec: &ArtifactSpec,
    artifact: AcquiredArtifact,
    retained_path: PathBuf,
    file: &mut File,
) -> Result<VerifiedArtifact, ArtifactRejection> {
    validate_artifact_provenance(spec, &artifact)?;
    let metadata = file.metadata().map_err(|_| ArtifactRejection::Unreadable)?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(ArtifactRejection::UnexpectedType);
    }
    match spec.size {
        SizePolicy::Exact(expected) if metadata.len() != expected => {
            return Err(ArtifactRejection::SizeMismatch);
        }
        SizePolicy::Bounded { min, max } if metadata.len() < min || metadata.len() > max => {
            return Err(ArtifactRejection::SizeMismatch);
        }
        SizePolicy::Exact(_) | SizePolicy::Bounded { .. } => {}
    }

    let (sha256, sha512) = hash_open_artifact(file)?;
    match spec.integrity {
        IntegrityPolicy::Sha256(expected) if sha256 != expected => {
            return Err(ArtifactRejection::DigestMismatch);
        }
        IntegrityPolicy::Sha512(expected) if sha512 != expected => {
            return Err(ArtifactRejection::DigestMismatch);
        }
        IntegrityPolicy::PeExecutable => validate_open_pe(file)?,
        IntegrityPolicy::Sha256(_) | IntegrityPolicy::Sha512(_) | IntegrityPolicy::Unverified => {}
    }
    if matches!(spec.integrity, IntegrityPolicy::PeExecutable)
        && artifact.retained_receipt_sha256.as_deref() != Some(sha256.as_str())
    {
        return Err(ArtifactRejection::DigestMismatch);
    }

    Ok(VerifiedArtifact {
        kind: spec.kind,
        path: retained_path,
        origin: artifact.origin,
        observed_sha256: sha256,
        retained_receipt_sha256: artifact.retained_receipt_sha256,
        requested_url: artifact.requested_url,
        final_url: artifact.final_url,
        custom_source: spec.is_custom().then(|| spec.request_url().to_owned()),
    })
}

fn hash_open_artifact(file: &mut File) -> Result<(String, String), ArtifactRejection> {
    file.seek(SeekFrom::Start(0))
        .map_err(|_| ArtifactRejection::Unreadable)?;
    let mut sha256 = Sha256::new();
    let mut sha512 = Sha512::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|_| ArtifactRejection::Unreadable)?;
        if count == 0 {
            break;
        }
        sha256.update(&buffer[..count]);
        sha512.update(&buffer[..count]);
    }
    Ok((hex(&sha256.finalize()), hex(&sha512.finalize())))
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    output
}

fn validate_open_pe(file: &mut File) -> Result<(), ArtifactRejection> {
    file.seek(SeekFrom::Start(0))
        .map_err(|_| ArtifactRejection::InvalidExecutable)?;
    let mut dos = [0_u8; 64];
    file.read_exact(&mut dos)
        .map_err(|_| ArtifactRejection::InvalidExecutable)?;
    if &dos[..2] != b"MZ" {
        return Err(ArtifactRejection::InvalidExecutable);
    }
    let offset = u32::from_le_bytes(
        dos[0x3c..0x40]
            .try_into()
            .expect("DOS header slice is four bytes"),
    );
    file.seek(SeekFrom::Start(u64::from(offset)))
        .map_err(|_| ArtifactRejection::InvalidExecutable)?;
    let mut signature = [0_u8; 4];
    file.read_exact(&mut signature)
        .map_err(|_| ArtifactRejection::InvalidExecutable)?;
    if signature != *b"PE\0\0" {
        return Err(ArtifactRejection::InvalidExecutable);
    }
    Ok(())
}

/// Read-only projection of the full Auditor report used for stage planning.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct InstallAudit {
    status: InstallationStatus,
    host: HostSupport,
    runtime: CheckStatus,
    prefix: CheckStatus,
    prefix_needs: PrefixNeeds,
    /// The player chose the built-in FEX and it is not installed.
    builtin_fex_needed: bool,
    /// The Android host: Blizzard's
    /// downloaders run in Install, not Download. The app provides the
    /// RootFS, the Wine runtime and the Wine components.
    android_host: bool,
}

impl InstallAudit {
    /// Without component evidence, an unaccepted prefix needs every
    /// prefix component.
    pub(crate) fn new(
        status: InstallationStatus,
        host: HostSupport,
        runtime: CheckStatus,
        prefix: CheckStatus,
    ) -> Self {
        let needed = prefix != CheckStatus::Satisfied;
        Self {
            status,
            host,
            runtime,
            prefix,
            prefix_needs: PrefixNeeds {
                gecko: needed,
                mono: needed,
                dotnet: needed,
            },
            builtin_fex_needed: false,
            android_host: false,
        }
    }

    /// The Android host. Its prefix never gets Wine Gecko or Wine Mono: no
    /// program the Launcher runs there needs them, so neither is downloaded
    /// or installed.
    pub(crate) fn with_android_host(mut self) -> Self {
        self.android_host = true;
        self.prefix_needs.gecko = false;
        self.prefix_needs.mono = false;
        self
    }

    #[cfg(test)]
    pub(crate) fn prefix_needs(&self) -> PrefixNeeds {
        self.prefix_needs
    }

    #[cfg(test)]
    pub(crate) fn android_host(&self) -> bool {
        self.android_host
    }

    /// The built-in FEX must be installed before Wine runs.
    pub(crate) fn with_builtin_fex_needed(mut self, needed: bool) -> Self {
        self.builtin_fex_needed = needed;
        self
    }

    /// Whether the Launcher's Wine archive is needed: its runtime is not
    /// accepted. Never on Android, whose app provides the runtime (its
    /// preflight needs the prefix, so before Install it is not accepted).
    fn needs_runtime_archive(&self) -> bool {
        !self.android_host && self.runtime != CheckStatus::Satisfied
    }

    /// Only the prefix components the audit rejected are provisioned.
    pub(crate) fn with_prefix_needs(mut self, needs: PrefixNeeds) -> Self {
        if self.prefix != CheckStatus::Satisfied {
            self.prefix_needs = needs;
        }
        self
    }
}

/// Prefix components the audit did not accept. An existing prefix is kept
/// and only these are installed into it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct PrefixNeeds {
    pub(crate) gecko: bool,
    pub(crate) mono: bool,
    pub(crate) dotnet: bool,
}

/// Verified prefix-component installers for one provisioning pass; `None`
/// keeps an accepted component exactly as audited. `template`: the template
/// a new prefix of the Android app's runtime is a copy of.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PrefixArtifacts<'a> {
    pub(crate) gecko: Option<&'a VerifiedArtifact>,
    pub(crate) mono: Option<&'a VerifiedArtifact>,
    pub(crate) dotnet: Option<&'a VerifiedArtifact>,
    pub(crate) template: crate::runtime::PrefixTemplate,
}

/// Whether the Android host's prefix (`paths.android()`) must be made again
/// from `template`: its mark ([`crate::runtime::PREFIX_TEMPLATE_MARK`]) names
/// the other template, and the Managed game folder holds nothing yet (Blizzard's
/// installers must never see the GOLDEN template's values of an installed
/// game; Fast download and an import get the official installers' state from
/// the GOLDEN one). A prefix without a readable mark is kept as it is.
pub(crate) fn prefix_has_other_template(
    paths: &ManagedPaths,
    template: crate::runtime::PrefixTemplate,
) -> bool {
    if !paths.android() {
        return false;
    }
    let mut mark = String::new();
    let marked = paths
        .directory()
        .open_directory(Path::new("prefix"))
        .and_then(|prefix| prefix.open_file(Path::new(crate::runtime::PREFIX_TEMPLATE_MARK)))
        .and_then(|file| file.take(64).read_to_string(&mut mark));
    let other = match (marked, mark.trim()) {
        (Ok(_), mark @ ("plain" | "golden")) => mark != template.mark(),
        _ => false,
    };
    other
        && match paths.open_game() {
            Ok(game) => game.names().is_ok_and(|names| names.is_empty()),
            Err(error) => error.kind() == std::io::ErrorKind::NotFound,
        }
}

pub(crate) trait InstallationAuditor {
    fn audit(&self, paths: &ManagedPaths) -> Result<InstallAudit, AdapterFailure>;
}

/// Stable Launcher-owned locations. Adapters receive these paths rather than
/// choosing an installation or prefix destination themselves.
pub(crate) struct ManagedPaths {
    directory: Directory,
    root: PathBuf,
    cache: PathBuf,
    runtime: PathBuf,
    prefix: PathBuf,
    game: PathBuf,
    game_folder: GameFolder,
    /// The layout of the Android host: the Managed game folder outside the
    /// prefix, the records of an import in the managed root.
    android: bool,
}

impl fmt::Debug for ManagedPaths {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManagedPaths")
            .field("root", &self.root)
            .field("cache", &self.cache)
            .field("runtime", &self.runtime)
            .field("prefix", &self.prefix)
            .field("game", &self.game)
            .finish_non_exhaustive()
    }
}

impl ManagedPaths {
    pub(crate) fn prepare(root: &Path) -> Result<Self, InstallError> {
        Self::prepare_on(root, crate::android_host())
    }

    /// [`Self::prepare`] with the layout of the Android host (`android`) or
    /// the desktop.
    pub(crate) fn prepare_on(root: &Path, android: bool) -> Result<Self, InstallError> {
        if !root.is_absolute() {
            return Err(InstallError::ManagedRootInvalid);
        }
        let directory =
            Directory::ensure_absolute(root).map_err(|_| InstallError::ManagedRootInvalid)?;
        let game_folder =
            GameFolder::read(&directory).map_err(|_| InstallError::GameFolderUnavailable)?;
        Ok(Self {
            game: game_folder.host_path_on(root, android),
            directory,
            root: root.to_owned(),
            cache: root.join("cache/artifacts"),
            runtime: root.join("runner").join(crate::runtime::current().identity),
            prefix: root.join("prefix"),
            game_folder,
            android,
        })
    }

    /// Whether these are the Android host's paths ([`Self::prepare_on`]).
    pub(crate) fn android(&self) -> bool {
        self.android
    }

    /// The folder of the records of an import
    /// ([`crate::game_folder::records_relative`]).
    pub(crate) fn records(&self) -> std::io::Result<Directory> {
        crate::game_folder::records_directory(&self.directory, self.android)
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn directory(&self) -> &Directory {
        &self.directory
    }

    #[cfg(test)]
    pub(crate) fn runtime(&self) -> &Path {
        &self.runtime
    }

    #[cfg(test)]
    pub(crate) fn prefix(&self) -> &Path {
        &self.prefix
    }

    /// The game folder's host path: the Managed one, or a folder used in
    /// place.
    pub(crate) fn game(&self) -> &Path {
        &self.game
    }

    /// Open the game folder: beneath the root, or a folder used in place
    /// proven to be the recorded one.
    pub(crate) fn open_game(&self) -> std::io::Result<Directory> {
        self.game_folder.open_on(&self.directory, self.android)
    }

    /// The recorded game folder: the Managed one or a folder used in place.
    pub(crate) fn game_folder(&self) -> &GameFolder {
        &self.game_folder
    }

    /// The game folder as Windows programs in the Managed prefix see it.
    pub(crate) fn windows_game(&self) -> String {
        self.game_folder.windows_path()
    }
}

/// Installer-side effect boundary. Implementations must use typed process
/// arguments, wait for the Launcher-owned process family and wineserver to be
/// quiescent, and write receipts only after the documented durable evidence is
/// present. Returning success never advances the workflow by itself.
pub(crate) trait InstallDriver {
    /// Provision the components the audit rejected: `wine` when the runtime
    /// was not accepted, and each prefix component (Gecko, Wine Mono, .NET
    /// Desktop Runtime) the prefix audit rejected. `None` keeps the accepted
    /// component exactly as audited.
    fn provision_runtime(
        &self,
        paths: &ManagedPaths,
        wine: Option<&VerifiedArtifact>,
        prefix: PrefixArtifacts<'_>,
        mutation_lease: &File,
        control: &OperationControl,
    ) -> Result<(), AdapterFailure>;

    /// Run a Blizzard downloader wrapper to fetch its product's complete
    /// installer, verify it against its frozen identity, and keep it as that
    /// product's complete installer in the artifact cache. The wrapper never
    /// receives a product key and nothing it fetched runs.
    fn download_vendor_payload(
        &self,
        paths: &ManagedPaths,
        wrapper: &VerifiedArtifact,
        mutation_lease: &File,
        control: &OperationControl,
    ) -> Result<(), VendorFailure>;

    /// The direct download: fetch the complete installers of `wrappers`'
    /// products at the same time, from the torrent and access cookie each
    /// verified wrapper carries, without Blizzard's downloader or Wine.
    /// Each verified payload is kept as its product's complete installer in
    /// the artifact cache; an unfinished one is kept for the next attempt.
    /// The failures, by product; none when every payload is kept.
    fn download_vendor_payloads_direct(
        &self,
        _paths: &ManagedPaths,
        wrappers: &[&VerifiedArtifact],
        _control: &OperationControl,
    ) -> Vec<(ArtifactKind, VendorFailure)> {
        wrappers
            .iter()
            .map(|wrapper| (wrapper.kind(), VendorFailure::Install))
            .collect()
    }

    fn install_vendor(
        &self,
        request: VendorInstallRequest<'_>,
        mutation_lease: &File,
        control: &OperationControl,
    ) -> Result<(), VendorFailure>;

    /// Remove what this Launcher's own downloads made, for an import
    /// (`discard_downloads`). The caller holds the installation lease.
    fn discard_downloads(&self, _paths: &ManagedPaths) -> Result<(), VendorFailure> {
        Err(VendorFailure::Install)
    }

    /// The native install of the direct download: the product's install
    /// script run from its verified complete installer into the game folder,
    /// without Blizzard's installer or Wine.
    fn install_vendor_native(
        &self,
        _request: VendorInstallRequest<'_>,
        _control: &OperationControl,
    ) -> Result<(), VendorFailure> {
        Err(VendorFailure::Install)
    }

    /// Make a validated Diablo II installation the game: copy it into
    /// the Managed game folder, or record it for use in place. Nothing
    /// else is written: no registry value, and last the record of the
    /// import. The caller holds the installation lease.
    fn import_existing(
        &self,
        paths: &ManagedPaths,
        existing: &ExistingInstall,
        mode: ImportMode,
        control: &OperationControl,
    ) -> Result<(), AdapterFailure>;

    fn install_client_bootstrap(
        &self,
        paths: &ManagedPaths,
        bootstrap: &VerifiedArtifact,
        mutation_lease: &File,
        control: &OperationControl,
    ) -> Result<(), AdapterFailure>;
}

/// Verified client synchronization is supplied by the updater workflow. This
/// keeps installation ordering here while manifest/staging policy remains in
/// `update`.
pub(crate) trait ClientSynchronizer {
    /// Download and verify every client file of the current manifest into
    /// the Launcher-owned cache (`cache/client`) without installing any.
    fn prefetch(
        &self,
        paths: &ManagedPaths,
        control: &OperationControl,
    ) -> Result<(), AdapterFailure>;

    fn synchronize(
        &self,
        paths: &ManagedPaths,
        control: &OperationControl,
    ) -> Result<(), AdapterFailure>;

    /// Record the client installed in the game folder as the accepted one,
    /// as it is: no client file is downloaded or changed. Each file of the
    /// current manifest that is there is recorded with its own MD5.
    fn accept_installed(
        &self,
        paths: &ManagedPaths,
        control: &OperationControl,
    ) -> Result<(), AdapterFailure>;

    /// Record the client that an import copied as the accepted one, from
    /// the copied files alone ([`crate::update::CopiedClientPd2Source`]):
    /// no file list from PD2's server, no download, no file changed. A
    /// record the folder brought (another PD2 Launcher's) is replaced.
    fn record_copied(
        &self,
        paths: &ManagedPaths,
        control: &OperationControl,
    ) -> Result<(), AdapterFailure>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum VendorProduct {
    BaseGame,
    Expansion,
}

impl VendorProduct {
    const fn artifact_kind(self) -> ArtifactKind {
        match self {
            Self::BaseGame => ArtifactKind::BaseGameWrapper,
            Self::Expansion => ArtifactKind::ExpansionWrapper,
        }
    }

    const fn download_stage(self) -> InstallStage {
        match self {
            Self::BaseGame => InstallStage::BaseGameDownload,
            Self::Expansion => InstallStage::ExpansionDownload,
        }
    }
}

pub(crate) struct VendorInstallRequest<'a> {
    product: VendorProduct,
    artifact: &'a VerifiedArtifact,
    paths: &'a ManagedPaths,
    product_key: &'a ProductKey,
}

impl VendorInstallRequest<'_> {
    pub(crate) fn product(&self) -> VendorProduct {
        self.product
    }

    pub(crate) fn artifact(&self) -> &VerifiedArtifact {
        self.artifact
    }

    pub(crate) fn paths(&self) -> &ManagedPaths {
        self.paths
    }

    /// Exposes the key only for the duration of one adapter-owned callback so
    /// workflow APIs cannot return or format it accidentally.
    pub(crate) fn with_product_key<T>(&self, use_key: impl FnOnce(&[u8]) -> T) -> T {
        use_key(self.product_key.expose())
    }
}

impl fmt::Debug for VendorInstallRequest<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VendorInstallRequest")
            .field("product", &self.product)
            .field("artifact", &self.artifact.path)
            .field("paths", &self.paths)
            .field("product_key", &Redacted)
            .finish()
    }
}

/// Boundary failures intentionally carry no adapter-provided text, because a
/// Vendor installer may echo private inputs in its own error rendering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AdapterFailure;

impl AdapterFailure {
    /// A failure at the caller's source line, which goes to standard error
    /// (some systems, such as DroidDeck, keep nothing else). Only the line
    /// is printed, never data. Some of these failures are expected and
    /// handled by the caller.
    #[track_caller]
    pub(crate) fn here() -> Self {
        let at = std::panic::Location::caller();
        eprintln!(
            "pd2-launcher: install step stopped at {}:{} (it may be handled)",
            at.file(),
            at.line()
        );
        Self
    }
}

/// Why the download cache could not be used.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CacheProblem {
    /// An entry the cache does not recognise, or an unsafe one.
    Inventory(ArtifactRejection),
    /// The cache folder itself could not be opened safely.
    Folder,
    /// A stopped write's temporary files could not be cleaned up.
    RetainedState,
}

/// Where a Vendor stage failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum VendorFailure {
    /// Blizzard's downloader ended without a verified payload in some other
    /// way (no window, could not be stopped, its own exit, its timeout), so
    /// no installer ran. A Blizzard wrapper's embedded download authorization
    /// expires about a week after it was fetched, so this and the next three
    /// are retried once with a freshly downloaded wrapper.
    WrapperDownload,
    /// Blizzard's downloader showed a message box (its helper prints the
    /// text).
    DownloaderErrorDialog,
    /// The downloader ended with its download unfinished: a file missing or
    /// still a `.part`.
    PayloadIncomplete,
    /// The downloader finished, but a file differs from its frozen identity
    /// (size, hash, an unexpected entry).
    PayloadRejected,
    /// Blizzard's downloader got no new data for its stall limit, as when
    /// the connection is lost: a network failure, so the wrapper is kept and
    /// the stage fails at once.
    DownloadStalled,
    /// The installer showed its error dialog about the product key it was
    /// given: the key was rejected.
    KeyRejected,
    /// The installer showed any other error dialog.
    ErrorDialog,
    /// The installer stayed at its main menu instead of installing.
    InstallerMenu,
    /// The direct download delivered no verified payload.
    DirectDownload(DirectFailure),
    /// Anything else, including a failure after the installer started.
    Install,
}

impl VendorFailure {
    /// Whether a freshly downloaded wrapper may succeed where this failed:
    /// every downloader failure but a stall, which is the connection's, and
    /// a direct download whose access cookie the piece host refused.
    fn may_need_fresh_wrapper(self) -> bool {
        matches!(
            self,
            Self::WrapperDownload
                | Self::DownloaderErrorDialog
                | Self::PayloadIncomplete
                | Self::PayloadRejected
                | Self::DirectDownload(DirectFailure::Refused)
        )
    }

    /// How Blizzard's downloader failed, when this is its failure.
    fn downloader_failure(self) -> Option<DownloaderFailure> {
        match self {
            Self::WrapperDownload => Some(DownloaderFailure::Ended),
            Self::DownloaderErrorDialog => Some(DownloaderFailure::ErrorDialog),
            Self::PayloadIncomplete => Some(DownloaderFailure::Incomplete),
            Self::PayloadRejected => Some(DownloaderFailure::Rejected),
            Self::DownloadStalled => Some(DownloaderFailure::Stalled),
            Self::KeyRejected
            | Self::ErrorDialog
            | Self::InstallerMenu
            | Self::DirectDownload(_)
            | Self::Install => None,
        }
    }
}

/// How the direct download failed to deliver a verified payload, each with
/// its own failure code (`install_failure_code`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DirectFailure {
    /// The piece host refused the wrapper's access cookie (403) before any
    /// piece arrived, or once it expired during the download, or it expires
    /// too soon to start: retried once with a freshly downloaded wrapper,
    /// which continues from the pieces kept.
    Refused,
    /// Pieces were still missing after every retry and round: the
    /// connection.
    Incomplete,
    /// The wrapper's torrent, or a finished file, differs from its pins:
    /// nothing is installed from it.
    Mismatch,
}

/// The error of a failed download stage: how Blizzard's downloader or the
/// direct download failed, when it was theirs.
fn vendor_download_error(
    control: &OperationControl,
    stage: InstallStage,
    failure: VendorFailure,
) -> InstallError {
    match (stage_failure(control, stage), failure) {
        (InstallError::StageFailed { stage }, VendorFailure::DirectDownload(failure)) => {
            InstallError::DirectDownloadFailed { stage, failure }
        }
        (InstallError::StageFailed { stage }, failure) => match failure.downloader_failure() {
            Some(failure) => InstallError::DownloaderFailed { stage, failure },
            None => InstallError::StageFailed { stage },
        },
        (error, _) => error,
    }
}

/// How Blizzard's downloader failed to deliver a verified payload, each with
/// its own failure code (`install_failure_code`): a lost connection, an
/// error it reported, a download it left unfinished, files that differ from
/// their identity, or any other end.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DownloaderFailure {
    Stalled,
    ErrorDialog,
    Incomplete,
    Rejected,
    Ended,
}

impl From<AdapterFailure> for VendorFailure {
    fn from(_: AdapterFailure) -> Self {
        Self::Install
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InstallStage {
    /// Recording the Download result.
    Download,
    ManagedRuntime,
    /// Blizzard's downloader fetching the Diablo II installer.
    BaseGameDownload,
    /// Blizzard's downloader fetching the Lord of Destruction installer.
    ExpansionDownload,
    /// Verifying the Project Diablo 2 client files into the cache.
    ClientPrefetch,
    BaseGame,
    Expansion,
    /// Copying or recording a Diablo II the user already has.
    Import,
    ClientBootstrap,
    ClientSynchronization,
    /// Unpacking the built-in FEX and its RootFS.
    BuiltinFex,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InstallOutcome {
    final_status: InstallationStatus,
    completed_stages: Vec<InstallStage>,
}

impl InstallOutcome {
    pub(crate) fn final_status(&self) -> InstallationStatus {
        self.final_status
    }

    #[cfg(test)]
    pub(crate) fn completed_stages(&self) -> &[InstallStage] {
        &self.completed_stages
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum InstallError {
    ManagedRootInvalid,
    /// With the update checks off, the Project Diablo 2 in the game folder
    /// cannot be kept as it is (no Game.exe or ProjectDiablo.dll, a file of
    /// its record gone, or its file list unreadable). Nothing replaces it.
    ClientNotKept,
    /// The recorded game folder used in place is unavailable.
    GameFolderUnavailable,
    InstallationBusy,
    AuditUnavailable,
    UnsupportedHost,
    HostEvidenceUnavailable,
    AuthorizationRequired,
    RepairRequired,
    Indeterminate,
    /// The audit found an interrupted stage; only Start over continues.
    StageInterrupted,
    RuntimeEvidenceInvalid,
    AcquisitionFailed {
        artifact: ArtifactKind,
    },
    /// The download cache itself cannot be used, before anything is
    /// downloaded: entries the Launcher does not recognise, or retained
    /// write state it cannot account for (a folder copied mid-write, or
    /// from another computer). Never a network failure.
    DownloadCacheUnusable {
        artifact: ArtifactKind,
        cause: CacheProblem,
    },
    ArtifactRejected {
        artifact: ArtifactKind,
        reason: ArtifactRejection,
    },
    StageFailed {
        stage: InstallStage,
    },
    /// Blizzard's downloader for `stage` (a download stage) delivered no
    /// verified payload, as `failure` says.
    DownloaderFailed {
        stage: InstallStage,
        failure: DownloaderFailure,
    },
    /// The direct download for `stage` (a download stage) delivered no
    /// verified payload, as `failure` says.
    DirectDownloadFailed {
        stage: InstallStage,
        failure: DirectFailure,
    },
    /// The Vendor installer showed its error dialog about the product key
    /// for `stage`: that key was rejected.
    KeyRejected {
        stage: InstallStage,
    },
    /// The key for `stage` failed the installers' own check before any
    /// installer ran.
    KeyInvalid {
        stage: InstallStage,
        problem: crate::blizzard::KeyProblem,
    },
    /// The Vendor installer for `stage` showed another error dialog.
    InstallerError {
        stage: InstallStage,
    },
    /// The Vendor installer for `stage` stayed at its main menu instead of
    /// installing.
    InstallerMenu {
        stage: InstallStage,
    },
    EvidenceDidNotAdvance {
        stage: InstallStage,
        expected: InstallationStatus,
        observed: InstallationStatus,
    },
    /// Diablo II is already installed; this step no longer applies.
    BaseGameInstalled,
    /// Project Diablo 2 needs Diablo II installed first.
    BaseGameRequired,
    /// The existing installation is no longer a usable Diablo II folder.
    ExistingInstallInvalid(ExistingInstallProblem),
    Cancelled,
}

impl fmt::Display for InstallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "managed installation stopped: {self:?}")
    }
}

impl Error for InstallError {}

pub(crate) struct ManagedInstaller<'a> {
    auditor: &'a dyn InstallationAuditor,
    artifacts: &'a dyn ArtifactSource,
    driver: &'a dyn InstallDriver,
    client: &'a dyn ClientSynchronizer,
    catalog: ArtifactCatalog,
    /// The Download job runs: acquisitions are downloads. In the install
    /// jobs they only reverify the cache ahead of installing.
    downloading: std::cell::Cell<bool>,
    /// Blizzard's installers come from the direct download, not from
    /// Blizzard's downloader under Wine.
    direct_download: bool,
    /// The player turned Project Diablo 2's update checks off: a Project
    /// Diablo 2 already in the game folder is kept as it is.
    client_as_installed: bool,
    /// The job imports a game: a new prefix of the Android app's runtime is
    /// a copy of the GOLDEN template, as for the direct download.
    importing: bool,
}

/// What the Download job verified.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DownloadOutcome {
    /// Bytes of every verified download the rest of the install uses.
    pub(crate) total_bytes: u64,
}

impl<'a> ManagedInstaller<'a> {
    pub(crate) fn new(
        auditor: &'a dyn InstallationAuditor,
        artifacts: &'a dyn ArtifactSource,
        driver: &'a dyn InstallDriver,
        client: &'a dyn ClientSynchronizer,
        catalog: ArtifactCatalog,
    ) -> Self {
        Self {
            auditor,
            artifacts,
            driver,
            client,
            catalog,
            downloading: std::cell::Cell::new(false),
            direct_download: false,
            client_as_installed: false,
            importing: false,
        }
    }

    /// The template a new prefix of the Android app's runtime is made from
    /// in this job: GOLDEN for the direct download and an import, PLAIN for
    /// Blizzard's installers.
    fn prefix_template(&self) -> crate::runtime::PrefixTemplate {
        crate::runtime::PrefixTemplate::for_source(self.direct_download, self.importing)
    }

    /// Whether the job's setup must run although the audit accepts the
    /// runtime and the prefix: the Android host's prefix was made from the
    /// other template, and the game is not there yet
    /// ([`prefix_has_other_template`]).
    fn needs_other_template(&self, paths: &ManagedPaths, report: &InstallAudit) -> bool {
        report.status == InstallationStatus::NeedsBaseGame
            && prefix_has_other_template(paths, self.prefix_template())
    }

    /// Fetch Blizzard's installers with the direct download.
    pub(crate) fn with_direct_download(mut self, direct_download: bool) -> Self {
        self.direct_download = direct_download;
        self
    }

    /// With Project Diablo 2's update checks off, a Project Diablo 2 already
    /// in the game folder (an import's) is accepted as it is installed, of
    /// any version or modded, instead of brought up to date.
    pub(crate) fn with_client_as_installed(mut self, as_installed: bool) -> Self {
        self.client_as_installed = as_installed;
        self
    }

    #[cfg(test)]
    pub(crate) fn run(
        &mut self,
        managed_root: &Path,
        authorization: Option<&InstallationAuthorization>,
    ) -> Result<InstallOutcome, InstallError> {
        self.run_controlled(managed_root, authorization, &OperationControl::default())
    }

    /// Every step in order, as unattended mode runs them: Download, then
    /// Install Diablo II, then Install Project Diablo 2.
    #[cfg(test)]
    pub(crate) fn run_controlled(
        &mut self,
        managed_root: &Path,
        authorization: Option<&InstallationAuthorization>,
        control: &OperationControl,
    ) -> Result<InstallOutcome, InstallError> {
        self.download_all(managed_root, DownloadPlan::Installers, control)?;
        let mut outcome = self.install_base_game(managed_root, authorization, control)?;
        if outcome.final_status == InstallationStatus::Ready {
            return Ok(outcome);
        }
        let project = self.install_project_diablo(managed_root, control)?;
        outcome.completed_stages.extend(project.completed_stages);
        outcome.final_status = project.final_status;
        Ok(outcome)
    }

    /// Step 3, Download: acquire and verify everything the remaining steps
    /// need, so they run from verified local files. Blizzard's installers
    /// are fetched by their downloader into the Launcher-owned cache (which
    /// needs the Wine runtime and prefix first); the Project Diablo 2 client
    /// files are verified into the cache but not installed. The result is
    /// recorded for "downloads complete". No CD key is needed.
    pub(crate) fn download_all(
        &mut self,
        managed_root: &Path,
        plan: DownloadPlan,
        control: &OperationControl,
    ) -> Result<DownloadOutcome, InstallError> {
        self.downloading.set(true);
        let result = self.download_everything(managed_root, plan, control);
        self.downloading.set(false);
        result
    }

    /// Only the built-in FEX the player chose, for an
    /// installation whose other downloads are done: from Settings, or after
    /// choosing it once the game was installed.
    pub(crate) fn install_builtin_fex_only(
        &mut self,
        managed_root: &Path,
        control: &OperationControl,
    ) -> Result<DownloadOutcome, InstallError> {
        self.downloading.set(true);
        let result = (|| {
            let paths = ManagedPaths::prepare(managed_root)?;
            drop(MutationLease::acquire(&paths)?);
            check_cancelled(control)?;
            let report = self.audit(&paths)?;
            check_host(report.host)?;
            check_recovery_status(report.status)?;
            let total_bytes = if report.builtin_fex_needed {
                self.install_builtin_fex(&paths, control)?
            } else {
                0
            };
            Ok(DownloadOutcome { total_bytes })
        })();
        self.downloading.set(false);
        result
    }

    fn download_everything(
        &self,
        managed_root: &Path,
        plan: DownloadPlan,
        control: &OperationControl,
    ) -> Result<DownloadOutcome, InstallError> {
        let paths = ManagedPaths::prepare(managed_root)?;
        drop(MutationLease::acquire(&paths)?);
        check_cancelled(control)?;
        let mut report = self.audit(&paths)?;
        check_host(report.host)?;
        check_recovery_status(report.status)?;
        // The built-in FEX comes first: every Wine program after it, Blizzard's
        // downloader included, runs through it.
        let mut fex_bytes = 0;
        if report.builtin_fex_needed {
            fex_bytes = self.install_builtin_fex(&paths, control)?;
        }
        if report.status == InstallationStatus::Ready {
            return Ok(DownloadOutcome {
                total_bytes: fex_bytes,
            });
        }
        {
            let _lease = MutationLease::acquire(&paths)?;
            crate::downloads::clear(paths.directory()).map_err(|_| InstallError::StageFailed {
                stage: InstallStage::Download,
            })?;
        }
        let status = report.status;
        let installers = plan == DownloadPlan::Installers;
        let vendor: &[VendorProduct] = match status {
            InstallationStatus::NeedsBaseGame if installers => {
                &[VendorProduct::BaseGame, VendorProduct::Expansion]
            }
            InstallationStatus::NeedsExpansion if installers => &[VendorProduct::Expansion],
            _ => &[],
        };
        // On the Android host an import brings its own Project Diablo 2
        // (the folder check asks for it) and records it as it is: neither
        // the PD2 installer nor the client is downloaded for it.
        let client = status != InstallationStatus::NeedsRuntime
            && !(report.android_host && plan == DownloadPlan::ExistingInstall);
        let needs = report.prefix_needs;
        let prefix_needed = report.prefix != CheckStatus::Satisfied;
        // What these downloads reach, so a later need beyond it (after Start
        // over, say) is not taken as served.
        let need = crate::downloads::DownloadNeed {
            prefix: prefix_needed,
            ..crate::downloads::DownloadNeed::for_status(status, prefix_needed)
        };
        let mut kinds = Vec::new();
        if report.needs_runtime_archive() {
            kinds.push(ArtifactKind::ManagedRuntime);
        }
        for (needed, kind) in [
            (needs.gecko, ArtifactKind::Gecko),
            (needs.mono, ArtifactKind::WineMono),
            (needs.dotnet, ArtifactKind::DotnetDesktop),
        ] {
            if prefix_needed && needed {
                kinds.push(kind);
            }
        }
        for product in vendor {
            kinds.push(product.artifact_kind());
        }
        if client {
            kinds.push(ArtifactKind::ClientBootstrap);
        }
        let steps = kinds.len() as u64 + u64::from(client);
        let mut recorded = Vec::new();
        let mut step = 0_u64;
        let mut direct_done = false;
        for kind in kinds {
            control.set_step(step, steps);
            step += 1;
            // On Android Blizzard's downloaders run in Install; Download
            // keeps their wrappers, verified.
            if matches!(
                kind,
                ArtifactKind::BaseGameWrapper | ArtifactKind::ExpansionWrapper
            ) && !report.android_host
            {
                if self.direct_download {
                    // Every product at the same time, at the first of them.
                    if !direct_done {
                        self.download_vendor_payloads_direct(&paths, vendor, control)?;
                        direct_done = true;
                    }
                } else {
                    let product = if kind == ArtifactKind::BaseGameWrapper {
                        VendorProduct::BaseGame
                    } else {
                        VendorProduct::Expansion
                    };
                    report = self.download_vendor_payload(&paths, report, product, control)?;
                }
                recorded.push(payload_record_path(kind));
                continue;
            }
            let artifact = {
                let _lease = MutationLease::acquire(&paths)?;
                self.acquire(&paths, kind, control)?
            };
            recorded.push(if is_complete_payload(&paths, &artifact) {
                payload_record_path(kind)
            } else {
                root_relative(&paths, artifact.path())?
            });
        }
        if client {
            control.set_step(step, steps);
            control.report(WorkProgress::for_phase(WorkPhase::ClientDownload));
            let _lease = MutationLease::acquire(&paths)?;
            self.client
                .prefetch(&paths, control)
                .map_err(|_| stage_failure(control, InstallStage::ClientPrefetch))?;
            recorded.push(CLIENT_PREFETCH.to_owned());
        }
        check_cancelled(control)?;
        control.set_step(steps, steps);
        let _lease = MutationLease::acquire(&paths)?;
        let total_bytes = crate::downloads::record(paths.directory(), plan, need, &recorded)
            .map_err(|_| InstallError::StageFailed {
                stage: InstallStage::Download,
            })?;
        Ok(DownloadOutcome { total_bytes })
    }

    /// Download the built-in FEX's package and RootFS, verified, and install
    /// them in the managed root. Their size, in bytes.
    fn install_builtin_fex(
        &self,
        paths: &ManagedPaths,
        control: &OperationControl,
    ) -> Result<u64, InstallError> {
        check_cancelled(control)?;
        let (package, rootfs) = {
            let _lease = MutationLease::acquire(paths)?;
            (
                self.acquire(paths, ArtifactKind::FexPackage, control)?,
                self.acquire(paths, ArtifactKind::FexRootFs, control)?,
            )
        };
        check_cancelled(control)?;
        let _lease = MutationLease::acquire(paths)?;
        crate::builtin_fex::install(
            paths.root(),
            crate::builtin_fex::FexVariant::current(),
            package.path(),
            rootfs.path(),
        )
        .map_err(|error| {
            eprintln!("pd2-launcher: the built-in FEX could not be installed: {error}");
            stage_failure(control, InstallStage::BuiltinFex)
        })?;
        Ok([package.path(), rootfs.path()]
            .iter()
            .filter_map(|path| std::fs::metadata(path).ok())
            .map(|metadata| metadata.len())
            .sum())
    }

    /// Fetch one product's complete installer into the cache. A payload
    /// already there (a folder's complete installers, or an earlier
    /// download) is used as it is; otherwise Blizzard's downloader runs in
    /// the Managed prefix, which is set up first when it isn't yet.
    fn download_vendor_payload(
        &self,
        paths: &ManagedPaths,
        mut report: InstallAudit,
        product: VendorProduct,
        control: &OperationControl,
    ) -> Result<InstallAudit, InstallError> {
        let kind = product.artifact_kind();
        let stage = product.download_stage();
        check_cancelled(control)?;
        let mut artifact = {
            let _lease = MutationLease::acquire(paths)?;
            self.acquire(paths, kind, control)?
        };
        if is_complete_payload(paths, &artifact) {
            return Ok(report);
        }
        if report.runtime != CheckStatus::Satisfied
            || report.prefix != CheckStatus::Satisfied
            || self.needs_other_template(paths, &report)
        {
            report = self.ensure_runtime(paths, report, control)?;
        }
        control.report(WorkProgress::for_phase(match product {
            VendorProduct::BaseGame => WorkPhase::BaseGameDownload,
            VendorProduct::Expansion => WorkPhase::ExpansionDownload,
        }));
        let lease = MutationLease::acquire(paths)?;
        let mut refetched = false;
        loop {
            match self
                .driver
                .download_vendor_payload(paths, &artifact, &lease.0, control)
            {
                Ok(()) => break,
                // A cached Blizzard wrapper carries a download authorization
                // that expires; discard it and download a fresh one once.
                // Selected media and custom sources report the failure, and
                // so does a stalled download (DownloadStalled).
                Err(failure)
                    if failure.may_need_fresh_wrapper()
                        && !refetched
                        && !control.is_cancelled()
                        && artifact.origin == ArtifactOrigin::ApprovedNetwork
                        && artifact.custom_source().is_none()
                        && !self.catalog.spec(kind).is_custom() =>
                {
                    refetched = true;
                    self.discard_cached_wrapper(paths, kind)?;
                    artifact = self.acquire(paths, kind, control)?;
                    check_cancelled(control)?;
                }
                Err(failure) => return Err(vendor_download_error(control, stage, failure)),
            }
        }
        check_cancelled(control)?;
        Ok(report)
    }

    /// Fetch the complete installers of `products` with the direct
    /// download, all at the same time. A payload already in the cache is
    /// used as it is. No Wine runs, so the runtime is not set up for it. A
    /// cached wrapper whose access cookie the piece host refuses is
    /// discarded and downloaded again once per product, as for Blizzard's
    /// downloader.
    fn download_vendor_payloads_direct(
        &self,
        paths: &ManagedPaths,
        products: &[VendorProduct],
        control: &OperationControl,
    ) -> Result<(), InstallError> {
        check_cancelled(control)?;
        let mut wanted = Vec::new();
        for &product in products {
            let artifact = {
                let _lease = MutationLease::acquire(paths)?;
                self.acquire(paths, product.artifact_kind(), control)?
            };
            if !is_complete_payload(paths, &artifact) {
                wanted.push((product, artifact, false));
            }
        }
        let Some((first, ..)) = wanted.first() else {
            return Ok(());
        };
        control.report(WorkProgress::for_phase(match first {
            VendorProduct::BaseGame => WorkPhase::BaseGameDownload,
            VendorProduct::Expansion => WorkPhase::ExpansionDownload,
        }));
        let _lease = MutationLease::acquire(paths)?;
        while !wanted.is_empty() {
            let failures = {
                let wrappers = wanted
                    .iter()
                    .map(|(_, artifact, _)| artifact)
                    .collect::<Vec<_>>();
                self.driver
                    .download_vendor_payloads_direct(paths, &wrappers, control)
            };
            let mut again = Vec::new();
            for (product, artifact, refetched) in wanted {
                let kind = product.artifact_kind();
                let Some(&(_, failure)) = failures.iter().find(|(failed, _)| *failed == kind)
                else {
                    continue;
                };
                if failure.may_need_fresh_wrapper()
                    && !refetched
                    && !control.is_cancelled()
                    && artifact.origin == ArtifactOrigin::ApprovedNetwork
                    && artifact.custom_source().is_none()
                    && !self.catalog.spec(kind).is_custom()
                {
                    self.discard_cached_wrapper(paths, kind)?;
                    again.push((product, self.acquire(paths, kind, control)?, true));
                    continue;
                }
                return Err(vendor_download_error(
                    control,
                    product.download_stage(),
                    failure,
                ));
            }
            check_cancelled(control)?;
            wanted = again;
        }
        Ok(())
    }

    /// Remove what this Launcher's own downloads made, under the
    /// installation lease, for a player who imports the game instead.
    pub(crate) fn discard_downloads(
        &self,
        managed_root: &Path,
        control: &OperationControl,
    ) -> Result<(), InstallError> {
        let paths = ManagedPaths::prepare(managed_root)?;
        let _lease = MutationLease::acquire(&paths)?;
        check_cancelled(control)?;
        self.driver
            .discard_downloads(&paths)
            .map_err(|_| InstallError::StageFailed {
                stage: InstallStage::Download,
            })
    }

    /// Step 4, Install Diablo II: Blizzard's installers for Diablo II and
    /// Lord of Destruction with the CD keys, from the verified downloads.
    /// The game is never patched: Project Diablo 2 brings its own game
    /// files. The Wine runtime and prefix are set up first
    /// when they aren't yet. Ends at `NeedsClient`.
    pub(crate) fn install_base_game(
        &mut self,
        managed_root: &Path,
        authorization: Option<&InstallationAuthorization>,
        control: &OperationControl,
    ) -> Result<InstallOutcome, InstallError> {
        let paths = ManagedPaths::prepare(managed_root)?;
        drop(MutationLease::acquire(&paths)?);
        let mut completed_stages = Vec::new();
        check_cancelled(control)?;
        let mut report = self.audit(&paths)?;
        check_host(report.host)?;
        match report.status {
            // Diablo II is already installed: nothing is left for this step
            // (a missing Wine component is Install Project Diablo 2's).
            InstallationStatus::Ready
            | InstallationStatus::NeedsClient
            | InstallationStatus::NeedsRuntime => {
                return Ok(InstallOutcome {
                    final_status: report.status,
                    completed_stages,
                });
            }
            _ => check_recovery_status(report.status)?,
        }
        if report.runtime != CheckStatus::Satisfied
            || report.prefix != CheckStatus::Satisfied
            || self.needs_other_template(&paths, &report)
        {
            report = self.ensure_runtime(&paths, report, control)?;
            completed_stages.push(InstallStage::ManagedRuntime);
        }

        // On Android each of Blizzard's downloaders runs here, right before
        // its installer, not in Download: Diablo II is downloaded and
        // installed, then Lord of Destruction.
        if report.status == InstallationStatus::NeedsBaseGame {
            let authorization = authorization.ok_or(InstallError::AuthorizationRequired)?;
            if report.android_host && self.direct_download {
                // Both at the same time; Lord of Destruction's is then kept
                // for its own step.
                self.download_vendor_payloads_direct(
                    &paths,
                    &[VendorProduct::BaseGame, VendorProduct::Expansion],
                    control,
                )?;
            } else if report.android_host {
                self.download_vendor_payload(&paths, report, VendorProduct::BaseGame, control)?;
            }
            self.install_vendor(&paths, authorization, VendorProduct::BaseGame, control)?;
            completed_stages.push(InstallStage::BaseGame);
            report = self.expect_after(
                &paths,
                InstallStage::BaseGame,
                InstallationStatus::NeedsExpansion,
            )?;
            clear_stage(&paths, InstallStage::BaseGame)?;
        }

        if report.status == InstallationStatus::NeedsExpansion {
            let authorization = authorization.ok_or(InstallError::AuthorizationRequired)?;
            if report.android_host && self.direct_download {
                self.download_vendor_payloads_direct(&paths, &[VendorProduct::Expansion], control)?;
            } else if report.android_host {
                self.download_vendor_payload(&paths, report, VendorProduct::Expansion, control)?;
            }
            self.install_vendor(&paths, authorization, VendorProduct::Expansion, control)?;
            completed_stages.push(InstallStage::Expansion);
            report = self.expect_after(
                &paths,
                InstallStage::Expansion,
                InstallationStatus::NeedsClient,
            )?;
            clear_stage(&paths, InstallStage::Expansion)?;
        }

        Ok(InstallOutcome {
            final_status: report.status,
            completed_stages,
        })
    }

    /// Step 4 for a Diablo II the user already has, of any version: set up
    /// the Wine runtime and prefix when needed, then copy the game into the
    /// Managed game folder or record it for use in place. No registry value
    /// is written (the game writes `InstallPath` when the value is missing) and
    /// no CD key is needed; the folder is never patched. Its files prove
    /// it. Ends at `NeedsClient`.
    pub(crate) fn import_existing(
        &mut self,
        managed_root: &Path,
        existing: &ExistingInstall,
        mode: ImportMode,
        control: &OperationControl,
    ) -> Result<InstallOutcome, InstallError> {
        let paths = ManagedPaths::prepare(managed_root)?;
        drop(MutationLease::acquire(&paths)?);
        // A new prefix is a copy of the GOLDEN template.
        self.importing = true;
        let mut completed_stages = Vec::new();
        check_cancelled(control)?;
        let mut report = self.audit(&paths)?;
        check_host(report.host)?;
        check_recovery_status(report.status)?;
        // Only a game folder that holds nothing yet is imported into.
        if report.status != InstallationStatus::NeedsBaseGame {
            return Err(InstallError::BaseGameInstalled);
        }
        let existing = existing
            .revalidate(managed_root)
            .map_err(InstallError::ExistingInstallInvalid)?;
        if mode == ImportMode::InPlace && !crate::game_folder::usable_in_place(&existing) {
            return Err(InstallError::ExistingInstallInvalid(
                crate::game_folder::ExistingInstallProblem::UnsupportedPath,
            ));
        }
        if report.runtime != CheckStatus::Satisfied
            || report.prefix != CheckStatus::Satisfied
            || self.needs_other_template(&paths, &report)
        {
            report = self.ensure_runtime(&paths, report, control)?;
            completed_stages.push(InstallStage::ManagedRuntime);
        }
        debug_assert_eq!(report.status, InstallationStatus::NeedsBaseGame);
        check_cancelled(control)?;
        control.report(match mode {
            ImportMode::Copy => WorkProgress::for_phase(WorkPhase::ImportGame)
                .with_file_bytes(0, Some(existing.copy_bytes())),
            // Nothing is copied: the folder's record and the import record
            // are written.
            ImportMode::InPlace => WorkProgress::for_phase(WorkPhase::ImportGame),
        });
        {
            let _lease = MutationLease::acquire(&paths)?;
            // A copy that stops part way leaves files only Start over
            // removes, like a Vendor installer.
            mark_stage(&paths, InterruptedStage::BaseGame, InstallStage::Import)?;
            self.driver
                .import_existing(&paths, &existing, mode, control)
                .map_err(|_| stage_failure(control, InstallStage::Import))?;
            finish_stage(&paths, InterruptedStage::BaseGame, InstallStage::Import)?;
        }
        check_cancelled(control)?;
        completed_stages.push(InstallStage::Import);
        // The game folder is read again: a folder used in place is the game
        // folder from now on (the driver wrote its record), and its
        // ProjectD2 is the one to record.
        let paths = ManagedPaths::prepare(managed_root)?;
        let mut report = self.audit(&paths)?;
        check_host(report.host)?;
        check_recovery_status(report.status)?;
        // On the Android host the import records the Project Diablo 2 it
        // copied, as it is, from the copied files alone: no update, no
        // download, no PD2 installer and no file list from PD2's server,
        // whatever the update checks (the owner's rule of 2026-10-26). The
        // folder check asked for its Game.exe, ProjectDiablo.dll and
        // PD2Launcher.exe. Play's update check then offers the update.
        let recorded = report.android_host
            && matches!(
                report.status,
                InstallationStatus::NeedsClient | InstallationStatus::Ready
            )
            && has_project_diablo(&paths);
        if recorded {
            {
                let _lease = MutationLease::acquire(&paths)?;
                self.client
                    .record_copied(&paths, control)
                    .map_err(|_| stage_failure(control, InstallStage::Import))?;
                // Without the mark the first Play only asks a question that
                // an Update with nothing to download answers.
                if mark_copied_client(&paths).is_err() {
                    eprintln!(
                        "pd2-launcher: the mark of the copied client's record could not be written: the first Play asks for the update"
                    );
                }
            }
            check_cancelled(control)?;
            report = self.audit(&paths)?;
            check_host(report.host)?;
            check_recovery_status(report.status)?;
            if report.status != InstallationStatus::Ready {
                return Err(InstallError::EvidenceDidNotAdvance {
                    stage: InstallStage::Import,
                    expected: InstallationStatus::Ready,
                    observed: report.status,
                });
            }
        }
        // A folder used in place may bring the Project Diablo 2 this
        // Launcher installed elsewhere (its ProjectD2 with the update
        // namespace), and so may a copy on the desktop, which brings
        // ProjectD2 along: the audit accepts it, so the PD2 installer is
        // skipped. The Android app's copies are its own; the engine's copy
        // there leaves ProjectD2 out.
        let accepted = report.status == InstallationStatus::NeedsClient
            || (report.status == InstallationStatus::Ready
                && (mode == ImportMode::InPlace || !report.android_host || recorded));
        if !accepted {
            return Err(InstallError::EvidenceDidNotAdvance {
                stage: InstallStage::Import,
                expected: InstallationStatus::NeedsClient,
                observed: report.status,
            });
        }
        clear_stage(&paths, InstallStage::Import)?;
        Ok(InstallOutcome {
            final_status: report.status,
            completed_stages,
        })
    }

    /// Step 5, Install Project Diablo 2: the PD2 installer, then the client
    /// files, from the verified downloads (only files that changed since
    /// are fetched). A complete game missing only a Wine component gets
    /// that component first. A ProjectD2 already in the game folder (one
    /// the official PD2 launcher or the PD2 installer put there, brought by
    /// an imported Diablo II) is adopted instead: brought up to date like an
    /// update, with no PD2 installer; with the update checks off, accepted
    /// as it is ([`Self::with_client_as_installed`]). On the Android host an
    /// import records its Project Diablo 2 itself ([`Self::import_existing`]),
    /// so this step does not run after it. Ends at `Ready`.
    pub(crate) fn install_project_diablo(
        &mut self,
        managed_root: &Path,
        control: &OperationControl,
    ) -> Result<InstallOutcome, InstallError> {
        let paths = ManagedPaths::prepare(managed_root)?;
        drop(MutationLease::acquire(&paths)?);
        let mut completed_stages = Vec::new();
        check_cancelled(control)?;
        let mut report = self.audit(&paths)?;
        check_host(report.host)?;
        check_recovery_status(report.status)?;
        match report.status {
            InstallationStatus::Ready => {
                return Ok(InstallOutcome {
                    final_status: InstallationStatus::Ready,
                    completed_stages,
                });
            }
            InstallationStatus::NeedsClient | InstallationStatus::NeedsRuntime => {}
            _ => return Err(InstallError::BaseGameRequired),
        }
        if report.runtime != CheckStatus::Satisfied || report.prefix != CheckStatus::Satisfied {
            report = self.ensure_runtime(&paths, report, control)?;
            completed_stages.push(InstallStage::ManagedRuntime);
        }
        // With the update checks off, a Project Diablo 2 already there is
        // kept as it is, or the step stops: the PD2 installer would replace
        // the version the player keeps.
        if report.status == InstallationStatus::NeedsClient
            && self.client_as_installed
            && has_project_folder(&paths)
            && !has_project_diablo(&paths)
        {
            eprintln!(
                "pd2-launcher: the Project Diablo 2 in the game folder has no Game.exe or ProjectDiablo.dll, and the update checks are off: it is not replaced"
            );
            return Err(InstallError::ClientNotKept);
        }
        if report.status == InstallationStatus::NeedsClient
            && has_project_diablo(&paths)
            && let Some(adopted) = self.adopt_client(&paths, control)?
        {
            completed_stages.push(InstallStage::ClientSynchronization);
            return Ok(InstallOutcome {
                final_status: adopted.status,
                completed_stages,
            });
        }
        if report.status == InstallationStatus::NeedsClient {
            self.install_client(&paths, control)?;
            completed_stages.push(InstallStage::ClientBootstrap);
            completed_stages.push(InstallStage::ClientSynchronization);
            report = self.expect_after(
                &paths,
                InstallStage::ClientSynchronization,
                InstallationStatus::Ready,
            )?;
        }
        Ok(InstallOutcome {
            final_status: report.status,
            completed_stages,
        })
    }

    /// Provision the Wine runtime and prefix components the audit rejected,
    /// and prove with a fresh audit that nothing else changed.
    fn ensure_runtime(
        &self,
        paths: &ManagedPaths,
        report: InstallAudit,
        control: &OperationControl,
    ) -> Result<InstallAudit, InstallError> {
        // A runtime or Gecko installed from a custom source while the
        // built-in source is configured may be replaced before any later
        // stage. The existing prefix is never recreated: the
        // driver only reinstalls Gecko in place.
        let status = report.status;
        if !matches!(
            status,
            InstallationStatus::NeedsBaseGame
                | InstallationStatus::NeedsExpansion
                | InstallationStatus::NeedsClient
                | InstallationStatus::NeedsRuntime
        ) {
            return Err(InstallError::RuntimeEvidenceInvalid);
        }
        self.provision_runtime(paths, &report, control)?;
        let report = self.audit(paths)?;
        check_host(report.host)?;
        check_recovery_status(report.status)?;
        // Only a complete game missing runtime components becomes Ready
        // here; every other checkpoint is unchanged by provisioning.
        let expected = if status == InstallationStatus::NeedsRuntime {
            InstallationStatus::Ready
        } else {
            status
        };
        if report.status != expected
            || report.runtime != CheckStatus::Satisfied
            || report.prefix != CheckStatus::Satisfied
        {
            return Err(InstallError::RuntimeEvidenceInvalid);
        }
        clear_stage(paths, InstallStage::ManagedRuntime)?;
        Ok(report)
    }

    fn audit(&self, paths: &ManagedPaths) -> Result<InstallAudit, InstallError> {
        self.auditor
            .audit(paths)
            .map_err(|_| InstallError::AuditUnavailable)
    }

    fn expect_after(
        &self,
        paths: &ManagedPaths,
        stage: InstallStage,
        expected: InstallationStatus,
    ) -> Result<InstallAudit, InstallError> {
        let report = self.audit(paths)?;
        check_host(report.host)?;
        check_recovery_status(report.status)?;
        if report.status != expected {
            return Err(InstallError::EvidenceDidNotAdvance {
                stage,
                expected,
                observed: report.status,
            });
        }
        Ok(report)
    }

    fn provision_runtime(
        &self,
        paths: &ManagedPaths,
        report: &InstallAudit,
        control: &OperationControl,
    ) -> Result<(), InstallError> {
        check_cancelled(control)?;
        control.report(WorkProgress::for_phase(if self.downloading.get() {
            WorkPhase::RuntimeAcquisition
        } else {
            WorkPhase::PrefixInitialization
        }));
        if report.builtin_fex_needed {
            self.install_builtin_fex(paths, control)?;
        }
        let lease = MutationLease::acquire(paths)?;
        // Never download an archive whose component the audit accepted.
        let wine = report
            .needs_runtime_archive()
            .then(|| self.acquire(paths, ArtifactKind::ManagedRuntime, control))
            .transpose()?;
        let needs = report.prefix_needs;
        let prefix_needed = report.prefix != CheckStatus::Satisfied;
        let gecko = (prefix_needed && needs.gecko)
            .then(|| self.acquire(paths, ArtifactKind::Gecko, control))
            .transpose()?;
        let mono = (prefix_needed && needs.mono)
            .then(|| self.acquire(paths, ArtifactKind::WineMono, control))
            .transpose()?;
        let dotnet = (prefix_needed && needs.dotnet)
            .then(|| self.acquire(paths, ArtifactKind::DotnetDesktop, control))
            .transpose()?;
        control.report(WorkProgress::for_phase(WorkPhase::PrefixInitialization));
        // Creating a new prefix cannot be resumed from its own evidence: an
        // interruption must be recognizable as such, never as Indeterminate.
        let new_prefix = prefix_needed
            && matches!(
                paths.directory().open_directory(Path::new("prefix")),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound
            );
        if new_prefix {
            mark_stage(
                paths,
                InterruptedStage::Prefix,
                InstallStage::ManagedRuntime,
            )?;
        }
        let prefix = PrefixArtifacts {
            gecko: gecko.as_ref(),
            mono: mono.as_ref(),
            dotnet: dotnet.as_ref(),
            template: self.prefix_template(),
        };
        self.driver
            .provision_runtime(paths, wine.as_ref(), prefix, &lease.0, control)
            .map_err(|_| stage_failure(control, InstallStage::ManagedRuntime))?;
        check_cancelled(control)
    }

    fn install_vendor(
        &self,
        paths: &ManagedPaths,
        authorization: &InstallationAuthorization,
        product: VendorProduct,
        control: &OperationControl,
    ) -> Result<(), InstallError> {
        let stage = match product {
            VendorProduct::BaseGame => InstallStage::BaseGame,
            VendorProduct::Expansion => InstallStage::Expansion,
        };
        let artifact_kind = match product {
            VendorProduct::BaseGame => ArtifactKind::BaseGameWrapper,
            VendorProduct::Expansion => ArtifactKind::ExpansionWrapper,
        };
        check_cancelled(control)?;
        control.report(WorkProgress::for_phase(match product {
            VendorProduct::BaseGame => WorkPhase::BaseGameInstall,
            VendorProduct::Expansion => WorkPhase::ExpansionInstall,
        }));
        let lease = MutationLease::acquire(paths)?;
        let artifact = self.acquire(paths, artifact_kind, control)?;
        check_cancelled(control)?;
        // A guard: the Control channel and the key page checked the keys
        // already, the same way.
        let product_key = checked_key(product, authorization.key_for(product))
            .map_err(|problem| InstallError::KeyInvalid { stage, problem })?;
        mark_stage(
            paths,
            match product {
                VendorProduct::BaseGame => InterruptedStage::BaseGame,
                VendorProduct::Expansion => InterruptedStage::Expansion,
            },
            stage,
        )?;
        let request = VendorInstallRequest {
            product,
            artifact: &artifact,
            paths,
            product_key: &product_key,
        };
        if self.direct_download {
            self.driver.install_vendor_native(request, control)
        } else {
            self.driver.install_vendor(request, &lease.0, control)
        }
        .map_err(|failure| match (stage_failure(control, stage), failure) {
            (InstallError::StageFailed { stage }, VendorFailure::KeyRejected) => {
                InstallError::KeyRejected { stage }
            }
            (InstallError::StageFailed { stage }, VendorFailure::ErrorDialog) => {
                InstallError::InstallerError { stage }
            }
            (InstallError::StageFailed { stage }, VendorFailure::InstallerMenu) => {
                InstallError::InstallerMenu { stage }
            }
            // Its download, not its installer, failed.
            (InstallError::StageFailed { stage }, failure) => match failure.downloader_failure() {
                Some(failure) => InstallError::DownloaderFailed {
                    stage: product.download_stage(),
                    failure,
                },
                None => InstallError::StageFailed { stage },
            },
            (error, _) => error,
        })?;
        finish_stage(
            paths,
            match product {
                VendorProduct::BaseGame => InterruptedStage::BaseGame,
                VendorProduct::Expansion => InterruptedStage::Expansion,
            },
            stage,
        )?;
        check_cancelled(control)
    }

    fn discard_cached_wrapper(
        &self,
        paths: &ManagedPaths,
        kind: ArtifactKind,
    ) -> Result<(), InstallError> {
        let rejected = |reason| InstallError::ArtifactRejected {
            artifact: kind,
            reason,
        };
        let cache_directory = paths
            .directory()
            .open_directory(Path::new("cache/artifacts"))
            .map_err(|_| rejected(ArtifactRejection::UnsafePath))?;
        adapters::discard_cached_wrapper(self.catalog.spec(kind), &cache_directory)
            .map_err(|_| rejected(ArtifactRejection::Unreadable))
    }

    fn install_client(
        &self,
        paths: &ManagedPaths,
        control: &OperationControl,
    ) -> Result<(), InstallError> {
        check_cancelled(control)?;
        control.report(WorkProgress::for_phase(WorkPhase::ClientManifest));
        let lease = MutationLease::acquire(paths)?;
        let bootstrap = self.acquire(paths, ArtifactKind::ClientBootstrap, control)?;
        self.driver
            .install_client_bootstrap(paths, &bootstrap, &lease.0, control)
            .map_err(|_| stage_failure(control, InstallStage::ClientBootstrap))?;
        self.client
            .synchronize(paths, control)
            .map_err(|_| stage_failure(control, InstallStage::ClientSynchronization))?;
        check_cancelled(control)
    }

    /// Bring a ProjectD2 the game folder already holds up to date without
    /// the PD2 installer: the verified updater checks every file against
    /// the current manifest, fetches only the ones that differ or are
    /// missing, and writes the Launcher's update records, so later updates
    /// work as usual. `None` when that did not reach `Ready`: the caller
    /// then runs the PD2 installer, as for a game without Project Diablo 2.
    fn adopt_client(
        &self,
        paths: &ManagedPaths,
        control: &OperationControl,
    ) -> Result<Option<InstallAudit>, InstallError> {
        check_cancelled(control)?;
        eprintln!(
            "pd2-launcher: Project Diablo 2 found in the game folder: {}, the PD2 installer is not run",
            if self.client_as_installed {
                "the update checks are off, so it is kept as it is"
            } else {
                "updating it in place"
            }
        );
        control.report(WorkProgress::for_phase(WorkPhase::ClientManifest));
        let synchronized = {
            let _lease = MutationLease::acquire(paths)?;
            if self.client_as_installed {
                self.client.accept_installed(paths, control)
            } else {
                self.client.synchronize(paths, control)
            }
        };
        check_cancelled(control)?;
        if synchronized.is_ok() {
            let report = self.audit(paths)?;
            check_host(report.host)?;
            check_recovery_status(report.status)?;
            if report.status == InstallationStatus::Ready {
                return Ok(Some(report));
            }
        }
        // The PD2 installer would replace the version the player keeps: the
        // step fails instead, and the files stay as they are.
        if self.client_as_installed {
            eprintln!(
                "pd2-launcher: the Project Diablo 2 found in the game folder could not be accepted as it is ({}): it is not replaced",
                match synchronized {
                    Ok(()) => "the audit after it was not Ready",
                    Err(_) => {
                        "a file of its record is gone, a file changed or was unreadable, or the manifest could not be read"
                    }
                }
            );
            return Err(InstallError::ClientNotKept);
        }
        eprintln!(
            "pd2-launcher: the Project Diablo 2 found in the game folder could not be updated in place: running the PD2 installer"
        );
        Ok(None)
    }

    fn acquire(
        &self,
        paths: &ManagedPaths,
        kind: ArtifactKind,
        control: &OperationControl,
    ) -> Result<VerifiedArtifact, InstallError> {
        check_cancelled(control)?;
        let downloading = self.downloading.get();
        let (phase, completed_stage) = match kind {
            ArtifactKind::ManagedRuntime
            | ArtifactKind::Gecko
            | ArtifactKind::WineMono
            | ArtifactKind::DotnetDesktop
                if downloading =>
            {
                (WorkPhase::RuntimeAcquisition, 0)
            }
            ArtifactKind::ManagedRuntime
            | ArtifactKind::Gecko
            | ArtifactKind::WineMono
            | ArtifactKind::DotnetDesktop => (WorkPhase::PrefixInitialization, 0),
            ArtifactKind::BaseGameWrapper if downloading => (WorkPhase::BaseGameDownload, 1),
            ArtifactKind::BaseGameWrapper => (WorkPhase::BaseGameInstall, 1),
            ArtifactKind::ExpansionWrapper if downloading => (WorkPhase::ExpansionDownload, 2),
            ArtifactKind::ExpansionWrapper => (WorkPhase::ExpansionInstall, 2),
            ArtifactKind::ClientBootstrap if downloading => (WorkPhase::ClientDownload, 3),
            ArtifactKind::ClientBootstrap => (WorkPhase::ClientManifest, 3),
            ArtifactKind::FexPackage | ArtifactKind::FexRootFs if downloading => {
                (WorkPhase::RuntimeAcquisition, 0)
            }
            ArtifactKind::FexPackage | ArtifactKind::FexRootFs => {
                (WorkPhase::PrefixInitialization, 0)
            }
        };
        control.report(
            WorkProgress::for_phase(phase)
                .with_overall(ProgressUnit::Stages, completed_stage, Some(4))
                .with_verification(0, Some(1)),
        );
        let unusable = |cause| InstallError::DownloadCacheUnusable {
            artifact: kind,
            cause,
        };
        prepare_artifact_cache(paths)
            .map_err(|reason| unusable(CacheProblem::Inventory(reason)))?;
        let spec = self.catalog.spec(kind);
        let cache_directory = paths
            .directory()
            .open_directory(Path::new("cache/artifacts"))
            .map_err(|_| unusable(CacheProblem::Folder))?;
        // An unfinished write of the cache must be cleaned up before a
        // download starts, so a cache that can't be is never reported as a
        // network failure. It removes the temporary files a stopped write
        // left.
        cache_directory
            .recover_atomic()
            .map_err(|_| unusable(CacheProblem::RetainedState))?;
        let acquired = self
            .artifacts
            .acquire_at(spec, &paths.cache, &cache_directory, control)
            .map_err(|_| {
                if control.is_cancelled() {
                    cancellation_error(control)
                } else {
                    InstallError::AcquisitionFailed { artifact: kind }
                }
            })?;
        validate_artifact_cache(paths).map_err(|reason| InstallError::ArtifactRejected {
            artifact: kind,
            reason,
        })?;
        verify_artifact_at(spec, &paths.cache, &cache_directory, acquired).map_err(|reason| {
            InstallError::ArtifactRejected {
                artifact: kind,
                reason,
            }
        })
    }
}

/// Whether the game folder holds a Project Diablo 2 to adopt: a ProjectD2
/// with the client's launch files (`Game.exe`, `ProjectDiablo.dll`) present
/// and not empty. Anything less is installed by the PD2 installer.
/// The game folder has a ProjectD2 with something in it besides the
/// Launcher's own records.
fn has_project_folder(paths: &ManagedPaths) -> bool {
    paths
        .open_game()
        .and_then(|game| game.open_directory(Path::new("ProjectD2")))
        .and_then(|project| project.names())
        .is_ok_and(|names| names.iter().any(|name| name != ".pd2launcher"))
}

/// The mark, in the records' folder (the Managed prefix, or the managed root
/// on the Android host: `game_folder::records_relative`), of the record that
/// an import wrote from the copied files: the SHA-256 of that record. Play's
/// update check then compares PD2's manifest with the files themselves, once
/// ([`is_copied_client_record`]): a client that already is the current
/// release gets PD2's record with no question. A later record (an update's)
/// has another SHA-256, so the mark no longer counts; Start over removes it.
pub(crate) const COPIED_CLIENT_MARK: &str = ".pd2launcher-copied-client";

/// Write [`COPIED_CLIENT_MARK`] for the record in the game folder. The
/// caller holds the installation lease.
fn mark_copied_client(paths: &ManagedPaths) -> std::io::Result<()> {
    let mut record = Vec::new();
    paths
        .open_game()?
        .open_file(Path::new("ProjectD2/.pd2launcher/accepted-manifest.json"))?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut record)?;
    paths.records()?.atomic_write(
        OsStr::new(COPIED_CLIENT_MARK),
        sha256_hex(&record).as_bytes(),
    )
}

/// Whether `accepted`, the client's record, is the one that an import wrote
/// from the copied files (its mark in the records' folder below `root`).
pub(crate) fn is_copied_client_record(root: &Directory, accepted: &[u8]) -> bool {
    is_copied_client_record_on(root, accepted, crate::android_host())
}

fn is_copied_client_record_on(root: &Directory, accepted: &[u8], android: bool) -> bool {
    let mut mark = String::new();
    crate::game_folder::records_directory(root, android)
        .and_then(|records| records.open_file(Path::new(COPIED_CLIENT_MARK)))
        .and_then(|file| file.take(128).read_to_string(&mut mark))
        .is_ok_and(|_| mark.trim() == sha256_hex(accepted))
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn has_project_diablo(paths: &ManagedPaths) -> bool {
    let Ok(game) = paths.open_game() else {
        return false;
    };
    ["ProjectD2/Game.exe", "ProjectD2/ProjectDiablo.dll"]
        .iter()
        .all(|file| {
            game.open_file(Path::new(file))
                .and_then(|file| file.metadata())
                .is_ok_and(|metadata| metadata.len() > 0)
        })
}

/// Record the stage about to change the prefix. The caller holds the lease.
fn mark_stage(
    paths: &ManagedPaths,
    stage: InterruptedStage,
    install_stage: InstallStage,
) -> Result<(), InstallError> {
    crate::stage_marker::write(paths.directory(), stage).map_err(|_| InstallError::StageFailed {
        stage: install_stage,
    })
}

/// Record that the stage's step returned without an error. The caller holds
/// the lease.
fn finish_stage(
    paths: &ManagedPaths,
    stage: InterruptedStage,
    install_stage: InstallStage,
) -> Result<(), InstallError> {
    crate::stage_marker::finish(paths.directory(), stage).map_err(|_| InstallError::StageFailed {
        stage: install_stage,
    })
}

/// `key` as the installers check it, for `product`: its form and its
/// decoded product.
fn checked_key(
    product: VendorProduct,
    key: &ProductKey,
) -> Result<ProductKey, crate::blizzard::KeyProblem> {
    let field = match product {
        VendorProduct::BaseGame => crate::blizzard::KeyField::BaseGame,
        VendorProduct::Expansion => crate::blizzard::KeyField::Expansion,
    };
    crate::blizzard::check_key(field, key.expose())
        .map(|checked| ProductKey::from_checked(&checked))
}

/// Clear the marker once a fresh audit proved the stage's evidence.
fn clear_stage(paths: &ManagedPaths, install_stage: InstallStage) -> Result<(), InstallError> {
    let _lease = MutationLease::acquire(paths)?;
    crate::stage_marker::clear(paths.directory()).map_err(|_| InstallError::StageFailed {
        stage: install_stage,
    })
}

/// Where the Download job keeps the verified Project Diablo 2 client files.
pub(crate) const CLIENT_PREFETCH: &str = "cache/client";

/// The cache folder that holds a product's complete installer.
fn payload_record_path(kind: ArtifactKind) -> String {
    match kind {
        ArtifactKind::ExpansionWrapper => "cache/artifacts/lod-payload".to_owned(),
        _ => "cache/artifacts/d2-payload".to_owned(),
    }
}

/// Whether `artifact` is a product's complete installer in the cache rather
/// than a downloader wrapper.
fn is_complete_payload(paths: &ManagedPaths, artifact: &VerifiedArtifact) -> bool {
    let directory = match artifact.kind() {
        ArtifactKind::BaseGameWrapper => "d2-payload",
        ArtifactKind::ExpansionWrapper => "lod-payload",
        _ => return false,
    };
    artifact.path() == paths.cache.join(directory).join("Installer.exe")
}

fn root_relative(paths: &ManagedPaths, path: &Path) -> Result<String, InstallError> {
    path.strip_prefix(&paths.root)
        .ok()
        .and_then(Path::to_str)
        .map(str::to_owned)
        .ok_or(InstallError::StageFailed {
            stage: InstallStage::Download,
        })
}

fn check_cancelled(control: &OperationControl) -> Result<(), InstallError> {
    if control.is_cancelled() {
        Err(cancellation_error(control))
    } else {
        Ok(())
    }
}

fn cancellation_error(control: &OperationControl) -> InstallError {
    control.mark_cancelled_safe();
    if control.cancellation_safety() == crate::operation::CancellationSafety::ProvenSafe {
        InstallError::Cancelled
    } else {
        InstallError::Indeterminate
    }
}

fn stage_failure(control: &OperationControl, stage: InstallStage) -> InstallError {
    if control.cancellation_safety() == crate::operation::CancellationSafety::ProvenSafe {
        InstallError::Cancelled
    } else if control.is_cancelled() {
        InstallError::Indeterminate
    } else {
        InstallError::StageFailed { stage }
    }
}

/// Start over: remove the Managed prefix and the stage marker
/// so the next install runs from Diablo II again. `runner/` (the Wine
/// runtime) and `cache/` (verified downloads, including unfinished ones)
/// are kept, and Wine Gecko, Mono, and .NET are reinstalled into the new
/// prefix from that cache. `lease` must be this root's installation lock,
/// held exclusively by the caller for the whole reset. On the Android host
/// the game folder, the records of an import and the app's prefix state are
/// outside the prefix, and go too, before it ([`ANDROID_START_OVER`]).
pub(crate) fn reset_managed_prefix(root: &Directory, lease: &File) -> std::io::Result<()> {
    reset_managed_installation_on(root, lease, crate::android_host())
}

/// What Start over removes on the Android host, in this order, each removal
/// on the disk before the next, once a started base-game stage marker is on
/// the disk ([`start_over_android`]; it goes after the last removal).
/// The app's prefix state goes first: the app makes a prefix again only for
/// an installation whose prefix state has `player.reg` (one that was Ready
/// once), and keeps that state only while the installation is Ready. The
/// game folder goes last: the audit reads a started game stage beside any
/// file of a game as an interrupted step that only Start over resolves
/// (`audit::classify_installation`). So a stop at any step before the end
/// shows Repair with Start over, which finishes the removal, never a Ready
/// installation; a stop after the game folder went leaves no part of the
/// installation (the marker beside no game reads as the start of an
/// install).
const ANDROID_START_OVER: [&str; 6] = [
    crate::game_folder::ANDROID_PREFIX_STATE,
    "Diablo II.partial",
    crate::game_folder::IMPORT_RECORD_NAME,
    COPIED_CLIENT_MARK,
    "prefix",
    crate::game_folder::ANDROID_GAME_RELATIVE,
];

/// [`reset_managed_prefix`] on the Android host (`android`) or the desktop.
/// On the Android host, after a started base-game stage marker, it removes
/// the app's prefix state (`game_folder::ANDROID_PREFIX_STATE`), Fast
/// download's partial game folder, the import record, the copied client's
/// mark, the prefix and then the Managed game folder ([`ANDROID_START_OVER`]).
/// No removal follows a link: a link (the game folder's `Save`, which leads
/// to the player's characters, or the prefix's link to the game folder) goes
/// as a link, and what it leads to stays.
fn reset_managed_installation_on(
    root: &Directory,
    lease: &File,
    android: bool,
) -> std::io::Result<()> {
    let lock = root.open_file(Path::new(".pd2launcher.lock"))?;
    if !crate::safe_fs::FileIdentity::of(&lock)?.matches(lease)? {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "reset requires this root's installation lease",
        ));
    }
    if android {
        start_over_android(root, &ANDROID_START_OVER)?;
    } else {
        match root.open_directory(Path::new("prefix")) {
            Ok(_) => root.remove_owned_tree(OsStr::new("prefix"))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    crate::stage_marker::clear(root)?;
    root.sync()
}

/// The Android host's Start over up to the marker's clear: a started
/// base-game stage marker, synced before the first removal (a stop before it
/// has removed nothing), then `names` removed in order
/// ([`remove_entries_in_order`]). Any marker that was there (an interrupted
/// stage that this Start over resolves) gives way to it.
fn start_over_android(root: &Directory, names: &[&str]) -> std::io::Result<()> {
    crate::stage_marker::write(root, crate::InterruptedStage::BaseGame)?;
    remove_entries_in_order(root, names)
}

/// Remove the entries `names` of `root` in their order ([`remove_entry`]),
/// each removal on the disk before the next starts.
fn remove_entries_in_order(root: &Directory, names: &[&str]) -> std::io::Result<()> {
    for name in names {
        remove_entry(root, OsStr::new(name))?;
        root.sync()?;
    }
    Ok(())
}

/// Remove the entry `name` of `root`, whatever it is, without following a
/// link: a folder as an owned tree ([`Directory::remove_owned_tree`]), a
/// file or a link by its name. Nothing there is no error.
fn remove_entry(root: &Directory, name: &OsStr) -> std::io::Result<()> {
    use rustix::fs::{AtFlags, FileType};
    match rustix::fs::statat(root.as_file(), name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) if FileType::from_raw_mode(stat.st_mode) == FileType::Directory => {
            root.remove_owned_tree(name)
        }
        Ok(_) => rustix::fs::unlinkat(root.as_file(), name, AtFlags::empty())
            .map_err(std::io::Error::from),
        Err(rustix::io::Errno::NOENT) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn prepare_artifact_cache(paths: &ManagedPaths) -> Result<(), ArtifactRejection> {
    let cache = paths
        .directory()
        .ensure_directory(Path::new("cache/artifacts"))
        .map_err(|_| ArtifactRejection::UnsafePath)?;
    validate_artifact_inventory(&cache)
}

fn validate_artifact_cache(paths: &ManagedPaths) -> Result<(), ArtifactRejection> {
    let cache = paths
        .directory()
        .open_directory(Path::new("cache"))
        .and_then(|cache| cache.open_directory(Path::new("artifacts")))
        .map_err(|_| ArtifactRejection::UnsafePath)?;
    validate_artifact_inventory(&cache)
}

fn validate_artifact_inventory(cache: &Directory) -> Result<(), ArtifactRejection> {
    const ALLOWED: &[&str] = &[
        crate::runtime::WINE_GE_8_26.cache_name,
        crate::runtime::WINE_WOW64_11_11.cache_name,
        // Downloaded by the x86-64 row's Wine 11.18 in caches that builds
        // before the first release filled: never used, tolerated so that
        // every download and install of those installations still works.
        "wine-11.18-amd64-wow64.verified",
        "wine-gecko-2.47.4-x86.verified",
        "wine-mono-11.3.0-x86.verified",
        "windowsdesktop-runtime-6.0.36-win-x86.verified",
        "d2-wrapper.verified",
        "lod-wrapper.verified",
        "pd2-bootstrap.verified",
        "d2-wrapper.verified.provenance.json",
        "lod-wrapper.verified.provenance.json",
        "custom-wine.download",
        "custom-gecko.download",
        "custom-mono.download",
        "custom-dotnet-desktop.download",
        "custom-d2-installer.download",
        "custom-lod-installer.download",
        "custom-pd2-installer.download",
        "custom-wine.download.provenance.json",
        "custom-gecko.download.provenance.json",
        "custom-mono.download.provenance.json",
        "custom-dotnet-desktop.download.provenance.json",
        "custom-d2-installer.download.provenance.json",
        "custom-lod-installer.download.provenance.json",
        "custom-pd2-installer.download.provenance.json",
        "artifact-00.partial",
        "artifact-01.partial",
        "artifact-02.partial",
        "artifact-03.partial",
        "artifact-04.partial",
        "artifact-05.partial",
        "artifact-06.partial",
        "artifact-07.partial",
        "artifact-08.partial",
        "artifact-09.partial",
        "artifact-00.partial.source",
        "artifact-01.partial.source",
        "artifact-02.partial.source",
        "artifact-03.partial.source",
        "artifact-04.partial.source",
        "artifact-05.partial.source",
        "artifact-06.partial.source",
        "artifact-07.partial.source",
        "artifact-08.partial.source",
        "artifact-09.partial.source",
        // The built-in FEX: each CPU level's package, so a
        // folder moved to another device keeps working.
        "fex-emu-armv8.0_2609.1.verified",
        "fex-emu-armv8.2_2609.1.verified",
        "fex-emu-armv8.4_2609.1.verified",
        crate::builtin_fex::ROOTFS_CACHE_NAME,
        "custom-fex.download",
        "custom-fex-rootfs.download",
        "custom-fex.download.provenance.json",
        "custom-fex-rootfs.download.provenance.json",
        // Left by the retired 1.14d patch stage in caches older releases
        // filled: never used, tolerated so those installations keep
        // working. `artifact-04.partial` above was its download slot.
        "lod-patch-1.14d.verified",
        "custom-patch.download",
        "custom-patch.download.provenance.json",
        "prepatch.log",
        // Which complete installers the direct download made.
        "d2-payload.direct",
        "lod-payload.direct",
    ];
    for name in cache.names().map_err(|_| ArtifactRejection::UnsafePath)? {
        // A product's complete installer, and the one the direct download
        // is still filling (kept between attempts), hold the same files.
        if matches!(
            name.to_str(),
            Some("d2-payload" | "lod-payload" | "d2-payload.partial" | "lod-payload.partial")
        ) {
            let payload = cache
                .open_directory(Path::new(&name))
                .map_err(|_| ArtifactRejection::UnsafePath)?;
            for payload_name in payload.names().map_err(|_| ArtifactRejection::UnsafePath)? {
                if payload_name == OsStr::new("DirectX") {
                    let directx = payload
                        .open_directory(Path::new(&payload_name))
                        .map_err(|_| ArtifactRejection::UnsafePath)?;
                    for directx_name in
                        directx.names().map_err(|_| ArtifactRejection::UnsafePath)?
                    {
                        if !matches!(
                            directx_name.to_str(),
                            Some(
                                "BDA.cab"
                                    | "BDANT.cab"
                                    | "BDAXP.cab"
                                    | "DSETUP.dll"
                                    | "DirectX.cab"
                                    | "ManagedDX.CAB"
                                    | "dsetup32.dll"
                                    | "dxnt.cab"
                                    | "dxsetup.exe"
                            )
                        ) {
                            return Err(ArtifactRejection::UnsafePath);
                        }
                        directx
                            .open_file(Path::new(&directx_name))
                            .map_err(|_| ArtifactRejection::UnsafePath)?;
                    }
                    continue;
                }
                if !matches!(
                    payload_name.to_str(),
                    Some("Installer.exe" | "Installer Tome.mpq")
                ) {
                    return Err(ArtifactRejection::UnsafePath);
                }
                payload
                    .open_file(Path::new(&payload_name))
                    .map_err(|_| ArtifactRejection::UnsafePath)?;
            }
            continue;
        }
        if !ALLOWED.iter().any(|allowed| name == OsStr::new(allowed))
            && !is_test_fixture_artifact(&name)
        {
            return Err(ArtifactRejection::UnsafePath);
        }
        cache
            .open_file(Path::new(&name))
            .map_err(|_| ArtifactRejection::UnsafePath)?;
    }
    Ok(())
}

#[cfg(test)]
fn is_test_fixture_artifact(name: &OsStr) -> bool {
    matches!(
        name.to_str(),
        Some(
            "ManagedRuntime.artifact"
                | "Gecko.artifact"
                | "WineMono.artifact"
                | "DotnetDesktop.artifact"
                | "BaseGameWrapper.artifact"
                | "ExpansionWrapper.artifact"
                | "ClientBootstrap.artifact"
                | "FexPackage.artifact"
                | "FexRootFs.artifact"
        )
    )
}

#[cfg(not(test))]
fn is_test_fixture_artifact(_name: &OsStr) -> bool {
    false
}

fn check_host(host: HostSupport) -> Result<(), InstallError> {
    match host {
        HostSupport::Supported => Ok(()),
        HostSupport::Unsupported => Err(InstallError::UnsupportedHost),
        HostSupport::Indeterminate => Err(InstallError::HostEvidenceUnavailable),
    }
}

fn check_recovery_status(status: InstallationStatus) -> Result<(), InstallError> {
    match status {
        InstallationStatus::RepairRequired => Err(InstallError::RepairRequired),
        InstallationStatus::Indeterminate => Err(InstallError::Indeterminate),
        InstallationStatus::StageInterrupted(_) => Err(InstallError::StageInterrupted),
        _ => Ok(()),
    }
}

struct MutationLease(File);

impl MutationLease {
    fn acquire(paths: &ManagedPaths) -> Result<Self, InstallError> {
        let file = paths
            .directory()
            .open_or_create_file(OsStr::new(".pd2launcher.lock"))
            .map_err(|_| InstallError::InstallationBusy)?;
        if !file.metadata().is_ok_and(|metadata| metadata.is_file()) {
            return Err(InstallError::InstallationBusy);
        }
        File::try_lock(&file).map_err(|_| InstallError::InstallationBusy)?;
        Ok(Self(file))
    }
}

impl Drop for MutationLease {
    fn drop(&mut self) {
        let _ = File::unlock(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, collections::VecDeque, fs, os::unix::fs::PermissionsExt, path::Path};

    use tempfile::TempDir;

    use super::*;

    /// A launcher folder copied from the other host row (an x86-64 PC's onto
    /// an ARM64 device, or back) keeps that row's runtime download: it is
    /// tolerated and never used, so the cache still serves this row. So is
    /// the Wine 11.18 download of builds before the first release.
    #[test]
    fn the_cache_inventory_accepts_either_host_rows_runtime_download() {
        let fixture = tempfile::tempdir().unwrap();
        let cache = fixture.path().join("cache/artifacts");
        std::fs::create_dir_all(&cache).unwrap();
        for name in [
            crate::runtime::WINE_GE_8_26.cache_name,
            crate::runtime::WINE_WOW64_11_11.cache_name,
            "wine-11.18-amd64-wow64.verified",
        ] {
            std::fs::write(cache.join(name), b"runtime").unwrap();
        }
        assert_eq!(
            validate_artifact_inventory(&Directory::open(&cache).unwrap()),
            Ok(())
        );
    }

    /// A direct download kept between attempts leaves the cache usable;
    /// anything else in its folder does not.
    #[test]
    fn the_cache_inventory_accepts_an_unfinished_direct_download() {
        let fixture = tempfile::tempdir().unwrap();
        let cache = fixture.path().join("cache/artifacts");
        for product in ["d2-payload.partial", "lod-payload.partial"] {
            std::fs::create_dir_all(cache.join(product).join("DirectX")).unwrap();
            std::fs::write(cache.join(product).join("Installer Tome.mpq"), b"part").unwrap();
            std::fs::write(cache.join(product).join("DirectX/dxnt.cab"), b"part").unwrap();
        }
        assert_eq!(
            validate_artifact_inventory(&Directory::open(&cache).unwrap()),
            Ok(())
        );
        std::fs::create_dir_all(cache.join("d2-payload.partial/Installer.app")).unwrap();
        assert_eq!(
            validate_artifact_inventory(&Directory::open(&cache).unwrap()),
            Err(ArtifactRejection::UnsafePath)
        );
    }
    use crate::audit::{CheckStatus, HostSupport};

    /// Clearly fake keys: the test builds' `blizzard::TEST_BASE_KEY` and
    /// `TEST_EXPANSION_KEY`, typed with dashes. Tests never hold real keys.
    const FAKE_BASE_KEY: &str = "BCDF-GHJK-MNPR-TVWX-YZ24-6789-22";
    const FAKE_EXPANSION_KEY: &str = "CDFG-HJKM-NPRT-VWXY-Z246-789B-CD";

    const FIXTURE_SHA256: &str = "f16d05ec6b29248d2c61adb1e9263f78e4f7bace1b955014a2d17872cfe4064d";

    #[test]
    fn managed_runtime_root_creation_rejects_a_symlink_ancestor() {
        let fixture = TempDir::new().unwrap();
        let outside = fixture.path().join("outside");
        fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, fixture.path().join("redirect")).unwrap();

        assert!(matches!(
            ManagedPaths::prepare(&fixture.path().join("redirect/managed")),
            Err(InstallError::ManagedRootInvalid)
        ));
        assert!(fs::read_dir(outside).unwrap().next().is_none());
    }

    #[test]
    fn fresh_authorization_reaches_ready_only_through_durable_evidence() {
        let fixture = TempDir::new().expect("fixture");
        let auditor = ScriptedAuditor::new(fresh_journey());
        let source = FixtureArtifactSource::default();
        let driver = RecordingDriver::default();
        let client = RecordingClientSynchronizer::default();
        let authorization = InstallationAuthorization::new(
            FAKE_BASE_KEY.to_owned(),
            FAKE_EXPANSION_KEY.to_owned(),
            true,
        )
        .expect("valid authorization");
        let mut installer =
            ManagedInstaller::new(&auditor, &source, &driver, &client, fixture_catalog());

        let outcome = installer
            .run(fixture.path(), Some(&authorization))
            .expect("evidence-driven installation should finish");

        assert_eq!(outcome.final_status(), InstallationStatus::Ready);
        assert_eq!(
            outcome.completed_stages(),
            [
                InstallStage::BaseGame,
                InstallStage::Expansion,
                InstallStage::ClientBootstrap,
                InstallStage::ClientSynchronization,
            ]
        );
        // Download fetches Blizzard's installers without a key (setting
        // up Wine for their downloader first); the install steps run from
        // the verified downloads.
        assert_eq!(
            driver.operations.borrow().as_slice(),
            [
                RecordedOperation::ManagedRuntime,
                RecordedOperation::BaseGameDownload,
                RecordedOperation::ExpansionDownload,
                RecordedOperation::BaseGame,
                RecordedOperation::Expansion,
                RecordedOperation::ClientBootstrap,
            ]
        );
        assert_eq!(*client.prefetches.borrow(), 1);
        assert_eq!(*client.calls.borrow(), 1);
        assert_eq!(
            source.acquisitions.borrow().as_slice(),
            [
                // Download.
                ArtifactKind::ManagedRuntime,
                ArtifactKind::Gecko,
                ArtifactKind::WineMono,
                ArtifactKind::DotnetDesktop,
                ArtifactKind::BaseGameWrapper,
                ArtifactKind::ManagedRuntime,
                ArtifactKind::Gecko,
                ArtifactKind::WineMono,
                ArtifactKind::DotnetDesktop,
                ArtifactKind::ExpansionWrapper,
                ArtifactKind::ClientBootstrap,
                // Install Diablo II: never a patch.
                ArtifactKind::BaseGameWrapper,
                ArtifactKind::ExpansionWrapper,
                // Install Project Diablo 2.
                ArtifactKind::ClientBootstrap,
            ]
        );
        assert!(driver.received_base_secret.borrow().as_ref().is_some());
        assert!(driver.received_expansion_secret.borrow().as_ref().is_some());
        assert_eq!(auditor.remaining(), 0);
    }

    #[test]
    fn a_key_not_of_the_installers_form_stops_before_the_installer_starts() {
        use crate::stage_marker::{MarkerState, read};
        let fixture = TempDir::new().expect("fixture");
        let auditor = ScriptedAuditor::new([
            audit(InstallationStatus::NeedsExpansion, CheckStatus::Satisfied),
            audit(InstallationStatus::NeedsExpansion, CheckStatus::Satisfied),
        ]);
        let source = FixtureArtifactSource::default();
        let driver = RecordingDriver::default();
        let client = RecordingClientSynchronizer::default();
        // An old 16-character key: the installers take only 26 characters.
        let authorization = InstallationAuthorization::new(
            FAKE_BASE_KEY.to_owned(),
            "BCDF-GHJK-MNPR-TVWX".to_owned(),
            true,
        )
        .unwrap();
        assert_eq!(
            ManagedInstaller::new(&auditor, &source, &driver, &client, fixture_catalog())
                .run(fixture.path(), Some(&authorization)),
            Err(InstallError::KeyInvalid {
                stage: InstallStage::Expansion,
                problem: crate::blizzard::KeyProblem::OldKeyLength,
            })
        );
        assert!(driver.received_expansion_secret.borrow().is_none());
        let root = Directory::open(fixture.path()).unwrap();
        assert_eq!(read(&root), MarkerState::Absent);

        // Of the right form, but not a key of the product.
        let auditor = ScriptedAuditor::new([
            audit(InstallationStatus::NeedsExpansion, CheckStatus::Satisfied),
            audit(InstallationStatus::NeedsExpansion, CheckStatus::Satisfied),
        ]);
        let authorization = InstallationAuthorization::new(
            FAKE_BASE_KEY.to_owned(),
            "ZZZZ-ZZZZ-ZZZZ-ZZZZ-ZZZZ-ZZZZ-ZZ".to_owned(),
            true,
        )
        .unwrap();
        assert_eq!(
            ManagedInstaller::new(&auditor, &source, &driver, &client, fixture_catalog())
                .run(fixture.path(), Some(&authorization)),
            Err(InstallError::KeyInvalid {
                stage: InstallStage::Expansion,
                problem: crate::blizzard::KeyProblem::WrongProduct,
            })
        );
        assert!(driver.received_expansion_secret.borrow().is_none());
        assert_eq!(read(&root), MarkerState::Absent);
    }

    #[test]
    fn prefix_and_vendor_stages_are_marked_until_their_evidence_is_proven() {
        use crate::stage_marker::{MarkerState, read};
        let fixture = TempDir::new().expect("fixture");
        let auditor = ScriptedAuditor::new(fresh_journey());
        let source = FixtureArtifactSource::default();
        let driver = RecordingDriver::default();
        let client = RecordingClientSynchronizer::default();
        let authorization = InstallationAuthorization::new(
            FAKE_BASE_KEY.to_owned(),
            FAKE_EXPANSION_KEY.to_owned(),
            true,
        )
        .unwrap();
        ManagedInstaller::new(&auditor, &source, &driver, &client, fixture_catalog())
            .run(fixture.path(), Some(&authorization))
            .expect("installation");
        // A downloader run changes no game file and needs no marker.
        assert_eq!(
            *driver.markers.borrow(),
            [
                MarkerState::Present(InterruptedStage::Prefix),
                MarkerState::Absent,
                MarkerState::Absent,
                MarkerState::Present(InterruptedStage::BaseGame),
                MarkerState::Present(InterruptedStage::Expansion),
            ]
        );
        let root = Directory::open(fixture.path()).unwrap();
        assert_eq!(read(&root), MarkerState::Absent);

        // A failed Vendor installer leaves its marker for the next audit.
        let auditor = ScriptedAuditor::new([
            audit(InstallationStatus::NeedsExpansion, CheckStatus::Satisfied),
            audit(InstallationStatus::NeedsExpansion, CheckStatus::Satisfied),
        ]);
        let driver = RecordingDriver {
            fail_expansion: true,
            ..RecordingDriver::default()
        };
        assert_eq!(
            ManagedInstaller::new(&auditor, &source, &driver, &client, fixture_catalog())
                .run(fixture.path(), Some(&authorization)),
            Err(InstallError::StageFailed {
                stage: InstallStage::Expansion
            })
        );
        assert_eq!(
            read(&root),
            MarkerState::Present(InterruptedStage::Expansion)
        );
        // An installer that returned marks its stage finished before the
        // audit that proves it: when the files do not show it, the
        // finished marker stays.
        let auditor = ScriptedAuditor::new([
            audit(InstallationStatus::NeedsExpansion, CheckStatus::Satisfied),
            audit(InstallationStatus::NeedsExpansion, CheckStatus::Satisfied),
            audit(InstallationStatus::NeedsExpansion, CheckStatus::Satisfied),
        ]);
        let result = ManagedInstaller::new(
            &auditor,
            &source,
            &RecordingDriver::default(),
            &client,
            fixture_catalog(),
        )
        .run(fixture.path(), Some(&authorization));
        assert!(
            matches!(result, Err(InstallError::EvidenceDidNotAdvance { .. })),
            "{result:?}"
        );
        assert_eq!(
            read(&root),
            MarkerState::Finished(InterruptedStage::Expansion)
        );

        // And an interrupted stage never runs another stage.
        let auditor = ScriptedAuditor::new([audit(
            InstallationStatus::StageInterrupted(InterruptedStage::Expansion),
            CheckStatus::Satisfied,
        )]);
        let driver = RecordingDriver::default();
        assert_eq!(
            ManagedInstaller::new(&auditor, &source, &driver, &client, fixture_catalog())
                .run(fixture.path(), Some(&authorization)),
            Err(InstallError::StageInterrupted)
        );
        assert!(driver.operations.borrow().is_empty());
    }

    #[test]
    fn a_vendor_error_dialog_is_reported_as_its_own_stage_failure() {
        let authorization = InstallationAuthorization::new(
            FAKE_BASE_KEY.to_owned(),
            FAKE_EXPANSION_KEY.to_owned(),
            true,
        )
        .unwrap();
        let client = RecordingClientSynchronizer::default();
        for (failure, expected) in [
            (
                VendorFailure::KeyRejected,
                InstallError::KeyRejected {
                    stage: InstallStage::Expansion,
                },
            ),
            (
                VendorFailure::ErrorDialog,
                InstallError::InstallerError {
                    stage: InstallStage::Expansion,
                },
            ),
            (
                VendorFailure::InstallerMenu,
                InstallError::InstallerMenu {
                    stage: InstallStage::Expansion,
                },
            ),
            // Its download, not its installer, failed.
            (
                VendorFailure::DownloadStalled,
                InstallError::DownloaderFailed {
                    stage: InstallStage::ExpansionDownload,
                    failure: DownloaderFailure::Stalled,
                },
            ),
        ] {
            let fixture = TempDir::new().unwrap();
            let auditor = ScriptedAuditor::new([
                audit(InstallationStatus::NeedsExpansion, CheckStatus::Satisfied),
                audit(InstallationStatus::NeedsExpansion, CheckStatus::Satisfied),
            ]);
            let source = FixtureArtifactSource::default();
            let driver = RecordingDriver {
                fail_expansion: true,
                expansion_failure: Some(failure),
                ..RecordingDriver::default()
            };
            assert_eq!(
                ManagedInstaller::new(&auditor, &source, &driver, &client, fixture_catalog())
                    .run(fixture.path(), Some(&authorization)),
                Err(expected)
            );
            // The installed Diablo II is untouched: the next audit decides
            // whether Lord of Destruction can simply run again.
            assert!(
                !driver
                    .operations
                    .borrow()
                    .contains(&RecordedOperation::BaseGame)
            );
        }
    }

    #[test]
    fn a_failed_wrapper_download_refetches_the_blizzard_wrapper_once() {
        let client = RecordingClientSynchronizer::default();
        let base_acquisitions = |source: &FixtureArtifactSource| {
            source
                .acquisitions
                .borrow()
                .iter()
                .filter(|kind| **kind == ArtifactKind::BaseGameWrapper)
                .count()
        };
        let download = |auditor: &ScriptedAuditor,
                        source: &FixtureArtifactSource,
                        driver: &RecordingDriver,
                        catalog: ArtifactCatalog| {
            let fixture = TempDir::new().unwrap();
            ManagedInstaller::new(auditor, source, driver, &client, catalog).download_all(
                fixture.path(),
                DownloadPlan::Installers,
                &OperationControl::default(),
            )
        };

        // An expired wrapper: the download runs again with a fresh one.
        let auditor = ScriptedAuditor::new([audit(
            InstallationStatus::NeedsBaseGame,
            CheckStatus::Satisfied,
        )]);
        let source = FixtureArtifactSource::default();
        let driver = RecordingDriver {
            wrapper_download_failures: 1.into(),
            ..RecordingDriver::default()
        };
        assert!(download(&auditor, &source, &driver, fixture_catalog()).is_ok());
        assert_eq!(base_acquisitions(&source), 2);
        assert_eq!(
            driver.operations.borrow()[0],
            RecordedOperation::BaseGameDownload
        );

        // A stalled download lost its connection: the wrapper is kept and
        // the stage fails at once.
        let auditor = ScriptedAuditor::new([audit(
            InstallationStatus::NeedsBaseGame,
            CheckStatus::Satisfied,
        )]);
        let source = FixtureArtifactSource::default();
        let driver = RecordingDriver {
            wrapper_download_stalls: 1.into(),
            ..RecordingDriver::default()
        };
        assert_eq!(
            download(&auditor, &source, &driver, fixture_catalog()),
            Err(InstallError::DownloaderFailed {
                stage: InstallStage::BaseGameDownload,
                failure: DownloaderFailure::Stalled,
            })
        );
        assert_eq!(base_acquisitions(&source), 1);

        // Only once: a second failure is reported.
        let auditor = ScriptedAuditor::new([audit(
            InstallationStatus::NeedsBaseGame,
            CheckStatus::Satisfied,
        )]);
        let source = FixtureArtifactSource::default();
        let driver = RecordingDriver {
            wrapper_download_failures: 2.into(),
            ..RecordingDriver::default()
        };
        assert_eq!(
            download(&auditor, &source, &driver, fixture_catalog()),
            Err(InstallError::DownloaderFailed {
                stage: InstallStage::BaseGameDownload,
                failure: DownloaderFailure::Ended,
            })
        );
        assert_eq!(base_acquisitions(&source), 2);

        // An error box, an unfinished download and other files are retried
        // with a fresh wrapper too, then reported as what they were.
        for (failure, expected) in [
            (
                VendorFailure::DownloaderErrorDialog,
                DownloaderFailure::ErrorDialog,
            ),
            (
                VendorFailure::PayloadIncomplete,
                DownloaderFailure::Incomplete,
            ),
            (VendorFailure::PayloadRejected, DownloaderFailure::Rejected),
        ] {
            let auditor = ScriptedAuditor::new([audit(
                InstallationStatus::NeedsBaseGame,
                CheckStatus::Satisfied,
            )]);
            let source = FixtureArtifactSource::default();
            let driver = RecordingDriver {
                wrapper_download_failures: 2.into(),
                wrapper_download_failure: Some(failure).into(),
                ..RecordingDriver::default()
            };
            assert_eq!(
                download(&auditor, &source, &driver, fixture_catalog()),
                Err(InstallError::DownloaderFailed {
                    stage: InstallStage::BaseGameDownload,
                    failure: expected,
                })
            );
            assert_eq!(base_acquisitions(&source), 2);
        }

        // A custom source is never replaced: its failure is reported.
        let auditor = ScriptedAuditor::new([audit(
            InstallationStatus::NeedsBaseGame,
            CheckStatus::Satisfied,
        )]);
        let source = FixtureArtifactSource::default();
        let driver = RecordingDriver {
            wrapper_download_failures: 1.into(),
            ..RecordingDriver::default()
        };
        let catalog = ArtifactCatalog::new([
            fixed_spec(ArtifactKind::ManagedRuntime),
            fixed_spec(ArtifactKind::Gecko),
            fixed_spec(ArtifactKind::WineMono),
            fixed_spec(ArtifactKind::DotnetDesktop),
            ArtifactSpec {
                request_url: Cow::Borrowed("https://mirror.invalid/d2.exe"),
                redirects: RedirectPolicy::AnyHttp,
                size: SizePolicy::Bounded { min: 1, max: 256 },
                integrity: IntegrityPolicy::Unverified,
                expected_filename: None,
                ..wrapper_spec(ArtifactKind::BaseGameWrapper)
            },
            wrapper_spec(ArtifactKind::ExpansionWrapper),
            fixed_spec(ArtifactKind::ClientBootstrap),
            fixed_spec(ArtifactKind::FexPackage),
            fixed_spec(ArtifactKind::FexRootFs),
        ]);
        assert_eq!(
            download(&auditor, &source, &driver, catalog),
            Err(InstallError::DownloaderFailed {
                stage: InstallStage::BaseGameDownload,
                failure: DownloaderFailure::Ended,
            })
        );
        assert_eq!(base_acquisitions(&source), 1);
    }

    #[test]
    fn start_over_removes_only_the_prefix_and_the_stage_marker_under_the_lease() {
        let fixture = TempDir::new().unwrap();
        let root_path = fixture.path();
        let outside = TempDir::new().unwrap();
        fs::write(outside.path().join("keep"), b"outside").unwrap();
        let current = format!("runner/{}/bin", crate::runtime::current().identity);
        for directory in [
            current.as_str(),
            "cache/artifacts",
            "cache/xdg",
            "prefix/drive_c/Program Files (x86)/Diablo II/Save",
            "prefix/dosdevices",
        ] {
            fs::create_dir_all(root_path.join(directory)).unwrap();
        }
        fs::write(root_path.join(&current).join("wine"), b"wine").unwrap();
        fs::write(
            root_path.join("cache/artifacts/artifact-01.partial"),
            b"half",
        )
        .unwrap();
        fs::write(
            root_path.join("prefix/drive_c/Program Files (x86)/Diablo II/d2data.mpq"),
            b"partial",
        )
        .unwrap();
        std::os::unix::fs::symlink(outside.path(), root_path.join("prefix/dosdevices/z:")).unwrap();
        // A directory Wine marked read-only.
        let read_only = root_path.join("prefix/drive_c/Program Files (x86)/Diablo II/Save");
        fs::write(read_only.join("hero.d2s"), b"save").unwrap();
        fs::set_permissions(&read_only, fs::Permissions::from_mode(0o500)).unwrap();
        let root = Directory::open(root_path).unwrap();
        crate::stage_marker::write(&root, InterruptedStage::BaseGame).unwrap();

        let paths = ManagedPaths::prepare(root_path).unwrap();
        // Another holder of the lease blocks the reset entirely.
        let other = MutationLease::acquire(&paths).unwrap();
        assert_eq!(
            MutationLease::acquire(&paths).err(),
            Some(InstallError::InstallationBusy)
        );
        // A file that is not this root's lock is refused.
        let stranger = File::open(outside.path().join("keep")).unwrap();
        assert!(reset_managed_prefix(&root, &stranger).is_err());
        assert!(root_path.join("prefix").is_dir());

        reset_managed_prefix(&root, &other.0).expect("reset under the lease");
        drop(other);
        assert!(!root_path.join("prefix").exists());
        assert_eq!(
            crate::stage_marker::read(&root),
            crate::stage_marker::MarkerState::Absent
        );
        assert_eq!(
            fs::read(root_path.join(&current).join("wine")).unwrap(),
            b"wine"
        );
        // Downloads are kept.
        assert_eq!(
            fs::read(root_path.join("cache/artifacts/artifact-01.partial")).unwrap(),
            b"half"
        );
        assert!(root_path.join("cache/xdg").is_dir());
        assert_eq!(fs::read(outside.path().join("keep")).unwrap(), b"outside");
        // A second Start over has nothing left to remove.
        let lease = MutationLease::acquire(&paths).unwrap();
        reset_managed_prefix(&root, &lease.0).unwrap();
    }

    /// A source that stops the job when it is asked for `stop_at`.
    struct StoppingSource<'a> {
        inner: &'a FixtureArtifactSource,
        stop_at: ArtifactKind,
    }

    impl ArtifactSource for StoppingSource<'_> {
        fn acquire_at(
            &self,
            spec: &ArtifactSpec,
            cache: &Path,
            cache_directory: &Directory,
            control: &OperationControl,
        ) -> Result<AcquiredArtifact, AdapterFailure> {
            if spec.kind == self.stop_at {
                control.request_cancel();
                return Err(AdapterFailure::here());
            }
            self.inner.acquire_at(spec, cache, cache_directory, control)
        }
    }

    #[test]
    fn a_chosen_built_in_fex_is_downloaded_and_installed_before_anything_else() {
        let fixture = TempDir::new().unwrap();
        let source = FixtureArtifactSource::default();
        let driver = RecordingDriver::default();
        let client = RecordingClientSynchronizer::default();
        let auditor =
            ScriptedAuditor::new([
                audit(InstallationStatus::NeedsBaseGame, CheckStatus::Missing)
                    .with_builtin_fex_needed(true),
            ]);
        // The fixture bytes are no `.deb`, so installing them stops.
        assert_eq!(
            ManagedInstaller::new(&auditor, &source, &driver, &client, fixture_catalog())
                .download_all(
                    fixture.path(),
                    DownloadPlan::Installers,
                    &OperationControl::default()
                ),
            Err(InstallError::StageFailed {
                stage: InstallStage::BuiltinFex
            })
        );
        assert_eq!(
            *source.acquisitions.borrow(),
            [ArtifactKind::FexPackage, ArtifactKind::FexRootFs],
            "nothing else, Blizzard's downloader included, runs before it"
        );
        assert!(!crate::builtin_fex::installed(fixture.path()));
    }

    /// The Android host's audit, as `ProductionAuditor` reports it there:
    /// the app provides the prefix's .NET Desktop Runtime (for the official
    /// PD2 launcher), and the prefix never gets Wine Gecko or Wine Mono,
    /// whatever the audit of the prefix says.
    fn android_audit(status: InstallationStatus, runtime: CheckStatus) -> InstallAudit {
        audit(status, runtime)
            .with_prefix_needs(PrefixNeeds {
                gecko: true,
                mono: true,
                dotnet: false,
            })
            .with_android_host()
    }

    #[test]
    fn on_android_blizzards_downloaders_run_in_install_not_in_download() {
        use crate::downloads::{DownloadReadiness, readiness};
        let fixture = TempDir::new().unwrap();
        let source = FixtureArtifactSource::default();
        let driver = RecordingDriver::default();
        let client = RecordingClientSynchronizer::default();
        let auditor = ScriptedAuditor::new([
            // Download.
            android_audit(InstallationStatus::NeedsBaseGame, CheckStatus::Missing),
            // Install Diablo II, after the runtime, after each installer.
            android_audit(InstallationStatus::NeedsBaseGame, CheckStatus::Missing),
            android_audit(InstallationStatus::NeedsBaseGame, CheckStatus::Satisfied),
            android_audit(InstallationStatus::NeedsExpansion, CheckStatus::Satisfied),
            android_audit(InstallationStatus::NeedsClient, CheckStatus::Satisfied),
            // Install Project Diablo 2.
            android_audit(InstallationStatus::NeedsClient, CheckStatus::Satisfied),
            android_audit(InstallationStatus::Ready, CheckStatus::Satisfied),
        ]);
        let mut installer =
            ManagedInstaller::new(&auditor, &source, &driver, &client, fixture_catalog());
        installer
            .download_all(
                fixture.path(),
                DownloadPlan::Installers,
                &OperationControl::default(),
            )
            .expect("downloads verified");
        // Download keeps Blizzard's wrappers, verified, and runs nothing: no
        // Wine (the app provides it, its prefix and the prefix's .NET), no
        // downloader, and no Wine Gecko or Wine Mono. The readiness check
        // counts no record of what the app provides there.
        assert_eq!(
            *source.acquisitions.borrow(),
            [
                ArtifactKind::BaseGameWrapper,
                ArtifactKind::ExpansionWrapper,
                ArtifactKind::ClientBootstrap,
            ]
        );
        assert!(
            source
                .acquisitions
                .borrow()
                .iter()
                .all(|kind| !crate::downloads::PROVIDED_ON_ANDROID.contains(kind))
        );
        assert!(driver.operations.borrow().is_empty());
        assert!(matches!(
            readiness(&Directory::open(fixture.path()).unwrap()),
            DownloadReadiness::Complete {
                plan: DownloadPlan::Installers,
                ..
            }
        ));
        let receipt = fs::read_to_string(fixture.path().join("cache/downloads.json")).unwrap();
        for wrapper in ["BaseGameWrapper", "ExpansionWrapper"] {
            assert!(receipt.contains(&format!("cache/artifacts/{wrapper}.artifact")));
        }
        assert!(!receipt.contains("payload"));

        // Install Diablo II: Wine, then each of Blizzard's downloaders right
        // before its installer.
        let authorization = InstallationAuthorization::new(
            FAKE_BASE_KEY.to_owned(),
            FAKE_EXPANSION_KEY.to_owned(),
            true,
        )
        .unwrap();
        let outcome = installer
            .install_base_game(
                fixture.path(),
                Some(&authorization),
                &OperationControl::default(),
            )
            .unwrap();
        assert_eq!(outcome.final_status(), InstallationStatus::NeedsClient);
        assert_eq!(
            driver.operations.borrow().as_slice(),
            [
                RecordedOperation::ManagedRuntime,
                RecordedOperation::BaseGameDownload,
                RecordedOperation::BaseGame,
                RecordedOperation::ExpansionDownload,
                RecordedOperation::Expansion,
            ]
        );
        let ready = installer
            .install_project_diablo(fixture.path(), &OperationControl::default())
            .unwrap();
        assert_eq!(ready.final_status(), InstallationStatus::Ready);
        assert_eq!(auditor.remaining(), 0);
    }

    /// The fixture source, but a product's complete installer already in
    /// the cache is acquired as it is, as the production source does.
    struct PayloadAwareSource<'a> {
        inner: &'a FixtureArtifactSource,
    }

    impl ArtifactSource for PayloadAwareSource<'_> {
        fn acquire_at(
            &self,
            spec: &ArtifactSpec,
            cache: &Path,
            cache_directory: &Directory,
            control: &OperationControl,
        ) -> Result<AcquiredArtifact, AdapterFailure> {
            let payload = match spec.kind {
                ArtifactKind::BaseGameWrapper => "d2-payload",
                ArtifactKind::ExpansionWrapper => "lod-payload",
                _ => return self.inner.acquire_at(spec, cache, cache_directory, control),
            };
            let installer = cache.join(payload).join("Installer.exe");
            if !installer.is_file() {
                return self.inner.acquire_at(spec, cache, cache_directory, control);
            }
            self.inner.acquisitions.borrow_mut().push(spec.kind);
            Ok(
                AcquiredArtifact::user_selected(installer, spec.expected_filename.unwrap())
                    .with_retained_receipt_sha256(hex(&Sha256::digest(minimal_pe()))),
            )
        }
    }

    #[test]
    fn the_direct_download_fetches_both_installers_at_once_without_wine() {
        let fixture = TempDir::new().unwrap();
        let source = FixtureArtifactSource::default();
        let driver = RecordingDriver::default();
        let client = RecordingClientSynchronizer::default();
        // One audit: no runtime is set up for it, though none is there.
        let auditor = ScriptedAuditor::new([audit(
            InstallationStatus::NeedsBaseGame,
            CheckStatus::Missing,
        )]);
        ManagedInstaller::new(&auditor, &source, &driver, &client, fixture_catalog())
            .with_direct_download(true)
            .download_all(
                fixture.path(),
                DownloadPlan::Installers,
                &OperationControl::default(),
            )
            .unwrap();
        assert_eq!(
            *driver.operations.borrow(),
            [RecordedOperation::DirectDownload {
                base_game: true,
                expansion: true,
            }]
        );
        assert_eq!(auditor.remaining(), 0);
        let receipt = fs::read_to_string(fixture.path().join("cache/downloads.json")).unwrap();
        assert!(receipt.contains("cache/artifacts/d2-payload"));
        assert!(receipt.contains("cache/artifacts/lod-payload"));
    }

    #[test]
    fn a_refused_direct_download_gets_one_fresh_wrapper_per_product() {
        let client = RecordingClientSynchronizer::default();
        let acquisitions = |source: &FixtureArtifactSource, kind| {
            source
                .acquisitions
                .borrow()
                .iter()
                .filter(|acquired| **acquired == kind)
                .count()
        };
        let download = |source: &FixtureArtifactSource, driver: &RecordingDriver| {
            let fixture = TempDir::new().unwrap();
            let auditor = ScriptedAuditor::new([audit(
                InstallationStatus::NeedsBaseGame,
                CheckStatus::Satisfied,
            )]);
            ManagedInstaller::new(&auditor, source, driver, &client, fixture_catalog())
                .with_direct_download(true)
                .download_all(
                    fixture.path(),
                    DownloadPlan::Installers,
                    &OperationControl::default(),
                )
        };
        let refused = VendorFailure::DirectDownload(DirectFailure::Refused);

        // Diablo II's cookie refused: only its wrapper is fetched again, and
        // only it is downloaded again.
        let source = FixtureArtifactSource::default();
        let driver = RecordingDriver {
            direct_failures: RefCell::new([vec![(ArtifactKind::BaseGameWrapper, refused)]].into()),
            ..RecordingDriver::default()
        };
        assert!(download(&source, &driver).is_ok());
        assert_eq!(acquisitions(&source, ArtifactKind::BaseGameWrapper), 2);
        assert_eq!(acquisitions(&source, ArtifactKind::ExpansionWrapper), 1);
        assert_eq!(
            *driver.operations.borrow(),
            [
                RecordedOperation::DirectDownload {
                    base_game: true,
                    expansion: true,
                },
                RecordedOperation::DirectDownload {
                    base_game: true,
                    expansion: false,
                },
            ]
        );

        // Refused again with the fresh wrapper: reported.
        let source = FixtureArtifactSource::default();
        let driver = RecordingDriver {
            direct_failures: RefCell::new(
                [
                    vec![(ArtifactKind::ExpansionWrapper, refused)],
                    vec![(ArtifactKind::ExpansionWrapper, refused)],
                ]
                .into(),
            ),
            ..RecordingDriver::default()
        };
        assert_eq!(
            download(&source, &driver),
            Err(InstallError::DirectDownloadFailed {
                stage: InstallStage::ExpansionDownload,
                failure: DirectFailure::Refused,
            })
        );
        assert_eq!(acquisitions(&source, ArtifactKind::ExpansionWrapper), 2);

        // Missing pieces or a pin mismatch: no fresh wrapper helps.
        for failure in [DirectFailure::Incomplete, DirectFailure::Mismatch] {
            let source = FixtureArtifactSource::default();
            let driver = RecordingDriver {
                direct_failures: RefCell::new(
                    [vec![(
                        ArtifactKind::BaseGameWrapper,
                        VendorFailure::DirectDownload(failure),
                    )]]
                    .into(),
                ),
                ..RecordingDriver::default()
            };
            assert_eq!(
                download(&source, &driver),
                Err(InstallError::DirectDownloadFailed {
                    stage: InstallStage::BaseGameDownload,
                    failure,
                })
            );
            assert_eq!(acquisitions(&source, ArtifactKind::BaseGameWrapper), 1);
        }
    }

    #[test]
    fn on_android_the_direct_download_fetches_both_before_diablo_ii() {
        let fixture = TempDir::new().unwrap();
        let fixtures = FixtureArtifactSource::default();
        let source = PayloadAwareSource { inner: &fixtures };
        let driver = RecordingDriver::default();
        let client = RecordingClientSynchronizer::default();
        let auditor = ScriptedAuditor::new([
            android_audit(InstallationStatus::NeedsBaseGame, CheckStatus::Missing),
            android_audit(InstallationStatus::NeedsBaseGame, CheckStatus::Satisfied),
            android_audit(InstallationStatus::NeedsExpansion, CheckStatus::Satisfied),
            android_audit(InstallationStatus::NeedsClient, CheckStatus::Satisfied),
        ]);
        let authorization = InstallationAuthorization::new(
            FAKE_BASE_KEY.to_owned(),
            FAKE_EXPANSION_KEY.to_owned(),
            true,
        )
        .unwrap();
        let outcome = ManagedInstaller::new(&auditor, &source, &driver, &client, fixture_catalog())
            .with_direct_download(true)
            .install_base_game(
                fixture.path(),
                Some(&authorization),
                &OperationControl::default(),
            )
            .unwrap();
        assert_eq!(outcome.final_status(), InstallationStatus::NeedsClient);
        // Lord of Destruction's installer was kept from the first download.
        assert_eq!(
            driver.operations.borrow().as_slice(),
            [
                RecordedOperation::ManagedRuntime,
                RecordedOperation::DirectDownload {
                    base_game: true,
                    expansion: true,
                },
                RecordedOperation::BaseGame,
                RecordedOperation::Expansion,
            ]
        );
        assert_eq!(auditor.remaining(), 0);
    }

    #[test]
    fn download_all_records_its_result_only_when_everything_verified() {
        use crate::downloads::{DownloadReadiness, readiness};
        let fixture = TempDir::new().unwrap();
        let root = || Directory::open(fixture.path()).unwrap();
        let source = FixtureArtifactSource::default();
        let driver = RecordingDriver::default();
        let client = RecordingClientSynchronizer::default();

        // A stop part way reports a safe cancellation and records nothing.
        let auditor = ScriptedAuditor::new([
            audit(InstallationStatus::NeedsBaseGame, CheckStatus::Missing),
            audit(InstallationStatus::NeedsBaseGame, CheckStatus::Satisfied),
        ]);
        let stopping = StoppingSource {
            inner: &source,
            stop_at: ArtifactKind::ClientBootstrap,
        };
        assert_eq!(
            ManagedInstaller::new(&auditor, &stopping, &driver, &client, fixture_catalog())
                .download_all(
                    fixture.path(),
                    DownloadPlan::Installers,
                    &OperationControl::default()
                ),
            Err(InstallError::Cancelled)
        );
        assert_eq!(readiness(&root()), DownloadReadiness::Incomplete);
        assert_eq!(*client.prefetches.borrow(), 0);

        // Downloading again finishes what's missing and records the result.
        let auditor = ScriptedAuditor::new([audit(
            InstallationStatus::NeedsBaseGame,
            CheckStatus::Satisfied,
        )]);
        let outcome = ManagedInstaller::new(&auditor, &source, &driver, &client, fixture_catalog())
            .download_all(
                fixture.path(),
                DownloadPlan::Installers,
                &OperationControl::default(),
            )
            .expect("downloads verified");
        assert!(outcome.total_bytes > 0);
        let complete = readiness(&root());
        assert_eq!(
            complete,
            DownloadReadiness::Complete {
                plan: DownloadPlan::Installers,
                total_bytes: outcome.total_bytes,
                reach: crate::downloads::DownloadReach::BaseGame,
                prefix: false,
            }
        );
        assert_eq!(*client.prefetches.borrow(), 1);
        // Nothing was installed: no key was needed, no game stage ran.
        assert!(driver.received_base_secret.borrow().is_none());
        assert!(!driver.operations.borrow().iter().any(|operation| matches!(
            operation,
            RecordedOperation::BaseGame | RecordedOperation::Expansion
        )));

        // For an existing installation, Blizzard's installers are not
        // downloaded, and no Wine prefix is set up yet.
        let fixture = TempDir::new().unwrap();
        let source = FixtureArtifactSource::default();
        let driver = RecordingDriver::default();
        let auditor = ScriptedAuditor::new([audit(
            InstallationStatus::NeedsBaseGame,
            CheckStatus::Missing,
        )]);
        ManagedInstaller::new(&auditor, &source, &driver, &client, fixture_catalog())
            .download_all(
                fixture.path(),
                DownloadPlan::ExistingInstall,
                &OperationControl::default(),
            )
            .expect("downloads verified");
        assert_eq!(
            *source.acquisitions.borrow(),
            [
                ArtifactKind::ManagedRuntime,
                ArtifactKind::Gecko,
                ArtifactKind::WineMono,
                ArtifactKind::DotnetDesktop,
                ArtifactKind::ClientBootstrap,
            ]
        );
        assert!(driver.operations.borrow().is_empty());
        let existing = readiness(&Directory::open(fixture.path()).unwrap());
        let fresh =
            crate::downloads::DownloadNeed::for_status(InstallationStatus::NeedsBaseGame, true);
        assert!(existing.serves(DownloadPlan::ExistingInstall, fresh));
        assert!(!existing.serves(DownloadPlan::Installers, fresh));
    }

    #[test]
    fn the_download_job_says_it_downloads_and_the_install_jobs_that_they_install() {
        use std::sync::{Arc, Mutex};
        let phases = Arc::new(Mutex::new(Vec::new()));
        let control = OperationControl::default();
        let seen = Arc::clone(&phases);
        control.observe(Arc::new(move |progress: WorkProgress| {
            seen.lock().unwrap().push(progress.phase());
        }));
        let fixture = TempDir::new().unwrap();
        let source = FixtureArtifactSource::default();
        let driver = RecordingDriver::default();
        let client = RecordingClientSynchronizer::default();
        let auditor = ScriptedAuditor::new(fresh_journey());
        let authorization = InstallationAuthorization::new(
            FAKE_BASE_KEY.to_owned(),
            FAKE_EXPANSION_KEY.to_owned(),
            true,
        )
        .unwrap();
        let mut installer =
            ManagedInstaller::new(&auditor, &source, &driver, &client, fixture_catalog());
        installer
            .download_all(fixture.path(), DownloadPlan::Installers, &control)
            .unwrap();
        let downloading = std::mem::take(&mut *phases.lock().unwrap());
        for phase in [
            WorkPhase::RuntimeAcquisition,
            WorkPhase::BaseGameDownload,
            WorkPhase::ExpansionDownload,
            WorkPhase::ClientDownload,
        ] {
            assert!(downloading.contains(&phase), "{phase:?}");
        }
        for phase in [WorkPhase::BaseGameInstall, WorkPhase::ExpansionInstall] {
            assert!(!downloading.contains(&phase), "{phase:?} while downloading");
        }
        installer
            .install_base_game(fixture.path(), Some(&authorization), &control)
            .unwrap();
        let installing = std::mem::take(&mut *phases.lock().unwrap());
        for phase in [WorkPhase::BaseGameInstall, WorkPhase::ExpansionInstall] {
            assert!(installing.contains(&phase), "{phase:?}");
        }
        for phase in [
            WorkPhase::RuntimeAcquisition,
            WorkPhase::BaseGameDownload,
            WorkPhase::ExpansionDownload,
        ] {
            assert!(!installing.contains(&phase), "{phase:?} while installing");
        }
    }

    #[test]
    fn each_install_step_runs_only_at_its_turn() {
        let fixture = TempDir::new().unwrap();
        let source = FixtureArtifactSource::default();
        let driver = RecordingDriver::default();
        let client = RecordingClientSynchronizer::default();
        // Project Diablo 2 needs Diablo II first.
        let auditor = ScriptedAuditor::new([audit(
            InstallationStatus::NeedsBaseGame,
            CheckStatus::Satisfied,
        )]);
        assert_eq!(
            ManagedInstaller::new(&auditor, &source, &driver, &client, fixture_catalog())
                .install_project_diablo(fixture.path(), &OperationControl::default()),
            Err(InstallError::BaseGameRequired)
        );
        // Diablo II installed: nothing is left for Install Diablo II.
        let auditor = ScriptedAuditor::new([audit(
            InstallationStatus::NeedsClient,
            CheckStatus::Satisfied,
        )]);
        let outcome = ManagedInstaller::new(&auditor, &source, &driver, &client, fixture_catalog())
            .install_base_game(fixture.path(), None, &OperationControl::default())
            .unwrap();
        assert_eq!(outcome.final_status(), InstallationStatus::NeedsClient);
        assert!(outcome.completed_stages().is_empty());
        // Diablo II's installers need both keys.
        let auditor = ScriptedAuditor::new([audit(
            InstallationStatus::NeedsBaseGame,
            CheckStatus::Satisfied,
        )]);
        assert_eq!(
            ManagedInstaller::new(&auditor, &source, &driver, &client, fixture_catalog())
                .install_base_game(fixture.path(), None, &OperationControl::default()),
            Err(InstallError::AuthorizationRequired)
        );
        assert!(driver.operations.borrow().is_empty());
        assert_eq!(*client.calls.borrow(), 0);
    }

    fn existing_install(fixture: &TempDir) -> ExistingInstall {
        let folder = fixture.path().join("My Games/Diablo II");
        crate::game_folder::tests::write_game_folder(&folder, b"game-1.13c", b"archive-1.13c");
        crate::game_folder::validate_existing(&folder, &fixture.path().join("managed"))
            .expect("a complete folder of any version")
    }

    #[test]
    fn an_existing_installation_is_imported_without_keys_under_a_stage_marker() {
        use crate::stage_marker::{MarkerState, read};
        let fixture = TempDir::new().unwrap();
        let existing = existing_install(&fixture);
        let root_path = fixture.path().join("managed");
        let source = FixtureArtifactSource::default();
        let driver = RecordingDriver::default();
        let client = RecordingClientSynchronizer::default();
        let auditor = ScriptedAuditor::new([
            audit(InstallationStatus::NeedsBaseGame, CheckStatus::Missing),
            audit(InstallationStatus::NeedsBaseGame, CheckStatus::Satisfied),
            audit(InstallationStatus::NeedsClient, CheckStatus::Satisfied),
        ]);
        let outcome = ManagedInstaller::new(&auditor, &source, &driver, &client, fixture_catalog())
            .import_existing(
                &root_path,
                &existing,
                ImportMode::InPlace,
                &OperationControl::default(),
            )
            .expect("imported");
        assert_eq!(outcome.final_status(), InstallationStatus::NeedsClient);
        assert_eq!(
            outcome.completed_stages(),
            [InstallStage::ManagedRuntime, InstallStage::Import]
        );
        assert_eq!(
            *driver.operations.borrow(),
            [
                RecordedOperation::ManagedRuntime,
                RecordedOperation::Import(ImportMode::InPlace)
            ]
        );
        // The import ran under its marker, cleared once proven; no key.
        assert_eq!(
            driver.markers.borrow().last(),
            Some(&MarkerState::Present(InterruptedStage::BaseGame))
        );
        assert_eq!(
            read(&Directory::open(&root_path).unwrap()),
            MarkerState::Absent
        );
        assert!(driver.received_base_secret.borrow().is_none());

        // A failed copy leaves its marker: only Start over continues.
        let driver = RecordingDriver {
            fail_import: true,
            ..RecordingDriver::default()
        };
        let auditor = ScriptedAuditor::new([audit(
            InstallationStatus::NeedsBaseGame,
            CheckStatus::Satisfied,
        )]);
        assert_eq!(
            ManagedInstaller::new(&auditor, &source, &driver, &client, fixture_catalog())
                .import_existing(
                    &root_path,
                    &existing,
                    ImportMode::Copy,
                    &OperationControl::default()
                ),
            Err(InstallError::StageFailed {
                stage: InstallStage::Import
            })
        );
        assert_eq!(
            read(&Directory::open(&root_path).unwrap()),
            MarkerState::Present(InterruptedStage::BaseGame)
        );
        // A copy that returned marks its stage finished before the audit:
        // when the audit does not show the game, the finished marker stays.
        let driver = RecordingDriver::default();
        let auditor = ScriptedAuditor::new([
            audit(InstallationStatus::NeedsBaseGame, CheckStatus::Satisfied),
            audit(InstallationStatus::NeedsBaseGame, CheckStatus::Satisfied),
        ]);
        assert!(matches!(
            ManagedInstaller::new(&auditor, &source, &driver, &client, fixture_catalog())
                .import_existing(
                    &root_path,
                    &existing,
                    ImportMode::Copy,
                    &OperationControl::default()
                ),
            Err(InstallError::EvidenceDidNotAdvance { .. })
        ));
        assert_eq!(
            read(&Directory::open(&root_path).unwrap()),
            MarkerState::Finished(InterruptedStage::BaseGame)
        );

        // Nothing is imported over a game that is already there, or from a
        // folder that stopped being usable.
        let auditor = ScriptedAuditor::new([audit(
            InstallationStatus::NeedsExpansion,
            CheckStatus::Satisfied,
        )]);
        assert_eq!(
            ManagedInstaller::new(&auditor, &source, &driver, &client, fixture_catalog())
                .import_existing(
                    &root_path,
                    &existing,
                    ImportMode::Copy,
                    &OperationControl::default()
                ),
            Err(InstallError::BaseGameInstalled)
        );
        fs::remove_file(existing.path().join("d2exp.mpq")).unwrap();
        let auditor = ScriptedAuditor::new([audit(
            InstallationStatus::NeedsBaseGame,
            CheckStatus::Satisfied,
        )]);
        assert_eq!(
            ManagedInstaller::new(&auditor, &source, &driver, &client, fixture_catalog())
                .import_existing(
                    &root_path,
                    &existing,
                    ImportMode::Copy,
                    &OperationControl::default()
                ),
            Err(InstallError::ExistingInstallInvalid(
                ExistingInstallProblem::NoExpansion
            ))
        );
    }

    #[test]
    fn on_android_a_folder_used_in_place_with_its_project_diablo_skips_the_pd2_installer() {
        let fixture = TempDir::new().unwrap();
        let existing = existing_install(&fixture);
        let root_path = fixture.path().join("managed");
        let source = FixtureArtifactSource::default();
        let client = RecordingClientSynchronizer::default();
        let import = |audits: [InstallAudit; 2], mode| {
            let driver = RecordingDriver::default();
            let auditor = ScriptedAuditor::new(audits);
            ManagedInstaller::new(&auditor, &source, &driver, &client, fixture_catalog())
                .import_existing(&root_path, &existing, mode, &OperationControl::default())
                .map(|outcome| outcome.final_status())
        };
        let satisfied = |status, android: bool| {
            let report = audit(status, CheckStatus::Satisfied);
            if android {
                report.with_android_host()
            } else {
                report
            }
        };

        // The copy's ProjectD2 is accepted: Ready, and nothing is installed.
        assert_eq!(
            import(
                [
                    satisfied(InstallationStatus::NeedsBaseGame, true),
                    satisfied(InstallationStatus::Ready, true),
                ],
                ImportMode::InPlace,
            ),
            Ok(InstallationStatus::Ready)
        );
        assert_eq!(*client.calls.borrow(), 0);
        // Without one (or with one the audit refuses) PD2 is installed next.
        assert_eq!(
            import(
                [
                    satisfied(InstallationStatus::NeedsBaseGame, true),
                    satisfied(InstallationStatus::NeedsClient, true),
                ],
                ImportMode::InPlace,
            ),
            Ok(InstallationStatus::NeedsClient)
        );
        // The desktop accepts it too, from a folder used in place and from
        // a copy, which brings ProjectD2 along.
        for mode in [ImportMode::InPlace, ImportMode::Copy] {
            assert_eq!(
                import(
                    [
                        satisfied(InstallationStatus::NeedsBaseGame, false),
                        satisfied(InstallationStatus::Ready, false),
                    ],
                    mode,
                ),
                Ok(InstallationStatus::Ready),
                "{mode:?}"
            );
        }
        // An Android engine's copy (which leaves ProjectD2 out) is
        // unchanged: a Ready after the import is unexplained evidence.
        assert!(matches!(
            import(
                [
                    satisfied(InstallationStatus::NeedsBaseGame, true),
                    satisfied(InstallationStatus::Ready, true),
                ],
                ImportMode::Copy,
            ),
            Err(InstallError::EvidenceDidNotAdvance { .. })
        ));
    }

    #[test]
    fn on_android_a_project_diablo_in_the_game_folder_is_updated_in_place_without_the_pd2_installer()
     {
        let source = FixtureArtifactSource::default();
        // `project` lists the ProjectD2 files the game folder holds.
        let install = |project: &[&str], android: bool, audits: Vec<InstallationStatus>| {
            let fixture = TempDir::new().unwrap();
            let project_d2 = fixture
                .path()
                .join("prefix/drive_c/Program Files (x86)/Diablo II/ProjectD2");
            fs::create_dir_all(&project_d2).unwrap();
            for file in project {
                fs::write(project_d2.join(file), b"pd2").unwrap();
            }
            let driver = RecordingDriver::default();
            let client = RecordingClientSynchronizer::default();
            let auditor = ScriptedAuditor::new(audits.into_iter().map(|status| {
                let report = audit(status, CheckStatus::Satisfied);
                if android {
                    report.with_android_host()
                } else {
                    report
                }
            }));
            let outcome =
                ManagedInstaller::new(&auditor, &source, &driver, &client, fixture_catalog())
                    .install_project_diablo(fixture.path(), &OperationControl::default())
                    .unwrap();
            assert_eq!(auditor.remaining(), 0);
            assert_eq!(outcome.final_status(), InstallationStatus::Ready);
            let bootstrap = driver
                .operations
                .borrow()
                .contains(&RecordedOperation::ClientBootstrap);
            let synchronized = *client.calls.borrow();
            (bootstrap, synchronized, outcome.completed_stages().to_vec())
        };
        let complete = ["Game.exe", "ProjectDiablo.dll"];
        let installer = [
            InstallStage::ClientBootstrap,
            InstallStage::ClientSynchronization,
        ];

        // Adopted: the updater brings it to Ready, the PD2 installer never
        // runs.
        assert_eq!(
            install(
                &complete,
                true,
                vec![InstallationStatus::NeedsClient, InstallationStatus::Ready]
            ),
            (false, 1, vec![InstallStage::ClientSynchronization])
        );
        // Unusable (no client launch files): the PD2 installer runs.
        assert_eq!(
            install(
                &["Game.exe"],
                true,
                vec![InstallationStatus::NeedsClient, InstallationStatus::Ready]
            ),
            (true, 1, installer.to_vec())
        );
        // Not Ready once updated: the PD2 installer runs, then the update.
        assert_eq!(
            install(
                &complete,
                true,
                vec![
                    InstallationStatus::NeedsClient,
                    InstallationStatus::NeedsClient,
                    InstallationStatus::Ready,
                ]
            ),
            (true, 2, installer.to_vec())
        );
        // The desktop adopts it the same way, and runs the PD2 installer
        // where there is nothing to adopt.
        assert_eq!(
            install(
                &complete,
                false,
                vec![InstallationStatus::NeedsClient, InstallationStatus::Ready]
            ),
            (false, 1, vec![InstallStage::ClientSynchronization])
        );
        assert_eq!(
            install(
                &[],
                false,
                vec![InstallationStatus::NeedsClient, InstallationStatus::Ready]
            ),
            (true, 1, installer.to_vec())
        );
    }

    #[test]
    fn on_android_start_over_removes_the_game_location_and_follows_no_link() {
        let fixture = TempDir::new().unwrap();
        let root_path = fixture.path().join("managed");
        // The player's own folders, outside the managed root: links lead there.
        let user_save = fixture.path().join("userdata/Save");
        fs::create_dir_all(&user_save).unwrap();
        fs::write(user_save.join("hero.d2s"), b"hero").unwrap();
        let outside = fixture.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("keep"), b"keep").unwrap();
        // The game folder outside the prefix, its Save a link to the characters.
        let game = root_path.join(crate::game_folder::ANDROID_GAME_RELATIVE);
        crate::game_folder::tests::write_game_folder(&game, b"game", b"archive");
        fs::remove_dir_all(game.join("Save")).unwrap();
        std::os::unix::fs::symlink(&user_save, game.join("Save")).unwrap();
        fs::write(game.join("ProjectD2/loot.filter"), b"mine").unwrap();
        // The prefix with its link to the game folder; the app's prefix
        // state with a folder link; the records of an import; Fast
        // download's partial folder.
        fs::create_dir_all(root_path.join("prefix/drive_c/Program Files (x86)")).unwrap();
        let root = Directory::open(&root_path).unwrap();
        crate::game_folder::link_game(&root).unwrap();
        let state = root_path.join(crate::game_folder::ANDROID_PREFIX_STATE);
        fs::create_dir_all(state.join("files")).unwrap();
        fs::write(state.join("player.reg"), b"WINE REGISTRY Version 2\n").unwrap();
        std::os::unix::fs::symlink(&outside, state.join("files/linked")).unwrap();
        fs::write(
            root_path.join(crate::game_folder::IMPORT_RECORD_NAME),
            b"{}",
        )
        .unwrap();
        fs::write(root_path.join(COPIED_CLIENT_MARK), b"mark").unwrap();
        fs::create_dir_all(root_path.join("Diablo II.partial")).unwrap();
        // A file of the game folder's name as a link: removed as a link.
        let paths = ManagedPaths::prepare_on(&root_path, true).unwrap();
        let lease = MutationLease::acquire(&paths).unwrap();
        reset_managed_installation_on(&root, &lease.0, true).expect("start over");
        drop(lease);
        // The marker that it wrote first goes last.
        assert_eq!(
            crate::stage_marker::read(&root),
            crate::stage_marker::MarkerState::Absent
        );
        for name in [
            "prefix",
            crate::game_folder::ANDROID_GAME_RELATIVE,
            "Diablo II.partial",
            crate::game_folder::ANDROID_PREFIX_STATE,
            crate::game_folder::IMPORT_RECORD_NAME,
            COPIED_CLIENT_MARK,
        ] {
            assert!(
                fs::symlink_metadata(root_path.join(name)).is_err(),
                "{name} is still there"
            );
        }
        // What the links led to stays: the characters and the other folder.
        assert_eq!(fs::read(user_save.join("hero.d2s")).unwrap(), b"hero");
        assert_eq!(fs::read(outside.join("keep")).unwrap(), b"keep");
        // The desktop's Start over leaves the root's other names alone.
        fs::create_dir_all(root_path.join("prefix")).unwrap();
        fs::create_dir_all(root_path.join(crate::game_folder::ANDROID_GAME_RELATIVE)).unwrap();
        let paths = ManagedPaths::prepare_on(&root_path, false).unwrap();
        let lease = MutationLease::acquire(&paths).unwrap();
        reset_managed_installation_on(&root, &lease.0, false).expect("start over");
        assert!(!root_path.join("prefix").exists());
        assert!(
            root_path
                .join(crate::game_folder::ANDROID_GAME_RELATIVE)
                .is_dir()
        );
    }

    /// A Rust copy of the Android app's rule, `LegacyPrefix.broken` and
    /// `LegacyPrefix.gameInLocation` in
    /// `android/app/src/main/java/com/pd2launcher/app/LegacyPrefix.kt`: the
    /// app makes the prefix again only with no move begun (no `.prefix.old`)
    /// and only for an installation whose prefix state has `player.reg`, and
    /// then only when the prefix is gone while the game folder holds an entry
    /// other than its `Save` link (a real `Save` folder counts), or the prefix
    /// lacks a hive or `drive_c/windows`. The app's guard for a template that
    /// is not whole is left out: without it this copy says "made again" in
    /// more states, so a test that it says no is the stricter one. The app's
    /// own test of the same steps is `DisposablePrefixTest`.
    fn android_app_makes_the_prefix_again(root: &Path) -> bool {
        let state = root.join(crate::game_folder::ANDROID_PREFIX_STATE);
        if fs::symlink_metadata(root.join(".prefix.old")).is_ok()
            || !state.join("player.reg").is_file()
        {
            return false;
        }
        let prefix = root.join("prefix");
        if fs::symlink_metadata(&prefix).is_err() {
            return crate::audit::GameTree::managed_on(&Directory::open(root).unwrap(), true)
                .holds_an_entry();
        }
        ["system.reg", "user.reg"]
            .iter()
            .any(|hive| !prefix.join(hive).is_file())
            || !prefix.join("drive_c/windows").is_dir()
    }

    /// A whole Android installation that was Ready once, below `fixture`: the
    /// game folder with its `Save` link to the player's characters, the prefix
    /// with its hives, `drive_c/windows` and the link to the game folder, the
    /// app's prefix state; with `imported`, the records of an import and Fast
    /// download's partial folder too. The managed root and the player's `Save`.
    fn whole_android_installation(fixture: &TempDir, imported: bool) -> (PathBuf, PathBuf) {
        let root_path = fixture.path().join("managed");
        let user_save = fixture.path().join("userdata/Save");
        fs::create_dir_all(&user_save).unwrap();
        fs::write(user_save.join("hero.d2s"), b"hero").unwrap();
        let game = root_path.join(crate::game_folder::ANDROID_GAME_RELATIVE);
        crate::game_folder::tests::write_game_folder(&game, b"game", b"archive");
        fs::remove_dir_all(game.join("Save")).unwrap();
        std::os::unix::fs::symlink(&user_save, game.join("Save")).unwrap();
        fs::create_dir_all(root_path.join("prefix/drive_c/windows")).unwrap();
        fs::create_dir_all(root_path.join("prefix/drive_c/Program Files (x86)")).unwrap();
        for hive in ["system.reg", "user.reg"] {
            fs::write(
                root_path.join("prefix").join(hive),
                b"WINE REGISTRY Version 2\n",
            )
            .unwrap();
        }
        crate::game_folder::link_game(&Directory::open(&root_path).unwrap()).unwrap();
        let state = root_path.join(crate::game_folder::ANDROID_PREFIX_STATE);
        fs::create_dir_all(state.join("files")).unwrap();
        fs::write(state.join("player.reg"), b"WINE REGISTRY Version 2\n").unwrap();
        fs::write(state.join("files.list"), b"").unwrap();
        if imported {
            fs::write(
                root_path.join(crate::game_folder::IMPORT_RECORD_NAME),
                b"{}",
            )
            .unwrap();
            fs::write(root_path.join(COPIED_CLIENT_MARK), b"mark").unwrap();
            fs::create_dir_all(root_path.join("Diablo II.partial")).unwrap();
        }
        (root_path, user_save)
    }

    #[test]
    fn on_android_a_start_over_stopped_at_any_step_leaves_nothing_the_app_makes_again() {
        use crate::stage_marker::{MarkerState, read};
        assert_eq!(
            ANDROID_START_OVER.first(),
            Some(&crate::game_folder::ANDROID_PREFIX_STATE)
        );
        assert_eq!(
            ANDROID_START_OVER.last(),
            Some(&crate::game_folder::ANDROID_GAME_RELATIVE)
        );
        // A stop after `done` steps, and part way through the next one
        // (`inside`): one file of it gone, the worst one for the rule (the
        // prefix state keeps its player.reg, the prefix loses a hive); in the
        // game folder's step also a late stop that leaves only `ProjectD2/`
        // or only a stray file, none of them a file that a check names.
        for done in 0..ANDROID_START_OVER.len() {
            for inside in ["no", "one file", "only ProjectD2", "only a stray file"] {
                let game_step =
                    ANDROID_START_OVER[done] == crate::game_folder::ANDROID_GAME_RELATIVE;
                if inside.starts_with("only") && !game_step {
                    continue;
                }
                let fixture = TempDir::new().unwrap();
                let (root_path, user_save) = whole_android_installation(&fixture, true);
                let root = Directory::open(&root_path).unwrap();
                assert!(!android_app_makes_the_prefix_again(&root_path));

                start_over_android(&root, &ANDROID_START_OVER[..done]).unwrap();
                let game = root_path.join(crate::game_folder::ANDROID_GAME_RELATIVE);
                if inside.starts_with("only") {
                    fs::write(game.join("stray.txt"), b"stray").unwrap();
                    let keep = if inside == "only ProjectD2" {
                        "ProjectD2"
                    } else {
                        "stray.txt"
                    };
                    for entry in fs::read_dir(&game).unwrap() {
                        let name = entry.unwrap().file_name();
                        if name != keep && name != "Save" {
                            remove_entry(&Directory::open(&game).unwrap(), &name).unwrap();
                        }
                    }
                } else if inside == "one file" {
                    let next = root_path.join(ANDROID_START_OVER[done]);
                    match ANDROID_START_OVER[done] {
                        crate::game_folder::ANDROID_PREFIX_STATE => {
                            fs::remove_file(next.join("files.list")).unwrap()
                        }
                        crate::game_folder::ANDROID_GAME_RELATIVE => {
                            fs::remove_file(next.join("Game.exe")).unwrap()
                        }
                        "prefix" => fs::remove_file(next.join("system.reg")).unwrap(),
                        _ => remove_entry(&root, OsStr::new(ANDROID_START_OVER[done])).unwrap(),
                    }
                }
                let case = format!("a stop after {done} steps (inside the next one: {inside})");
                assert!(!android_app_makes_the_prefix_again(&root_path), "{case}");
                // The started base-game marker is on the disk beside an entry of
                // the game folder, which the audit reads as an interrupted step
                // (Repair with Start over), never as a Ready installation: the
                // audit's own reader of the folder
                // (`audit::tests::a_game_folder_with_only_unnamed_entries_beside_a_started_marker_is_interrupted`).
                assert_eq!(
                    read(&root),
                    MarkerState::Present(InterruptedStage::BaseGame),
                    "{case}"
                );
                assert!(
                    crate::audit::GameTree::managed_on(&root, true).holds_an_entry(),
                    "{case}"
                );
                // The next Start over finishes.
                start_over_android(&root, &ANDROID_START_OVER).unwrap();
                crate::stage_marker::clear(&root).unwrap();
                for name in ANDROID_START_OVER {
                    assert!(
                        fs::symlink_metadata(root_path.join(name)).is_err(),
                        "{name}"
                    );
                }
                // No removal followed the Save link.
                assert_eq!(fs::read(user_save.join("hero.d2s")).unwrap(), b"hero");
            }
        }
    }

    /// A GOLDEN installation from Fast download, on a device: a kill 1.27 s
    /// after the Start over command found the prefix state gone, the game
    /// folder and the prefix whole, no partial folder and no marker, and the
    /// next start was Ready again, the Start over undone. That state now has
    /// the started base-game marker, which the audit reads as an interrupted
    /// step beside the whole game
    /// (`audit::tests::a_start_over_stopped_after_the_prefix_state_is_an_interrupted_step`).
    #[test]
    fn on_android_a_start_over_stopped_after_the_prefix_state_leaves_the_marker() {
        use crate::stage_marker::{MarkerState, read};
        let fixture = TempDir::new().unwrap();
        let (root_path, _) = whole_android_installation(&fixture, false);
        let root = Directory::open(&root_path).unwrap();
        start_over_android(&root, &ANDROID_START_OVER[..1]).unwrap();
        assert!(
            fs::symlink_metadata(root_path.join(crate::game_folder::ANDROID_PREFIX_STATE)).is_err()
        );
        let game = root_path.join(crate::game_folder::ANDROID_GAME_RELATIVE);
        assert_eq!(fs::read(game.join("Game.exe")).unwrap(), b"game");
        assert!(
            fs::symlink_metadata(game.join("Save"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        for hive in ["system.reg", "user.reg"] {
            assert!(root_path.join("prefix").join(hive).is_file());
        }
        assert!(fs::symlink_metadata(root_path.join("Diablo II.partial")).is_err());
        assert_eq!(
            read(&root),
            MarkerState::Present(InterruptedStage::BaseGame)
        );
        assert!(!android_app_makes_the_prefix_again(&root_path));
    }

    #[test]
    fn on_android_the_prefix_is_made_again_only_for_the_other_template_and_no_game() {
        use crate::runtime::PrefixTemplate;
        let fixture = TempDir::new().unwrap();
        let root_path = fixture.path();
        let mark = root_path
            .join("prefix")
            .join(crate::runtime::PREFIX_TEMPLATE_MARK);
        fs::create_dir_all(mark.parent().unwrap()).unwrap();
        let android = ManagedPaths::prepare_on(root_path, true).unwrap();
        // No mark (a prefix of an older layout): kept.
        assert!(!prefix_has_other_template(&android, PrefixTemplate::Plain));
        fs::write(&mark, b"golden\n").unwrap();
        assert!(!prefix_has_other_template(&android, PrefixTemplate::Golden));
        // The GOLDEN one before Blizzard's installers, with no game yet (no
        // game folder, or an empty one): made again from PLAIN.
        assert!(prefix_has_other_template(&android, PrefixTemplate::Plain));
        let game = root_path.join(crate::game_folder::ANDROID_GAME_RELATIVE);
        fs::create_dir_all(&game).unwrap();
        assert!(prefix_has_other_template(&android, PrefixTemplate::Plain));
        fs::write(&mark, b"plain\n").unwrap();
        assert!(prefix_has_other_template(&android, PrefixTemplate::Golden));
        // Once a file of the game is there, the prefix stays.
        fs::write(game.join("Game.exe"), b"game").unwrap();
        assert!(!prefix_has_other_template(&android, PrefixTemplate::Golden));
        // A mark of another kind, and the desktop: kept.
        fs::remove_file(game.join("Game.exe")).unwrap();
        fs::write(&mark, b"other\n").unwrap();
        assert!(!prefix_has_other_template(&android, PrefixTemplate::Golden));
        fs::write(&mark, b"plain\n").unwrap();
        let desktop = ManagedPaths::prepare_on(root_path, false).unwrap();
        assert!(!prefix_has_other_template(&desktop, PrefixTemplate::Golden));
    }

    #[test]
    fn on_android_the_copied_clients_mark_is_in_the_managed_root() {
        let fixture = TempDir::new().unwrap();
        let root_path = fixture.path();
        fs::create_dir_all(root_path.join("prefix")).unwrap();
        let root = Directory::open(root_path).unwrap();
        let record = b"{\"schema\":1}";
        fs::write(root_path.join(COPIED_CLIENT_MARK), sha256_hex(record)).unwrap();
        assert!(is_copied_client_record_on(&root, record, true));
        assert!(!is_copied_client_record_on(&root, record, false));
        fs::rename(
            root_path.join(COPIED_CLIENT_MARK),
            root_path.join("prefix").join(COPIED_CLIENT_MARK),
        )
        .unwrap();
        assert!(is_copied_client_record_on(&root, record, false));
        assert!(!is_copied_client_record_on(&root, record, true));
    }

    #[test]
    fn start_over_never_touches_a_game_folder_used_in_place() {
        use std::os::unix::fs::MetadataExt;
        let fixture = TempDir::new().unwrap();
        let root_path = fixture.path().join("managed");
        fs::create_dir_all(root_path.join("prefix/drive_c/windows")).unwrap();
        let external = fixture.path().join("My Games/Diablo II");
        crate::game_folder::tests::write_game_folder(&external, b"game", b"archive");
        fs::write(external.join("ProjectD2/loot.filter"), b"mine").unwrap();
        let metadata = fs::metadata(&external).unwrap();
        fs::write(
            root_path
                .join("prefix")
                .join(crate::game_folder::POINTER_NAME),
            format!(
                r#"{{"schema":1,"path":"{}","filesystem":{},"inode":{}}}"#,
                external.display(),
                rustix::fs::statvfs(&external).unwrap().f_fsid,
                metadata.ino()
            ),
        )
        .unwrap();
        // Wine's own link to the host root stays a link: never followed.
        fs::create_dir_all(root_path.join("prefix/dosdevices")).unwrap();
        std::os::unix::fs::symlink("/", root_path.join("prefix/dosdevices/z:")).unwrap();
        let root = Directory::open(&root_path).unwrap();
        assert!(crate::game_folder::GameFolder::resolve(&root).is_ok());

        let paths = ManagedPaths::prepare(&root_path).unwrap();
        let lease = MutationLease::acquire(&paths).unwrap();
        reset_managed_prefix(&root, &lease.0).expect("start over");
        drop(lease);
        assert!(!root_path.join("prefix").exists());
        // The folder, and what Project Diablo 2 added to it, are untouched;
        // the launcher simply no longer uses it.
        assert_eq!(fs::read(external.join("Game.exe")).unwrap(), b"game");
        assert_eq!(
            fs::read(external.join("ProjectD2/loot.filter")).unwrap(),
            b"mine"
        );
        assert_eq!(
            crate::game_folder::GameFolder::read(&root),
            Ok(crate::game_folder::GameFolder::Managed)
        );
    }

    #[test]
    fn a_rejected_custom_gecko_is_replaced_without_downloading_the_accepted_runtime() {
        let fixture = TempDir::new().expect("fixture");
        let rejected_gecko = || {
            InstallAudit::new(
                InstallationStatus::NeedsClient,
                HostSupport::Supported,
                CheckStatus::Satisfied,
                CheckStatus::Invalid,
            )
            .with_prefix_needs(PrefixNeeds {
                gecko: true,
                ..PrefixNeeds::default()
            })
        };
        let auditor = ScriptedAuditor::new([
            // Download, Install Diablo II (nothing to do), Install PD2.
            rejected_gecko(),
            rejected_gecko(),
            rejected_gecko(),
            audit(InstallationStatus::NeedsClient, CheckStatus::Satisfied),
            audit(InstallationStatus::Ready, CheckStatus::Satisfied),
        ]);
        let source = FixtureArtifactSource::default();
        let driver = RecordingDriver::default();
        let client = RecordingClientSynchronizer::default();
        let mut installer =
            ManagedInstaller::new(&auditor, &source, &driver, &client, fixture_catalog());

        let outcome = installer.run(fixture.path(), None).expect("restored");
        assert_eq!(outcome.final_status(), InstallationStatus::Ready);
        // Downloaded once, then used from the cache by the install steps.
        assert_eq!(
            *source.acquisitions.borrow(),
            [
                ArtifactKind::Gecko,
                ArtifactKind::ClientBootstrap,
                ArtifactKind::Gecko,
                ArtifactKind::ClientBootstrap,
            ]
        );
        assert_eq!(
            driver.operations.borrow()[..2],
            [
                RecordedOperation::ManagedRuntime,
                RecordedOperation::ClientBootstrap
            ]
        );
    }

    #[test]
    fn a_complete_installation_missing_wine_components_gets_only_those_in_place() {
        // An installation an older release made without Wine Mono and the
        // .NET Desktop Runtime in its prefix: the game is complete.
        let needs_runtime = || {
            InstallAudit::new(
                InstallationStatus::NeedsRuntime,
                HostSupport::Supported,
                CheckStatus::Satisfied,
                CheckStatus::Missing,
            )
            .with_prefix_needs(PrefixNeeds {
                gecko: false,
                mono: true,
                dotnet: true,
            })
        };
        let fixture = TempDir::new().expect("fixture");
        let auditor = ScriptedAuditor::new([
            needs_runtime(),
            needs_runtime(),
            needs_runtime(),
            audit(InstallationStatus::Ready, CheckStatus::Satisfied),
        ]);
        let source = FixtureArtifactSource::default();
        let driver = RecordingDriver::default();
        let client = RecordingClientSynchronizer::default();
        let mut installer =
            ManagedInstaller::new(&auditor, &source, &driver, &client, fixture_catalog());

        // No product keys are needed, and nothing but the two components is
        // downloaded (then used from the cache); no game stage runs again.
        let outcome = installer.run(fixture.path(), None).expect("migrated");
        assert_eq!(outcome.final_status(), InstallationStatus::Ready);
        assert_eq!(outcome.completed_stages(), [InstallStage::ManagedRuntime]);
        assert_eq!(
            *source.acquisitions.borrow(),
            [
                ArtifactKind::WineMono,
                ArtifactKind::DotnetDesktop,
                ArtifactKind::WineMono,
                ArtifactKind::DotnetDesktop
            ]
        );
        assert_eq!(
            *driver.operations.borrow(),
            [RecordedOperation::ManagedRuntime]
        );
        assert_eq!(*client.calls.borrow(), 0);
        assert_eq!(*client.prefetches.borrow(), 0);
        assert_eq!(auditor.remaining(), 0);

        // A provisioning pass that does not produce Ready fails closed.
        let auditor = ScriptedAuditor::new([
            needs_runtime(),
            needs_runtime(),
            needs_runtime(),
            needs_runtime(),
        ]);
        let mut installer =
            ManagedInstaller::new(&auditor, &source, &driver, &client, fixture_catalog());
        assert_eq!(
            installer.run(fixture.path(), None),
            Err(InstallError::RuntimeEvidenceInvalid)
        );
    }

    #[test]
    fn successful_process_without_new_evidence_does_not_advance_installation() {
        let fixture = TempDir::new().expect("fixture");
        let auditor = ScriptedAuditor::new([
            audit(InstallationStatus::NeedsBaseGame, CheckStatus::Satisfied),
            audit(InstallationStatus::NeedsBaseGame, CheckStatus::Satisfied),
            audit(InstallationStatus::NeedsBaseGame, CheckStatus::Satisfied),
        ]);
        let source = FixtureArtifactSource::default();
        let driver = RecordingDriver::default();
        let client = RecordingClientSynchronizer::default();
        let authorization = InstallationAuthorization::new(
            FAKE_BASE_KEY.to_owned(),
            FAKE_EXPANSION_KEY.to_owned(),
            true,
        )
        .expect("valid authorization");
        let mut installer =
            ManagedInstaller::new(&auditor, &source, &driver, &client, fixture_catalog());

        let error = installer
            .run(fixture.path(), Some(&authorization))
            .expect_err("process completion is not Installation evidence");

        assert_eq!(
            error,
            InstallError::EvidenceDidNotAdvance {
                stage: InstallStage::BaseGame,
                expected: InstallationStatus::NeedsExpansion,
                observed: InstallationStatus::NeedsBaseGame,
            }
        );
    }

    #[test]
    fn conflicting_evidence_after_a_vendor_stage_fails_closed() {
        let fixture = TempDir::new().expect("fixture");
        let auditor = ScriptedAuditor::new([
            audit(InstallationStatus::NeedsBaseGame, CheckStatus::Satisfied),
            audit(InstallationStatus::NeedsBaseGame, CheckStatus::Satisfied),
            audit(InstallationStatus::Indeterminate, CheckStatus::Satisfied),
        ]);
        let source = FixtureArtifactSource::default();
        let driver = RecordingDriver::default();
        let client = RecordingClientSynchronizer::default();
        let authorization = InstallationAuthorization::new(
            FAKE_BASE_KEY.to_owned(),
            FAKE_EXPANSION_KEY.to_owned(),
            true,
        )
        .expect("valid authorization");
        let mut installer =
            ManagedInstaller::new(&auditor, &source, &driver, &client, fixture_catalog());

        assert_eq!(
            installer.run(fixture.path(), Some(&authorization)),
            Err(InstallError::Indeterminate)
        );
    }

    #[test]
    fn a_cache_left_by_the_retired_patch_stage_is_still_accepted() {
        // An installation an older release patched keeps its cached patch,
        // the stub's log, and a patch download slot: tolerated, never used,
        // and never a reason to stop.
        let fixture = TempDir::new().unwrap();
        let cache = fixture.path().join("cache/artifacts");
        fs::create_dir_all(&cache).unwrap();
        for leftover in [
            "lod-patch-1.14d.verified",
            "custom-patch.download",
            "custom-patch.download.provenance.json",
            "prepatch.log",
            "artifact-04.partial",
        ] {
            fs::write(cache.join(leftover), b"leftover").unwrap();
        }
        assert_eq!(
            validate_artifact_inventory(&Directory::open(&cache).unwrap()),
            Ok(())
        );
        fs::write(cache.join("unexpected.exe"), b"x").unwrap();
        assert_eq!(
            validate_artifact_inventory(&Directory::open(&cache).unwrap()),
            Err(ArtifactRejection::UnsafePath)
        );
    }

    #[test]
    fn proven_vendor_stages_resume_without_product_keys() {
        let fixture = TempDir::new().expect("fixture");
        let auditor = ScriptedAuditor::new([
            audit(InstallationStatus::NeedsClient, CheckStatus::Satisfied),
            audit(InstallationStatus::NeedsClient, CheckStatus::Satisfied),
            audit(InstallationStatus::NeedsClient, CheckStatus::Satisfied),
            audit(InstallationStatus::Ready, CheckStatus::Satisfied),
        ]);
        let source = FixtureArtifactSource::default();
        let driver = RecordingDriver::default();
        let client = RecordingClientSynchronizer::default();
        let mut installer =
            ManagedInstaller::new(&auditor, &source, &driver, &client, fixture_catalog());

        let outcome = installer
            .run(fixture.path(), None)
            .expect("proven vendor checkpoints do not require fresh keys");

        assert_eq!(outcome.final_status(), InstallationStatus::Ready);
        assert_eq!(
            outcome.completed_stages(),
            [
                InstallStage::ClientBootstrap,
                InstallStage::ClientSynchronization,
            ]
        );
        assert!(driver.received_base_secret.borrow().is_none());
        assert!(driver.received_expansion_secret.borrow().is_none());
    }

    #[test]
    fn authorization_requires_consent_and_never_formats_product_keys() {
        let missing_consent = InstallationAuthorization::new(
            FAKE_BASE_KEY.to_owned(),
            FAKE_EXPANSION_KEY.to_owned(),
            false,
        );
        assert!(matches!(
            missing_consent,
            Err(AuthorizationError::ConsentRequired)
        ));

        let authorization = InstallationAuthorization::new(
            FAKE_BASE_KEY.to_owned(),
            FAKE_EXPANSION_KEY.to_owned(),
            true,
        )
        .expect("valid authorization");
        let rendered = format!("{authorization:?}");
        assert!(!rendered.contains(&FAKE_BASE_KEY[..9]));
        assert!(!rendered.contains(&FAKE_EXPANSION_KEY[..9]));
        assert_eq!(rendered.matches("[REDACTED]").count(), 2);
    }

    #[test]
    fn artifact_verification_rejects_unapproved_origin_and_digest() {
        let fixture = TempDir::new().expect("fixture");
        let cache = fixture.path().join("cache");
        fs::create_dir(&cache).expect("cache");
        let path = cache.join("artifact.bin");
        fs::write(&path, b"fixture").expect("artifact");
        let spec = fixed_spec(ArtifactKind::Gecko);

        let rejected_origin = verify_artifact(
            &spec,
            &cache,
            AcquiredArtifact::new(
                path.clone(),
                spec.request_url(),
                "https://mirror.invalid/artifact.bin",
                None,
            ),
        );
        assert_eq!(rejected_origin, Err(ArtifactRejection::UnapprovedFinalUrl));

        fs::write(&path, b"tamper!").expect("tampered artifact");
        let rejected_digest = verify_artifact(
            &spec,
            &cache,
            AcquiredArtifact::new(path, spec.request_url(), spec.request_url(), None),
        );
        assert_eq!(rejected_digest, Err(ArtifactRejection::DigestMismatch));
    }

    #[test]
    fn wrapper_verification_requires_the_expected_name_and_pe_structure() {
        let fixture = TempDir::new().expect("fixture");
        let cache = fixture.path().join("cache");
        fs::create_dir(&cache).expect("cache");
        let path = cache.join("wrapper.exe");
        fs::write(&path, minimal_pe()).expect("wrapper");
        let spec = wrapper_spec(ArtifactKind::BaseGameWrapper);

        let wrong_name = verify_artifact(
            &spec,
            &cache,
            AcquiredArtifact::new(
                path.clone(),
                spec.request_url(),
                spec.request_url(),
                Some("not-the-approved-wrapper.exe".to_owned()),
            ),
        );
        assert_eq!(wrong_name, Err(ArtifactRejection::UnexpectedFilename));

        fs::write(&path, [0_u8; 128]).expect("invalid executable");
        let invalid_pe = verify_artifact(
            &spec,
            &cache,
            AcquiredArtifact::new(
                path,
                spec.request_url(),
                spec.request_url(),
                spec.expected_filename.map(str::to_owned),
            ),
        );
        assert_eq!(invalid_pe, Err(ArtifactRejection::InvalidExecutable));
    }

    #[test]
    fn complete_payload_verification_retains_the_nested_installer_path() {
        let fixture = TempDir::new().expect("fixture");
        let cache = fixture.path().join("cache");
        let payload = cache.join("d2-payload");
        fs::create_dir_all(&payload).expect("payload cache");
        let path = payload.join("Installer.exe");
        let bytes = minimal_pe();
        fs::write(&path, &bytes).expect("complete installer");
        let spec = wrapper_spec(ArtifactKind::BaseGameWrapper);
        let verified = verify_artifact(
            &spec,
            &cache,
            AcquiredArtifact::user_selected(path.clone(), spec.expected_filename.unwrap())
                .with_retained_receipt_sha256(hex(&Sha256::digest(&bytes))),
        )
        .expect("verified nested complete installer");

        assert_eq!(verified.path(), path);
    }

    #[test]
    fn the_builtin_fex_comes_from_the_mirror_then_launchpad_with_the_same_pins() {
        let catalog = ArtifactCatalog::pinned();
        let package = crate::builtin_fex::FexVariant::current().package();
        let spec = catalog.spec(ArtifactKind::FexPackage);
        let sources = spec
            .sources()
            .map(|source| (source.request_url().to_owned(), source.redirects))
            .collect::<Vec<_>>();
        assert_eq!(
            sources,
            [
                (
                    package.mirror_url().to_owned(),
                    RedirectPolicy::GithubReleaseAsset
                ),
                (
                    package.upstream()[0].to_owned(),
                    RedirectPolicy::LaunchpadLibrarian
                ),
                (package.upstream()[1].to_owned(), RedirectPolicy::None),
            ]
        );
        assert!(sources[0].0.starts_with("https://github.com/"));
        assert!(sources[1].0.contains("/+files/"));
        assert!(sources[2].0.contains("/pool/"));
        for source in spec.sources() {
            assert_eq!(source.size, SizePolicy::Exact(package.size()));
            assert_eq!(source.integrity, IntegrityPolicy::Sha256(package.sha256()));
            assert!(source.fallback_urls.is_empty());
        }

        let rootfs = crate::builtin_fex::rootfs();
        let spec = catalog.spec(ArtifactKind::FexRootFs);
        assert_eq!(
            spec.sources()
                .map(|source| (source.request_url().to_owned(), source.redirects))
                .collect::<Vec<_>>(),
            [
                (
                    rootfs.mirror_url().to_owned(),
                    RedirectPolicy::GithubReleaseAsset
                ),
                (rootfs.upstream()[0].to_owned(), RedirectPolicy::None),
            ]
        );
        assert_eq!(spec.size, SizePolicy::Exact(rootfs.size()));
        assert_eq!(spec.integrity, IntegrityPolicy::Sha256(rootfs.sha256()));

        // Only the built-in FEX has other sources; a custom source has none.
        for spec in &catalog.specs {
            let builtin_fex = matches!(
                spec.kind,
                ArtifactKind::FexPackage | ArtifactKind::FexRootFs
            );
            assert_eq!(spec.fallback_urls.is_empty(), !builtin_fex, "{spec:?}");
        }
        let sources = crate::sources::DownloadSources::resolve(
            &[(
                crate::sources::DownloadArtifact::Fex,
                "https://mirror.example/fex.deb".to_owned(),
            )]
            .into(),
            &std::collections::BTreeMap::new(),
        );
        let custom = ArtifactCatalog::with_sources(&sources);
        let custom = custom.spec(ArtifactKind::FexPackage);
        assert!(custom.is_custom());
        assert_eq!(custom.sources().count(), 1);
    }

    #[test]
    fn launchpads_file_link_may_lead_only_to_its_librarians_copy_of_that_file() {
        let spec = ArtifactCatalog::pinned()
            .spec(ArtifactKind::FexPackage)
            .sources()
            .nth(1)
            .unwrap();
        let name = spec.request_url().rsplit_once('/').unwrap().1.to_owned();
        assert!(name.ends_with("_arm64.deb"), "{name}");
        assert!(spec.approves_final_url(spec.request_url()));
        assert!(
            spec.approves_final_url(&format!("https://launchpadlibrarian.net/877920585/{name}"))
        );
        for refused in [
            format!("https://launchpadlibrarian.net/877920585/other_{name}"),
            format!("https://launchpadlibrarian.net/87a/{name}"),
            format!("https://launchpadlibrarian.net//{name}"),
            "https://launchpadlibrarian.net/877920585/".to_owned(),
            format!("https://launchpadlibrarian.net.evil.invalid/877920585/{name}"),
            format!("http://launchpadlibrarian.net/877920585/{name}"),
            format!("https://launchpadlibrarian.net/877920585/x/{name}"),
            format!("https://elsewhere.invalid/{name}"),
        ] {
            assert!(!spec.approves_final_url(&refused), "{refused}");
        }

        // The workflow accepts what came from a fallback, and only from where
        // that fallback may lead.
        let catalog = ArtifactCatalog::pinned();
        let pinned = catalog.spec(ArtifactKind::FexPackage);
        let librarian = format!("https://launchpadlibrarian.net/877920585/{name}");
        let fixture = tempfile::tempdir().unwrap();
        let artifact = |requested: &str, served: &str| {
            AcquiredArtifact::new(fixture.path().join("fex"), requested, served, None)
        };
        assert!(
            validate_artifact_provenance(pinned, &artifact(spec.request_url(), &librarian)).is_ok()
        );
        assert_eq!(
            validate_artifact_provenance(
                pinned,
                &artifact(spec.request_url(), "https://elsewhere.invalid/fex.deb")
            ),
            Err(ArtifactRejection::UnapprovedFinalUrl)
        );
        // A librarian link is not a source of its own.
        assert_eq!(
            validate_artifact_provenance(pinned, &artifact(&librarian, &librarian)),
            Err(ArtifactRejection::RequestUrlMismatch)
        );
    }

    #[test]
    fn pinned_catalog_freezes_reviewed_artifact_contracts() {
        let catalog = ArtifactCatalog::pinned();
        let runtime = catalog.spec(ArtifactKind::ManagedRuntime);
        assert_eq!(runtime.kind(), ArtifactKind::ManagedRuntime);
        let profile = crate::runtime::current();
        assert_eq!(runtime.request_url(), profile.archive_url);
        assert_eq!(runtime.size, SizePolicy::Exact(profile.archive_size));
        assert_eq!(
            runtime.integrity,
            IntegrityPolicy::Sha512(profile.archive_sha512)
        );
        assert_eq!(runtime.redirects, RedirectPolicy::GithubReleaseAsset);
        assert!(matches!(runtime.integrity, IntegrityPolicy::Sha512(_)));
        assert!(runtime.approves_final_url(
            "https://release-assets.githubusercontent.com/github-production-release-asset/file?token=fixture"
        ));
        assert!(
            !runtime.approves_final_url(
                "https://release-assets.githubusercontent.com.evil.invalid/file"
            )
        );

        // No built-in source is cleartext.
        for source in catalog.specs.iter().flat_map(ArtifactSpec::sources) {
            assert!(source.request_url().starts_with("https://"), "{source:?}");
        }

        let base = catalog.spec(ArtifactKind::BaseGameWrapper);
        assert_eq!(base.expected_filename, Some("Downloader_Diablo2_enUS.exe"));
        assert_eq!(base.integrity, IntegrityPolicy::PeExecutable);

        let mono = catalog.spec(ArtifactKind::WineMono);
        assert_eq!(
            mono.request_url(),
            "https://dl.winehq.org/wine/wine-mono/11.3.0/wine-mono-11.3.0-x86.msi"
        );
        assert_eq!(mono.redirects, RedirectPolicy::None);
        assert_eq!(mono.size, SizePolicy::Exact(83_348_992));
        assert_eq!(
            mono.integrity,
            IntegrityPolicy::Sha256(
                "df2dfc1665c2511882e7cabd56eafd0c0a3d94e5a7e86f969277f6c189d418d3"
            )
        );

        let dotnet = catalog.spec(ArtifactKind::DotnetDesktop);
        assert_eq!(
            dotnet.request_url(),
            "https://builds.dotnet.microsoft.com/dotnet/WindowsDesktop/6.0.36/windowsdesktop-runtime-6.0.36-win-x86.exe"
        );
        assert_eq!(dotnet.redirects, RedirectPolicy::None);
        assert_eq!(dotnet.size, SizePolicy::Exact(52_173_224));
        assert_eq!(
            dotnet.integrity,
            IntegrityPolicy::Sha512(
                "a18351aabfe1590e58af79e57ac2414254ba80cb7a1fef19545a6b8418575c735fc1dc164c3c7fed426c4698f099991487fa4f443bab93afd41d1563845fbcf4"
            )
        );
    }

    #[test]
    fn a_custom_source_replaces_only_its_entry_and_never_relaxes_builtins() {
        use crate::sources::{DownloadArtifact, DownloadSources};
        let builtin = ArtifactCatalog::pinned();
        assert_eq!(
            ArtifactCatalog::with_sources(&DownloadSources::builtin()).specs,
            builtin.specs
        );
        let sources = DownloadSources::resolve(
            &[(
                DownloadArtifact::D2Installer,
                "https://mirror.example/d2.exe".to_owned(),
            )]
            .into(),
            &[
                (
                    DownloadArtifact::WineMono,
                    "http://mirror.example/mono.msi".to_owned(),
                ),
                (
                    DownloadArtifact::DotnetDesktop,
                    "https://mirror.example/dotnet.exe".to_owned(),
                ),
            ]
            .into(),
        );
        let catalog = ArtifactCatalog::with_sources(&sources);
        for (kind, url) in [
            (ArtifactKind::WineMono, "http://mirror.example/mono.msi"),
            (
                ArtifactKind::DotnetDesktop,
                "https://mirror.example/dotnet.exe",
            ),
        ] {
            let spec = catalog.spec(kind);
            assert!(spec.is_custom());
            assert_eq!(spec.request_url(), url);
            assert_eq!(spec.integrity, IntegrityPolicy::Unverified);
        }
        for spec in &catalog.specs {
            if matches!(
                spec.kind,
                ArtifactKind::WineMono | ArtifactKind::DotnetDesktop
            ) {
                continue;
            }
            if spec.kind == ArtifactKind::BaseGameWrapper {
                assert!(spec.is_custom());
                assert_eq!(spec.request_url(), "https://mirror.example/d2.exe");
                assert_eq!(spec.expected_filename, None);
                assert_eq!(spec.redirects, RedirectPolicy::AnyHttp);
                assert_eq!(
                    spec.maximum_bytes(),
                    crate::sources::CUSTOM_ARTIFACT_MAX_BYTES
                );
            } else {
                assert_eq!(spec, builtin.spec(spec.kind));
                assert!(!spec.is_custom());
            }
        }
    }

    #[test]
    fn a_second_mutator_cannot_own_the_managed_installation() {
        let fixture = TempDir::new().expect("fixture");
        let lock_path = fixture.path().join(".pd2launcher.lock");
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)
            .expect("lock file");
        File::try_lock(&lock).expect("fixture owns exclusive lease");
        let auditor = ScriptedAuditor::new([audit(
            InstallationStatus::NeedsBaseGame,
            CheckStatus::Satisfied,
        )]);
        let source = FixtureArtifactSource::default();
        let driver = RecordingDriver::default();
        let client = RecordingClientSynchronizer::default();
        let mut installer =
            ManagedInstaller::new(&auditor, &source, &driver, &client, fixture_catalog());

        assert_eq!(
            installer.run(fixture.path(), None),
            Err(InstallError::InstallationBusy)
        );
    }

    #[test]
    fn artifact_cache_cannot_escape_through_a_symlink() {
        let fixture = TempDir::new().expect("fixture");
        let outside = TempDir::new().expect("outside fixture");
        std::os::unix::fs::symlink(outside.path(), fixture.path().join("cache"))
            .expect("cache symlink");
        let auditor = ScriptedAuditor::new([audit(
            InstallationStatus::NeedsBaseGame,
            CheckStatus::Missing,
        )]);
        let source = FixtureArtifactSource::default();
        let driver = RecordingDriver::default();
        let client = RecordingClientSynchronizer::default();
        let mut installer =
            ManagedInstaller::new(&auditor, &source, &driver, &client, fixture_catalog());

        // The Download result lives in the cache too: nothing is read or
        // written through the link.
        assert_eq!(
            installer.run(fixture.path(), None),
            Err(InstallError::StageFailed {
                stage: InstallStage::Download,
            })
        );
        assert_eq!(
            outside
                .path()
                .read_dir()
                .expect("outside remains readable")
                .count(),
            0
        );
    }

    fn audit(status: InstallationStatus, runtime: CheckStatus) -> InstallAudit {
        InstallAudit::new(status, HostSupport::Supported, runtime, runtime)
    }

    /// The audits of a fresh install through all three steps: Download
    /// (which sets up Wine for Blizzard's downloader), Install Diablo II,
    /// and Install Project Diablo 2.
    fn fresh_journey() -> [InstallAudit; 7] {
        [
            audit(InstallationStatus::NeedsBaseGame, CheckStatus::Missing),
            audit(InstallationStatus::NeedsBaseGame, CheckStatus::Satisfied),
            audit(InstallationStatus::NeedsBaseGame, CheckStatus::Satisfied),
            audit(InstallationStatus::NeedsExpansion, CheckStatus::Satisfied),
            audit(InstallationStatus::NeedsClient, CheckStatus::Satisfied),
            audit(InstallationStatus::NeedsClient, CheckStatus::Satisfied),
            audit(InstallationStatus::Ready, CheckStatus::Satisfied),
        ]
    }

    struct ScriptedAuditor {
        reports: RefCell<VecDeque<InstallAudit>>,
    }

    impl ScriptedAuditor {
        fn new(reports: impl IntoIterator<Item = InstallAudit>) -> Self {
            Self {
                reports: RefCell::new(reports.into_iter().collect()),
            }
        }

        fn remaining(&self) -> usize {
            self.reports.borrow().len()
        }
    }

    impl InstallationAuditor for ScriptedAuditor {
        fn audit(&self, _paths: &ManagedPaths) -> Result<InstallAudit, AdapterFailure> {
            self.reports
                .borrow_mut()
                .pop_front()
                .ok_or_else(|| AdapterFailure::here())
        }
    }

    #[derive(Default)]
    struct FixtureArtifactSource {
        acquisitions: RefCell<Vec<ArtifactKind>>,
    }

    impl ArtifactSource for FixtureArtifactSource {
        fn acquire_at(
            &self,
            spec: &ArtifactSpec,
            cache: &Path,
            _cache_directory: &Directory,
            _control: &OperationControl,
        ) -> Result<AcquiredArtifact, AdapterFailure> {
            self.acquisitions.borrow_mut().push(spec.kind);
            fs::create_dir_all(cache).map_err(|_| AdapterFailure::here())?;
            let path = cache.join(format!("{:?}.artifact", spec.kind));
            let bytes = if spec.expected_filename.is_some() {
                minimal_pe()
            } else {
                b"fixture".to_vec()
            };
            assert!(bytes.len() as u64 <= spec.maximum_bytes());
            assert!(spec.approves_final_url(spec.request_url()));
            fs::write(&path, &bytes).map_err(|_| AdapterFailure::here())?;
            let acquired = AcquiredArtifact::new(
                path,
                spec.request_url(),
                spec.request_url(),
                spec.expected_filename.map(str::to_owned),
            );
            Ok(if spec.expected_filename.is_some() {
                acquired.with_retained_receipt_sha256(hex(&Sha256::digest(&bytes)))
            } else {
                acquired
            })
        }
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum RecordedOperation {
        ManagedRuntime,
        BaseGameDownload,
        ExpansionDownload,
        /// One direct download, of the products named.
        DirectDownload {
            base_game: bool,
            expansion: bool,
        },
        BaseGame,
        Expansion,
        Import(ImportMode),
        ClientBootstrap,
    }

    #[derive(Default)]
    struct RecordingDriver {
        operations: RefCell<Vec<RecordedOperation>>,
        received_base_secret: RefCell<Option<bool>>,
        received_expansion_secret: RefCell<Option<bool>>,
        markers: RefCell<Vec<crate::stage_marker::MarkerState>>,
        fail_expansion: bool,
        /// The failure a failing expansion run reports.
        expansion_failure: Option<VendorFailure>,
        /// How many base-game downloads fail in the downloader wrapper.
        wrapper_download_failures: std::cell::Cell<usize>,
        /// What those failures are; WrapperDownload when none is set.
        wrapper_download_failure: std::cell::Cell<Option<VendorFailure>>,
        wrapper_download_stalls: std::cell::Cell<usize>,
        /// What each direct download in turn reports; none fail when empty.
        direct_failures: RefCell<VecDeque<Vec<(ArtifactKind, VendorFailure)>>>,
        fail_import: bool,
        /// The import writes the records the real driver writes (a folder
        /// used in place, and the import record).
        writes_records: bool,
    }

    impl InstallDriver for RecordingDriver {
        fn provision_runtime(
            &self,
            paths: &ManagedPaths,
            wine: Option<&VerifiedArtifact>,
            prefix: PrefixArtifacts<'_>,
            _mutation_lease: &File,
            _control: &OperationControl,
        ) -> Result<(), AdapterFailure> {
            if let Some(wine) = wine {
                assert_eq!(wine.kind(), ArtifactKind::ManagedRuntime);
                assert!(wine.path().is_file());
                assert_eq!(wine.observed_sha256(), FIXTURE_SHA256);
                assert_eq!(wine.requested_url(), wine.final_url());
            }
            assert!(
                prefix
                    .gecko
                    .is_none_or(|gecko| gecko.kind() == ArtifactKind::Gecko)
            );
            assert!(
                prefix
                    .mono
                    .is_none_or(|mono| mono.kind() == ArtifactKind::WineMono)
            );
            assert!(
                prefix
                    .dotnet
                    .is_none_or(|dotnet| dotnet.kind() == ArtifactKind::DotnetDesktop)
            );
            assert!(paths.runtime().starts_with(paths.root()));
            assert!(paths.prefix().starts_with(paths.root()));
            assert!(paths.game().starts_with(paths.prefix()));
            self.markers
                .borrow_mut()
                .push(crate::stage_marker::read(paths.directory()));
            self.operations
                .borrow_mut()
                .push(RecordedOperation::ManagedRuntime);
            Ok(())
        }

        fn download_vendor_payload(
            &self,
            paths: &ManagedPaths,
            wrapper: &VerifiedArtifact,
            _mutation_lease: &File,
            _control: &OperationControl,
        ) -> Result<(), VendorFailure> {
            assert!(wrapper.path().is_file());
            self.markers
                .borrow_mut()
                .push(crate::stage_marker::read(paths.directory()));
            if wrapper.kind() == ArtifactKind::BaseGameWrapper
                && self.wrapper_download_failures.get() > 0
            {
                self.wrapper_download_failures
                    .set(self.wrapper_download_failures.get() - 1);
                return Err(self
                    .wrapper_download_failure
                    .get()
                    .unwrap_or(VendorFailure::WrapperDownload));
            }
            if wrapper.kind() == ArtifactKind::BaseGameWrapper
                && self.wrapper_download_stalls.get() > 0
            {
                self.wrapper_download_stalls
                    .set(self.wrapper_download_stalls.get() - 1);
                return Err(VendorFailure::DownloadStalled);
            }
            // The verified payload lands in the product's cache folder.
            let directory = match wrapper.kind() {
                ArtifactKind::BaseGameWrapper => "d2-payload",
                _ => "lod-payload",
            };
            let payload = paths.root().join("cache/artifacts").join(directory);
            fs::create_dir_all(&payload).map_err(|_| VendorFailure::Install)?;
            fs::write(payload.join("Installer.exe"), minimal_pe())
                .map_err(|_| VendorFailure::Install)?;
            self.operations.borrow_mut().push(match wrapper.kind() {
                ArtifactKind::BaseGameWrapper => RecordedOperation::BaseGameDownload,
                _ => RecordedOperation::ExpansionDownload,
            });
            Ok(())
        }

        fn download_vendor_payloads_direct(
            &self,
            paths: &ManagedPaths,
            wrappers: &[&VerifiedArtifact],
            _control: &OperationControl,
        ) -> Vec<(ArtifactKind, VendorFailure)> {
            let failures = self
                .direct_failures
                .borrow_mut()
                .pop_front()
                .unwrap_or_default();
            let named = |kind| wrappers.iter().any(|wrapper| wrapper.kind() == kind);
            self.operations
                .borrow_mut()
                .push(RecordedOperation::DirectDownload {
                    base_game: named(ArtifactKind::BaseGameWrapper),
                    expansion: named(ArtifactKind::ExpansionWrapper),
                });
            for wrapper in wrappers {
                assert!(wrapper.path().is_file());
                if failures.iter().any(|(kind, _)| *kind == wrapper.kind()) {
                    continue;
                }
                let directory = match wrapper.kind() {
                    ArtifactKind::BaseGameWrapper => "d2-payload",
                    _ => "lod-payload",
                };
                let payload = paths.root().join("cache/artifacts").join(directory);
                fs::create_dir_all(&payload).unwrap();
                fs::write(payload.join("Installer.exe"), minimal_pe()).unwrap();
            }
            failures
        }

        fn import_existing(
            &self,
            paths: &ManagedPaths,
            existing: &ExistingInstall,
            mode: ImportMode,
            _control: &OperationControl,
        ) -> Result<(), AdapterFailure> {
            self.markers
                .borrow_mut()
                .push(crate::stage_marker::read(paths.directory()));
            if self.fail_import {
                return Err(AdapterFailure::here());
            }
            self.operations
                .borrow_mut()
                .push(RecordedOperation::Import(mode));
            if self.writes_records {
                let prefix = paths
                    .directory()
                    .ensure_directory(Path::new("prefix"))
                    .map_err(|_| AdapterFailure::here())?;
                if mode == ImportMode::InPlace {
                    crate::game_folder::write_pointer(&prefix, existing.folder())
                        .map_err(|_| AdapterFailure::here())?;
                }
                crate::game_folder::write_import_record(&prefix, mode, r"G:\Diablo II")
                    .map_err(|_| AdapterFailure::here())?;
            }
            Ok(())
        }

        fn install_vendor_native(
            &self,
            request: VendorInstallRequest<'_>,
            control: &OperationControl,
        ) -> Result<(), VendorFailure> {
            // The same record as Blizzard's installer: the stage, the key.
            self.install_vendor(request, &tempfile::tempfile().unwrap(), control)
        }

        fn install_vendor(
            &self,
            request: VendorInstallRequest<'_>,
            _mutation_lease: &File,
            _control: &OperationControl,
        ) -> Result<(), VendorFailure> {
            assert!(request.artifact().path().is_file());
            assert!(request.paths().game().starts_with(request.paths().prefix()));
            let was_present = request.with_product_key(|key| !key.is_empty());
            self.markers
                .borrow_mut()
                .push(crate::stage_marker::read(request.paths().directory()));
            if self.fail_expansion && request.product() == VendorProduct::Expansion {
                return Err(self.expansion_failure.unwrap_or(VendorFailure::Install));
            }
            match request.product() {
                VendorProduct::BaseGame => {
                    *self.received_base_secret.borrow_mut() = Some(was_present);
                    self.operations
                        .borrow_mut()
                        .push(RecordedOperation::BaseGame);
                }
                VendorProduct::Expansion => {
                    *self.received_expansion_secret.borrow_mut() = Some(was_present);
                    self.operations
                        .borrow_mut()
                        .push(RecordedOperation::Expansion);
                }
            }
            Ok(())
        }

        fn install_client_bootstrap(
            &self,
            _paths: &ManagedPaths,
            _bootstrap: &VerifiedArtifact,
            _mutation_lease: &File,
            _control: &OperationControl,
        ) -> Result<(), AdapterFailure> {
            self.operations
                .borrow_mut()
                .push(RecordedOperation::ClientBootstrap);
            Ok(())
        }
    }

    /// With the update checks off, a Project Diablo 2 in the game folder (an
    /// import's, of any version) is accepted as it is: no update, no PD2
    /// installer. When it cannot be, the step fails and the PD2 installer
    /// does not replace the version the player keeps.
    #[test]
    fn with_the_update_checks_off_an_imported_project_diablo_is_kept_as_it_is() {
        let source = FixtureArtifactSource::default();
        let fixture = TempDir::new().unwrap();
        let project_d2 = fixture
            .path()
            .join("prefix/drive_c/Program Files (x86)/Diablo II/ProjectD2");
        fs::create_dir_all(&project_d2).unwrap();
        for file in ["Game.exe", "ProjectDiablo.dll"] {
            fs::write(project_d2.join(file), b"an older pd2").unwrap();
        }
        let run = |accept_fails: bool, audits: Vec<InstallationStatus>| {
            let driver = RecordingDriver::default();
            let client = RecordingClientSynchronizer::default();
            *client.accept_fails.borrow_mut() = accept_fails;
            let auditor = ScriptedAuditor::new(
                audits
                    .into_iter()
                    .map(|status| audit(status, CheckStatus::Satisfied).with_android_host()),
            );
            let outcome =
                ManagedInstaller::new(&auditor, &source, &driver, &client, fixture_catalog())
                    .with_client_as_installed(true)
                    .install_project_diablo(fixture.path(), &OperationControl::default());
            let bootstrap = driver
                .operations
                .borrow()
                .contains(&RecordedOperation::ClientBootstrap);
            let accepted = *client.accepted.borrow();
            let synchronized = *client.calls.borrow();
            (outcome, bootstrap, accepted, synchronized)
        };

        let (outcome, bootstrap, accepted, synchronized) = run(
            false,
            vec![InstallationStatus::NeedsClient, InstallationStatus::Ready],
        );
        let outcome = outcome.unwrap();
        assert_eq!(outcome.final_status(), InstallationStatus::Ready);
        assert_eq!(
            outcome.completed_stages(),
            &[InstallStage::ClientSynchronization]
        );
        assert_eq!((bootstrap, accepted, synchronized), (false, 1, 0));

        let (outcome, bootstrap, accepted, synchronized) =
            run(true, vec![InstallationStatus::NeedsClient]);
        assert!(matches!(outcome, Err(InstallError::ClientNotKept)));
        assert_eq!((bootstrap, accepted, synchronized), (false, 1, 0));

        // A ProjectD2 without its launch files is not replaced either.
        fs::remove_file(project_d2.join("ProjectDiablo.dll")).unwrap();
        let (outcome, bootstrap, accepted, synchronized) =
            run(false, vec![InstallationStatus::NeedsClient]);
        assert!(matches!(outcome, Err(InstallError::ClientNotKept)));
        assert_eq!((bootstrap, accepted, synchronized), (false, 0, 0));
    }

    /// What the recording synchronizer writes as an import's record.
    const COPIED_RECORD: &[u8] = b"{\"checksum\":[\"00000000000000000000000000000000  Game.exe\"]}";

    /// On the Android host an import records the Project Diablo 2 it copied,
    /// as it is, from the copied files (a record another PD2 Launcher left
    /// is replaced), and ends at Ready: no update, no download, no PD2
    /// installer and no file list from PD2's server, with the update checks
    /// on. The desktop leaves Project Diablo 2 to its own step.
    #[test]
    fn on_android_an_import_records_its_copied_project_diablo_and_never_updates() {
        let source = FixtureArtifactSource::default();
        // (final status, copied records, synchronized, accepted, prefetched,
        // PD2 installer, downloads)
        let import = |recorded_before: bool, android: bool| {
            let fixture = TempDir::new().unwrap();
            let existing = existing_install(&fixture);
            let root_path = fixture.path().join("managed");
            let project_d2 =
                root_path.join("prefix/drive_c/Program Files (x86)/Diablo II/ProjectD2");
            fs::create_dir_all(project_d2.join(".pd2launcher")).unwrap();
            for file in ["Game.exe", "ProjectDiablo.dll", "PD2Launcher.exe"] {
                fs::write(project_d2.join(file), b"an older pd2").unwrap();
            }
            if recorded_before {
                fs::write(
                    project_d2.join(".pd2launcher/accepted-manifest.json"),
                    b"{}",
                )
                .unwrap();
            }
            let after_import = if recorded_before {
                InstallationStatus::Ready
            } else {
                InstallationStatus::NeedsClient
            };
            let driver = RecordingDriver::default();
            let client = RecordingClientSynchronizer::default();
            let auditor = ScriptedAuditor::new(
                [
                    InstallationStatus::NeedsBaseGame,
                    after_import,
                    InstallationStatus::Ready,
                ]
                .map(|status| {
                    let report = audit(status, CheckStatus::Satisfied);
                    if android {
                        report.with_android_host()
                    } else {
                        report
                    }
                }),
            );
            let status =
                ManagedInstaller::new(&auditor, &source, &driver, &client, fixture_catalog())
                    .with_client_as_installed(false)
                    .import_existing(
                        &root_path,
                        &existing,
                        ImportMode::InPlace,
                        &OperationControl::default(),
                    )
                    .map(|outcome| outcome.final_status())
                    .unwrap();
            let bootstrap = driver
                .operations
                .borrow()
                .contains(&RecordedOperation::ClientBootstrap);
            // The mark names this record, once; another record does not count.
            let root = Directory::open(&root_path).unwrap();
            assert_eq!(is_copied_client_record(&root, COPIED_RECORD), android);
            assert!(!is_copied_client_record(&root, b"{}"));
            (
                status,
                *client.copied_records.borrow(),
                *client.calls.borrow(),
                *client.accepted.borrow(),
                *client.prefetches.borrow(),
                bootstrap,
                source.acquisitions.borrow().len(),
            )
        };

        assert_eq!(
            import(false, true),
            (InstallationStatus::Ready, 1, 0, 0, 0, false, 0)
        );
        assert_eq!(
            import(true, true),
            (InstallationStatus::Ready, 1, 0, 0, 0, false, 0)
        );
        assert_eq!(
            import(false, false),
            (InstallationStatus::NeedsClient, 0, 0, 0, 0, false, 0)
        );
    }

    /// The app's way: the folder copied into the app is used in place
    /// (ImportExistingInstall with IN_PLACE after SelectExistingInstall).
    /// The driver writes the in-place record, and the import then records
    /// the ProjectD2 of that folder, not of the Managed one, and ends at
    /// Ready: no Install Project Diablo 2, no synchronize, no download.
    #[test]
    fn on_android_an_import_in_place_records_the_project_diablo_of_the_folder_it_uses() {
        let fixture = TempDir::new().unwrap();
        let existing = existing_install(&fixture);
        let folder = fixture.path().join("My Games/Diablo II");
        fs::create_dir_all(folder.join("ProjectD2")).unwrap();
        for file in ["Game.exe", "ProjectDiablo.dll", "PD2Launcher.exe"] {
            fs::write(folder.join("ProjectD2").join(file), b"an older pd2").unwrap();
        }
        let root_path = fixture.path().join("managed");
        let source = FixtureArtifactSource::default();
        let driver = RecordingDriver {
            writes_records: true,
            ..RecordingDriver::default()
        };
        let client = RecordingClientSynchronizer::default();
        let auditor = ScriptedAuditor::new(
            [
                InstallationStatus::NeedsBaseGame,
                InstallationStatus::NeedsClient,
                InstallationStatus::Ready,
            ]
            .map(|status| audit(status, CheckStatus::Satisfied).with_android_host()),
        );
        let outcome = ManagedInstaller::new(&auditor, &source, &driver, &client, fixture_catalog())
            .import_existing(
                &root_path,
                &existing,
                ImportMode::InPlace,
                &OperationControl::default(),
            )
            .unwrap();
        assert_eq!(outcome.final_status(), InstallationStatus::Ready);
        assert_eq!(*client.copied_records.borrow(), 1);
        assert_eq!(*client.calls.borrow(), 0);
        assert_eq!(*client.prefetches.borrow(), 0);
        assert!(source.acquisitions.borrow().is_empty());
        // The record went to the folder used in place, and the mark names it.
        assert!(
            folder
                .join("ProjectD2/.pd2launcher/accepted-manifest.json")
                .is_file()
        );
        assert!(
            !root_path
                .join("prefix/drive_c/Program Files (x86)/Diablo II/ProjectD2")
                .exists()
        );
        let root = Directory::open(&root_path).unwrap();
        assert!(is_copied_client_record(&root, COPIED_RECORD));
    }

    /// On the Android host the Download step of an import fetches nothing of
    /// Project Diablo 2: neither its installer nor its client.
    #[test]
    fn on_android_the_download_for_an_import_fetches_nothing_of_project_diablo() {
        let fixture = TempDir::new().unwrap();
        let source = FixtureArtifactSource::default();
        let driver = RecordingDriver::default();
        let client = RecordingClientSynchronizer::default();
        let auditor = ScriptedAuditor::new([android_audit(
            InstallationStatus::NeedsBaseGame,
            CheckStatus::Satisfied,
        )]);
        ManagedInstaller::new(&auditor, &source, &driver, &client, fixture_catalog())
            .download_all(
                fixture.path(),
                DownloadPlan::ExistingInstall,
                &OperationControl::default(),
            )
            .expect("downloads verified");
        assert!(
            !source
                .acquisitions
                .borrow()
                .contains(&ArtifactKind::ClientBootstrap)
        );
        assert_eq!(*client.prefetches.borrow(), 0);
        assert!(driver.operations.borrow().is_empty());
    }

    /// `discard_downloads` before an import on the Android host: the
    /// import's Download then still fetches nothing (no Blizzard program, no
    /// PD2 installer, no client) and is complete.
    #[test]
    fn on_android_an_import_after_discard_downloads_fetches_nothing() {
        use crate::downloads::{DownloadNeed, readiness_on};
        let fixture = TempDir::new().unwrap();
        let paths = ManagedPaths::prepare(fixture.path()).unwrap();
        let cache = fixture.path().join("cache/artifacts");
        fs::create_dir_all(&cache).unwrap();
        for ours in ["pd2-bootstrap.verified", "artifact-02.partial"] {
            fs::write(cache.join(ours), b"ours").unwrap();
        }
        fs::create_dir_all(fixture.path().join("cache/client/objects")).unwrap();
        fs::write(fixture.path().join("cache/client/objects/a"), b"pd2").unwrap();
        adapters::discard_downloads(&paths).unwrap();
        assert!(!cache.join("pd2-bootstrap.verified").exists());
        assert!(!fixture.path().join("cache/client").exists());

        let source = FixtureArtifactSource::default();
        let driver = RecordingDriver::default();
        let client = RecordingClientSynchronizer::default();
        let auditor = ScriptedAuditor::new([android_audit(
            InstallationStatus::NeedsBaseGame,
            CheckStatus::Satisfied,
        )]);
        ManagedInstaller::new(&auditor, &source, &driver, &client, fixture_catalog())
            .download_all(
                fixture.path(),
                DownloadPlan::ExistingInstall,
                &OperationControl::default(),
            )
            .expect("nothing to download");
        assert!(source.acquisitions.borrow().is_empty());
        assert_eq!(*client.prefetches.borrow(), 0);
        assert!(driver.operations.borrow().is_empty());
        assert!(!cache.join("pd2-bootstrap.verified").exists());
        assert!(!fixture.path().join("cache/client").exists());
        assert!(
            readiness_on(&Directory::open(fixture.path()).unwrap(), true).serves(
                DownloadPlan::ExistingInstall,
                DownloadNeed::for_status(InstallationStatus::NeedsBaseGame, false)
            )
        );
    }

    /// A fresh app (no game, no earlier download): the import's Download
    /// fetches nothing and is complete, so the import follows and ends at
    /// Ready, with no network and no download.
    #[test]
    fn on_android_a_fresh_import_goes_from_its_download_to_ready_with_no_download() {
        use crate::downloads::{DownloadNeed, DownloadReadiness, readiness_on};
        let fixture = TempDir::new().unwrap();
        let existing = existing_install(&fixture);
        let folder = fixture.path().join("My Games/Diablo II");
        fs::create_dir_all(folder.join("ProjectD2")).unwrap();
        for file in ["Game.exe", "ProjectDiablo.dll", "PD2Launcher.exe"] {
            fs::write(folder.join("ProjectD2").join(file), b"an older pd2").unwrap();
        }
        let root_path = fixture.path().join("managed");
        let source = FixtureArtifactSource::default();
        let driver = RecordingDriver {
            writes_records: true,
            ..RecordingDriver::default()
        };
        let client = RecordingClientSynchronizer::default();
        let auditor = ScriptedAuditor::new(
            [
                // Download.
                InstallationStatus::NeedsBaseGame,
                // The import, after the copy, after the record.
                InstallationStatus::NeedsBaseGame,
                InstallationStatus::NeedsClient,
                InstallationStatus::Ready,
            ]
            .map(|status| audit(status, CheckStatus::Satisfied).with_android_host()),
        );
        let mut installer =
            ManagedInstaller::new(&auditor, &source, &driver, &client, fixture_catalog());
        installer
            .download_all(
                &root_path,
                DownloadPlan::ExistingInstall,
                &OperationControl::default(),
            )
            .expect("nothing to download");
        let readiness = readiness_on(&Directory::open(&root_path).unwrap(), true);
        assert!(matches!(
            readiness,
            DownloadReadiness::Complete {
                plan: DownloadPlan::ExistingInstall,
                total_bytes: 0,
                ..
            }
        ));
        assert!(readiness.serves(
            DownloadPlan::ExistingInstall,
            DownloadNeed::for_status(InstallationStatus::NeedsBaseGame, false)
        ));
        let outcome = installer
            .import_existing(
                &root_path,
                &existing,
                ImportMode::InPlace,
                &OperationControl::default(),
            )
            .unwrap();
        assert_eq!(outcome.final_status(), InstallationStatus::Ready);
        assert!(source.acquisitions.borrow().is_empty());
        assert_eq!(*client.prefetches.borrow(), 0);
        assert_eq!(*client.calls.borrow(), 0);
        assert_eq!(*client.copied_records.borrow(), 1);
    }

    #[derive(Default)]
    struct RecordingClientSynchronizer {
        calls: RefCell<usize>,
        prefetches: RefCell<usize>,
        accepted: RefCell<usize>,
        accept_fails: RefCell<bool>,
        /// Records of an import's copied client.
        copied_records: RefCell<usize>,
    }

    impl ClientSynchronizer for RecordingClientSynchronizer {
        fn prefetch(
            &self,
            paths: &ManagedPaths,
            _control: &OperationControl,
        ) -> Result<(), AdapterFailure> {
            *self.prefetches.borrow_mut() += 1;
            let cache = paths.root().join(CLIENT_PREFETCH);
            fs::create_dir_all(&cache).map_err(|_| AdapterFailure::here())?;
            fs::write(cache.join("manifest.json"), b"{}").map_err(|_| AdapterFailure::here())
        }

        fn synchronize(
            &self,
            _paths: &ManagedPaths,
            _control: &OperationControl,
        ) -> Result<(), AdapterFailure> {
            *self.calls.borrow_mut() += 1;
            Ok(())
        }

        fn accept_installed(
            &self,
            _paths: &ManagedPaths,
            _control: &OperationControl,
        ) -> Result<(), AdapterFailure> {
            *self.accepted.borrow_mut() += 1;
            if *self.accept_fails.borrow() {
                Err(AdapterFailure::here())
            } else {
                Ok(())
            }
        }

        fn record_copied(
            &self,
            paths: &ManagedPaths,
            _control: &OperationControl,
        ) -> Result<(), AdapterFailure> {
            *self.copied_records.borrow_mut() += 1;
            let record = paths.game().join("ProjectD2/.pd2launcher");
            fs::create_dir_all(&record).map_err(|_| AdapterFailure::here())?;
            fs::write(record.join("accepted-manifest.json"), COPIED_RECORD)
                .map_err(|_| AdapterFailure::here())
        }
    }

    fn fixture_catalog() -> ArtifactCatalog {
        ArtifactCatalog::new([
            fixed_spec(ArtifactKind::ManagedRuntime),
            fixed_spec(ArtifactKind::Gecko),
            fixed_spec(ArtifactKind::WineMono),
            fixed_spec(ArtifactKind::DotnetDesktop),
            wrapper_spec(ArtifactKind::BaseGameWrapper),
            wrapper_spec(ArtifactKind::ExpansionWrapper),
            fixed_spec(ArtifactKind::ClientBootstrap),
            fixed_spec(ArtifactKind::FexPackage),
            fixed_spec(ArtifactKind::FexRootFs),
        ])
    }

    fn fixed_spec(kind: ArtifactKind) -> ArtifactSpec {
        ArtifactSpec {
            kind,
            request_url: Cow::Borrowed(fixture_url(kind)),
            redirects: RedirectPolicy::None,
            size: SizePolicy::Exact(7),
            integrity: IntegrityPolicy::Sha256(FIXTURE_SHA256),
            expected_filename: None,
            fallback_urls: &[],
        }
    }

    fn wrapper_spec(kind: ArtifactKind) -> ArtifactSpec {
        let filename = match kind {
            ArtifactKind::BaseGameWrapper => "Downloader_Diablo2_enUS.exe",
            ArtifactKind::ExpansionWrapper => "Downloader_Diablo2_Lord_of_Destruction_enUS.exe",
            _ => panic!("wrapper kind expected"),
        };
        ArtifactSpec {
            kind,
            request_url: Cow::Borrowed(fixture_url(kind)),
            redirects: RedirectPolicy::None,
            size: SizePolicy::Bounded { min: 64, max: 256 },
            integrity: IntegrityPolicy::PeExecutable,
            expected_filename: Some(filename),
            fallback_urls: &[],
        }
    }

    fn fixture_url(kind: ArtifactKind) -> &'static str {
        match kind {
            ArtifactKind::ManagedRuntime => "https://fixtures.invalid/runtime",
            ArtifactKind::Gecko => "https://fixtures.invalid/gecko",
            ArtifactKind::WineMono => "https://fixtures.invalid/mono",
            ArtifactKind::DotnetDesktop => "https://fixtures.invalid/dotnet",
            ArtifactKind::BaseGameWrapper => "https://fixtures.invalid/d2",
            ArtifactKind::ExpansionWrapper => "https://fixtures.invalid/lod",
            ArtifactKind::ClientBootstrap => "https://fixtures.invalid/bootstrap",
            ArtifactKind::FexPackage => "https://fixtures.invalid/fex.deb",
            ArtifactKind::FexRootFs => "https://fixtures.invalid/rootfs.sqsh",
        }
    }

    fn minimal_pe() -> Vec<u8> {
        let mut bytes = vec![0_u8; 128];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[0x3c..0x40].copy_from_slice(&64_u32.to_le_bytes());
        bytes[64..68].copy_from_slice(b"PE\0\0");
        bytes
    }
}
