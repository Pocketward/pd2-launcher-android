//! User-overridable download sources.
//!
//! Every downloadable input has a built-in, pinned source. A user may replace
//! any of them with their own URL, either through a `PD2_URL_*` environment
//! variable (also read from `.env.local`) or through the Diagnostics page,
//! whose value is persisted in Launcher settings. Precedence is
//! GUI setting > environment > built-in. A custom URL is used exactly as given
//! and its content is deliberately *not* integrity-verified; only non-integrity
//! safety (size cap, bounded staging, extraction rules) still applies.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use zed_reqwest::Url;

/// Upper bound for any custom-source download. Generous enough for a full
/// Wine build or an offline installer, but it still stops a hostile or broken
/// server from filling the disk.
pub(crate) const CUSTOM_ARTIFACT_MAX_BYTES: u64 = 4 * 1024 * 1024 * 1024;
pub(crate) const MAX_URL_BYTES: usize = 2048;
const RECEIPT_SCHEMA: u32 = 1;
const CUSTOM_PROVENANCE: &str = "custom-unverified";

/// One user-overridable download.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum DownloadArtifact {
    Wine,
    Gecko,
    WineMono,
    DotnetDesktop,
    D2Installer,
    LodInstaller,
    Pd2Installer,
    Pd2ClientBase,
    /// The built-in FEX's package (ARM64 only).
    Fex,
    /// The built-in FEX's RootFS (ARM64 only).
    FexRootFs,
}

impl DownloadArtifact {
    pub(crate) const ALL: [Self; 10] = [
        Self::Wine,
        Self::Gecko,
        Self::WineMono,
        Self::DotnetDesktop,
        Self::D2Installer,
        Self::LodInstaller,
        Self::Pd2Installer,
        Self::Pd2ClientBase,
        Self::Fex,
        Self::FexRootFs,
    ];

    /// Whether this build ever downloads it: the built-in FEX only on ARM64.
    pub(crate) const fn applies(self) -> bool {
        !matches!(self, Self::Fex | Self::FexRootFs) || cfg!(target_arch = "aarch64")
    }

    pub(crate) const fn environment_name(self) -> &'static str {
        match self {
            Self::Wine => "PD2_URL_WINE",
            Self::Gecko => "PD2_URL_GECKO",
            Self::WineMono => "PD2_URL_MONO",
            Self::DotnetDesktop => "PD2_URL_DOTNET_DESKTOP",
            Self::D2Installer => "PD2_URL_D2_INSTALLER",
            Self::LodInstaller => "PD2_URL_LOD_INSTALLER",
            Self::Pd2Installer => "PD2_URL_PD2_INSTALLER",
            Self::Pd2ClientBase => "PD2_URL_PD2_CLIENT_BASE",
            Self::Fex => "PD2_URL_FEX",
            Self::FexRootFs => "PD2_URL_FEX_ROOTFS",
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Wine => "Wine runtime",
            Self::Gecko => "Wine Gecko",
            Self::WineMono => "Wine Mono",
            Self::DotnetDesktop => ".NET Desktop Runtime 6",
            Self::D2Installer => "Diablo II installer",
            Self::LodInstaller => "Lord of Destruction installer",
            Self::Pd2Installer => "Project Diablo 2 installer",
            Self::Pd2ClientBase => "Project Diablo 2 client files",
            Self::Fex => "Built-in FEX",
            Self::FexRootFs => "Built-in FEX RootFS (Ubuntu 24.04)",
        }
    }

    /// The reviewed, pinned source used when no override is in effect.
    pub(crate) fn builtin_url(self) -> String {
        use crate::install::{ArtifactCatalog, ArtifactKind};
        let kind = match self {
            Self::Wine => ArtifactKind::ManagedRuntime,
            Self::Gecko => ArtifactKind::Gecko,
            Self::WineMono => ArtifactKind::WineMono,
            Self::DotnetDesktop => ArtifactKind::DotnetDesktop,
            Self::D2Installer => ArtifactKind::BaseGameWrapper,
            Self::LodInstaller => ArtifactKind::ExpansionWrapper,
            Self::Pd2Installer => ArtifactKind::ClientBootstrap,
            Self::Fex => ArtifactKind::FexPackage,
            Self::FexRootFs => ArtifactKind::FexRootFs,
            Self::Pd2ClientBase => return crate::manifest::PD2_ORIGIN.to_owned(),
        };
        ArtifactCatalog::pinned().request_url(kind).to_owned()
    }

    fn from_environment_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|artifact| artifact.environment_name() == name)
    }
}

/// Where the effective URL of one artifact came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SourceOrigin {
    BuiltIn,
    Environment,
    Settings,
}

/// Effective download sources after applying the documented precedence.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct DownloadSources {
    custom: BTreeMap<DownloadArtifact, (String, SourceOrigin)>,
}

impl DownloadSources {
    #[cfg(test)]
    pub(crate) fn builtin() -> Self {
        Self::default()
    }

    /// GUI (persisted settings) wins over the environment, which wins over
    /// the built-in source. Values that are not valid custom URLs are ignored
    /// so a malformed override can never weaken a built-in source, and a
    /// value naming the built-in source itself is not an override at all: it
    /// would otherwise disable the built-in source's pins.
    pub(crate) fn resolve(
        settings: &BTreeMap<DownloadArtifact, String>,
        environment: &BTreeMap<DownloadArtifact, String>,
    ) -> Self {
        let mut custom = BTreeMap::new();
        for artifact in DownloadArtifact::ALL {
            let chosen = settings
                .get(&artifact)
                .and_then(|value| custom_override(artifact, value))
                .map(|url| (url, SourceOrigin::Settings))
                .or_else(|| {
                    environment
                        .get(&artifact)
                        .and_then(|value| custom_override(artifact, value))
                        .map(|url| (url, SourceOrigin::Environment))
                });
            if let Some(chosen) = chosen {
                custom.insert(artifact, chosen);
            }
        }
        Self { custom }
    }

    /// Drop overrides for the listed artifacts, restoring their built-in
    /// sources for one operation.
    pub(crate) fn without(mut self, artifacts: &[DownloadArtifact]) -> Self {
        for artifact in artifacts {
            self.custom.remove(artifact);
        }
        self
    }

    pub(crate) fn custom_url(&self, artifact: DownloadArtifact) -> Option<&str> {
        self.custom.get(&artifact).map(|(url, _)| url.as_str())
    }

    pub(crate) fn origin(&self, artifact: DownloadArtifact) -> SourceOrigin {
        self.custom
            .get(&artifact)
            .map_or(SourceOrigin::BuiltIn, |(_, origin)| *origin)
    }

    pub(crate) fn effective_url(&self, artifact: DownloadArtifact) -> String {
        self.custom_url(artifact)
            .map_or_else(|| artifact.builtin_url(), str::to_owned)
    }

    pub(crate) fn custom_artifacts(&self) -> impl Iterator<Item = (DownloadArtifact, &str)> {
        self.custom
            .iter()
            .map(|(artifact, (url, _))| (*artifact, url.as_str()))
    }
}

/// Rejection of a user-supplied URL. Carries no user text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InvalidSourceUrl {
    Empty,
    TooLong,
    Unparseable,
    UnsupportedScheme,
    MissingHost,
    BaseHasQuery,
}

impl InvalidSourceUrl {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::Empty => "source-url-empty",
            Self::TooLong => "source-url-too-long",
            Self::Unparseable => "source-url-invalid",
            Self::UnsupportedScheme => "source-url-must-be-http-or-https",
            Self::MissingHost => "source-url-needs-a-host",
            Self::BaseHasQuery => "source-base-url-cannot-have-query",
        }
    }
}

/// Accepts any absolute `http` or `https` URL with a host. Nothing about the
/// target is verified; this only keeps the value well-formed and bounded.
pub(crate) fn normalize_custom_url(
    artifact: DownloadArtifact,
    value: &str,
) -> Result<String, InvalidSourceUrl> {
    let value = value.trim();
    if value.is_empty() {
        return Err(InvalidSourceUrl::Empty);
    }
    if value.len() > MAX_URL_BYTES {
        return Err(InvalidSourceUrl::TooLong);
    }
    if value
        .chars()
        .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(InvalidSourceUrl::Unparseable);
    }
    let url = Url::parse(value).map_err(|_| InvalidSourceUrl::Unparseable)?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(InvalidSourceUrl::UnsupportedScheme);
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err(InvalidSourceUrl::MissingHost);
    }
    if artifact == DownloadArtifact::Pd2ClientBase {
        if url.query().is_some() || url.fragment().is_some() {
            return Err(InvalidSourceUrl::BaseHasQuery);
        }
        return Ok(url.as_str().trim_end_matches('/').to_owned());
    }
    Ok(value.to_owned())
}

/// The normalized override for `value`, or `None` when it is invalid or
/// names the built-in source itself (compared as parsed URLs, so case and
/// default-port spellings of the built-in URL are recognized too).
pub(crate) fn custom_override(artifact: DownloadArtifact, value: &str) -> Option<String> {
    let normalized = normalize_custom_url(artifact, value).ok()?;
    (!is_builtin_url(artifact, &normalized)).then_some(normalized)
}

/// Whether a normalized URL is the built-in source of `artifact`.
pub(crate) fn is_builtin_url(artifact: DownloadArtifact, normalized: &str) -> bool {
    let builtin = artifact.builtin_url();
    let builtin = normalize_custom_url(artifact, &builtin).unwrap_or(builtin);
    normalized == builtin
        || matches!(
            (Url::parse(normalized), Url::parse(&builtin)),
            (Ok(left), Ok(right)) if left == right
        )
}

/// Host only, for diagnostics: no scheme, path, query string, or credentials.
pub(crate) fn redacted_host(url: &str) -> String {
    Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .filter(|host| valid_diagnostic_host(host))
        .unwrap_or_else(|| "unknown-host".to_owned())
}

/// Scheme, host, port, and path without credentials, query, or fragment, so a
/// signed-URL token is never copied into installation receipts.
pub(crate) fn redacted_url(url: &str) -> String {
    let Ok(mut parsed) = Url::parse(url) else {
        return "unknown".to_owned();
    };
    let _ = parsed.set_username("");
    let _ = parsed.set_password(None);
    parsed.set_query(None);
    parsed.set_fragment(None);
    parsed.to_string()
}

/// Whether `host` is a bounded DNS name or IP literal safe to persist in the
/// otherwise free-text-free diagnostic history.
pub(crate) fn valid_diagnostic_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 255
        && host.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'.' | b'-' | b':' | b'[' | b']')
        })
}

/// Parse `PD2_URL_*` assignments from process-environment style `NAME=value`
/// pairs. Unknown names are ignored.
pub(crate) fn overrides_from_pairs<'a>(
    pairs: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> BTreeMap<DownloadArtifact, String> {
    let mut overrides = BTreeMap::new();
    for (name, value) in pairs {
        if let Some(artifact) = DownloadArtifact::from_environment_name(name)
            && let Some(url) = custom_override(artifact, value)
        {
            overrides.entry(artifact).or_insert(url);
        }
    }
    overrides
}

/// Installation receipt for a component materialized from a custom source.
/// Built-in components keep their existing pinned-digest receipts.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CustomSourceReceipt {
    schema: u32,
    provenance: String,
    artifact: DownloadArtifact,
    url: String,
    sha256: String,
}

impl CustomSourceReceipt {
    pub(crate) fn new(artifact: DownloadArtifact, url: &str, sha256: &str) -> Self {
        Self {
            schema: RECEIPT_SCHEMA,
            provenance: CUSTOM_PROVENANCE.to_owned(),
            artifact,
            url: redacted_url(url),
            sha256: sha256.to_owned(),
        }
    }

    pub(crate) fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("receipt fields always encode")
    }

    /// Decode a receipt and require the fixed schema, provenance marker,
    /// expected component, and a well-formed digest.
    pub(crate) fn decode(bytes: &[u8], artifact: DownloadArtifact) -> Option<Self> {
        let receipt: Self = serde_json::from_slice(bytes).ok()?;
        (receipt.schema == RECEIPT_SCHEMA
            && receipt.provenance == CUSTOM_PROVENANCE
            && receipt.artifact == artifact
            && !receipt.url.is_empty()
            && receipt.url.len() <= MAX_URL_BYTES
            && receipt.sha256.len() == 64
            && receipt
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
        .then_some(receipt)
    }

    pub(crate) fn host(&self) -> String {
        redacted_host(&self.url)
    }

    #[cfg(test)]
    pub(crate) fn sha256(&self) -> &str {
        &self.sha256
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(entries: &[(DownloadArtifact, &str)]) -> BTreeMap<DownloadArtifact, String> {
        entries
            .iter()
            .map(|(artifact, url)| (*artifact, (*url).to_owned()))
            .collect()
    }

    #[test]
    fn gui_setting_wins_over_environment_which_wins_over_builtin() {
        let settings = map(&[(DownloadArtifact::Wine, "https://gui.example/wine.tar.xz")]);
        let environment = map(&[
            (DownloadArtifact::Wine, "https://env.example/wine.tar.xz"),
            (DownloadArtifact::Gecko, "https://env.example/gecko.msi"),
        ]);
        let sources = DownloadSources::resolve(&settings, &environment);

        assert_eq!(
            sources.custom_url(DownloadArtifact::Wine),
            Some("https://gui.example/wine.tar.xz")
        );
        assert_eq!(
            sources.origin(DownloadArtifact::Wine),
            SourceOrigin::Settings
        );
        assert_eq!(
            sources.custom_url(DownloadArtifact::Gecko),
            Some("https://env.example/gecko.msi")
        );
        assert_eq!(
            sources.origin(DownloadArtifact::Gecko),
            SourceOrigin::Environment
        );
        assert_eq!(sources.custom_url(DownloadArtifact::Pd2Installer), None);
        assert_eq!(
            sources.origin(DownloadArtifact::Pd2Installer),
            SourceOrigin::BuiltIn
        );
        assert_eq!(
            sources.effective_url(DownloadArtifact::Pd2Installer),
            DownloadArtifact::Pd2Installer.builtin_url()
        );
        assert_eq!(
            DownloadArtifact::Wine.builtin_url(),
            crate::runtime::current().archive_url
        );
        assert_eq!(
            DownloadArtifact::Pd2ClientBase.builtin_url(),
            crate::manifest::PD2_ORIGIN
        );
    }

    #[test]
    fn invalid_overrides_fall_back_instead_of_weakening_the_builtin() {
        let settings = map(&[(DownloadArtifact::Wine, "ftp://gui.example/wine")]);
        let environment = map(&[(DownloadArtifact::Wine, "not a url")]);
        let sources = DownloadSources::resolve(&settings, &environment);
        assert_eq!(sources.custom_url(DownloadArtifact::Wine), None);

        let settings = map(&[(DownloadArtifact::Wine, "file:///etc/passwd")]);
        let environment = map(&[(DownloadArtifact::Wine, "https://env.example/w.tar.xz")]);
        let sources = DownloadSources::resolve(&settings, &environment);
        assert_eq!(
            sources.origin(DownloadArtifact::Wine),
            SourceOrigin::Environment
        );
    }

    #[test]
    fn an_override_equal_to_the_builtin_source_keeps_the_builtin_pins() {
        for artifact in DownloadArtifact::ALL {
            let builtin = artifact.builtin_url();
            let spellings = [
                builtin.clone(),
                format!(" {builtin} "),
                builtin.replacen("https://", "HTTPS://", 1),
                builtin.replacen("http://", "HTTP://", 1),
            ];
            for spelling in spellings {
                if normalize_custom_url(artifact, &spelling).is_err() {
                    continue;
                }
                assert_eq!(custom_override(artifact, &spelling), None, "{spelling}");
                let layer = map(&[(artifact, spelling.as_str())]);
                for sources in [
                    DownloadSources::resolve(&layer, &BTreeMap::new()),
                    DownloadSources::resolve(&BTreeMap::new(), &layer),
                ] {
                    assert_eq!(
                        sources.origin(artifact),
                        SourceOrigin::BuiltIn,
                        "{spelling}"
                    );
                    assert_eq!(sources.custom_url(artifact), None);
                }
                assert!(
                    overrides_from_pairs([(artifact.environment_name(), spelling.as_str())])
                        .is_empty(),
                    "{spelling}"
                );
            }
        }
        // A built-in URL in settings never masks an environment override.
        let settings = map(&[(
            DownloadArtifact::Gecko,
            &DownloadArtifact::Gecko.builtin_url(),
        )]);
        let environment = map(&[(DownloadArtifact::Gecko, "https://env.example/g.msi")]);
        assert_eq!(
            DownloadSources::resolve(&settings, &environment).origin(DownloadArtifact::Gecko),
            SourceOrigin::Environment
        );
    }

    #[test]
    fn custom_urls_accept_http_and_https_but_nothing_else() {
        let plain = "http://mirror.example/pd2.exe".to_owned();
        assert_eq!(
            normalize_custom_url(DownloadArtifact::Pd2Installer, &plain).as_deref(),
            Ok(plain.as_str())
        );
        assert_eq!(
            normalize_custom_url(
                DownloadArtifact::Pd2Installer,
                " https://m.example/p.exe?t=1 "
            ),
            Ok("https://m.example/p.exe?t=1".to_owned())
        );
        for (value, error) in [
            ("", InvalidSourceUrl::Empty),
            ("relative/path", InvalidSourceUrl::Unparseable),
            ("ftp://m.example/p", InvalidSourceUrl::UnsupportedScheme),
            ("file:///tmp/p", InvalidSourceUrl::UnsupportedScheme),
            ("https://m.example/a b", InvalidSourceUrl::Unparseable),
        ] {
            assert_eq!(
                normalize_custom_url(DownloadArtifact::Pd2Installer, value),
                Err(error),
                "{value}"
            );
        }
        let long = format!("https://m.example/{}", "a".repeat(MAX_URL_BYTES));
        assert_eq!(
            normalize_custom_url(DownloadArtifact::Pd2Installer, &long),
            Err(InvalidSourceUrl::TooLong)
        );
        assert_eq!(
            normalize_custom_url(DownloadArtifact::Pd2ClientBase, "https://m.example/pd2/"),
            Ok("https://m.example/pd2".to_owned())
        );
        assert_eq!(
            normalize_custom_url(DownloadArtifact::Pd2ClientBase, "https://m.example/?x=1"),
            Err(InvalidSourceUrl::BaseHasQuery)
        );
    }

    #[test]
    fn diagnostics_and_receipts_never_keep_tokens_or_credentials() {
        let url = "https://user:secret@Mirror.Example:8443/wine.tar.xz?token=abc#frag";
        assert_eq!(redacted_host(url), "mirror.example");
        assert_eq!(redacted_url(url), "https://mirror.example:8443/wine.tar.xz");
        let receipt = CustomSourceReceipt::new(DownloadArtifact::Wine, url, &"a".repeat(64));
        let encoded = String::from_utf8(receipt.encode()).unwrap();
        assert!(!encoded.contains("secret"));
        assert!(!encoded.contains("token"));
        assert!(encoded.contains("custom-unverified"));
        assert_eq!(
            CustomSourceReceipt::decode(encoded.as_bytes(), DownloadArtifact::Wine),
            Some(receipt)
        );
        assert_eq!(
            CustomSourceReceipt::decode(encoded.as_bytes(), DownloadArtifact::Gecko),
            None
        );
    }

    #[test]
    fn environment_pairs_map_only_the_documented_names() {
        let overrides = overrides_from_pairs([
            ("PD2_URL_WINE", "https://a.example/w.tar.xz"),
            ("PD2_URL_UNKNOWN", "https://a.example/x"),
            // Retired with the 1.14d patch stage.
            ("PD2_URL_PATCH", "https://a.example/patch.exe"),
            ("PD2_URL_PD2_CLIENT_BASE", "https://a.example/pd2/"),
            ("PD2_URL_GECKO", "nonsense"),
            ("PD2_URL_MONO", "https://a.example/mono.msi"),
            ("PD2_URL_DOTNET_DESKTOP", "https://a.example/dotnet.exe"),
        ]);
        assert_eq!(
            overrides,
            map(&[
                (DownloadArtifact::Wine, "https://a.example/w.tar.xz"),
                (DownloadArtifact::WineMono, "https://a.example/mono.msi"),
                (
                    DownloadArtifact::DotnetDesktop,
                    "https://a.example/dotnet.exe"
                ),
                (DownloadArtifact::Pd2ClientBase, "https://a.example/pd2"),
            ])
        );
        assert_eq!(
            DownloadArtifact::WineMono.builtin_url(),
            "https://dl.winehq.org/wine/wine-mono/11.3.0/wine-mono-11.3.0-x86.msi"
        );
        assert_eq!(
            DownloadArtifact::DotnetDesktop.builtin_url(),
            "https://builds.dotnet.microsoft.com/dotnet/WindowsDesktop/6.0.36/windowsdesktop-runtime-6.0.36-win-x86.exe"
        );
        let names = DownloadArtifact::ALL.map(DownloadArtifact::environment_name);
        assert_eq!(
            names,
            [
                "PD2_URL_WINE",
                "PD2_URL_GECKO",
                "PD2_URL_MONO",
                "PD2_URL_DOTNET_DESKTOP",
                "PD2_URL_D2_INSTALLER",
                "PD2_URL_LOD_INSTALLER",
                "PD2_URL_PD2_INSTALLER",
                "PD2_URL_PD2_CLIENT_BASE",
                "PD2_URL_FEX",
                "PD2_URL_FEX_ROOTFS",
            ]
        );
    }
}
